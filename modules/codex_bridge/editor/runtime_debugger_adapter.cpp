/**************************************************************************/
/*  runtime_debugger_adapter.cpp                                         */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "runtime_debugger_adapter.h"

#include "bounded_variant_projector.h"
#include "bridge_revision_clock.h"

#include "core/config/project_settings.h"
#include "core/crypto/crypto_core.h"
#include "core/io/dir_access.h"
#include "core/io/file_access.h"
#include "core/io/image.h"
#include "core/io/json.h"
#include "core/object/callable_mp.h"
#include "core/os/os.h"
#include "editor/debugger/script_editor_debugger.h"
#include "editor/editor_node.h"
#include "editor/run/editor_run.h"
#include "editor/run/editor_run_bar.h"
#include "editor/script/script_editor_plugin.h"
#include "modules/codex_bridge/protocol/bridge_crypto.h"
#include "modules/codex_bridge/transport/bridge_transport_worker.h"
#include "scene/debugger/scene_debugger_object.h"

namespace {

static Array changed_domain(const String &p_domain) {
	Array domains;
	domains.push_back(p_domain);
	return domains;
}

static bool safe_res_path(const String &p_path) {
	return p_path.begins_with("res://") && !p_path.replace("\\", "/").contains("/../") && !p_path.ends_with("/..");
}

static String bounded_string(const String &p_value, int p_limit, bool &r_truncated) {
	if (p_value.length() <= p_limit) {
		return p_value;
	}
	r_truncated = true;
	return p_value.left(p_limit);
}

static String sha256_hex_utf8(const String &p_value) {
	const CharString bytes = p_value.utf8();
	PackedByteArray digest;
	digest.resize(32);
	if (CryptoCore::sha256(reinterpret_cast<const uint8_t *>(bytes.get_data()), bytes.length(), digest.ptrw()) != OK) {
		return String();
	}
	return BridgeCrypto::bytes_to_lower_hex(digest);
}

} // namespace

ScriptEditorDebugger *RuntimeDebuggerAdapter::_get_debugger(int p_session_id) const {
	const ObjectID *id = debugger_sessions.getptr(p_session_id);
	return id ? Object::cast_to<ScriptEditorDebugger>(ObjectDB::get_instance(*id)) : nullptr;
}

ScriptEditorDebugger *RuntimeDebuggerAdapter::_get_active_debugger() const {
	return _get_debugger(active_debugger_session);
}

String RuntimeDebuggerAdapter::_make_runtime_session_id() const {
	PackedByteArray random;
	if (BridgeCrypto::random_bytes(16, random) != OK) {
		return String();
	}
	return "runtime:" + BridgeCrypto::bytes_to_lower_hex(random);
}

String RuntimeDebuggerAdapter::_make_opaque_id(const String &p_prefix, const String &p_domain, const String &p_value) const {
	const CharString input = (p_domain + "\n" + runtime_session_id + "\n" + p_value).utf8();
	PackedByteArray digest;
	digest.resize(32);
	if (CryptoCore::sha256(reinterpret_cast<const uint8_t *>(input.get_data()), input.length(), digest.ptrw()) != OK) {
		return String();
	}
	String encoded;
	if (BridgeCrypto::base64url_encode_32(digest, encoded) != OK) {
		return String();
	}
	return p_prefix + encoded;
}

Dictionary RuntimeDebuggerAdapter::_make_context() const {
	Dictionary context;
	context["project_id"] = transport ? transport->get_project_id() : String();
	context["editor_session_id"] = transport ? transport->get_editor_session_id() : String();
	return context;
}

Dictionary RuntimeDebuggerAdapter::_make_limits(bool p_truncated) const {
	Dictionary limits;
	limits["tree_nodes"] = MAX_TREE_NODES;
	limits["tree_depth"] = MAX_TREE_DEPTH;
	limits["snapshot_bytes"] = 16777216;
	limits["snapshot_chunk_bytes"] = 524288;
	limits["snapshot_window_bytes"] = 33554432;
	limits["snapshot_timeout_ms"] = 10000;
	limits["properties"] = MAX_PROPERTIES;
	limits["variant_depth"] = BoundedVariantProjector::MAX_DEPTH;
	limits["container_items"] = BoundedVariantProjector::MAX_CONTAINER_ITEMS;
	limits["string_characters"] = BoundedVariantProjector::MAX_STRING_CHARACTERS;
	limits["projected_value_bytes"] = BoundedVariantProjector::MAX_ENCODED_BYTES;
	limits["object_bytes"] = MAX_OBJECT_BYTES;
	limits["diagnostics"] = MAX_DIAGNOSTICS;
	limits["diagnostic_message_bytes"] = 16384;
	limits["diagnostics_bytes"] = MAX_DIAGNOSTIC_BYTES;
	limits["stacks"] = MAX_STACKS;
	limits["stack_frames"] = MAX_STACK_FRAMES;
	limits["stacks_bytes"] = MAX_STACK_BYTES;
	limits["truncated"] = p_truncated;
	return limits;
}

Dictionary RuntimeDebuggerAdapter::_make_state_result() const {
	Dictionary result;
	result["schema_version"] = "runtime/1.0";
	result["project_id"] = transport->get_project_id();
	result["editor_session_id"] = transport->get_editor_session_id();
	result["runtime_session_id"] = runtime_session_id;
	result["runtime_event_seq"] = (int64_t)revisions->get_runtime_event_seq();
	result["state"] = state;
	result["origin"] = origin;
	result["target"] = target;
	if (safe_res_path(scene_path)) {
		result["scene_path"] = scene_path;
	}
	return result;
}

Dictionary RuntimeDebuggerAdapter::_safe_coordinates() const {
	Dictionary data;
	if (!runtime_session_id.is_empty()) {
		data["runtime_session_id"] = runtime_session_id;
		data["runtime_event_seq"] = (int64_t)revisions->get_runtime_event_seq();
		data["state"] = state;
	}
	return data;
}

bool RuntimeDebuggerAdapter::_is_terminal() const {
	return state == "stopped" || state == "crashed" || state == "failed" || state == "timed_out";
}

bool RuntimeDebuggerAdapter::_guard(uint64_t p_request_id, const Dictionary &p_params, bool p_require_running, bool p_allow_paused) {
	if (runtime_session_id.is_empty()) {
		transport->complete_request_error(p_request_id, "runtime_inactive", "No runtime session is available.", true);
		return false;
	}
	if (String(p_params.get("runtime_session_id", String())) != runtime_session_id) {
		transport->complete_request_error(p_request_id, "stale_runtime_session", "The runtime session was replaced.", true, _safe_coordinates());
		return false;
	}
	if (p_params.has("expected_runtime_event_seq") && (int64_t)p_params["expected_runtime_event_seq"] != (int64_t)revisions->get_runtime_event_seq()) {
		transport->complete_request_error(p_request_id, "stale_runtime_state", "The runtime state changed.", true, _safe_coordinates());
		return false;
	}
	if (p_require_running && state != "running" && (!p_allow_paused || state != "paused")) {
		const String code = state == "disconnected" ? "runtime_disconnected" : (_is_terminal() ? "runtime_crashed" : "runtime_not_running");
		transport->complete_request_error(p_request_id, code, "The runtime is not in a compatible live state.", true, _safe_coordinates());
		return false;
	}
	return true;
}

void RuntimeDebuggerAdapter::_begin_session(const String &p_origin, const String &p_target, const String &p_scene_path) {
	const String previous_session_id = runtime_session_id;
	const uint64_t previous_event_seq = revisions->get_runtime_event_seq();
	if (!previous_session_id.is_empty() && transport) {
		Dictionary params;
		params["runtime_session_id"] = previous_session_id;
		params["last_contiguous_runtime_event_seq"] = (int64_t)previous_event_seq;
		params["reason"] = "session_replaced";
		Dictionary notification;
		notification["protocol_version"] = "1.6";
		notification["kind"] = "notification";
		notification["method"] = "runtime.invalidated";
		notification["params"] = params;
		notification["context"] = _make_context();
		transport->publish_notification(notification);
	}
	runtime_session_id = _make_runtime_session_id();
	origin = p_origin;
	target = p_target;
	scene_path = p_scene_path;
	state = "starting";
	terminal_reason.clear();
	normal_quit_requested = false;
	internal_forced_stop = false;
	state_since_usec = OS::get_singleton()->get_ticks_usec();
	disconnected_since_usec = 0;
	_retire_live_data();
	diagnostics.clear();
	diagnostic_bytes = 0;
	stacks.clear();
	stack_order.clear();
	stack_bytes = 0;
	revisions->begin_runtime_session(runtime_session_id);
	_publish_event("session_started", changed_domain("runtime_state"));
}

void RuntimeDebuggerAdapter::_transition(const String &p_state, const String &p_event_type, const Array &p_changed_domains, const String &p_reason, bool p_advance) {
	if (runtime_session_id.is_empty()) {
		return;
	}
	state = p_state;
	terminal_reason = p_reason;
	state_since_usec = OS::get_singleton()->get_ticks_usec();
	if (p_advance) {
		revisions->record_runtime_change();
	}
	if (_is_terminal() || state == "disconnected") {
		_retire_live_data();
	}
	_publish_event(p_event_type, p_changed_domains);
}

void RuntimeDebuggerAdapter::_publish_event(const String &p_event_type, const Array &p_changed_domains) {
	if (!transport || runtime_session_id.is_empty()) {
		return;
	}
	Dictionary params;
	params["runtime_session_id"] = runtime_session_id;
	params["runtime_event_seq"] = (int64_t)revisions->get_runtime_event_seq();
	params["event_type"] = p_event_type;
	params["state"] = state;
	params["changed_domains"] = p_changed_domains;
	params["revisions"] = revisions->get_revision_vector();
	Dictionary notification;
	notification["protocol_version"] = "1.6";
	notification["kind"] = "notification";
	notification["method"] = "runtime.event";
	notification["params"] = params;
	notification["context"] = _make_context();
	transport->publish_notification(notification);
}

void RuntimeDebuggerAdapter::_retire_live_data() {
	runtime_tree.clear();
	runtime_tree_checksum.clear();
	object_ids.clear();
	opaque_by_object_id.clear();
}

void RuntimeDebuggerAdapter::_fail_pending_live_requests(const String &p_code, const String &p_message, bool p_retryable) {
	for (uint64_t *pending : { &pending_pause, &pending_continue, &pending_snapshot, &pending_object, &pending_capture }) {
		if (*pending != 0) {
			transport->complete_request_error(*pending, p_code, p_message, p_retryable, _safe_coordinates());
			*pending = 0;
		}
	}
	pending_snapshot_params.clear();
	pending_object_params.clear();
	pending_capture_params.clear();
	pending_tree_correlation.clear();
	pending_object_correlation.clear();
}

void RuntimeDebuggerAdapter::_complete_pending_controls(const String &p_confirmed_state) {
	uint64_t *request = nullptr;
	if (p_confirmed_state == "running") {
		request = pending_run ? &pending_run : &pending_continue;
	} else if (p_confirmed_state == "paused") {
		request = &pending_pause;
	} else if (p_confirmed_state == "stopped") {
		request = &pending_stop;
	}
	if (request && *request != 0) {
		transport->complete_request(*request, _make_state_result());
		*request = 0;
	}
}

String RuntimeDebuggerAdapter::_redact_message(const String &p_message, bool &r_redacted, bool &r_truncated) const {
	String message = bounded_string(p_message, 16384, r_truncated);
	const String lower = message.to_lower();
	for (const char *marker : { "authorization:", "bearer ", "api_key", "apikey", "password=", "password:", "secret=", "secret:", "token=", "token:" }) {
		if (lower.contains(marker)) {
			r_redacted = true;
			return "<redacted sensitive runtime output>";
		}
	}
	for (const String &sensitive_root : { ProjectSettings::get_singleton()->get_resource_path().replace("\\", "/"), OS::get_singleton()->get_environment("HOME").replace("\\", "/") }) {
		if (!sensitive_root.is_empty() && message.replace("\\", "/").contains(sensitive_root)) {
			message = message.replace("\\", "/").replace(sensitive_root, sensitive_root == ProjectSettings::get_singleton()->get_resource_path().replace("\\", "/") ? "<project>" : "<home>");
			r_redacted = true;
		}
	}
	return message;
}

void RuntimeDebuggerAdapter::_append_diagnostic(const String &p_severity, const String &p_source, const String &p_message, const String &p_script_path, int p_line, const String &p_function, const String &p_stack_id) {
	bool redacted = false;
	bool truncated = false;
	Dictionary diagnostic;
	const uint64_t next_seq = revisions->get_runtime_event_seq() + 1;
	diagnostic["kind"] = "runtime_diagnostic";
	const String id = _make_opaque_id("runtime-diagnostic:", "godot-codex-runtime-diagnostic/v1", String::num_uint64(next_seq) + "\n" + p_message);
	diagnostic["entity_id"] = id;
	diagnostic["runtime_diagnostic_id"] = id;
	diagnostic["severity"] = p_severity;
	diagnostic["source"] = p_source;
	diagnostic["message"] = _redact_message(p_message, redacted, truncated);
	diagnostic["repeat_count"] = (int64_t)1;
	diagnostic["runtime_event_seq"] = (int64_t)next_seq;
	if (safe_res_path(p_script_path)) {
		diagnostic["script_path"] = bounded_string(p_script_path, 1024, truncated);
		if (p_line > 0) {
			diagnostic["line"] = p_line;
		}
	}
	if (!p_function.is_empty()) {
		diagnostic["function"] = bounded_string(p_function, 1024, truncated);
	}
	if (!p_stack_id.is_empty()) {
		diagnostic["runtime_stack_id"] = p_stack_id;
	}
	diagnostic["redacted"] = redacted;
	const int bytes = JSON::stringify(diagnostic, "", true, true).utf8().length();
	while (!diagnostics.is_empty() && (diagnostics.size() >= MAX_DIAGNOSTICS || diagnostic_bytes + bytes > MAX_DIAGNOSTIC_BYTES)) {
		diagnostic_bytes -= JSON::stringify(diagnostics[0], "", true, true).utf8().length();
		diagnostics.remove_at(0);
	}
	if (bytes <= MAX_DIAGNOSTIC_BYTES) {
		diagnostics.push_back(diagnostic);
		diagnostic_bytes += bytes;
	}
}

String RuntimeDebuggerAdapter::_append_stack(const String &p_kind, const Array &p_frames, const String &p_identity) {
	bool truncated = false;
	Array frames;
	for (int index = 0; index < MIN(p_frames.size(), MAX_STACK_FRAMES); index++) {
		if (p_frames[index].get_type() != Variant::DICTIONARY) {
			truncated = true;
			continue;
		}
		const Dictionary source = p_frames[index];
		Dictionary frame;
		frame["frame"] = frames.size();
		const String path = source.get("script_path", source.get("file", String()));
		if (safe_res_path(path)) {
			frame["script_path"] = bounded_string(path, 1024, truncated);
		}
		frame["function"] = bounded_string(String(source.get("function", String())), 1024, truncated);
		frame["line"] = MAX(1, (int)source.get("line", 1));
		frames.push_back(frame);
	}
	truncated = truncated || p_frames.size() > MAX_STACK_FRAMES;
	const String id = _make_opaque_id("runtime-stack:", "godot-codex-runtime-stack/v1", p_kind + "\n" + p_identity + "\n" + String::num_uint64(revisions->get_runtime_event_seq() + 1));
	Dictionary stack;
	stack["kind"] = "runtime_stack";
	stack["entity_id"] = id;
	stack["runtime_stack_id"] = id;
	stack["stack_kind"] = p_kind;
	stack["frames"] = frames;
	stack["truncated"] = truncated;
	const int bytes = JSON::stringify(stack, "", true, true).utf8().length();
	while (!stack_order.is_empty() && (stack_order.size() >= MAX_STACKS || stack_bytes + bytes > MAX_STACK_BYTES)) {
		const String retired_id = stack_order[0];
		stack_bytes -= JSON::stringify(stacks[retired_id], "", true, true).utf8().length();
		stacks.erase(retired_id);
		stack_order.remove_at(0);
	}
	if (bytes > MAX_STACK_BYTES) {
		return String();
	}
	stacks[id] = stack;
	stack_order.push_back(id);
	stack_bytes += bytes;
	return id;
}

Array RuntimeDebuggerAdapter::_project_tree(const Array &p_serialized, bool &r_truncated) {
	Array entities;
	object_ids.clear();
	opaque_by_object_id.clear();
	if (p_serialized.size() % 6 != 0 || p_serialized.size() / 6 > MAX_TREE_NODES) {
		r_truncated = true;
		return entities;
	}
	struct Parent {
		String id;
		String path;
		String source_scene;
		String source_root_path;
		Array instance_scenes;
		int remaining = 0;
	};
	Vector<Parent> parents;
	for (int offset = 0; offset < p_serialized.size(); offset += 6) {
		while (!parents.is_empty() && parents[parents.size() - 1].remaining == 0) {
			parents.resize(parents.size() - 1);
		}
		if (p_serialized[offset].get_type() != Variant::INT || p_serialized[offset + 1].get_type() != Variant::STRING || p_serialized[offset + 2].get_type() != Variant::STRING || p_serialized[offset + 3].get_type() != Variant::INT || p_serialized[offset + 4].get_type() != Variant::STRING || p_serialized[offset + 5].get_type() != Variant::INT) {
			r_truncated = true;
			break;
		}
		const int child_count = p_serialized[offset];
		if (child_count < 0 || child_count > MAX_TREE_NODES || parents.size() >= MAX_TREE_DEPTH) {
			r_truncated = true;
			break;
		}
		if (!parents.is_empty()) {
			parents.write[parents.size() - 1].remaining--;
		}
		const uint64_t raw_id = p_serialized[offset + 3];
		const String opaque_id = _make_opaque_id("runtime-object:", "godot-codex-runtime-object/v1", String::num_uint64(raw_id));
		const String name = bounded_string(String(p_serialized[offset + 1]), 1024, r_truncated);
		String node_path = parents.is_empty() ? "/" + name : parents[parents.size() - 1].path.path_join(name);
		node_path = bounded_string(node_path, 1024, r_truncated);
		String source_scene = parents.is_empty() ? String() : parents[parents.size() - 1].source_scene;
		String source_root_path = parents.is_empty() ? String() : parents[parents.size() - 1].source_root_path;
		Array instance_scenes = parents.is_empty() ? Array() : parents[parents.size() - 1].instance_scenes.duplicate();
		const String observed_scene = p_serialized[offset + 4];
		if (safe_res_path(observed_scene)) {
			source_scene = observed_scene;
			source_root_path = node_path;
			if (!instance_scenes.has(observed_scene) && instance_scenes.size() < 256) {
				instance_scenes.push_back(observed_scene);
			}
		}
		Dictionary entity;
		entity["kind"] = "runtime_node";
		entity["entity_id"] = opaque_id;
		entity["runtime_object_id"] = opaque_id;
		entity["parent_runtime_object_id"] = parents.is_empty() ? Variant() : Variant(parents[parents.size() - 1].id);
		entity["name"] = name;
		entity["godot_type"] = bounded_string(String(p_serialized[offset + 2]), 1024, r_truncated);
		entity["runtime_node_path"] = node_path;
		entity["depth"] = parents.size();
		entity["child_count"] = child_count;
		const int flags = p_serialized[offset + 5];
		Dictionary visibility;
		visibility["available"] = (flags & SceneDebuggerTree::RemoteNode::VIEW_HAS_VISIBLE_METHOD) != 0;
		if ((bool)visibility["available"]) {
			visibility["visible"] = (flags & SceneDebuggerTree::RemoteNode::VIEW_VISIBLE) != 0;
			visibility["visible_in_tree"] = (flags & SceneDebuggerTree::RemoteNode::VIEW_VISIBLE_IN_TREE) != 0;
		}
		entity["visibility"] = visibility;
		if (!source_scene.is_empty()) {
			Dictionary source_hint;
			source_hint["scene_path"] = source_scene;
			String relative = node_path == source_root_path ? "." : node_path.trim_prefix(source_root_path + "/");
			source_hint["relative_node_path"] = bounded_string(relative, 1024, r_truncated);
			source_hint["instance_scene_paths"] = instance_scenes;
			entity["source_hint"] = source_hint;
		}
		entities.push_back(entity);
		object_ids.insert(opaque_id, raw_id);
		opaque_by_object_id.insert(raw_id, opaque_id);
		if (child_count > 0) {
			Parent parent;
			parent.id = opaque_id;
			parent.path = node_path;
			parent.source_scene = source_scene;
			parent.source_root_path = source_root_path;
			parent.instance_scenes = instance_scenes;
			parent.remaining = child_count;
			parents.push_back(parent);
		}
	}
	if (!parents.is_empty()) {
		for (const Parent &parent : parents) {
			if (parent.remaining != 0) {
				r_truncated = true;
				break;
			}
		}
	}
	return entities;
}

Dictionary RuntimeDebuggerAdapter::_project_object(const Array &p_serialized, bool p_game_truncated, bool &r_truncated) {
	Dictionary result;
	r_truncated = p_game_truncated;
	if (p_serialized.size() != 3 || p_serialized[0].get_type() != Variant::INT || p_serialized[1].get_type() != Variant::STRING || p_serialized[2].get_type() != Variant::ARRAY) {
		return result;
	}
	const uint64_t raw_id = p_serialized[0];
	const String *opaque_id = opaque_by_object_id.getptr(raw_id);
	if (!opaque_id) {
		return Dictionary();
	}
	result["schema_version"] = "runtime/1.0";
	result["runtime_session_id"] = runtime_session_id;
	result["runtime_event_seq"] = (int64_t)(revisions->get_runtime_event_seq() + 1);
	result["state"] = state;
	result["runtime_object_id"] = *opaque_id;
	result["godot_type"] = bounded_string(String(p_serialized[1]), 1024, r_truncated);
	Array projected_properties;
	const Array properties = p_serialized[2];
	int total_bytes = 0;
	for (int index = 0; index < MIN(properties.size(), MAX_PROPERTIES); index++) {
		if (properties[index].get_type() != Variant::ARRAY) {
			r_truncated = true;
			continue;
		}
		const Array property = properties[index];
		if ((property.size() != 6 && property.size() != 7) || property[0].get_type() != Variant::STRING || property[1].get_type() != Variant::INT || property[4].get_type() != Variant::INT || (property.size() == 7 && property[6].get_type() != Variant::BOOL)) {
			r_truncated = true;
			continue;
		}
		Dictionary projected;
		projected["name"] = bounded_string(String(property[0]), 1024, r_truncated);
		const Variant::Type type = (Variant::Type)(int)property[1];
		projected["variant_type"] = Variant::get_type_name(type);
		projected["read_only"] = true;
		projected["usage"] = property[4];
		Dictionary value;
		if ((type == Variant::OBJECT || (int)property[2] == PROPERTY_HINT_OBJECT_ID) && property[5].get_type() == Variant::INT) {
			const uint64_t referenced_raw_id = property[5];
			const String *referenced_id = opaque_by_object_id.getptr(referenced_raw_id);
			value["type"] = "object";
			if (referenced_id) {
				value["runtime_object_id"] = *referenced_id;
			} else {
				value["omitted_reason"] = "object_outside_snapshot";
			}
		} else if (property.size() == 7 && (bool)property[6] && property[5].get_type() == Variant::DICTIONARY) {
			value = property[5];
		} else {
			value = BoundedVariantProjector::project_typed(property[5]);
		}
		projected["value"] = value;
		const int bytes = JSON::stringify(projected, "", true, true).utf8().length();
		if (bytes > BoundedVariantProjector::MAX_ENCODED_BYTES || total_bytes + bytes > MAX_OBJECT_BYTES) {
			r_truncated = true;
			break;
		}
		total_bytes += bytes;
		projected_properties.push_back(projected);
	}
	r_truncated = r_truncated || properties.size() > MAX_PROPERTIES;
	result["properties"] = projected_properties;
	result["limits_applied"] = _make_limits(r_truncated);
	return result;
}

void RuntimeDebuggerAdapter::_complete_snapshot(const Array &p_tree, bool p_tree_truncated) {
	if (!pending_snapshot) {
		return;
	}
	Array domains = pending_snapshot_params.get("domains", Array());
	if (domains.is_empty()) {
		for (const char *domain : { "runtime_state", "runtime_tree", "runtime_diagnostics", "runtime_stacks" }) {
			domains.push_back(domain);
		}
	}
	Array entities;
	if (domains.has("runtime_state")) {
		Dictionary runtime_state;
		runtime_state["kind"] = "runtime_state";
		runtime_state["entity_id"] = runtime_session_id;
		runtime_state["runtime_session_id"] = runtime_session_id;
		runtime_state["runtime_event_seq"] = (int64_t)revisions->get_runtime_event_seq();
		runtime_state["state"] = state;
		runtime_state["origin"] = origin;
		runtime_state["target"] = target;
		if (safe_res_path(scene_path)) {
			runtime_state["scene_path"] = scene_path;
		}
		if (!terminal_reason.is_empty()) {
			runtime_state["terminal_reason"] = terminal_reason.left(128);
		}
		entities.push_back(runtime_state);
	}
	if (domains.has("runtime_tree")) {
		entities.append_array(p_tree);
	}
	if (domains.has("runtime_diagnostics")) {
		entities.append_array(diagnostics);
	}
	if (domains.has("runtime_stacks")) {
		for (const Variant &id : stack_order) {
			entities.push_back(stacks[id]);
		}
	}
	bool truncated = p_tree_truncated;
	Array bounded_entities;
	int total_bytes = 0;
	for (const Variant &entity : entities) {
		const int bytes = JSON::stringify(entity, "", true, true).utf8().length();
		if (bytes > 256 * 1024 || total_bytes + bytes > 16777216) {
			truncated = true;
			break;
		}
		total_bytes += bytes;
		bounded_entities.push_back(entity);
	}
	const String snapshot_id = "snapshot:" + _make_runtime_session_id().trim_prefix("runtime:");
	Array entity_chunks;
	Array current_chunk;
	int current_chunk_bytes = 192;
	for (const Variant &entity : bounded_entities) {
		const int entity_bytes = JSON::stringify(entity, "", true, true).utf8().length() + 1;
		// Each chunk carries both the structured payload and its canonical JSON.
		// Keep the canonical half below 240 KiB so the complete framed message
		// remains inside the protocol's 1 MiB envelope in the worst escaping case.
		if (!current_chunk.is_empty() && current_chunk_bytes + entity_bytes > 240 * 1024) {
			entity_chunks.push_back(current_chunk);
			current_chunk = Array();
			current_chunk_bytes = 192;
		}
		current_chunk.push_back(entity);
		current_chunk_bytes += entity_bytes;
	}
	if (!current_chunk.is_empty() || entity_chunks.is_empty()) {
		entity_chunks.push_back(current_chunk);
	}
	const int chunk_count = entity_chunks.size();
	const Dictionary revision_vector = revisions->get_revision_vector();
	Dictionary result;
	result["snapshot_id"] = snapshot_id;
	result["runtime_session_id"] = runtime_session_id;
	result["runtime_event_seq"] = (int64_t)revisions->get_runtime_event_seq();
	result["state"] = state;
	result["revisions"] = revision_vector;
	result["domains"] = domains;
	result["limits_applied"] = _make_limits(truncated);
	Array messages;
	Dictionary begin_params;
	begin_params["snapshot_id"] = snapshot_id;
	begin_params["domain"] = "runtime";
	begin_params["runtime_session_id"] = runtime_session_id;
	begin_params["runtime_event_seq"] = (int64_t)revisions->get_runtime_event_seq();
	begin_params["revisions"] = revision_vector;
	begin_params["chunk_count"] = chunk_count;
	Dictionary begin;
	begin["protocol_version"] = "1.6";
	begin["kind"] = "notification";
	begin["method"] = "snapshot.begin";
	begin["params"] = begin_params;
	begin["context"] = _make_context();
	messages.push_back(begin);
	String snapshot_checksum_input;
	for (int index = 0; index < chunk_count; index++) {
		const Array chunk_entities = entity_chunks[index];
		Dictionary payload;
		payload["domain"] = "runtime";
		payload["runtime_session_id"] = runtime_session_id;
		payload["runtime_event_seq"] = (int64_t)revisions->get_runtime_event_seq();
		payload["entities"] = chunk_entities;
		const String payload_json = JSON::stringify(payload, "", true, true);
		const String chunk_checksum = sha256_hex_utf8(payload_json);
		Dictionary chunk;
		chunk["protocol_version"] = "1.6";
		chunk["kind"] = "chunk";
		chunk["domain"] = "runtime";
		chunk["snapshot_id"] = snapshot_id;
		chunk["chunk_index"] = index;
		chunk["payload"] = payload;
		chunk["payload_json"] = payload_json;
		chunk["checksum"] = chunk_checksum;
		chunk["context"] = _make_context();
		messages.push_back(chunk);
		snapshot_checksum_input += chunk_checksum;
	}
	Dictionary end_params;
	end_params["snapshot_id"] = snapshot_id;
	end_params["domain"] = "runtime";
	end_params["runtime_session_id"] = runtime_session_id;
	end_params["runtime_event_seq"] = (int64_t)revisions->get_runtime_event_seq();
	end_params["chunk_count"] = chunk_count;
	end_params["entity_count"] = bounded_entities.size();
	end_params["checksum"] = sha256_hex_utf8(snapshot_checksum_input);
	end_params["revisions"] = revision_vector;
	Dictionary end;
	end["protocol_version"] = "1.6";
	end["kind"] = "notification";
	end["method"] = "snapshot.end";
	end["params"] = end_params;
	end["context"] = _make_context();
	messages.push_back(end);
	transport->complete_request(pending_snapshot, result, messages);
	pending_snapshot = 0;
	pending_snapshot_params.clear();
	pending_tree_correlation.clear();
}

void RuntimeDebuggerAdapter::_on_started(int p_session_id) {
	if (active_debugger_session >= 0 && active_debugger_session != p_session_id) {
		ScriptEditorDebugger *active = _get_active_debugger();
		if (active && active->is_session_active() && !runtime_session_id.is_empty() && !_is_terminal()) {
			_transition("failed", "failed", changed_domain("runtime_state"), "runtime_ambiguous");
			_fail_pending_live_requests("runtime_ambiguous", "Multiple debugger sessions are active; the Bridge will not choose one.", false);
			if (pending_run) {
				transport->complete_request_error(pending_run, "runtime_ambiguous", "Multiple debugger sessions are active; the Bridge will not choose one.", false, _safe_coordinates());
				pending_run = 0;
			}
			active_debugger_session = -1;
			return;
		}
	}
	if (runtime_session_id.is_empty() || _is_terminal()) {
		EditorRunBar *run_bar = EditorRunBar::get_singleton();
		_begin_session("editor", run_bar ? run_bar->get_playing_target() : "project", run_bar ? run_bar->get_playing_scene() : String());
	}
	active_debugger_session = p_session_id;
	disconnected_since_usec = 0;
	_transition("running", "debugger_connected", changed_domain("runtime_state"));
	_complete_pending_controls("running");
}

void RuntimeDebuggerAdapter::_on_stopped(int p_session_id) {
	if (p_session_id != active_debugger_session || runtime_session_id.is_empty() || _is_terminal()) {
		return;
	}
	if (EditorRunBar::get_singleton() && EditorRunBar::get_singleton()->is_playing() && state != "stopping") {
		disconnected_since_usec = OS::get_singleton()->get_ticks_usec();
		_transition("disconnected", "disconnected", changed_domain("runtime_state"));
		_fail_pending_live_requests("runtime_disconnected", "The runtime debugger disconnected.", true);
		return;
	}
	const bool stopped_normally = state == "stopping" || normal_quit_requested;
	_transition(stopped_normally ? "stopped" : "crashed", stopped_normally ? "stopped" : "crashed", changed_domain("runtime_state"), stopped_normally ? "requested" : "process_exit");
	if (stopped_normally) {
		_complete_pending_controls("stopped");
	} else if (pending_stop) {
		transport->complete_request_error(pending_stop, "runtime_crashed", "The runtime crashed before stop was confirmed.", false, _safe_coordinates());
		pending_stop = 0;
	}
	_fail_pending_live_requests(stopped_normally ? "runtime_data_retired" : "runtime_crashed", stopped_normally ? "The runtime stopped and retired live data." : "The runtime process exited unexpectedly.", false);
	if (pending_run) {
		transport->complete_request_error(pending_run, "runtime_start_failed", "The game stopped before the debugger became ready.", true, _safe_coordinates());
		pending_run = 0;
	}
}

void RuntimeDebuggerAdapter::_on_editor_stop_requested() {
	if (internal_forced_stop || runtime_session_id.is_empty() || _is_terminal()) {
		return;
	}
	normal_quit_requested = true;
	if (state != "stopping") {
		_transition("stopping", "stopping", changed_domain("runtime_state"), "editor_stop_requested");
	}
}

void RuntimeDebuggerAdapter::_on_stop_requested(int p_session_id) {
	if (p_session_id == active_debugger_session) {
		normal_quit_requested = true;
	}
}

void RuntimeDebuggerAdapter::_on_breaked(bool p_really_did, bool p_can_debug, const String &p_message, bool p_has_stackdump, int p_session_id) {
	if (p_session_id != active_debugger_session || runtime_session_id.is_empty() || _is_terminal()) {
		return;
	}
	if (p_really_did) {
		_transition("paused", "paused", changed_domain("runtime_state"));
		_complete_pending_controls("paused");
	} else {
		_transition("running", "continued", changed_domain("runtime_state"));
		_complete_pending_controls("running");
	}
}

void RuntimeDebuggerAdapter::_on_output(const String &p_message, int p_level, int p_session_id) {
	if (p_session_id != active_debugger_session || runtime_session_id.is_empty() || _is_terminal()) {
		return;
	}
	_append_diagnostic("info", "output", p_message);
	_transition(state, "diagnostic_added", changed_domain("runtime_diagnostics"));
}

void RuntimeDebuggerAdapter::_on_runtime_error(const Dictionary &p_error, int p_session_id) {
	if (p_session_id != active_debugger_session || runtime_session_id.is_empty()) {
		return;
	}
	String stack_id;
	const Array frames = p_error.get("frames", Array());
	if (!frames.is_empty()) {
		stack_id = _append_stack("diagnostic", frames, String::num_uint64(revisions->get_runtime_event_seq() + 1));
		if (!stack_id.is_empty()) {
			_transition(state, "stack_changed", changed_domain("runtime_stacks"));
		}
	}
	_append_diagnostic((bool)p_error.get("warning", false) ? "warning" : "error", "script", p_error.get("message", String()), p_error.get("script_path", String()), p_error.get("line", 0), p_error.get("function", String()), stack_id);
	_transition(state, "diagnostic_added", changed_domain("runtime_diagnostics"));
}

void RuntimeDebuggerAdapter::_on_runtime_stack(int64_t p_thread_id, const Array &p_frames, int p_session_id) {
	if (p_session_id != active_debugger_session || runtime_session_id.is_empty() || state != "paused") {
		return;
	}
	if (!_append_stack("pause", p_frames, String::num_int64(p_thread_id)).is_empty()) {
		_transition(state, "stack_changed", changed_domain("runtime_stacks"));
	}
}

void RuntimeDebuggerAdapter::_on_screenshot(int p_width, int p_height, const String &p_path, const Rect2i &p_rect) {
	if (!pending_capture) {
		return;
	}
	const uint64_t request_id = pending_capture;
	pending_capture = 0;
	const String temp_root = OS::get_singleton()->get_temp_path().simplify_path();
	const String safe_path = p_path.simplify_path();
	Ref<DirAccess> filesystem = DirAccess::create(DirAccess::ACCESS_FILESYSTEM);
	const bool path_valid = safe_path.get_base_dir() == temp_root && safe_path.get_file().begins_with("scr-") && safe_path.get_extension().to_lower() == "png" && filesystem.is_valid() && !filesystem->is_link(safe_path);
	if (!path_valid || p_width < 1 || p_height < 1 || p_width > 16384 || p_height > 16384 || (int64_t)p_width * p_height > 67108864 || !FileAccess::exists(safe_path) || FileAccess::get_size(safe_path) > MAX_SCREENSHOT_SOURCE_BYTES) {
		if (path_valid && FileAccess::exists(safe_path)) {
			DirAccess::remove_absolute(safe_path);
		}
		transport->complete_request_error(request_id, "runtime_capture_too_large", "The runtime screenshot source exceeded a safety limit.", false);
		return;
	}
	Ref<Image> image;
	image.instantiate();
	const Error load_error = image->load(safe_path);
	DirAccess::remove_absolute(safe_path);
	if (load_error != OK || image->is_empty()) {
		transport->complete_request_error(request_id, "runtime_capture_unavailable", "The runtime screenshot could not be decoded.", true);
		return;
	}
	const int max_width = pending_capture_params.get("max_width", MAX_SCREENSHOT_WIDTH);
	const int max_height = pending_capture_params.get("max_height", MAX_SCREENSHOT_HEIGHT);
	const float scale = MIN(1.0f, MIN((float)max_width / image->get_width(), (float)max_height / image->get_height()));
	if (scale < 1.0f) {
		image->resize(MAX(1, (int)(image->get_width() * scale)), MAX(1, (int)(image->get_height() * scale)), Image::INTERPOLATE_LANCZOS);
	}
	PackedByteArray png = image->save_png_to_buffer();
	while (png.size() > MAX_SCREENSHOT_BYTES && (image->get_width() > 160 || image->get_height() > 90)) {
		image->resize(MAX(1, image->get_width() / 2), MAX(1, image->get_height() / 2), Image::INTERPOLATE_LANCZOS);
		png = image->save_png_to_buffer();
	}
	if (png.is_empty() || png.size() > MAX_SCREENSHOT_BYTES) {
		transport->complete_request_error(request_id, "runtime_capture_too_large", "The bounded runtime screenshot still exceeds 512 KiB.", false);
		return;
	}
	PackedByteArray digest;
	digest.resize(32);
	if (CryptoCore::sha256(png.ptr(), png.size(), digest.ptrw()) != OK) {
		transport->complete_request_error(request_id, "runtime_capture_unavailable", "The runtime screenshot digest failed.", true);
		return;
	}
	String encoded = CryptoCore::b64_encode_str(png.ptr(), png.size()).replace("+", "-").replace("/", "_");
	while (encoded.ends_with("=")) {
		encoded = encoded.left(encoded.length() - 1);
	}
	_transition(state, "viewport_captured", changed_domain("runtime_state"));
	Dictionary result;
	result["schema_version"] = "runtime/1.0";
	result["runtime_session_id"] = runtime_session_id;
	result["runtime_event_seq"] = (int64_t)revisions->get_runtime_event_seq();
	result["state"] = state;
	result["mime_type"] = "image/png";
	result["width"] = image->get_width();
	result["height"] = image->get_height();
	result["byte_length"] = png.size();
	result["sha256"] = BridgeCrypto::bytes_to_lower_hex(digest);
	result["data_base64url"] = encoded;
	transport->complete_request(request_id, result);
	pending_capture_params.clear();
}

void RuntimeDebuggerAdapter::initialize(BridgeTransportWorker *p_transport, BridgeRevisionClock *p_revisions) {
	transport = p_transport;
	revisions = p_revisions;
	EditorRunBar *run_bar = EditorRunBar::get_singleton();
	if (run_bar && !run_bar->is_connected("stop_requested", callable_mp(this, &RuntimeDebuggerAdapter::_on_editor_stop_requested))) {
		run_bar->connect("stop_requested", callable_mp(this, &RuntimeDebuggerAdapter::_on_editor_stop_requested));
	}
}

void RuntimeDebuggerAdapter::shutdown() {
	EditorRunBar *run_bar = EditorRunBar::get_singleton();
	if (run_bar && run_bar->is_connected("stop_requested", callable_mp(this, &RuntimeDebuggerAdapter::_on_editor_stop_requested))) {
		run_bar->disconnect("stop_requested", callable_mp(this, &RuntimeDebuggerAdapter::_on_editor_stop_requested));
	}
	for (const KeyValue<int, ObjectID> &entry : debugger_sessions) {
		ScriptEditorDebugger *debugger = Object::cast_to<ScriptEditorDebugger>(ObjectDB::get_instance(entry.value));
		if (!debugger) {
			continue;
		}
		debugger->disconnect("started", callable_mp(this, &RuntimeDebuggerAdapter::_on_started).bind(entry.key));
		debugger->disconnect("stopped", callable_mp(this, &RuntimeDebuggerAdapter::_on_stopped).bind(entry.key));
		debugger->disconnect("stop_requested", callable_mp(this, &RuntimeDebuggerAdapter::_on_stop_requested).bind(entry.key));
		debugger->disconnect("breaked", callable_mp(this, &RuntimeDebuggerAdapter::_on_breaked).bind(entry.key));
		debugger->disconnect("output", callable_mp(this, &RuntimeDebuggerAdapter::_on_output).bind(entry.key));
		debugger->disconnect("runtime_error", callable_mp(this, &RuntimeDebuggerAdapter::_on_runtime_error).bind(entry.key));
		debugger->disconnect("runtime_stack_dump", callable_mp(this, &RuntimeDebuggerAdapter::_on_runtime_stack).bind(entry.key));
	}
	debugger_sessions.clear();
	transport = nullptr;
	revisions = nullptr;
}

void RuntimeDebuggerAdapter::process() {
	if (!transport || !revisions) {
		return;
	}
	EditorRunBar *run_bar = EditorRunBar::get_singleton();
	const bool playing = run_bar && run_bar->is_playing();
	if (playing && (runtime_session_id.is_empty() || _is_terminal())) {
		_begin_session("editor", run_bar->get_playing_target(), run_bar->get_playing_scene());
	}
	const uint64_t now = OS::get_singleton()->get_ticks_usec();
	if (state == "starting" && now - state_since_usec >= 10000000) {
		if (run_bar && run_bar->is_playing()) {
			internal_forced_stop = true;
			run_bar->stop_playing();
			internal_forced_stop = false;
		}
		_transition("timed_out", "timed_out", changed_domain("runtime_state"), "debugger_start_timeout");
		_fail_pending_live_requests("runtime_start_timeout", "The debugger did not connect within 10 seconds.", true);
		if (pending_run) {
			transport->complete_request_error(pending_run, "runtime_start_timeout", "The debugger did not connect within 10 seconds.", true, _safe_coordinates());
			pending_run = 0;
		}
	} else if (state == "disconnected" && disconnected_since_usec > 0 && now - disconnected_since_usec >= 2000000) {
		if (run_bar && run_bar->is_playing()) {
			internal_forced_stop = true;
			run_bar->stop_playing();
			internal_forced_stop = false;
		}
		_transition("crashed", "crashed", changed_domain("runtime_state"), "debugger_disconnect_timeout");
		_fail_pending_live_requests("runtime_crashed", "The disconnected runtime did not reconnect within two seconds.", false);
	} else if (!playing && !runtime_session_id.is_empty() && !_is_terminal() && state != "starting") {
		const bool stopped_normally = state == "stopping" || normal_quit_requested;
		_transition(stopped_normally ? "stopped" : "crashed", stopped_normally ? "stopped" : "crashed", changed_domain("runtime_state"), stopped_normally ? "requested" : "process_exit");
		if (stopped_normally) {
			_complete_pending_controls("stopped");
		} else if (pending_stop) {
			transport->complete_request_error(pending_stop, "runtime_crashed", "The runtime crashed before stop was confirmed.", false, _safe_coordinates());
			pending_stop = 0;
		}
		_fail_pending_live_requests(stopped_normally ? "runtime_data_retired" : "runtime_crashed", stopped_normally ? "The runtime stopped and retired live data." : "The runtime process exited unexpectedly.", false);
	}
}

void RuntimeDebuggerAdapter::cancel(uint64_t p_request_id) {
	for (uint64_t *pending : { &pending_run, &pending_stop, &pending_pause, &pending_continue, &pending_snapshot, &pending_object, &pending_capture }) {
		if (*pending == p_request_id) {
			*pending = 0;
		}
	}
}

void RuntimeDebuggerAdapter::run(uint64_t p_request_id, const Dictionary &p_params) {
	EditorRunBar *run_bar = EditorRunBar::get_singleton();
	if (!run_bar) {
		transport->complete_request_error(p_request_id, "runtime_start_failed", "The editor run controller is unavailable.", true);
		return;
	}
	if (run_bar->is_playing() || (!runtime_session_id.is_empty() && !_is_terminal())) {
		transport->complete_request_error(p_request_id, "runtime_already_active", "A runtime is already active; stop it explicitly first.", false, _safe_coordinates());
		return;
	}
	if (EditorNode::has_unsaved_changes() || (ScriptEditor::get_singleton() && !ScriptEditor::get_singleton()->get_unsaved_scripts().is_empty())) {
		transport->complete_request_error(p_request_id, "runtime_unsaved_changes", "Save editor scenes and scripts before a read-only diagnostic run.", false);
		return;
	}
	const String requested_target = p_params["target"];
	String requested_scene;
	if (requested_target == "current_scene") {
		Node *root = EditorNode::get_editor_data().get_edited_scene_root();
		if (!root || !safe_res_path(root->get_scene_file_path())) {
			transport->complete_request_error(p_request_id, "runtime_start_failed", "The current scene is not a saved project scene.", false);
			return;
		}
		requested_scene = root->get_scene_file_path();
	} else {
		requested_scene = GLOBAL_GET("application/run/main_scene");
		if (!safe_res_path(requested_scene)) {
			transport->complete_request_error(p_request_id, "runtime_start_failed", "The project main scene is not configured.", false);
			return;
		}
	}
	_begin_session("mcp", requested_target, requested_scene);
	pending_run = p_request_id;
	const Error error = requested_target == "current_scene" ? run_bar->play_current_scene_read_only() : run_bar->play_main_scene_read_only();
	if (error != OK) {
		pending_run = 0;
		_transition("failed", "failed", changed_domain("runtime_state"), "editor_run_failed");
		transport->complete_request_error(p_request_id, "runtime_start_failed", "The editor rejected the diagnostic run.", true, _safe_coordinates());
	}
}

void RuntimeDebuggerAdapter::stop(uint64_t p_request_id, const Dictionary &p_params) {
	if (!_guard(p_request_id, p_params, true)) {
		return;
	}
	if (pending_stop) {
		transport->complete_request_error(p_request_id, "runtime_control_timeout", "A stop request is already pending.", true, _safe_coordinates());
		return;
	}
	pending_stop = p_request_id;
	_transition("stopping", "stopping", changed_domain("runtime_state"), "stop_requested");
	EditorRunBar::get_singleton()->stop_playing();
	if (!EditorRunBar::get_singleton()->is_playing() && state == "stopping") {
		_transition("stopped", "stopped", changed_domain("runtime_state"), "requested");
		_complete_pending_controls("stopped");
	}
}

void RuntimeDebuggerAdapter::pause(uint64_t p_request_id, const Dictionary &p_params) {
	if (!_guard(p_request_id, p_params, true, false)) {
		return;
	}
	if (state != "running") {
		transport->complete_request_error(p_request_id, "runtime_not_running", "The runtime is not running.", true, _safe_coordinates());
		return;
	}
	ScriptEditorDebugger *debugger = _get_active_debugger();
	if (!debugger || !debugger->is_session_active()) {
		transport->complete_request_error(p_request_id, "runtime_disconnected", "The debugger is disconnected.", true, _safe_coordinates());
		return;
	}
	pending_pause = p_request_id;
	debugger->debug_break();
}

void RuntimeDebuggerAdapter::continue_run(uint64_t p_request_id, const Dictionary &p_params) {
	if (!_guard(p_request_id, p_params, true)) {
		return;
	}
	if (state != "paused") {
		transport->complete_request_error(p_request_id, "runtime_not_paused", "The runtime is not paused.", true, _safe_coordinates());
		return;
	}
	ScriptEditorDebugger *debugger = _get_active_debugger();
	if (!debugger || !debugger->is_session_active()) {
		transport->complete_request_error(p_request_id, "runtime_disconnected", "The debugger is disconnected.", true, _safe_coordinates());
		return;
	}
	pending_continue = p_request_id;
	debugger->debug_continue();
}

void RuntimeDebuggerAdapter::snapshot(uint64_t p_request_id, const Dictionary &p_params) {
	if (!_guard(p_request_id, p_params, false)) {
		return;
	}
	if (pending_snapshot) {
		transport->complete_request_error(p_request_id, "runtime_request_timeout", "A runtime snapshot is already pending.", true, _safe_coordinates());
		return;
	}
	pending_snapshot = p_request_id;
	pending_snapshot_params = p_params;
	const Array domains = p_params.get("domains", Array());
	if (!domains.is_empty() && !domains.has("runtime_tree")) {
		_complete_snapshot(runtime_tree, false);
		return;
	}
	if (_is_terminal()) {
		_complete_snapshot(Array(), false);
		return;
	}
	ScriptEditorDebugger *debugger = _get_active_debugger();
	if (!debugger || !debugger->is_session_active()) {
		pending_snapshot = 0;
		transport->complete_request_error(p_request_id, "runtime_disconnected", "The runtime tree is unavailable while disconnected.", true, _safe_coordinates());
		return;
	}
	pending_tree_correlation = "runtime-tree-" + String::num_uint64(p_request_id);
	debugger->send_message("scene:codex_runtime_tree", Array{ pending_tree_correlation, MAX_TREE_NODES, MAX_TREE_DEPTH });
}

void RuntimeDebuggerAdapter::inspect_object(uint64_t p_request_id, const Dictionary &p_params) {
	if (!_guard(p_request_id, p_params, true)) {
		return;
	}
	if (pending_object) {
		transport->complete_request_error(p_request_id, "runtime_request_timeout", "A runtime object observation is already pending.", true, _safe_coordinates());
		return;
	}
	const String opaque_id = p_params["runtime_object_id"];
	const uint64_t *raw_id = object_ids.getptr(opaque_id);
	if (!raw_id) {
		transport->complete_request_error(p_request_id, "runtime_object_stale", "The runtime object is not present in the current tree snapshot.", true, _safe_coordinates());
		return;
	}
	ScriptEditorDebugger *debugger = _get_active_debugger();
	if (!debugger || !debugger->is_session_active()) {
		transport->complete_request_error(p_request_id, "runtime_disconnected", "The debugger is disconnected.", true, _safe_coordinates());
		return;
	}
	pending_object = p_request_id;
	pending_object_params = p_params;
	pending_object_correlation = "runtime-object-" + String::num_uint64(p_request_id);
	debugger->send_message("scene:codex_runtime_object", Array{ pending_object_correlation, (int64_t)*raw_id });
}

void RuntimeDebuggerAdapter::get_stack(uint64_t p_request_id, const Dictionary &p_params) {
	if (!_guard(p_request_id, p_params, false)) {
		return;
	}
	const String stack_id = p_params["runtime_stack_id"];
	if (!stacks.has(stack_id)) {
		transport->complete_request_error(p_request_id, "runtime_data_retired", "The requested runtime stack was retired.", false, _safe_coordinates());
		return;
	}
	Dictionary result;
	result["schema_version"] = "runtime/1.0";
	result["runtime_session_id"] = runtime_session_id;
	result["runtime_event_seq"] = (int64_t)revisions->get_runtime_event_seq();
	result["state"] = state;
	result["stack"] = stacks[stack_id];
	transport->complete_request(p_request_id, result);
}

void RuntimeDebuggerAdapter::capture_viewport(uint64_t p_request_id, const Dictionary &p_params) {
	if (!_guard(p_request_id, p_params, true)) {
		return;
	}
	const uint64_t now = OS::get_singleton()->get_ticks_usec();
	if (pending_capture || (last_capture_usec > 0 && now - last_capture_usec < 1000000)) {
		transport->complete_request_error(p_request_id, "runtime_capture_rate_limited", "Runtime capture is limited to one request per second.", true, _safe_coordinates());
		return;
	}
	pending_capture = p_request_id;
	pending_capture_params = p_params;
	last_capture_usec = now;
	if (!EditorRun::request_screenshot(callable_mp(this, &RuntimeDebuggerAdapter::_on_screenshot))) {
		pending_capture = 0;
		transport->complete_request_error(p_request_id, "runtime_capture_unavailable", "The runtime viewport cannot provide a screenshot.", true, _safe_coordinates());
	}
}

bool RuntimeDebuggerAdapter::has_capture(const String &p_capture) const {
	return p_capture == "codex_runtime";
}

bool RuntimeDebuggerAdapter::capture(const String &p_message, const Array &p_data, int p_session_id) {
	if (p_session_id != active_debugger_session) {
		return true;
	}
	if (p_message == "codex_runtime:tree") {
		if (!pending_snapshot || p_data.size() != 3 || String(p_data[0]) != pending_tree_correlation || p_data[1].get_type() != Variant::BOOL || p_data[2].get_type() != Variant::ARRAY) {
			return true;
		}
		bool truncated = p_data[1];
		Array projected_tree = _project_tree(p_data[2], truncated);
		const String projected_checksum = sha256_hex_utf8(JSON::stringify(projected_tree, "", true, true));
		if (projected_checksum.is_empty()) {
			const uint64_t request_id = pending_snapshot;
			pending_snapshot = 0;
			transport->complete_request_error(request_id, "runtime_request_timeout", "The runtime tree digest could not be produced.", true, _safe_coordinates());
			return true;
		}
		const bool changed = projected_checksum != runtime_tree_checksum;
		runtime_tree = projected_tree;
		runtime_tree_checksum = projected_checksum;
		if (changed) {
			_transition(state, "tree_changed", changed_domain("runtime_tree"));
		}
		_complete_snapshot(runtime_tree, truncated);
		return true;
	}
	if (p_message == "codex_runtime:object") {
		if (!pending_object || p_data.size() != 4 || String(p_data[0]) != pending_object_correlation || p_data[1].get_type() != Variant::BOOL || p_data[2].get_type() != Variant::BOOL || p_data[3].get_type() != Variant::ARRAY) {
			return true;
		}
		const uint64_t request_id = pending_object;
		pending_object = 0;
		if (!(bool)p_data[1]) {
			transport->complete_request_error(request_id, "runtime_object_not_found", "The runtime object no longer exists.", true, _safe_coordinates());
			return true;
		}
		bool truncated = p_data[2];
		Dictionary result = _project_object(p_data[3], truncated, truncated);
		if (result.is_empty()) {
			transport->complete_request_error(request_id, "runtime_object_stale", "The runtime object response did not match the current tree.", true, _safe_coordinates());
			return true;
		}
		_transition(state, "object_observed", changed_domain("runtime_state"));
		result["runtime_event_seq"] = (int64_t)revisions->get_runtime_event_seq();
		result["limits_applied"] = _make_limits(truncated);
		transport->complete_request(request_id, result);
		pending_object_params.clear();
		pending_object_correlation.clear();
		return true;
	}
	return false;
}

void RuntimeDebuggerAdapter::setup_session(int p_session_id) {
	Ref<EditorDebuggerSession> session = get_session(p_session_id);
	ERR_FAIL_COND(session.is_null());
	ScriptEditorDebugger *debugger = session->get_debugger();
	ERR_FAIL_NULL(debugger);
	debugger_sessions.insert(p_session_id, debugger->get_instance_id());
	debugger->connect("started", callable_mp(this, &RuntimeDebuggerAdapter::_on_started).bind(p_session_id));
	debugger->connect("stopped", callable_mp(this, &RuntimeDebuggerAdapter::_on_stopped).bind(p_session_id));
	debugger->connect("stop_requested", callable_mp(this, &RuntimeDebuggerAdapter::_on_stop_requested).bind(p_session_id));
	debugger->connect("breaked", callable_mp(this, &RuntimeDebuggerAdapter::_on_breaked).bind(p_session_id));
	debugger->connect("output", callable_mp(this, &RuntimeDebuggerAdapter::_on_output).bind(p_session_id));
	debugger->connect("runtime_error", callable_mp(this, &RuntimeDebuggerAdapter::_on_runtime_error).bind(p_session_id));
	debugger->connect("runtime_stack_dump", callable_mp(this, &RuntimeDebuggerAdapter::_on_runtime_stack).bind(p_session_id));
}
