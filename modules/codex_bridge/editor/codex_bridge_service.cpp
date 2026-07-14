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
#include "core/string/print_string.h"
#include "editor/docks/inspector_dock.h"
#include "editor/editor_data.h"
#include "editor/editor_node.h"
#include "editor/editor_undo_redo_manager.h"
#include "modules/codex_bridge/editor/editor_context_adapter.h"
#include "modules/codex_bridge/protocol/bridge_crypto.h"

namespace {

// The chunk message carries both the structured payload and its exact canonical
// JSON checksum input. Keeping the payload target at 256 KiB leaves bounded
// headroom for JSON escaping under the 1 MiB frame limit.
static constexpr int SNAPSHOT_CHUNK_BYTES = 262144;
static constexpr int SNAPSHOT_ENTITY_LIMIT = 1000;

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
			service->_complete_snapshot(p_command.request_id);
			break;
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
	if (InspectorDock::get_singleton() && InspectorDock::get_inspector_singleton()) {
		InspectorDock::get_inspector_singleton()->connect(SNAME("property_edited"), callable_mp(this, &CodexBridgeService::_on_property_edited));
	}
	if (EditorUndoRedoManager::get_singleton()) {
		EditorUndoRedoManager::get_singleton()->connect(SNAME("version_changed"), callable_mp(this, &CodexBridgeService::_on_undo_redo_version_changed));
		EditorUndoRedoManager::get_singleton()->connect(SNAME("history_changed"), callable_mp(this, &CodexBridgeService::_on_undo_redo_version_changed));
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
	if (InspectorDock::get_singleton() && InspectorDock::get_inspector_singleton() && InspectorDock::get_inspector_singleton()->is_connected(SNAME("property_edited"), callable_mp(this, &CodexBridgeService::_on_property_edited))) {
		InspectorDock::get_inspector_singleton()->disconnect(SNAME("property_edited"), callable_mp(this, &CodexBridgeService::_on_property_edited));
	}
	if (EditorUndoRedoManager::get_singleton() && EditorUndoRedoManager::get_singleton()->is_connected(SNAME("version_changed"), callable_mp(this, &CodexBridgeService::_on_undo_redo_version_changed))) {
		EditorUndoRedoManager::get_singleton()->disconnect(SNAME("version_changed"), callable_mp(this, &CodexBridgeService::_on_undo_redo_version_changed));
	}
	if (EditorUndoRedoManager::get_singleton() && EditorUndoRedoManager::get_singleton()->is_connected(SNAME("history_changed"), callable_mp(this, &CodexBridgeService::_on_undo_redo_version_changed))) {
		EditorUndoRedoManager::get_singleton()->disconnect(SNAME("history_changed"), callable_mp(this, &CodexBridgeService::_on_undo_redo_version_changed));
	}
	editor_signals_connected = false;
}

void CodexBridgeService::_publish_event(const String &p_event_type, const String &p_property, bool p_scene_mutation) {
	const String scene_id = _get_current_scene_id();
	if (p_scene_mutation && !scene_id.is_empty()) {
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
	_publish_event("scene_changed", String(), true);
}

void CodexBridgeService::_on_property_edited(const String &p_property) {
	pending_property = p_property;
	if (!scene_change_pending) {
		scene_change_pending = true;
		callable_mp(this, &CodexBridgeService::_flush_scene_change).call_deferred();
	}
}

void CodexBridgeService::_on_undo_redo_version_changed() {
	if (!scene_change_pending) {
		scene_change_pending = true;
		callable_mp(this, &CodexBridgeService::_flush_scene_change).call_deferred();
	}
}

void CodexBridgeService::_flush_scene_change() {
	scene_change_pending = false;
	const String property = pending_property;
	pending_property.clear();
	_publish_event(property.is_empty() ? "scene_changed" : "property_changed", property, true);
}

void CodexBridgeService::_complete_snapshot(uint64_t p_request_id) {
	Dictionary snapshot;
	const Dictionary revisions = revision_clock.get_revision_vector();
	if (EditorContextAdapter::capture(transport_worker.get_project_id(), transport_worker.get_editor_session_id(), revisions, snapshot) != OK) {
		transport_worker.complete_request(p_request_id, Dictionary());
		return;
	}

	const String snapshot_id = make_snapshot_id();
	const Array entities = snapshot["entities"];
	Vector<Array> chunks;
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
	if (chunks.is_empty()) {
		chunks.push_back(Array());
	}

	Dictionary result;
	result["snapshot_id"] = snapshot_id;
	result["base_event_seq"] = revisions["event_seq"];
	result["revisions"] = revisions;
	Array domains;
	domains.push_back("editor_context");
	domains.push_back("editor_inspector");
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
		const String payload_json = JSON::stringify(payload, "", true, true);
		const String checksum = sha256_hex_utf8(payload_json);
		snapshot_checksum_input += checksum;
		Dictionary chunk;
		chunk["protocol_version"] = "1.1";
		chunk["kind"] = "chunk";
		chunk["snapshot_id"] = snapshot_id;
		chunk["chunk_index"] = chunk_index;
		chunk["payload"] = payload;
		chunk["payload_json"] = payload_json;
		chunk["checksum"] = checksum;
		chunk["context"] = _make_context();
		messages.push_back(chunk);
	}

	Dictionary end_params;
	end_params["snapshot_id"] = snapshot_id;
	end_params["chunk_count"] = chunks.size();
	end_params["entity_count"] = entities.size();
	end_params["checksum"] = sha256_hex_utf8(snapshot_checksum_input);
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

void CodexBridgeService::_notification(int p_what) {
	switch (p_what) {
		case NOTIFICATION_ENTER_TREE: {
			start();
		} break;
		case NOTIFICATION_PROCESS: {
			if (state == STATE_RUNNING) {
				dispatcher.process(_dispatch_command, this);
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
		ERR_PRINT("[codex_bridge] Failed to start the transport worker.");
		return error;
	}

	revision_clock.initialize(transport_worker.get_editor_session_id());
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
	scene_change_pending = false;
	pending_property.clear();
	dispatcher.begin_shutdown();
	const BridgeTransportWorker::StopResult stop_result = transport_worker.stop();
	if (stop_result == BridgeTransportWorker::STOP_TIMED_OUT) {
		ERR_PRINT("[codex_bridge] Transport worker did not stop within the shutdown timeout.");
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
