/**************************************************************************/
/*  codex_bridge_service.cpp                                              */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/
/* Copyright (c) 2014-present Godot Engine contributors (see AUTHORS.md). */
/* Copyright (c) 2007-2014 Juan Linietsky, Ariel Manzur.                  */
/*                                                                        */
/* Permission is hereby granted, free of charge, to any person obtaining  */
/* a copy of this software and associated documentation files (the        */
/* "Software"), to deal in the Software without restriction, including    */
/* without limitation the rights to use, copy, modify, merge, publish,    */
/* distribute, sublicense, and/or sell copies of the Software, and to     */
/* permit persons to whom the Software is furnished to do so, subject to  */
/* the following conditions:                                              */
/*                                                                        */
/* The above copyright notice and this permission notice shall be         */
/* included in all copies or substantial portions of the Software.        */
/*                                                                        */
/* THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,        */
/* EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF     */
/* MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. */
/* IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY   */
/* CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,   */
/* TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE      */
/* SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.                 */
/**************************************************************************/

#include "codex_bridge_service.h"

#include "core/config/project_settings.h"
#include "core/crypto/crypto_core.h"
#include "core/io/json.h"
#include "core/object/callable_mp.h"
#include "core/os/os.h"
#include "core/string/print_string.h"
#include "core/templates/hash_set.h"
#include "editor/docks/inspector_dock.h"
#include "editor/editor_data.h"
#include "editor/editor_node.h"
#include "editor/editor_undo_redo_manager.h"
#include "editor/file_system/editor_file_system.h"

#include "modules/codex_bridge/editor/editor_context_adapter.h"
#include "modules/codex_bridge/protocol/bridge_crypto.h"

namespace {

// The chunk message carries both the structured payload and its exact canonical
// JSON checksum input. Keeping the payload target at 256 KiB leaves bounded
// headroom for JSON escaping under the 1 MiB frame limit.
static constexpr int SNAPSHOT_CHUNK_BYTES = 262144;
static constexpr int SNAPSHOT_ENTITY_LIMIT = 1000;
static constexpr int EDITOR_SNAPSHOT_CHUNKS_PER_FRAME = 2;
static constexpr uint64_t EDITOR_CHANGE_DEBOUNCE_USEC = 100000;
static_assert(
		MainThreadDispatcher::MAX_PROCESS_USEC_PER_FRAME >= SceneStateAdapter::FRAME_SAFETY_MARGIN_USEC + SceneStateAdapter::SCENE_BUDGET_USEC,
		"Scene scheduling must leave a nonnegative dispatcher lane.");
static_assert(
		MainThreadDispatcher::MAX_PROCESS_USEC_PER_FRAME >= ScriptGraphAdapter::FRAME_SAFETY_MARGIN_USEC + ScriptGraphAdapter::SCRIPT_BUDGET_USEC,
		"Script scheduling must leave a nonnegative dispatcher lane.");

static String sha256_hex_utf8(const String &p_value) {
	const CharString bytes = p_value.utf8();
	PackedByteArray digest;
	digest.resize(32);
	if (CryptoCore::sha256(reinterpret_cast<const uint8_t *>(bytes.get_data()), bytes.length(), digest.ptrw()) != OK) {
		return "0000000000000000000000000000000000000000000000000000000000000000";
	}
	return BridgeCrypto::bytes_to_lower_hex(digest);
}

static String make_snapshot_id() {
	PackedByteArray random;
	if (BridgeCrypto::random_bytes(16, random) != OK) {
		return "snapshot:00000000000000000000000000000000";
	}
	return "snapshot:" + BridgeCrypto::bytes_to_lower_hex(random);
}

} // namespace

CodexBridgeService *CodexBridgeService::singleton = nullptr;

void CodexBridgeService::_dispatch_command(const MainThreadDispatcher::Command &p_command, void *p_userdata) {
	CodexBridgeService *service = static_cast<CodexBridgeService *>(p_userdata);
	ERR_FAIL_NULL(service);

	switch (p_command.type) {
		case MainThreadDispatcher::COMMAND_NO_OP:
			break;
		case MainThreadDispatcher::COMMAND_INITIALIZE:
		case MainThreadDispatcher::COMMAND_CAPABILITIES: {
			Dictionary result;
			result["revisions"] = service->revision_clock.get_revision_vector();
			service->transport_worker.complete_request(p_command.request_id, result);
		} break;
		case MainThreadDispatcher::COMMAND_EDITOR_SNAPSHOT:
			service->_complete_snapshot(p_command.request_id, p_command.params);
			break;
		case MainThreadDispatcher::COMMAND_RESOURCE_SNAPSHOT: {
			Dictionary error_data;
			const Error error = service->resource_graph_adapter.begin_snapshot(p_command.request_id, OS::get_singleton()->get_ticks_usec(), service->_make_context(), error_data);
			if (error == ERR_BUSY) {
				service->transport_worker.complete_request_error(p_command.request_id, "resource_snapshot_in_progress", "A resource graph snapshot is already in progress.", true, error_data);
			} else if (error == ERR_OUT_OF_MEMORY) {
				service->transport_worker.complete_request_error(p_command.request_id, "resource_limit_exceeded", "The editor resource catalog exceeds a negotiated hard limit.", false);
			} else if (error != OK) {
				service->transport_worker.complete_request_error(p_command.request_id, "resource_catalog_building", "The editor resource catalog is still building.", true);
			}
		} break;
		case MainThreadDispatcher::COMMAND_RESOURCE_DELTA:
			service->_complete_resource_delta(p_command.request_id, (uint64_t)(int64_t)p_command.params["after_resource_revision"]);
			break;
		case MainThreadDispatcher::COMMAND_SCENE_SNAPSHOT: {
			Dictionary error_data;
			const Error error = service->scene_state_adapter.begin_snapshot(p_command.request_id, OS::get_singleton()->get_ticks_usec(), service->_make_context(), error_data);
			if (error == ERR_BUSY) {
				service->transport_worker.complete_request_error(p_command.request_id, "scene_snapshot_in_progress", "A scene graph snapshot is already in progress.", true, error_data);
			} else if (error != OK) {
				service->transport_worker.complete_request_error(p_command.request_id, "scene_catalog_building", "The editor scene catalog is still building.", true);
			}
		} break;
		case MainThreadDispatcher::COMMAND_SCENE_DELTA:
			service->_complete_scene_delta(p_command.request_id, (uint64_t)(int64_t)p_command.params["after_scene_graph_revision"]);
			break;
		case MainThreadDispatcher::COMMAND_SCRIPT_SNAPSHOT: {
			if (!ScriptSemanticAdapter::is_gdscript_available()) {
				service->transport_worker.complete_request_error(p_command.request_id, "capability_unavailable", "The saved-script semantic adapter is not available in this build.", false);
				break;
			}
			Dictionary error_data;
			const Error error = service->script_graph_adapter.begin_snapshot(p_command.request_id, OS::get_singleton()->get_ticks_usec(), service->_make_context(), error_data);
			if (error == ERR_BUSY) {
				service->transport_worker.complete_request_error(p_command.request_id, "script_snapshot_in_progress", "A script graph snapshot is already in progress.", true, error_data);
			} else if (error == ERR_OUT_OF_MEMORY) {
				service->transport_worker.complete_request_error(p_command.request_id, "script_limit_exceeded", "The editor script catalog exceeds a negotiated hard limit.", false);
			} else if (error != OK) {
				service->transport_worker.complete_request_error(p_command.request_id, "script_catalog_building", "The editor script catalog is still building.", true);
			}
		} break;
		case MainThreadDispatcher::COMMAND_SCRIPT_DELTA:
			if (!ScriptSemanticAdapter::is_gdscript_available()) {
				service->transport_worker.complete_request_error(p_command.request_id, "capability_unavailable", "The saved-script semantic adapter is not available in this build.", false);
			} else {
				service->_complete_script_delta(p_command.request_id, (uint64_t)(int64_t)p_command.params["after_script_graph_revision"]);
			}
			break;
		case MainThreadDispatcher::COMMAND_CANCEL: {
			const Array abandoned = service->resource_graph_adapter.cancel_snapshot(p_command.request_id);
			if (!abandoned.is_empty()) {
				service->transport_worker.abort_resource_snapshot(p_command.request_id, abandoned);
			}
			const Array abandoned_scene = service->scene_state_adapter.cancel_snapshot(p_command.request_id);
			if (!abandoned_scene.is_empty()) {
				service->transport_worker.abort_resource_snapshot(p_command.request_id, abandoned_scene);
			}
			const Array abandoned_script = service->script_graph_adapter.cancel_snapshot(p_command.request_id);
			if (!abandoned_script.is_empty()) {
				service->transport_worker.abort_resource_snapshot(p_command.request_id, abandoned_script);
			}
		} break;
		case MainThreadDispatcher::COMMAND_PING:
		case MainThreadDispatcher::COMMAND_SHUTDOWN:
			service->transport_worker.complete_request(p_command.request_id);
			break;
	}
}

Dictionary CodexBridgeService::_make_context() const {
	Dictionary context;
	context["project_id"] = transport_worker.get_project_id();
	context["editor_session_id"] = transport_worker.get_editor_session_id();
	return context;
}

String CodexBridgeService::_get_current_scene_id() const {
	return EditorContextAdapter::make_scene_id(transport_worker.get_editor_session_id(), EditorNode::get_editor_data().get_edited_scene_root());
}

void CodexBridgeService::_connect_editor_signals() {
	if (editor_signals_connected || !EditorNode::get_singleton()) {
		return;
	}
	EditorSelection *selection = EditorNode::get_singleton()->get_editor_selection();
	if (selection) {
		selection->connect(SNAME("selection_changed"), callable_mp(this, &CodexBridgeService::_on_selection_changed));
	}
	EditorNode::get_singleton()->connect(SNAME("scene_changed"), callable_mp(this, &CodexBridgeService::_on_scene_changed));
	EditorNode::get_singleton()->connect(SNAME("scene_closed"), callable_mp(this, &CodexBridgeService::_on_scene_closed));
	if (InspectorDock::get_singleton() && InspectorDock::get_inspector_singleton()) {
		InspectorDock::get_inspector_singleton()->connect(SNAME("property_edited"), callable_mp(this, &CodexBridgeService::_on_property_edited));
	}
	if (EditorUndoRedoManager::get_singleton()) {
		EditorUndoRedoManager::get_singleton()->connect(SNAME("version_changed"), callable_mp(this, &CodexBridgeService::_on_undo_redo_version_changed));
		EditorUndoRedoManager::get_singleton()->connect(SNAME("history_changed"), callable_mp(this, &CodexBridgeService::_on_undo_redo_version_changed));
	}
	if (EditorFileSystem::get_singleton()) {
		EditorFileSystem::get_singleton()->connect(SNAME("filesystem_changed"), callable_mp(this, &CodexBridgeService::_on_filesystem_changed));
		EditorFileSystem::get_singleton()->connect(SNAME("resources_reimported"), callable_mp(this, &CodexBridgeService::_on_resources_reimported));
		EditorFileSystem::get_singleton()->connect(SNAME("resources_reload"), callable_mp(this, &CodexBridgeService::_on_resources_reload));
	}
	if (ProjectSettings::get_singleton()) {
		ProjectSettings::get_singleton()->connect(SNAME("settings_changed"), callable_mp(this, &CodexBridgeService::_on_project_settings_changed));
	}
	editor_signals_connected = true;
}

void CodexBridgeService::_disconnect_editor_signals() {
	if (!editor_signals_connected || !EditorNode::get_singleton()) {
		return;
	}
	EditorSelection *selection = EditorNode::get_singleton()->get_editor_selection();
	if (selection && selection->is_connected(SNAME("selection_changed"), callable_mp(this, &CodexBridgeService::_on_selection_changed))) {
		selection->disconnect(SNAME("selection_changed"), callable_mp(this, &CodexBridgeService::_on_selection_changed));
	}
	if (EditorNode::get_singleton()->is_connected(SNAME("scene_changed"), callable_mp(this, &CodexBridgeService::_on_scene_changed))) {
		EditorNode::get_singleton()->disconnect(SNAME("scene_changed"), callable_mp(this, &CodexBridgeService::_on_scene_changed));
	}
	if (EditorNode::get_singleton()->is_connected(SNAME("scene_closed"), callable_mp(this, &CodexBridgeService::_on_scene_closed))) {
		EditorNode::get_singleton()->disconnect(SNAME("scene_closed"), callable_mp(this, &CodexBridgeService::_on_scene_closed));
	}
	if (InspectorDock::get_singleton() && InspectorDock::get_inspector_singleton() && InspectorDock::get_inspector_singleton()->is_connected(SNAME("property_edited"), callable_mp(this, &CodexBridgeService::_on_property_edited))) {
		InspectorDock::get_inspector_singleton()->disconnect(SNAME("property_edited"), callable_mp(this, &CodexBridgeService::_on_property_edited));
	}
	if (EditorUndoRedoManager::get_singleton() && EditorUndoRedoManager::get_singleton()->is_connected(SNAME("version_changed"), callable_mp(this, &CodexBridgeService::_on_undo_redo_version_changed))) {
		EditorUndoRedoManager::get_singleton()->disconnect(SNAME("version_changed"), callable_mp(this, &CodexBridgeService::_on_undo_redo_version_changed));
	}
	if (EditorUndoRedoManager::get_singleton() && EditorUndoRedoManager::get_singleton()->is_connected(SNAME("history_changed"), callable_mp(this, &CodexBridgeService::_on_undo_redo_version_changed))) {
		EditorUndoRedoManager::get_singleton()->disconnect(SNAME("history_changed"), callable_mp(this, &CodexBridgeService::_on_undo_redo_version_changed));
	}
	if (EditorFileSystem::get_singleton() && EditorFileSystem::get_singleton()->is_connected(SNAME("filesystem_changed"), callable_mp(this, &CodexBridgeService::_on_filesystem_changed))) {
		EditorFileSystem::get_singleton()->disconnect(SNAME("filesystem_changed"), callable_mp(this, &CodexBridgeService::_on_filesystem_changed));
	}
	if (EditorFileSystem::get_singleton() && EditorFileSystem::get_singleton()->is_connected(SNAME("resources_reimported"), callable_mp(this, &CodexBridgeService::_on_resources_reimported))) {
		EditorFileSystem::get_singleton()->disconnect(SNAME("resources_reimported"), callable_mp(this, &CodexBridgeService::_on_resources_reimported));
	}
	if (EditorFileSystem::get_singleton() && EditorFileSystem::get_singleton()->is_connected(SNAME("resources_reload"), callable_mp(this, &CodexBridgeService::_on_resources_reload))) {
		EditorFileSystem::get_singleton()->disconnect(SNAME("resources_reload"), callable_mp(this, &CodexBridgeService::_on_resources_reload));
	}
	if (ProjectSettings::get_singleton() && ProjectSettings::get_singleton()->is_connected(SNAME("settings_changed"), callable_mp(this, &CodexBridgeService::_on_project_settings_changed))) {
		ProjectSettings::get_singleton()->disconnect(SNAME("settings_changed"), callable_mp(this, &CodexBridgeService::_on_project_settings_changed));
	}
	editor_signals_connected = false;
}

void CodexBridgeService::_publish_event(const String &p_event_type, const String &p_property, bool p_scene_mutation, bool p_native_operation) {
	const String scene_id = _get_current_scene_id();
	if (p_native_operation) {
		revision_clock.record_native_operation(scene_id);
	} else if (p_scene_mutation && !scene_id.is_empty()) {
		revision_clock.record_scene_change(scene_id);
	} else {
		revision_clock.record_selection_change();
	}
	const Dictionary revisions = revision_clock.get_revision_vector();
	Dictionary params;
	params["event_seq"] = revisions["event_seq"];
	params["event_type"] = p_event_type;
	params["revisions"] = revisions;
	if (!scene_id.is_empty()) {
		params["scene_id"] = scene_id;
	}
	if (!p_property.is_empty()) {
		params["property"] = p_property;
	}
	Dictionary notification;
	notification["protocol_version"] = "1.1";
	notification["kind"] = "notification";
	notification["method"] = "sync.event";
	notification["params"] = params;
	notification["context"] = _make_context();
	if (!transport_worker.publish_notification(notification)) {
		const int64_t event_seq = revisions["event_seq"];
		Dictionary invalidated_params;
		invalidated_params["reason"] = "journal_overflow";
		invalidated_params["last_contiguous_event_seq"] = MAX((int64_t)0, event_seq - 1);
		Dictionary invalidated;
		invalidated["protocol_version"] = "1.1";
		invalidated["kind"] = "notification";
		invalidated["method"] = "sync.invalidated";
		invalidated["params"] = invalidated_params;
		invalidated["context"] = _make_context();
		transport_worker.publish_notification(invalidated);
	}
}

void CodexBridgeService::_on_selection_changed() {
	_publish_event("selection_changed");
}

void CodexBridgeService::_on_scene_changed() {
	_refresh_open_scene_ids(true);
	_publish_event("scene_changed", String(), true);
}

void CodexBridgeService::_on_scene_closed(const String &p_path) {
	(void)p_path;
	_refresh_open_scene_ids(true);
	_publish_event("scene_closed");
}

void CodexBridgeService::_on_property_edited(const String &p_property) {
	pending_property = p_property;
	scene_change_pending = true;
	scene_change_not_before_usec = OS::get_singleton()->get_ticks_usec() + EDITOR_CHANGE_DEBOUNCE_USEC;
}

bool CodexBridgeService::_observe_native_histories(bool p_record_changes) {
	EditorUndoRedoManager *manager = EditorUndoRedoManager::get_singleton();
	if (!manager) {
		return false;
	}
	EditorData &editor_data = EditorNode::get_editor_data();
	Vector<int> history_ids;
	HashSet<int> unique_ids;
	history_ids.push_back(EditorUndoRedoManager::GLOBAL_HISTORY);
	unique_ids.insert(EditorUndoRedoManager::GLOBAL_HISTORY);
	for (int scene_index = 0; scene_index < editor_data.get_edited_scene_count(); scene_index++) {
		const int history_id = editor_data.get_scene_history_id(scene_index);
		if (!unique_ids.has(history_id)) {
			history_ids.push_back(history_id);
			unique_ids.insert(history_id);
		}
	}
	bool changed = false;
	for (int history_id : history_ids) {
		if (!manager->has_history(history_id)) {
			continue;
		}
		UndoRedo *history = manager->get_history_undo_redo(history_id);
		if (!history) {
			continue;
		}
		NativeHistoryObservation current;
		current.action_count = history->get_history_count();
		current.current_action = history->get_current_action();
		current.version = history->get_version();
		const NativeHistoryObservation *previous = native_history_observations.getptr(history_id);
		if (previous && previous->action_count == current.action_count && previous->current_action == current.current_action && previous->version == current.version) {
			continue;
		}
		if (previous && p_record_changes) {
			String transition_kind = "unknown";
			if (current.action_count < previous->action_count) {
				transition_kind = "clear";
			} else if (current.current_action < previous->current_action) {
				transition_kind = "undo";
			} else if (current.current_action > previous->current_action && current.action_count == previous->action_count) {
				transition_kind = "redo";
			} else if (current.action_count > previous->action_count || current.version > previous->version) {
				transition_kind = "commit";
			}
			String scene_id;
			for (int scene_index = 0; scene_index < editor_data.get_edited_scene_count(); scene_index++) {
				if (editor_data.get_scene_history_id(scene_index) == history_id) {
					scene_id = EditorContextAdapter::make_scene_id(transport_worker.get_editor_session_id(), editor_data.get_edited_scene_root(scene_index));
					break;
				}
			}
			const uint64_t operation_seq = revision_clock.record_native_operation(scene_id);
			Dictionary summary;
			summary["transition_kind"] = transition_kind;
			summary["last_operation_seq"] = (int64_t)operation_seq;
			native_history_transitions[String::num_int64(history_id)] = summary;
			changed = true;
		}
		native_history_observations.insert(history_id, current);
	}
	return changed;
}

bool CodexBridgeService::_refresh_open_scene_ids(bool p_retire_missing) {
	HashSet<String> current_scene_ids;
	EditorData &editor_data = EditorNode::get_editor_data();
	for (int scene_index = 0; scene_index < editor_data.get_edited_scene_count(); scene_index++) {
		Node *scene_root = editor_data.get_edited_scene_root(scene_index);
		if (!scene_root) {
			continue;
		}
		current_scene_ids.insert(EditorContextAdapter::make_scene_id(transport_worker.get_editor_session_id(), scene_root));
	}
	bool retired = false;
	if (p_retire_missing) {
		for (const String &scene_id : observed_scene_ids) {
			if (!current_scene_ids.has(scene_id)) {
				revision_clock.retire_scene(scene_id);
				retired = true;
			}
		}
	}
	observed_scene_ids = current_scene_ids;
	return retired;
}

void CodexBridgeService::_on_undo_redo_version_changed() {
	native_operation_pending = _observe_native_histories(true) || native_operation_pending;
	scene_change_pending = true;
	scene_change_not_before_usec = OS::get_singleton()->get_ticks_usec() + EDITOR_CHANGE_DEBOUNCE_USEC;
}

void CodexBridgeService::_on_filesystem_changed() {
	resource_graph_adapter.request_refresh();
	scene_state_adapter.request_refresh();
	script_graph_adapter.request_refresh();
}

void CodexBridgeService::_on_resources_reimported(const Vector<String> &p_paths) {
	resource_graph_adapter.mark_reimported(p_paths);
	scene_state_adapter.request_refresh();
	script_graph_adapter.invalidate_saved_paths(p_paths);
	script_graph_adapter.request_refresh();
	_publish_event("resources_reimported");
}

void CodexBridgeService::_on_resources_reload(const PackedStringArray &p_paths) {
	// Reload signals identify affected paths, but a resumable full diff remains
	// the correctness fallback for moves, removals, and dependency fan-out.
	if (!p_paths.is_empty()) {
		resource_graph_adapter.request_refresh();
		scene_state_adapter.request_refresh();
		script_graph_adapter.invalidate_saved_paths(p_paths);
		script_graph_adapter.request_refresh();
	}
}

void CodexBridgeService::_on_project_settings_changed() {
	scene_state_adapter.invalidate_project_context();
}

void CodexBridgeService::_flush_scene_change() {
	scene_change_pending = false;
	scene_change_not_before_usec = 0;
	const bool native_operation = native_operation_pending;
	native_operation_pending = false;
	const String property = pending_property;
	pending_property.clear();
	_publish_event(native_operation ? "editor_operation" : (property.is_empty() ? "scene_changed" : "property_changed"), property, !native_operation, false);
}

void CodexBridgeService::_complete_snapshot(uint64_t p_request_id, const Dictionary &p_params) {
	_refresh_open_scene_ids(true);
	const Dictionary revisions = revision_clock.get_revision_vector();
	const bool full_live_context = String(p_params.get("_protocol_version", "1.1")) == "1.5";
	if (full_live_context) {
		PendingEditorSnapshot pending;
		pending.request_id = p_request_id;
		pending.revisions = revisions;
		pending.domains = p_params.get("domains", Array());
		pending_editor_snapshots.push_back(pending);
		return;
	}
	Dictionary snapshot;
	if (EditorContextAdapter::capture(transport_worker.get_project_id(), transport_worker.get_editor_session_id(), revisions, snapshot, full_live_context, native_history_transitions) != OK) {
		transport_worker.complete_request(p_request_id, Dictionary());
		return;
	}

	const String snapshot_id = make_snapshot_id();
	const Array entities = snapshot["entities"];
	Vector<Array> chunks;
	{
		Array current;
		for (int entity_index = 0; entity_index < entities.size(); entity_index++) {
			current.push_back(entities[entity_index]);
			Dictionary candidate;
			candidate["entities"] = current;
			if (JSON::stringify(candidate, "", true, true).utf8().length() > SNAPSHOT_CHUNK_BYTES && current.size() > 1) {
				const Variant last = current[current.size() - 1];
				current.resize(current.size() - 1);
				chunks.push_back(current.duplicate());
				current.clear();
				current.push_back(last);
			}
			if (current.size() >= SNAPSHOT_ENTITY_LIMIT) {
				chunks.push_back(current.duplicate());
				current.clear();
			}
		}
		if (!current.is_empty()) {
			chunks.push_back(current.duplicate());
		}
	}
	if (chunks.is_empty()) {
		chunks.push_back(Array());
	}

	Dictionary result;
	result["snapshot_id"] = snapshot_id;
	result["base_event_seq"] = revisions["event_seq"];
	result["revisions"] = revisions;
	Array domains;
	if (p_params.has("domains") && p_params["domains"].get_type() == Variant::ARRAY) {
		domains = p_params["domains"];
	} else {
		domains.push_back("editor_context");
		domains.push_back("editor_inspector");
	}
	result["domains"] = domains;
	Dictionary limits;
	limits["snapshot_chunk_bytes"] = SNAPSHOT_CHUNK_BYTES;
	limits["negotiated_snapshot_chunk_bytes"] = 524288;
	limits["variant_depth"] = EditorContextAdapter::MAX_VARIANT_DEPTH;
	limits["container_items"] = EditorContextAdapter::MAX_CONTAINER_ITEMS;
	limits["identity_characters"] = EditorContextAdapter::MAX_IDENTITY_CHARACTERS;
	limits["projected_value_bytes"] = EditorContextAdapter::MAX_PROJECTED_VALUE_BYTES;
	limits["inspector_bytes_per_node"] = EditorContextAdapter::MAX_INSPECTOR_BYTES_PER_NODE;
	limits["total_inspector_bytes"] = EditorContextAdapter::MAX_TOTAL_INSPECTOR_BYTES;
	if (full_live_context) {
		limits["open_scenes"] = EditorContextAdapter::MAX_OPEN_SCENES;
		limits["scene_nodes"] = EditorContextAdapter::MAX_SCENE_NODES;
		limits["selected_nodes"] = EditorContextAdapter::MAX_SELECTED_NODES;
		limits["inspector_properties"] = EditorContextAdapter::MAX_INSPECTOR_PROPERTIES;
	}
	limits["truncated"] = snapshot["truncated"];
	result["limits_applied"] = limits;

	Array messages;
	Dictionary begin_params;
	begin_params["snapshot_id"] = snapshot_id;
	begin_params["base_event_seq"] = revisions["event_seq"];
	begin_params["revisions"] = revisions;
	begin_params["chunk_count"] = chunks.size();
	Dictionary begin;
	begin["protocol_version"] = "1.1";
	begin["kind"] = "notification";
	begin["method"] = "snapshot.begin";
	begin["params"] = begin_params;
	begin["context"] = _make_context();
	messages.push_back(begin);

	String snapshot_checksum_input;
	for (int chunk_index = 0; chunk_index < chunks.size(); chunk_index++) {
		Dictionary payload;
		payload["entities"] = chunks[chunk_index];
		Dictionary chunk;
		chunk["protocol_version"] = "1.1";
		chunk["kind"] = "chunk";
		chunk["snapshot_id"] = snapshot_id;
		chunk["chunk_index"] = chunk_index;
		chunk["payload"] = payload;
		{
			const String payload_json = JSON::stringify(payload, "", true, true);
			const String checksum = sha256_hex_utf8(payload_json);
			snapshot_checksum_input += checksum;
			chunk["payload_json"] = payload_json;
			chunk["checksum"] = checksum;
		}
		chunk["context"] = _make_context();
		messages.push_back(chunk);
	}

	Dictionary end_params;
	end_params["snapshot_id"] = snapshot_id;
	end_params["chunk_count"] = chunks.size();
	end_params["entity_count"] = entities.size();
	{
		end_params["checksum"] = sha256_hex_utf8(snapshot_checksum_input);
	}
	end_params["revisions"] = revisions;
	Dictionary end;
	end["protocol_version"] = "1.1";
	end["kind"] = "notification";
	end["method"] = "snapshot.end";
	end["params"] = end_params;
	end["context"] = _make_context();
	messages.push_back(end);
	transport_worker.complete_request(p_request_id, result, messages);
}

void CodexBridgeService::_process_editor_snapshot() {
	if (pending_editor_snapshots.is_empty()) {
		return;
	}
	PendingEditorSnapshot &pending = pending_editor_snapshots.front()->get();
	if (pending.stage == PendingEditorSnapshot::STAGE_CAPTURE) {
		static const int capture_domains[] = {
			EditorContextAdapter::CAPTURE_CONTEXT,
			EditorContextAdapter::CAPTURE_INSPECTOR,
			EditorContextAdapter::CAPTURE_SCRIPTS,
			EditorContextAdapter::CAPTURE_HISTORY,
			EditorContextAdapter::CAPTURE_DIAGNOSTICS,
			EditorContextAdapter::CAPTURE_VIEWPORT,
		};
		const bool capture_context = capture_domains[pending.capture_domain] == EditorContextAdapter::CAPTURE_CONTEXT;
		Dictionary partial;
		if (EditorContextAdapter::capture(
					transport_worker.get_project_id(),
					transport_worker.get_editor_session_id(),
					pending.revisions,
					partial,
					true,
					native_history_transitions,
					capture_domains[pending.capture_domain],
					capture_context ? pending.context_scene_index : -1,
					capture_context ? 1 : -1,
					capture_context ? MAX(0, pending.context_node_index) : -1,
					capture_context ? (pending.context_node_index < 0 ? 0 : 1) : -1,
					!capture_context || pending.context_node_index < 0) != OK) {
			transport_worker.complete_request(pending.request_id, Dictionary());
			pending_editor_snapshots.pop_front();
			return;
		}
		const Array captured_entities = partial.get("entities", Array());
		if (capture_context) {
			for (const Variant &captured_variant : captured_entities) {
				const Dictionary captured = captured_variant;
				const String kind = captured.get("kind", String());
				if (kind == "scene") {
					pending.context_scene_entities.push_back(captured);
					continue;
				}
				if (kind != "editor_state") {
					if (kind == "node" && String(captured.get("node_path", String())) == ".") {
						const String scene_id = captured.get("scene_id", String());
						for (int scene_index = 0; scene_index < pending.context_scene_entities.size(); scene_index++) {
							Dictionary scene_entity = pending.context_scene_entities[scene_index];
							if (String(scene_entity.get("entity_id", String())) == scene_id) {
								scene_entity["root_node_id"] = captured.get("entity_id", String());
								pending.context_scene_entities[scene_index] = scene_entity;
								break;
							}
						}
					}
					pending.entities.push_back(captured);
					continue;
				}
				if (pending.editor_entity.is_empty()) {
					pending.editor_entity = captured.duplicate(true);
				} else {
					Dictionary merged = pending.editor_entity;
					for (const StringName &array_key : { SNAME("open_scene_ids"), SNAME("selected_node_ids") }) {
						Array values = merged.get(array_key, Array());
						const Array additions = captured.get(array_key, Array());
						for (const Variant &addition : additions) {
							if (!values.has(addition)) {
								values.push_back(addition);
							}
						}
						merged[array_key] = values;
					}
					const String captured_current_scene_id = captured.get("current_scene_id", String());
					if (!captured_current_scene_id.is_empty()) {
						merged["current_scene_id"] = captured_current_scene_id;
						merged["current_scene_dirty"] = captured.get("current_scene_dirty", false);
					}
					const Variant inspector_object_id = captured.get("inspector_object_id", Variant());
					if (inspector_object_id.get_type() != Variant::NIL) {
						merged["inspector_object_id"] = inspector_object_id;
					}
					merged["projected_selection_count"] = Array(merged.get("selected_node_ids", Array())).size();
					merged["open_scenes_truncated"] = (bool)merged.get("open_scenes_truncated", false) || (bool)captured.get("open_scenes_truncated", false);
					pending.editor_entity = merged;
				}
			}
			if (pending.context_scene_count < 0) {
				pending.context_scene_count = MIN((int)pending.editor_entity.get("open_scene_count", 0), EditorContextAdapter::MAX_OPEN_SCENES);
			}
			if (pending.context_scene_count > 0) {
				if (pending.context_node_index < 0) {
					pending.context_node_count = !pending.context_scene_entities.is_empty() ? (int)Dictionary(pending.context_scene_entities[pending.context_scene_entities.size() - 1]).get("node_count", 0) : 0;
					pending.context_node_index = 0;
				} else {
					pending.context_node_index++;
				}
				if (pending.context_node_index < pending.context_node_count) {
					pending.truncated = pending.truncated || (bool)partial.get("truncated", false);
					return;
				}
				pending.context_scene_index++;
				pending.context_node_index = -1;
				pending.context_node_count = -1;
			}
			if (pending.context_scene_index < pending.context_scene_count) {
				pending.truncated = pending.truncated || (bool)partial.get("truncated", false);
				return;
			}
			const String current_scene_id = pending.editor_entity.get("current_scene_id", String());
			const Array selected_node_ids = pending.editor_entity.get("selected_node_ids", Array());
			for (int scene_index = 0; scene_index < pending.context_scene_entities.size(); scene_index++) {
				Dictionary scene_entity = pending.context_scene_entities[scene_index];
				if (String(scene_entity.get("entity_id", String())) == current_scene_id) {
					scene_entity["selected_node_ids"] = selected_node_ids;
					pending.context_scene_entities[scene_index] = scene_entity;
					break;
				}
			}
			for (int scene_index = pending.context_scene_entities.size() - 1; scene_index >= 0; scene_index--) {
				pending.entities.insert(0, pending.context_scene_entities[scene_index]);
			}
			pending.entities.insert(0, pending.editor_entity);
		} else {
			pending.entities.append_array(captured_entities);
		}
		pending.truncated = pending.truncated || (bool)partial.get("truncated", false);
		pending.capture_domain++;
		if (pending.capture_domain < (int)(sizeof(capture_domains) / sizeof(capture_domains[0]))) {
			return;
		}
		const Dictionary current_revisions = revision_clock.get_revision_vector();
		if ((int64_t)current_revisions.get("event_seq", 0) != (int64_t)pending.revisions.get("event_seq", 0)) {
			pending.revisions = current_revisions;
			pending.entities.clear();
			pending.editor_entity.clear();
			pending.context_scene_entities.clear();
			pending.capture_domain = 0;
			pending.context_scene_index = 0;
			pending.context_scene_count = -1;
			pending.context_node_index = -1;
			pending.context_node_count = -1;
			pending.truncated = false;
			return;
		}
		pending.snapshot_id = make_snapshot_id();
		pending.result["snapshot_id"] = pending.snapshot_id;
		pending.result["base_event_seq"] = pending.revisions["event_seq"];
		pending.result["revisions"] = pending.revisions;
		pending.result["domains"] = pending.domains;
		Dictionary limits;
		limits["snapshot_chunk_bytes"] = SNAPSHOT_CHUNK_BYTES;
		limits["negotiated_snapshot_chunk_bytes"] = 524288;
		limits["variant_depth"] = EditorContextAdapter::MAX_VARIANT_DEPTH;
		limits["container_items"] = EditorContextAdapter::MAX_CONTAINER_ITEMS;
		limits["identity_characters"] = EditorContextAdapter::MAX_IDENTITY_CHARACTERS;
		limits["projected_value_bytes"] = EditorContextAdapter::MAX_PROJECTED_VALUE_BYTES;
		limits["inspector_bytes_per_node"] = EditorContextAdapter::MAX_INSPECTOR_BYTES_PER_NODE;
		limits["total_inspector_bytes"] = EditorContextAdapter::MAX_TOTAL_INSPECTOR_BYTES;
		limits["open_scenes"] = EditorContextAdapter::MAX_OPEN_SCENES;
		limits["scene_nodes"] = EditorContextAdapter::MAX_SCENE_NODES;
		limits["selected_nodes"] = EditorContextAdapter::MAX_SELECTED_NODES;
		limits["inspector_properties"] = EditorContextAdapter::MAX_INSPECTOR_PROPERTIES;
		limits["truncated"] = pending.truncated;
		pending.result["limits_applied"] = limits;
		pending.stage = PendingEditorSnapshot::STAGE_BEGIN;
		return;
	}
	const int chunk_count = MAX(1, pending.entities.size());
	if (pending.stage == PendingEditorSnapshot::STAGE_BEGIN) {
		Dictionary params;
		params["domain"] = "editor_context";
		params["snapshot_id"] = pending.snapshot_id;
		params["base_event_seq"] = pending.revisions["event_seq"];
		params["revisions"] = pending.revisions;
		params["chunk_count"] = chunk_count;
		Dictionary begin;
		begin["protocol_version"] = "1.5";
		begin["kind"] = "notification";
		begin["method"] = "snapshot.begin";
		begin["params"] = params;
		begin["context"] = _make_context();
		transport_worker.stage_resource_snapshot_message(pending.request_id, begin);
		pending.stage = PendingEditorSnapshot::STAGE_CHUNKS;
		return;
	}
	if (pending.stage == PendingEditorSnapshot::STAGE_CHUNKS) {
		for (int staged = 0; staged < EDITOR_SNAPSHOT_CHUNKS_PER_FRAME && pending.next_entity < chunk_count; staged++) {
			Array chunk_entities;
			if (!pending.entities.is_empty()) {
				chunk_entities.push_back(pending.entities[pending.next_entity]);
			}
			Dictionary payload;
			payload["entities"] = chunk_entities;
			Dictionary chunk;
			chunk["protocol_version"] = "1.5";
			chunk["kind"] = "chunk";
			chunk["domain"] = "editor_context";
			chunk["snapshot_id"] = pending.snapshot_id;
			chunk["chunk_index"] = pending.next_entity;
			chunk["payload"] = payload;
			chunk["context"] = _make_context();
			transport_worker.stage_resource_snapshot_message(pending.request_id, chunk);
			pending.next_entity++;
		}
		if (pending.next_entity >= chunk_count) {
			pending.stage = PendingEditorSnapshot::STAGE_END;
		}
		return;
	}
	Dictionary params;
	params["domain"] = "editor_context";
	params["snapshot_id"] = pending.snapshot_id;
	params["chunk_count"] = chunk_count;
	params["entity_count"] = pending.entities.size();
	params["revisions"] = pending.revisions;
	Dictionary end;
	end["protocol_version"] = "1.5";
	end["kind"] = "notification";
	end["method"] = "snapshot.end";
	end["params"] = params;
	end["context"] = _make_context();
	transport_worker.complete_resource_snapshot(pending.request_id, pending.result, end);
	pending_editor_snapshots.pop_front();
}

void CodexBridgeService::_complete_resource_delta(uint64_t p_request_id, uint64_t p_after_resource_revision) {
	const ResourceDeltaJournal::QueryResult query = resource_graph_adapter.query_delta(p_after_resource_revision);
	if (query.status == ResourceDeltaJournal::QUERY_FUTURE) {
		Dictionary data;
		data["requested_after"] = (int64_t)p_after_resource_revision;
		data["current_resource_revision"] = (int64_t)query.current_resource_revision;
		transport_worker.complete_request_error(p_request_id, "invalid_revision", "The requested resource revision is in the future.", false, data);
		return;
	}
	if (query.status == ResourceDeltaJournal::QUERY_GAP) {
		Dictionary data;
		data["requested_after"] = (int64_t)p_after_resource_revision;
		data["oldest_available"] = (int64_t)query.oldest_available_resource_revision;
		data["current_resource_revision"] = (int64_t)query.current_resource_revision;
		transport_worker.complete_request_error(p_request_id, "resource_journal_gap", "The requested resource delta is no longer available.", true, data);
		return;
	}
	Dictionary result;
	result["current_resource_revision"] = (int64_t)query.current_resource_revision;
	if (query.status == ResourceDeltaJournal::QUERY_CURRENT) {
		result["status"] = "current";
	} else {
		result["status"] = "batch";
		result["batch"] = query.batch;
	}
	transport_worker.complete_request(p_request_id, result);
}

void CodexBridgeService::_complete_scene_delta(uint64_t p_request_id, uint64_t p_after_scene_graph_revision) {
	const SceneDeltaJournal::QueryResult query = scene_state_adapter.query_delta(p_after_scene_graph_revision);
	if (query.status == SceneDeltaJournal::QUERY_FUTURE) {
		Dictionary data;
		data["requested_after"] = (int64_t)p_after_scene_graph_revision;
		data["current_scene_graph_revision"] = (int64_t)query.current_scene_graph_revision;
		transport_worker.complete_request_error(p_request_id, "invalid_revision", "The requested scene graph revision is in the future.", false, data);
		return;
	}
	if (query.status == SceneDeltaJournal::QUERY_GAP) {
		Dictionary data;
		data["requested_after"] = (int64_t)p_after_scene_graph_revision;
		data["oldest_available"] = (int64_t)query.oldest_available_scene_graph_revision;
		data["current_scene_graph_revision"] = (int64_t)query.current_scene_graph_revision;
		transport_worker.complete_request_error(p_request_id, "scene_journal_gap", "The requested scene graph delta is no longer available.", true, data);
		return;
	}
	Dictionary result;
	result["current_scene_graph_revision"] = (int64_t)query.current_scene_graph_revision;
	if (query.status == SceneDeltaJournal::QUERY_CURRENT) {
		result["status"] = "current";
	} else {
		result["status"] = "batch";
		result["batch"] = query.batch;
	}
	transport_worker.complete_request(p_request_id, result);
}

void CodexBridgeService::_complete_script_delta(uint64_t p_request_id, uint64_t p_after_script_graph_revision) {
	const ScriptDeltaJournal::QueryResult query = script_graph_adapter.query_delta(p_after_script_graph_revision);
	if (query.status == ScriptDeltaJournal::QUERY_FUTURE) {
		Dictionary data;
		data["requested_after"] = (int64_t)p_after_script_graph_revision;
		data["current_script_graph_revision"] = (int64_t)query.current_script_graph_revision;
		transport_worker.complete_request_error(p_request_id, "invalid_revision", "The requested script graph revision is in the future.", false, data);
		return;
	}
	if (query.status == ScriptDeltaJournal::QUERY_GAP) {
		Dictionary data;
		data["requested_after"] = (int64_t)p_after_script_graph_revision;
		data["oldest_available"] = (int64_t)query.oldest_available_script_graph_revision;
		data["current_script_graph_revision"] = (int64_t)query.current_script_graph_revision;
		transport_worker.complete_request_error(p_request_id, "script_journal_gap", "The requested script graph delta is no longer available.", true, data);
		return;
	}
	if (script_graph_adapter.has_catalog_limit_failure()) {
		transport_worker.complete_request_error(p_request_id, "script_limit_exceeded", "The editor script catalog exceeds a negotiated hard limit.", false);
		return;
	}
	if (!script_graph_adapter.is_catalog_ready()) {
		transport_worker.complete_request_error(p_request_id, "script_catalog_building", "The editor script catalog is still building.", true);
		return;
	}
	Dictionary result;
	result["current_script_graph_revision"] = (int64_t)query.current_script_graph_revision;
	if (query.status == ScriptDeltaJournal::QUERY_CURRENT) {
		result["status"] = "current";
	} else {
		result["status"] = "batch";
		result["batch"] = query.batch;
	}
	transport_worker.complete_request(p_request_id, result);
}

void CodexBridgeService::_process_resource_graph(uint64_t p_budget_usec) {
	if (resource_graph_adapter.is_snapshot_active()) {
		ResourceGraphAdapter::SnapshotCompletion snapshot;
		if (resource_graph_adapter.process_snapshot(OS::get_singleton()->get_ticks_usec(), p_budget_usec, snapshot) && snapshot.ready) {
			if (snapshot.is_error) {
				if (!snapshot.abandoned_messages.is_empty()) {
					transport_worker.abort_resource_snapshot(snapshot.request_id, snapshot.abandoned_messages);
				}
				transport_worker.complete_request_error(snapshot.request_id, snapshot.error_code, snapshot.error_message, snapshot.error_retryable, snapshot.error_data);
				return;
			}
			if (snapshot.terminal) {
				transport_worker.complete_resource_snapshot(snapshot.request_id, snapshot.result, snapshot.server_message);
			} else {
				transport_worker.stage_resource_snapshot_message(snapshot.request_id, snapshot.server_message);
			}
		}
		return;
	}

	ResourceGraphAdapter::RefreshOutcome refresh;
	if (resource_graph_adapter.process_refresh(p_budget_usec, refresh)) {
		if (refresh.changed || refresh.invalidated) {
			// Resource, scene, and script scans rotate independently. A dependent
			// scan may finish before the resource clock advances, so explicitly
			// schedule a new checkpoint-bound pass after the resource outcome.
			scene_state_adapter.request_refresh();
			script_graph_adapter.request_refresh();
		}
		if (refresh.changed) {
			Dictionary params;
			params["event_seq"] = refresh.revisions["event_seq"];
			params["event_type"] = "resource_graph_changed";
			params["resource_revision"] = (int64_t)refresh.current_resource_revision;
			params["revisions"] = refresh.revisions;
			Dictionary notification;
			notification["protocol_version"] = "1.2";
			notification["kind"] = "notification";
			notification["method"] = "sync.event";
			notification["params"] = params;
			notification["context"] = _make_context();
			transport_worker.publish_notification(notification);
		}
		if (refresh.invalidated) {
			Dictionary params;
			params["reason"] = "resource_journal_gap";
			params["last_contiguous_resource_revision"] = (int64_t)refresh.last_contiguous_resource_revision;
			params["current_resource_revision"] = (int64_t)refresh.current_resource_revision;
			Dictionary notification;
			notification["protocol_version"] = "1.2";
			notification["kind"] = "notification";
			notification["method"] = "sync.invalidated";
			notification["params"] = params;
			notification["context"] = _make_context();
			transport_worker.publish_notification(notification);
		}
	}
}

void CodexBridgeService::_process_scene_graph(uint64_t p_budget_usec) {
	if (scene_state_adapter.is_snapshot_active()) {
		SceneStateAdapter::SnapshotCompletion snapshot;
		if (scene_state_adapter.process_snapshot(OS::get_singleton()->get_ticks_usec(), p_budget_usec, snapshot) && snapshot.ready) {
			if (snapshot.is_error) {
				if (!snapshot.abandoned_messages.is_empty()) {
					transport_worker.abort_resource_snapshot(snapshot.request_id, snapshot.abandoned_messages);
				}
				transport_worker.complete_request_error(snapshot.request_id, snapshot.error_code, snapshot.error_message, snapshot.error_retryable, snapshot.error_data);
			} else if (snapshot.terminal) {
				transport_worker.complete_resource_snapshot(snapshot.request_id, snapshot.result, snapshot.server_message);
			} else {
				transport_worker.stage_resource_snapshot_message(snapshot.request_id, snapshot.server_message);
			}
		}
		return;
	}

	SceneStateAdapter::RefreshOutcome refresh;
	if (!scene_state_adapter.process_refresh(p_budget_usec, refresh)) {
		return;
	}
	if (refresh.changed) {
		Dictionary params;
		params["event_type"] = "scene_graph_changed";
		params["scene_graph_revision"] = (int64_t)refresh.current_scene_graph_revision;
		params["resource_revision"] = revision_clock.get_resource_revision();
		params["revisions"] = refresh.revisions;
		Dictionary notification;
		notification["protocol_version"] = "1.3";
		notification["kind"] = "notification";
		notification["method"] = "scene_graph_changed";
		notification["params"] = params;
		notification["context"] = _make_context();
		transport_worker.publish_notification(notification);
	}
	if (refresh.invalidated) {
		Dictionary params;
		params["event_type"] = "scene_journal_gap";
		params["last_contiguous_scene_graph_revision"] = (int64_t)refresh.last_contiguous_scene_graph_revision;
		params["current_scene_graph_revision"] = (int64_t)refresh.current_scene_graph_revision;
		params["revisions"] = refresh.revisions;
		Dictionary notification;
		notification["protocol_version"] = "1.3";
		notification["kind"] = "notification";
		notification["method"] = "scene_journal_gap";
		notification["params"] = params;
		notification["context"] = _make_context();
		transport_worker.publish_notification(notification);
	}
}

void CodexBridgeService::_process_script_graph(uint64_t p_budget_usec) {
	if (script_graph_adapter.is_snapshot_active()) {
		ScriptGraphAdapter::SnapshotCompletion snapshot;
		if (script_graph_adapter.process_snapshot(OS::get_singleton()->get_ticks_usec(), p_budget_usec, snapshot) && snapshot.ready) {
			if (snapshot.is_error) {
				if (!snapshot.abandoned_messages.is_empty()) {
					transport_worker.abort_resource_snapshot(snapshot.request_id, snapshot.abandoned_messages);
				}
				transport_worker.complete_request_error(snapshot.request_id, snapshot.error_code, snapshot.error_message, snapshot.error_retryable, snapshot.error_data);
			} else if (snapshot.terminal) {
				transport_worker.complete_resource_snapshot(snapshot.request_id, snapshot.result, snapshot.server_message);
			} else {
				transport_worker.stage_resource_snapshot_message(snapshot.request_id, snapshot.server_message);
			}
		}
		return;
	}

	ScriptGraphAdapter::RefreshOutcome refresh;
	if (!script_graph_adapter.process_refresh(p_budget_usec, refresh)) {
		return;
	}
	if (refresh.changed) {
		Dictionary params;
		params["event_type"] = "script_graph_changed";
		params["script_graph_revision"] = (int64_t)refresh.current_script_graph_revision;
		params["resource_revision"] = revision_clock.get_resource_revision();
		params["scene_graph_revision"] = revision_clock.get_scene_graph_revision();
		params["revisions"] = refresh.revisions;
		Dictionary notification;
		notification["protocol_version"] = "1.4";
		notification["kind"] = "notification";
		notification["method"] = "script_graph_changed";
		notification["params"] = params;
		notification["context"] = _make_context();
		transport_worker.publish_notification(notification);
	}
	if (refresh.invalidated) {
		Dictionary params;
		params["event_type"] = "script_journal_gap";
		params["last_contiguous_script_graph_revision"] = (int64_t)refresh.last_contiguous_script_graph_revision;
		params["current_script_graph_revision"] = (int64_t)refresh.current_script_graph_revision;
		params["revisions"] = refresh.revisions;
		Dictionary notification;
		notification["protocol_version"] = "1.4";
		notification["kind"] = "notification";
		notification["method"] = "script_journal_gap";
		notification["params"] = params;
		notification["context"] = _make_context();
		transport_worker.publish_notification(notification);
	}
}

void CodexBridgeService::_notification(int p_what) {
	switch (p_what) {
		case NOTIFICATION_ENTER_TREE: {
			start();
		} break;
		case NOTIFICATION_PROCESS: {
			if (state == STATE_RUNNING) {
				if (scene_change_pending && OS::get_singleton()->get_ticks_usec() >= scene_change_not_before_usec) {
					_flush_scene_change();
				}
				const uint64_t frame_started_usec = OS::get_singleton()->get_ticks_usec();
				const bool resource_work = resource_graph_adapter.has_pending_work();
				const bool scene_work = scene_state_adapter.has_pending_work();
				const bool script_work = script_graph_adapter.has_pending_work();
				const bool control_work = dispatcher.get_queue_size() > 0 || !pending_editor_snapshots.is_empty();
				bool run_resource_bulk = false;
				bool run_scene_bulk = false;
				bool run_script_bulk = false;
				if (!resource_work && script_work) {
					run_script_bulk = true;
					work_lane_turn = 3;
				}
				for (int offset = 0; offset < 4; offset++) {
					if (run_script_bulk) {
						break;
					}
					const int candidate = (work_lane_turn + offset) % 4;
					// Scene and script observations bind one resource checkpoint. Let
					// the resource pass settle before either dependent lane can commit.
					const bool dependent_lane_ready = !resource_work;
					if ((candidate == 0 && resource_work) || (candidate == 1 && scene_work && dependent_lane_ready) || (candidate == 2 && script_work && dependent_lane_ready) || (candidate == 3 && control_work)) {
						run_resource_bulk = candidate == 0;
						run_scene_bulk = candidate == 1;
						run_script_bulk = candidate == 2;
						work_lane_turn = (candidate + 1) % 4;
						break;
					}
				}
				const uint64_t frame_safety_margin = run_script_bulk ? ScriptGraphAdapter::FRAME_SAFETY_MARGIN_USEC : (run_scene_bulk ? SceneStateAdapter::FRAME_SAFETY_MARGIN_USEC : ResourceGraphAdapter::FRAME_SAFETY_MARGIN_USEC);
				const uint64_t dispatcher_budget = MainThreadDispatcher::MAX_PROCESS_USEC_PER_FRAME -
						frame_safety_margin -
						(run_resource_bulk ? ResourceGraphAdapter::RESOURCE_BUDGET_USEC : 0) -
						(run_scene_bulk ? SceneStateAdapter::SCENE_BUDGET_USEC : 0) -
						(run_script_bulk ? ScriptGraphAdapter::SCRIPT_BUDGET_USEC : 0);
				MainThreadDispatcher::ProcessStats dispatcher_stats;
				if (dispatcher_budget > 0) {
						dispatcher_stats = dispatcher.process(_dispatch_command, this, MainThreadDispatcher::MAX_COMMANDS_PER_FRAME, dispatcher_budget);
				}
				bool editor_snapshot_step = false;
				if (!pending_editor_snapshots.is_empty() && dispatcher_stats.consumed == 0) {
					_process_editor_snapshot();
					editor_snapshot_step = true;
				}
				const bool bulk_lane_available = !editor_snapshot_step && (dispatcher_stats.consumed == 0 || dispatcher_stats.elapsed_usec < dispatcher_budget);
				// Resource, scene, script, and queued control work remain bounded.
				// Script frames reserve a measured dispatcher slice; if it is consumed,
				// bulk work waits for the next frame instead of crossing the 2 ms gate.
				if (run_resource_bulk && bulk_lane_available) {
					_process_resource_graph(ResourceGraphAdapter::RESOURCE_BUDGET_USEC);
				}
				if (run_scene_bulk && bulk_lane_available) {
					_process_scene_graph(SceneStateAdapter::SCENE_BUDGET_USEC);
				}
				if (run_script_bulk && bulk_lane_available) {
					_process_script_graph(ScriptGraphAdapter::SCRIPT_BUDGET_USEC);
				}
				const uint64_t frame_elapsed_usec = OS::get_singleton()->get_ticks_usec() - frame_started_usec;
				frame_telemetry.record(frame_elapsed_usec, resource_work || scene_work || script_work || control_work || dispatcher_stats.consumed > 0);
			}
		} break;
		case NOTIFICATION_EXIT_TREE: {
			stop();
		} break;
	}
}

CodexBridgeService *CodexBridgeService::get_singleton() {
	return singleton;
}

Error CodexBridgeService::start() {
	if (state == STATE_RUNNING) {
		return OK;
	}
	ERR_FAIL_COND_V_MSG(state != STATE_STOPPED, ERR_BUSY, "Codex bridge service cannot start while changing state.");

	state = STATE_STARTING;
	dispatcher.start_accepting();
#if defined(MACOS_ENABLED) || defined(WINDOWS_ENABLED)
	const Error error = transport_worker.start(ProjectSettings::get_singleton()->get_resource_path(), &dispatcher);
#else
	const Error error = transport_worker.start();
#endif
	if (error != OK) {
		dispatcher.begin_shutdown();
		state = STATE_STOPPED;
		ERR_PRINT(vformat("[codex_bridge] Failed to start the transport worker (error %d).", (int)error));
		return error;
	}

	revision_clock.initialize(transport_worker.get_editor_session_id());
	resource_graph_adapter.initialize(&revision_clock);
	scene_state_adapter.initialize(&revision_clock);
	script_graph_adapter.initialize(&revision_clock);
	work_lane_turn = 0;
	frame_telemetry.reset(OS::get_singleton()->get_environment("GODOT_CODEX_EVIDENCE_TELEMETRY") == "1");
	native_history_observations.clear();
	native_history_transitions.clear();
	observed_scene_ids.clear();
	pending_editor_snapshots.clear();
	_refresh_open_scene_ids(false);
	_observe_native_histories(false);
	_connect_editor_signals();
	state = STATE_RUNNING;
	print_verbose("[codex_bridge] Service started.");
	return OK;
}

void CodexBridgeService::stop() {
	if (state == STATE_STOPPED || state == STATE_STOPPING) {
		return;
	}

	state = STATE_STOPPING;
	_disconnect_editor_signals();
	resource_graph_adapter.shutdown();
	scene_state_adapter.shutdown();
	script_graph_adapter.shutdown();
	work_lane_turn = 0;
	scene_change_pending = false;
	scene_change_not_before_usec = 0;
	native_operation_pending = false;
	native_history_observations.clear();
	native_history_transitions.clear();
	observed_scene_ids.clear();
	pending_editor_snapshots.clear();
	pending_property.clear();
	dispatcher.begin_shutdown();
	const BridgeTransportWorker::StopResult stop_result = transport_worker.stop();
	if (stop_result == BridgeTransportWorker::STOP_TIMED_OUT) {
		ERR_PRINT("[codex_bridge] Transport worker did not stop within the shutdown timeout.");
	}
	if (frame_telemetry.is_enabled()) {
		print_line("[codex_bridge_evidence] " + JSON::stringify(frame_telemetry.to_dictionary(), "", true, true));
	}
	state = STATE_STOPPED;
	print_verbose("[codex_bridge] Service stopped.");
}

CodexBridgeService::State CodexBridgeService::get_service_state() const {
	return state;
}

MainThreadDispatcher &CodexBridgeService::get_dispatcher() {
	return dispatcher;
}

CodexBridgeService::CodexBridgeService() {
	ERR_FAIL_COND_MSG(singleton != nullptr, "Only one Codex bridge service may exist.");
	singleton = this;
	set_process(true);
}

CodexBridgeService::~CodexBridgeService() {
	stop();
	if (singleton == this) {
		singleton = nullptr;
	}
}
