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
#include "core/io/marshalls.h"
#include "core/object/callable_mp.h"
#include "core/os/os.h"
#include "editor/debugger/script_editor_debugger.h"
#include "editor/editor_log.h"
#include "editor/editor_node.h"
#include "editor/run/editor_run.h"
#include "editor/run/editor_run_bar.h"
#include "editor/script/script_editor_plugin.h"
#include "scene/debugger/scene_debugger_object.h"

#include "modules/codex_bridge/protocol/bridge_crypto.h"
#include "modules/codex_bridge/transport/bridge_transport_worker.h"

namespace {

static Array changed_domain(const String &p_domain) {
	Array domains;
	domains.push_back(p_domain);
	return domains;
}

static bool safe_res_path(const String &p_path) {
	return p_path.begins_with("res://") && p_path.length() <= CodexRuntimeLimits::NODE_STRING_CHARACTERS && !p_path.contains("\\") && !p_path.contains("/../") && !p_path.ends_with("/..");
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

static bool token_has_absolute_path(const String &p_token) {
	const String normalized = p_token.replace("\\", "/");
	const String lower = normalized.to_lower();
	if (lower.contains("user://")) {
		return true;
	}
	for (int index = 0; index < normalized.length(); index++) {
		const char32_t current = normalized[index];
		const bool boundary = index == 0 || String("=:;,([{\"'").contains_char(normalized[index - 1]);
		if (current == '/' && index >= 4 && lower.substr(index - 4, 6) == "res://") {
			continue;
		}
		if (current == '/' && boundary) {
			return true;
		}
		if (index + 2 < normalized.length() && boundary && ((current >= 'A' && current <= 'Z') || (current >= 'a' && current <= 'z')) && normalized[index + 1] == ':' && normalized[index + 2] == '/') {
			return true;
		}
	}
	return false;
}

static bool token_has_endpoint(const String &p_token) {
	const String lower = p_token.to_lower();
	for (const char *scheme : { "http://", "https://", "ws://", "wss://", "tcp://", "udp://" }) {
		if (lower.contains(scheme)) {
			return true;
		}
	}
	return false;
}

static String scrub_message_tokens(const String &p_message, bool &r_redacted) {
	String result;
	int start = 0;
	while (start < p_message.length()) {
		if (p_message[start] <= 0x20) {
			result += String::chr(p_message[start]);
			start++;
			continue;
		}
		int end = start + 1;
		while (end < p_message.length() && p_message[end] > 0x20) {
			end++;
		}
		const String token = p_message.substr(start, end - start);
		if (token_has_endpoint(token)) {
			result += "<endpoint>";
			r_redacted = true;
		} else if (token_has_absolute_path(token)) {
			result += "<path>";
			r_redacted = true;
		} else {
			result += token;
		}
		start = end;
	}
	return result;
}

} // namespace

bool RuntimeLifecyclePolicy::is_terminal(const String &p_state) {
	return p_state == "stopped" || p_state == "crashed" || p_state == "failed" || p_state == "timed_out";
}

bool RuntimeLifecyclePolicy::is_reconnect(const String &p_state) {
	return p_state == "disconnected";
}

String RuntimeLifecyclePolicy::classify_debugger_stop(const String &p_state, bool p_editor_playing, bool p_normal_quit_requested) {
	if (p_state == "stopping" || p_normal_quit_requested) {
		return "stopped";
	}
	return p_editor_playing ? "disconnected" : "crashed";
}

String RuntimeLifecyclePolicy::classify_process_exit(const String &p_state, bool p_normal_quit_requested) {
	return p_state == "stopping" || p_normal_quit_requested ? "stopped" : "crashed";
}

String RuntimeDiagnosticPolicy::bound_utf8(const String &p_value, int p_max_bytes, bool &r_truncated, bool p_add_marker) {
	ERR_FAIL_COND_V(p_max_bytes < 0, String());
	if (p_value.utf8().length() <= p_max_bytes) {
		return p_value;
	}
	r_truncated = true;
	const String marker = p_add_marker ? " [truncated]" : String();
	const int marker_bytes = marker.utf8().length();
	const int content_limit = MAX(0, p_max_bytes - marker_bytes);
	int low = 0;
	int high = p_value.length();
	while (low < high) {
		const int middle = low + (high - low + 1) / 2;
		if (p_value.left(middle).utf8().length() <= content_limit) {
			low = middle;
		} else {
			high = middle - 1;
		}
	}
	String bounded = p_value.left(low);
	if (p_add_marker && marker_bytes <= p_max_bytes) {
		bounded += marker;
	}
	return bounded;
}

String RuntimeDiagnosticPolicy::sanitize_message(const String &p_message, const String &p_project_root, const String &p_home_root, const String &p_temp_root, bool &r_redacted, bool &r_truncated) {
	r_redacted = false;
	r_truncated = false;
	String input = p_message;
	if (input.length() > 65536) {
		input = input.left(65536);
		r_truncated = true;
	}
	String cleaned;
	for (int index = 0; index < input.length(); index++) {
		const char32_t character = input[index];
		if (character == 0x1b) {
			r_redacted = true;
			if (index + 1 < input.length() && input[index + 1] == '[') {
				index += 2;
				while (index < input.length() && !(input[index] >= 0x40 && input[index] <= 0x7e)) {
					index++;
				}
			}
			continue;
		}
		if (character == '\r') {
			if (index + 1 < input.length() && input[index + 1] == '\n') {
				continue;
			}
			cleaned += "\n";
			continue;
		}
		if ((character < 0x20 && character != '\n' && character != '\t') || character == 0x7f) {
			cleaned += " ";
			r_redacted = true;
			continue;
		}
		cleaned += String::chr(character);
	}

	const String lower = cleaned.to_lower();
	for (const char *marker : { "authorization:", "bearer ", "api_key", "api-key", "apikey", "password=", "password:", "secret=", "secret:", "token=", "token:", "access_token", "private_key" }) {
		if (lower.contains(marker)) {
			r_redacted = true;
			return "<redacted sensitive runtime output>";
		}
	}

	String normalized = cleaned.replace("\\", "/");
	const String roots[] = { p_project_root.replace("\\", "/"), p_home_root.replace("\\", "/"), p_temp_root.replace("\\", "/") };
	const String placeholders[] = { "<project>", "<home>", "<temp>" };
	for (int index = 0; index < 3; index++) {
		if (!roots[index].is_empty() && normalized.contains(roots[index])) {
			normalized = normalized.replace(roots[index], placeholders[index]);
			r_redacted = true;
		}
	}
	normalized = scrub_message_tokens(normalized, r_redacted);
	bool byte_truncated = false;
	normalized = bound_utf8(normalized, 16384, byte_truncated, true);
	r_truncated = r_truncated || byte_truncated;
	return normalized;
}

Array RuntimeDiagnosticPolicy::sanitize_frames(const Array &p_frames, int p_max_frames, bool &r_truncated) {
	Array frames;
	for (int index = 0; index < p_frames.size(); index++) {
		if (frames.size() >= p_max_frames) {
			r_truncated = true;
			break;
		}
		if (p_frames[index].get_type() != Variant::DICTIONARY) {
			r_truncated = true;
			continue;
		}
		const Dictionary source = p_frames[index];
		const String path = source.get("script_path", source.get("file", String()));
		if (!safe_res_path(path)) {
			r_truncated = true;
			continue;
		}
		Dictionary frame;
		frame["frame"] = frames.size();
		bool field_truncated = false;
		frame["script_path"] = bound_utf8(path, 1024, field_truncated);
		frame["function"] = bound_utf8(String(source.get("function", String())), 1024, field_truncated);
		frame["line"] = MAX(1, (int)source.get("line", 1));
		const int column = source.get("column", 0);
		if (column > 0) {
			frame["column"] = column;
		}
		r_truncated = r_truncated || field_truncated;
		frames.push_back(frame);
	}
	return frames;
}

String RuntimeDiagnosticPolicy::normalize_output_severity(int p_level) {
	if (p_level == EditorLog::MSG_TYPE_ERROR) {
		return "error";
	}
	if (p_level == EditorLog::MSG_TYPE_WARNING) {
		return "warning";
	}
	return "info";
}

bool RuntimeScreenshotPolicy::normalize_callback_path(const String &p_path, const String &p_temp_root, String &r_safe_path) {
	r_safe_path.clear();
	const String temp_root = p_temp_root.simplify_path();
	const String candidate = p_path.simplify_path();
	const String filename = candidate.get_file();
	if (temp_root.is_empty() || candidate.get_base_dir() != temp_root || !filename.begins_with("scr-") || filename.length() <= 8 || candidate.get_extension().to_lower() != "png") {
		return false;
	}
	r_safe_path = candidate;
	return true;
}

bool RuntimeScreenshotPolicy::cleanup_callback_path(const String &p_path, const String &p_temp_root) {
	String safe_path;
	if (!normalize_callback_path(p_path, p_temp_root, safe_path)) {
		return false;
	}
	Ref<DirAccess> filesystem = DirAccess::create(DirAccess::ACCESS_FILESYSTEM);
	if (filesystem.is_null() || (!filesystem->file_exists(safe_path) && !filesystem->is_link(safe_path))) {
		return false;
	}
	return DirAccess::remove_absolute(safe_path) == OK;
}

bool RuntimeScreenshotPolicy::is_regular_callback_file(const String &p_path, const String &p_temp_root, String &r_safe_path) {
	if (!normalize_callback_path(p_path, p_temp_root, r_safe_path)) {
		return false;
	}
	Ref<DirAccess> filesystem = DirAccess::create(DirAccess::ACCESS_FILESYSTEM);
	return filesystem.is_valid() && filesystem->file_exists(r_safe_path) && FileAccess::exists(r_safe_path) && !filesystem->is_link(r_safe_path);
}

bool RuntimeScreenshotPolicy::parse_png_header(const PackedByteArray &p_header, int &r_width, int &r_height) {
	r_width = 0;
	r_height = 0;
	static constexpr uint8_t PNG_PREFIX[] = { 0x89, 'P', 'N', 'G', '\r', '\n', 0x1a, '\n', 0x00, 0x00, 0x00, 0x0d, 'I', 'H', 'D', 'R' };
	if (p_header.size() < 24) {
		return false;
	}
	for (int index = 0; index < 16; index++) {
		if (p_header[index] != PNG_PREFIX[index]) {
			return false;
		}
	}
	const uint32_t width = ((uint32_t)p_header[16] << 24) | ((uint32_t)p_header[17] << 16) | ((uint32_t)p_header[18] << 8) | (uint32_t)p_header[19];
	const uint32_t height = ((uint32_t)p_header[20] << 24) | ((uint32_t)p_header[21] << 16) | ((uint32_t)p_header[22] << 8) | (uint32_t)p_header[23];
	if (width == 0 || height == 0 || width > INT32_MAX || height > INT32_MAX) {
		return false;
	}
	r_width = (int)width;
	r_height = (int)height;
	return true;
}

bool RuntimeScreenshotPolicy::source_is_bounded(int p_width, int p_height, uint64_t p_bytes) {
	return p_width > 0 && p_height > 0 && p_width <= MAX_SOURCE_DIMENSION && p_height <= MAX_SOURCE_DIMENSION && (int64_t)p_width * p_height <= MAX_SOURCE_PIXELS && p_bytes > 0 && p_bytes <= MAX_SOURCE_BYTES;
}

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
	limits["snapshot_bytes"] = CodexRuntimeLimits::SNAPSHOT_BYTES;
	limits["snapshot_chunk_bytes"] = CodexRuntimeLimits::SNAPSHOT_CHUNK_BYTES;
	limits["snapshot_window_bytes"] = CodexRuntimeLimits::SNAPSHOT_WINDOW_BYTES;
	limits["snapshot_timeout_ms"] = CodexRuntimeLimits::SNAPSHOT_TIMEOUT_MS;
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
	return RuntimeLifecyclePolicy::is_terminal(state);
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
		_publish_invalidated(previous_session_id, previous_event_seq, "session_replaced");
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
	diagnostic_by_fingerprint.clear();
	diagnostic_fingerprint_by_id.clear();
	diagnostic_id_seq = 0;
	diagnostics_truncated = false;
	stacks.clear();
	stack_order.clear();
	stack_bytes = 0;
	stack_by_fingerprint.clear();
	stack_fingerprint_by_id.clear();
	stack_id_seq = 0;
	stacks_truncated = false;
	active_stack_id.clear();
	pause_generation = 0;
	expecting_pause_stack = false;
	revisions->begin_runtime_session(runtime_session_id);
	_publish_event("session_started", changed_domain("runtime_state"));
}

bool RuntimeDebuggerAdapter::_transition(const String &p_state, const String &p_event_type, const Array &p_changed_domains, const String &p_reason, bool p_advance) {
	if (runtime_session_id.is_empty()) {
		return false;
	}
	// A terminal transition wins exactly once. Late debugger callbacks, forced
	// process cleanup, and expired RPC completions cannot rewrite its outcome.
	if (_is_terminal()) {
		return false;
	}
	state = p_state;
	terminal_reason = p_reason;
	state_since_usec = OS::get_singleton()->get_ticks_usec();
	if (_is_terminal() || state == "disconnected" || state == "stopping") {
		active_stack_id.clear();
		expecting_pause_stack = false;
	}
	if (p_advance) {
		revisions->record_runtime_change();
	}
	if (_is_terminal() || state == "disconnected") {
		_retire_live_data();
	}
	_publish_event(p_event_type, p_changed_domains);
	return true;
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

void RuntimeDebuggerAdapter::_publish_invalidated(const String &p_runtime_session_id, uint64_t p_last_contiguous_event_seq, const String &p_reason) {
	if (!transport || p_runtime_session_id.is_empty()) {
		return;
	}
	Dictionary params;
	params["runtime_session_id"] = p_runtime_session_id;
	params["last_contiguous_runtime_event_seq"] = (int64_t)p_last_contiguous_event_seq;
	params["reason"] = p_reason;
	Dictionary notification;
	notification["protocol_version"] = "1.6";
	notification["kind"] = "notification";
	notification["method"] = "runtime.invalidated";
	notification["params"] = params;
	notification["context"] = _make_context();
	transport->publish_notification(notification);
}

void RuntimeDebuggerAdapter::_retire_live_data() {
	runtime_tree.clear();
	runtime_tree_checksum.clear();
	object_ids.clear();
	opaque_by_object_id.clear();
	runtime_tree_generation++;
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
	pending_object_generation = 0;
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
	return RuntimeDiagnosticPolicy::sanitize_message(p_message, ProjectSettings::get_singleton()->get_resource_path(), OS::get_singleton()->get_environment("HOME"), OS::get_singleton()->get_temp_path(), r_redacted, r_truncated);
}

void RuntimeDebuggerAdapter::_remove_diagnostic_at(int p_index) {
	ERR_FAIL_INDEX(p_index, diagnostics.size());
	const Dictionary diagnostic = diagnostics[p_index];
	const String id = diagnostic.get("runtime_diagnostic_id", String());
	const String *fingerprint = diagnostic_fingerprint_by_id.getptr(id);
	if (fingerprint) {
		diagnostic_by_fingerprint.erase(*fingerprint);
		diagnostic_fingerprint_by_id.erase(id);
	}
	diagnostic_bytes -= JSON::stringify(diagnostic, "", true, true).utf8().length();
	diagnostics.remove_at(p_index);
}

bool RuntimeDebuggerAdapter::_clear_stack_references(const String &p_stack_id) {
	bool changed = false;
	for (int index = 0; index < diagnostics.size(); index++) {
		Dictionary diagnostic = diagnostics[index];
		if (String(diagnostic.get("runtime_stack_id", String())) != p_stack_id) {
			continue;
		}
		diagnostic_bytes -= JSON::stringify(diagnostic, "", true, true).utf8().length();
		diagnostic.erase("runtime_stack_id");
		diagnostics[index] = diagnostic;
		diagnostic_bytes += JSON::stringify(diagnostic, "", true, true).utf8().length();
		changed = true;
	}
	return changed;
}

bool RuntimeDebuggerAdapter::_retire_stack(const String &p_stack_id) {
	if (!stacks.has(p_stack_id)) {
		return false;
	}
	stack_bytes -= JSON::stringify(stacks[p_stack_id], "", true, true).utf8().length();
	stacks.erase(p_stack_id);
	stack_order.erase(p_stack_id);
	const String *fingerprint = stack_fingerprint_by_id.getptr(p_stack_id);
	if (fingerprint) {
		stack_by_fingerprint.erase(*fingerprint);
		stack_fingerprint_by_id.erase(p_stack_id);
	}
	return _clear_stack_references(p_stack_id);
}

bool RuntimeDebuggerAdapter::_append_diagnostic(const String &p_severity, const String &p_source, const String &p_message, uint64_t p_event_seq, const String &p_script_path, int p_line, const String &p_function, const String &p_stack_id) {
	bool redacted = false;
	bool truncated = false;
	const String message = _redact_message(p_message, redacted, truncated);
	String script_path;
	if (safe_res_path(p_script_path)) {
		script_path = RuntimeDiagnosticPolicy::bound_utf8(p_script_path, 1024, truncated);
	}
	const String function = RuntimeDiagnosticPolicy::bound_utf8(p_function, 1024, truncated);
	const String fingerprint = sha256_hex_utf8(p_severity + "\n" + p_source + "\n" + message + "\n" + script_path + "\n" + String::num_int64(p_line) + "\n" + function + "\n" + p_stack_id);
	if (fingerprint.is_empty()) {
		return false;
	}

	Dictionary diagnostic;
	const String *existing_id = diagnostic_by_fingerprint.getptr(fingerprint);
	if (existing_id) {
		for (int index = 0; index < diagnostics.size(); index++) {
			const Dictionary existing = diagnostics[index];
			if (String(existing.get("runtime_diagnostic_id", String())) != *existing_id) {
				continue;
			}
			diagnostic = existing;
			const int64_t repeat_count = diagnostic.get("repeat_count", 1);
			diagnostic["repeat_count"] = MIN(repeat_count + 1, (int64_t)9007199254740991LL);
			diagnostic["runtime_event_seq"] = (int64_t)p_event_seq;
			diagnostic["redacted"] = (bool)diagnostic.get("redacted", false) || redacted;
			_remove_diagnostic_at(index);
			break;
		}
	}

	if (diagnostic.is_empty()) {
		diagnostic_id_seq++;
		diagnostic["kind"] = "runtime_diagnostic";
		const String id = _make_opaque_id("runtime-diagnostic:", "godot-codex-runtime-diagnostic/v1", String::num_uint64(diagnostic_id_seq));
		if (id.is_empty()) {
			return false;
		}
		diagnostic["entity_id"] = id;
		diagnostic["runtime_diagnostic_id"] = id;
		diagnostic["severity"] = p_severity;
		diagnostic["source"] = p_source;
		diagnostic["message"] = message;
		diagnostic["repeat_count"] = (int64_t)1;
		diagnostic["runtime_event_seq"] = (int64_t)p_event_seq;
		if (!script_path.is_empty()) {
			diagnostic["script_path"] = script_path;
			if (p_line > 0) {
				diagnostic["line"] = p_line;
			}
		}
		if (!function.is_empty()) {
			diagnostic["function"] = function;
		}
		if (!p_stack_id.is_empty() && stacks.has(p_stack_id)) {
			diagnostic["runtime_stack_id"] = p_stack_id;
		}
		diagnostic["redacted"] = redacted;
	}

	const int bytes = JSON::stringify(diagnostic, "", true, true).utf8().length();
	while (!diagnostics.is_empty() && (diagnostics.size() >= MAX_DIAGNOSTICS || diagnostic_bytes + bytes > MAX_DIAGNOSTIC_BYTES)) {
		_remove_diagnostic_at(0);
		diagnostics_truncated = true;
	}
	if (bytes > MAX_DIAGNOSTIC_BYTES) {
		diagnostics_truncated = true;
		return false;
	}
	const String id = diagnostic["runtime_diagnostic_id"];
	diagnostics.push_back(diagnostic);
	diagnostic_bytes += bytes;
	diagnostic_by_fingerprint[fingerprint] = id;
	diagnostic_fingerprint_by_id[id] = fingerprint;
	diagnostics_truncated = diagnostics_truncated || truncated;
	return true;
}

String RuntimeDebuggerAdapter::_append_stack(const String &p_kind, const Array &p_frames, uint64_t p_event_seq, bool p_deduplicate, const String &p_dedup_identity, bool &r_added, bool &r_diagnostic_links_changed) {
	r_added = false;
	r_diagnostic_links_changed = false;
	bool truncated = false;
	const Array frames = RuntimeDiagnosticPolicy::sanitize_frames(p_frames, MAX_STACK_FRAMES, truncated);
	if (frames.is_empty()) {
		stacks_truncated = stacks_truncated || truncated || !p_frames.is_empty();
		return String();
	}
	Array fingerprint_frames = frames;
	if (p_deduplicate && fingerprint_frames.size() > 3) {
		fingerprint_frames.resize(3);
	}
	const String fingerprint = sha256_hex_utf8(p_kind + "\n" + p_dedup_identity + "\n" + (truncated ? "truncated\n" : "complete\n") + JSON::stringify(fingerprint_frames, "", true, true));
	if (fingerprint.is_empty()) {
		return String();
	}
	if (p_deduplicate) {
		const String *existing_id = stack_by_fingerprint.getptr(fingerprint);
		if (existing_id && stacks.has(*existing_id)) {
			stacks_truncated = stacks_truncated || truncated;
			return *existing_id;
		}
	}

	stack_id_seq++;
	const String id = _make_opaque_id("runtime-stack:", "godot-codex-runtime-stack/v1", String::num_uint64(stack_id_seq));
	if (id.is_empty()) {
		return String();
	}
	Dictionary stack;
	stack["kind"] = "runtime_stack";
	stack["entity_id"] = id;
	stack["runtime_stack_id"] = id;
	stack["runtime_event_seq"] = (int64_t)p_event_seq;
	stack["stack_kind"] = p_kind;
	stack["frames"] = frames;
	stack["truncated"] = truncated;
	const int bytes = JSON::stringify(stack, "", true, true).utf8().length();
	if (bytes > MAX_STACK_BYTES) {
		stacks_truncated = true;
		return String();
	}
	while (!stack_order.is_empty() && (stack_order.size() >= MAX_STACKS || stack_bytes + bytes > MAX_STACK_BYTES)) {
		int retire_index = -1;
		for (int index = 0; index < stack_order.size(); index++) {
			if (String(stack_order[index]) != active_stack_id) {
				retire_index = index;
				break;
			}
		}
		if (retire_index < 0) {
			stacks_truncated = true;
			return String();
		}
		const String retired_id = stack_order[retire_index];
		r_diagnostic_links_changed = _retire_stack(retired_id) || r_diagnostic_links_changed;
		stacks_truncated = true;
	}
	stacks[id] = stack;
	stack_order.push_back(id);
	stack_bytes += bytes;
	stack_by_fingerprint[fingerprint] = id;
	stack_fingerprint_by_id[id] = fingerprint;
	stacks_truncated = stacks_truncated || truncated;
	r_added = true;
	return id;
}

bool RuntimeDebuggerAdapter::_project_tree(const Array &p_serialized, Array &r_entities, HashMap<String, uint64_t> &r_object_ids, HashMap<uint64_t, String> &r_opaque_by_object_id, bool &r_truncated) {
	Array entities;
	HashMap<String, uint64_t> projected_object_ids;
	HashMap<uint64_t, String> projected_opaque_by_object_id;
	if (p_serialized.size() % 6 != 0 || p_serialized.size() / 6 > MAX_TREE_NODES) {
		return false;
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
			return false;
		}
		const int child_count = p_serialized[offset];
		if (child_count < 0 || child_count > MAX_TREE_NODES || parents.size() >= MAX_TREE_DEPTH) {
			return false;
		}
		if (!parents.is_empty()) {
			parents.write[parents.size() - 1].remaining--;
		}
		const uint64_t raw_id = p_serialized[offset + 3];
		const String opaque_id = _make_opaque_id("runtime-object:", "godot-codex-runtime-object/v1", String::num_uint64(raw_id));
		const String name = bounded_string(String(p_serialized[offset + 1]), CodexRuntimeLimits::NODE_STRING_CHARACTERS, r_truncated);
		String node_path = parents.is_empty() ? "/" + name : parents[parents.size() - 1].path.path_join(name);
		node_path = bounded_string(node_path, CodexRuntimeLimits::NODE_STRING_CHARACTERS, r_truncated);
		String source_scene = parents.is_empty() ? String() : parents[parents.size() - 1].source_scene;
		String source_root_path = parents.is_empty() ? String() : parents[parents.size() - 1].source_root_path;
		Array instance_scenes = parents.is_empty() ? Array() : parents[parents.size() - 1].instance_scenes.duplicate();
		const String observed_scene = p_serialized[offset + 4];
		if (safe_res_path(observed_scene)) {
			source_scene = observed_scene;
			source_root_path = node_path;
			if (!instance_scenes.has(observed_scene) && instance_scenes.size() < CodexRuntimeLimits::SOURCE_SCENE_PATHS) {
				instance_scenes.push_back(observed_scene);
			}
		}
		Dictionary entity;
		entity["kind"] = "runtime_node";
		entity["entity_id"] = opaque_id;
		entity["runtime_object_id"] = opaque_id;
		entity["parent_runtime_object_id"] = parents.is_empty() ? Variant() : Variant(parents[parents.size() - 1].id);
		entity["name"] = name;
		entity["godot_type"] = bounded_string(String(p_serialized[offset + 2]), CodexRuntimeLimits::NODE_STRING_CHARACTERS, r_truncated);
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
			source_hint["relative_node_path"] = bounded_string(relative, CodexRuntimeLimits::NODE_STRING_CHARACTERS, r_truncated);
			source_hint["instance_scene_paths"] = instance_scenes;
			entity["source_hint"] = source_hint;
		}
		entities.push_back(entity);
		projected_object_ids.insert(opaque_id, raw_id);
		projected_opaque_by_object_id.insert(raw_id, opaque_id);
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
				return false;
			}
		}
	}
	r_entities = entities;
	r_object_ids = projected_object_ids;
	r_opaque_by_object_id = projected_opaque_by_object_id;
	return true;
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
	result["godot_type"] = bounded_string(String(p_serialized[1]), CodexRuntimeLimits::NODE_STRING_CHARACTERS, r_truncated);
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
		projected["name"] = bounded_string(String(property[0]), CodexRuntimeLimits::NODE_STRING_CHARACTERS, r_truncated);
		const Variant::Type type = (Variant::Type)(int)property[1];
		projected["variant_type"] = Variant::get_type_name(type);
		projected["read_only"] = true;
		projected["usage"] = property[4];
		Dictionary value;
		uint64_t referenced_raw_id = 0;
		bool has_object_reference = false;
		if (type == Variant::OBJECT || (int)property[2] == PROPERTY_HINT_OBJECT_ID) {
			if (property[5].get_type() == Variant::INT) {
				referenced_raw_id = property[5];
				has_object_reference = true;
			} else if (property[5].get_type() == Variant::OBJECT) {
				Object *encoded_object = property[5];
				if (EncodedObjectAsID *encoded_id = Object::cast_to<EncodedObjectAsID>(encoded_object)) {
					referenced_raw_id = encoded_id->get_object_id();
					has_object_reference = true;
				}
			}
		}
		if (has_object_reference) {
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
	const String protocol_version = pending_snapshot_params.get("_protocol_version", "1.6");
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
		if (state == "paused" && !active_stack_id.is_empty() && stacks.has(active_stack_id)) {
			runtime_state["active_stack_id"] = active_stack_id;
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
	bool truncated = p_tree_truncated || (domains.has("runtime_diagnostics") && diagnostics_truncated) || (domains.has("runtime_stacks") && stacks_truncated);
	Array bounded_entities;
	int total_bytes = 0;
	for (const Variant &entity : entities) {
		const int bytes = JSON::stringify(entity, "", true, true).utf8().length();
		if (bytes > CodexRuntimeLimits::OBJECT_BYTES || total_bytes + bytes > CodexRuntimeLimits::SNAPSHOT_BYTES) {
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
	begin["protocol_version"] = protocol_version;
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
		chunk["protocol_version"] = protocol_version;
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
	end["protocol_version"] = protocol_version;
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
	EditorRunBar *run_bar = EditorRunBar::get_singleton();
	if (_is_terminal() && (!run_bar || !run_bar->is_playing())) {
		return;
	}
	if (runtime_session_id.is_empty() || _is_terminal()) {
		_begin_session("editor", run_bar ? run_bar->get_playing_target() : "project", run_bar ? run_bar->get_playing_scene() : String());
	}
	const bool reconnected = RuntimeLifecyclePolicy::is_reconnect(state);
	if (reconnected) {
		_publish_invalidated(runtime_session_id, revisions->get_runtime_event_seq(), "reconnected");
	}
	active_debugger_session = p_session_id;
	disconnected_since_usec = 0;
	_transition("running", "debugger_connected", changed_domain("runtime_state"));
	_complete_pending_controls("running");
}

void RuntimeDebuggerAdapter::_on_stopped(int p_session_id) {
	if (p_session_id != active_debugger_session || runtime_session_id.is_empty()) {
		return;
	}
	ScriptEditorDebugger *debugger = _get_debugger(p_session_id);
	normal_quit_requested = normal_quit_requested || (debugger && debugger->was_remote_quit_requested());
	if (_is_terminal()) {
		return;
	}
	// Let the complete signal emission and final debugger flush settle before
	// terminal classification. ScriptEditorDebugger retains the request_quit
	// marker emitted by orderly engine shutdown until the next session starts.
	callable_mp(this, &RuntimeDebuggerAdapter::_finalize_debugger_stop).call_deferred(p_session_id, runtime_session_id);
}

void RuntimeDebuggerAdapter::_finalize_debugger_stop(int p_session_id, const String &p_runtime_session_id) {
	if (!transport || !revisions || p_session_id != active_debugger_session || p_runtime_session_id != runtime_session_id || runtime_session_id.is_empty() || _is_terminal()) {
		return;
	}
	ScriptEditorDebugger *debugger = _get_debugger(p_session_id);
	normal_quit_requested = normal_quit_requested || (debugger && debugger->was_remote_quit_requested());
	EditorRunBar *run_bar = EditorRunBar::get_singleton();
	const bool editor_playing = run_bar && run_bar->is_playing();
	const String stopped_state = RuntimeLifecyclePolicy::classify_debugger_stop(state, editor_playing, normal_quit_requested);
	if (stopped_state == "disconnected") {
		disconnected_since_usec = OS::get_singleton()->get_ticks_usec();
		_transition("disconnected", "disconnected", changed_domain("runtime_state"));
		_fail_pending_live_requests("runtime_disconnected", "The runtime debugger disconnected.", true);
		return;
	}
	const bool stopped_normally = stopped_state == "stopped";
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
	if (stopped_normally && run_bar && run_bar->is_playing()) {
		internal_forced_stop = true;
		run_bar->stop_playing();
		internal_forced_stop = false;
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
		pause_generation++;
		active_stack_id.clear();
		// ScriptEditorDebugger requests get_stack_dump immediately after this
		// synchronous signal. `has_stackdump` describes the enter packet, not
		// whether the correlated response will be delivered.
		expecting_pause_stack = true;
		_transition("paused", "paused", changed_domain("runtime_state"));
		_complete_pending_controls("paused");
	} else {
		active_stack_id.clear();
		expecting_pause_stack = false;
		_transition("running", "continued", changed_domain("runtime_state"));
		_complete_pending_controls("running");
	}
}

void RuntimeDebuggerAdapter::_on_output(const String &p_message, int p_level, int p_session_id) {
	if (p_session_id != active_debugger_session || runtime_session_id.is_empty() || _is_terminal()) {
		return;
	}
	const uint64_t event_seq = revisions->get_runtime_event_seq() + 1;
	if (_append_diagnostic(RuntimeDiagnosticPolicy::normalize_output_severity(p_level), "output", p_message, event_seq)) {
		_transition(state, "diagnostic_added", changed_domain("runtime_diagnostics"));
	}
}

void RuntimeDebuggerAdapter::_on_runtime_error(const Dictionary &p_error, int p_session_id) {
	if (p_session_id != active_debugger_session || runtime_session_id.is_empty() || _is_terminal()) {
		return;
	}
	const uint64_t event_seq = revisions->get_runtime_event_seq() + 1;
	String stack_id;
	bool stack_added = false;
	bool diagnostic_links_changed = false;
	const Array frames = p_error.get("frames", Array());
	const String script_path = p_error.get("script_path", String());
	bool identity_redacted = false;
	bool identity_truncated = false;
	const String safe_message = _redact_message(p_error.get("message", String()), identity_redacted, identity_truncated);
	const String stack_identity = sha256_hex_utf8(safe_message + "\n" + (safe_res_path(script_path) ? script_path : String()) + "\n" + String::num_int64((int64_t)p_error.get("line", 0)) + "\n" + String(p_error.get("function", String())));
	if (!frames.is_empty()) {
		stack_id = _append_stack("diagnostic", frames, event_seq, true, stack_identity, stack_added, diagnostic_links_changed);
	}
	const String source = safe_res_path(script_path) || !stack_id.is_empty() ? "script" : "engine";
	const bool diagnostic_added = _append_diagnostic((bool)p_error.get("warning", false) ? "warning" : "error", source, p_error.get("message", String()), event_seq, script_path, p_error.get("line", 0), p_error.get("function", String()), stack_id);
	if (diagnostic_added || stack_added || diagnostic_links_changed) {
		Array domains = changed_domain("runtime_diagnostics");
		if (stack_added || diagnostic_links_changed) {
			domains.push_back("runtime_stacks");
		}
		_transition(state, "diagnostic_added", domains);
	}
}

void RuntimeDebuggerAdapter::_on_runtime_stack(int64_t p_thread_id, const Array &p_frames, int p_session_id) {
	if (p_session_id != active_debugger_session || runtime_session_id.is_empty() || state != "paused" || !expecting_pause_stack) {
		return;
	}
	(void)p_thread_id;
	expecting_pause_stack = false;
	const uint64_t event_seq = revisions->get_runtime_event_seq() + 1;
	bool stack_added = false;
	bool diagnostic_links_changed = false;
	const String stack_id = _append_stack("pause", p_frames, event_seq, false, String::num_uint64(pause_generation), stack_added, diagnostic_links_changed);
	if (stack_added) {
		active_stack_id = stack_id;
		Array domains = changed_domain("runtime_state");
		domains.push_back("runtime_stacks");
		if (diagnostic_links_changed) {
			domains.push_back("runtime_diagnostics");
		}
		_transition(state, "stack_changed", domains);
	} else if (_append_diagnostic("warning", "bridge", "Runtime pause stack contained no safe project frames.", event_seq)) {
		_transition(state, "diagnostic_added", changed_domain("runtime_diagnostics"));
	}
}

void RuntimeDebuggerAdapter::_on_screenshot(int p_width, int p_height, const String &p_path, const Rect2i &p_rect) {
	(void)p_rect;
	String safe_path;
	auto cleanup_callback_file = [&]() {
		RuntimeScreenshotPolicy::cleanup_callback_path(p_path, OS::get_singleton()->get_temp_path());
	};
	if (!pending_capture) {
		cleanup_callback_file();
		return;
	}
	const uint64_t request_id = pending_capture;
	const int max_width = pending_capture_params.get("max_width", MAX_SCREENSHOT_WIDTH);
	const int max_height = pending_capture_params.get("max_height", MAX_SCREENSHOT_HEIGHT);
	pending_capture = 0;
	pending_capture_params.clear();
	const bool source_file_valid = RuntimeScreenshotPolicy::is_regular_callback_file(p_path, OS::get_singleton()->get_temp_path(), safe_path);
	const uint64_t source_bytes = source_file_valid ? FileAccess::get_size(safe_path) : 0;
	if (!source_file_valid || !RuntimeScreenshotPolicy::source_is_bounded(p_width, p_height, source_bytes)) {
		cleanup_callback_file();
		transport->complete_request_error(request_id, "runtime_capture_too_large", "The runtime screenshot source exceeded a safety limit.", false, _safe_coordinates());
		return;
	}
	Ref<FileAccess> source = FileAccess::open(safe_path, FileAccess::READ);
	int header_width = 0;
	int header_height = 0;
	const PackedByteArray header = source.is_valid() ? source->get_buffer(24) : PackedByteArray();
	source.unref();
	if (!RuntimeScreenshotPolicy::parse_png_header(header, header_width, header_height) || header_width != p_width || header_height != p_height || !RuntimeScreenshotPolicy::source_is_bounded(header_width, header_height, source_bytes)) {
		cleanup_callback_file();
		transport->complete_request_error(request_id, "runtime_capture_unavailable", "The runtime screenshot header was invalid.", true, _safe_coordinates());
		return;
	}
	Ref<Image> image;
	image.instantiate();
	const Error load_error = image->load(safe_path);
	cleanup_callback_file();
	if (load_error != OK || image->is_empty() || image->get_width() != header_width || image->get_height() != header_height) {
		transport->complete_request_error(request_id, "runtime_capture_unavailable", "The runtime screenshot could not be decoded.", true, _safe_coordinates());
		return;
	}
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
		transport->complete_request_error(request_id, "runtime_capture_too_large", "The bounded runtime screenshot still exceeds 512 KiB.", false, _safe_coordinates());
		return;
	}
	PackedByteArray digest;
	digest.resize(32);
	if (CryptoCore::sha256(png.ptr(), png.size(), digest.ptrw()) != OK) {
		transport->complete_request_error(request_id, "runtime_capture_unavailable", "The runtime screenshot digest failed.", true, _safe_coordinates());
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
	for (uint64_t *pending : { &pending_run, &pending_stop, &pending_pause, &pending_continue, &pending_snapshot, &pending_object, &pending_capture }) {
		*pending = 0;
	}
	_retire_live_data();
	diagnostics.clear();
	diagnostic_bytes = 0;
	diagnostic_by_fingerprint.clear();
	diagnostic_fingerprint_by_id.clear();
	diagnostic_id_seq = 0;
	diagnostics_truncated = false;
	stacks.clear();
	stack_order.clear();
	stack_bytes = 0;
	stack_by_fingerprint.clear();
	stack_fingerprint_by_id.clear();
	stack_id_seq = 0;
	stacks_truncated = false;
	active_stack_id.clear();
	pause_generation = 0;
	expecting_pause_stack = false;
	pending_snapshot_params.clear();
	pending_object_params.clear();
	pending_capture_params.clear();
	pending_tree_correlation.clear();
	pending_object_correlation.clear();
	if (revisions) {
		revisions->clear_runtime_session();
	}
	runtime_session_id.clear();
	state.clear();
	active_debugger_session = -1;
	transport = nullptr;
	revisions = nullptr;
}

void RuntimeDebuggerAdapter::process() {
	if (!transport || !revisions) {
		return;
	}
	ScriptEditorDebugger *debugger = _get_active_debugger();
	normal_quit_requested = normal_quit_requested || (debugger && debugger->was_remote_quit_requested());
	EditorRunBar *run_bar = EditorRunBar::get_singleton();
	const bool playing = run_bar && run_bar->is_playing();
	if (playing && (runtime_session_id.is_empty() || _is_terminal())) {
		_begin_session("editor", run_bar->get_playing_target(), run_bar->get_playing_scene());
	}
	const uint64_t now = OS::get_singleton()->get_ticks_usec();
	if (state == "starting" && now - state_since_usec >= 10000000) {
		_transition("timed_out", "timed_out", changed_domain("runtime_state"), "debugger_start_timeout");
		_fail_pending_live_requests("runtime_start_timeout", "The debugger did not connect within 10 seconds.", true);
		if (pending_run) {
			transport->complete_request_error(pending_run, "runtime_start_timeout", "The debugger did not connect within 10 seconds.", true, _safe_coordinates());
			pending_run = 0;
		}
		if (run_bar && run_bar->is_playing()) {
			internal_forced_stop = true;
			run_bar->stop_playing();
			internal_forced_stop = false;
		}
	} else if (state == "disconnected" && disconnected_since_usec > 0 && now - disconnected_since_usec >= 2000000) {
		_transition("crashed", "crashed", changed_domain("runtime_state"), "debugger_disconnect_timeout");
		_fail_pending_live_requests("runtime_crashed", "The disconnected runtime did not reconnect within two seconds.", false);
		if (run_bar && run_bar->is_playing()) {
			internal_forced_stop = true;
			run_bar->stop_playing();
			internal_forced_stop = false;
		}
	} else if (!playing && !runtime_session_id.is_empty() && !_is_terminal() && state != "starting") {
		const bool stopped_normally = RuntimeLifecyclePolicy::classify_process_exit(state, normal_quit_requested) == "stopped";
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
	bool retire_observation = false;
	if (pending_snapshot == p_request_id) {
		pending_snapshot_params.clear();
		pending_tree_correlation.clear();
		retire_observation = true;
	}
	if (pending_object == p_request_id) {
		pending_object_params.clear();
		pending_object_correlation.clear();
		pending_object_generation = 0;
		retire_observation = true;
	}
	if (pending_capture == p_request_id) {
		pending_capture_params.clear();
	}
	for (uint64_t *pending : { &pending_run, &pending_stop, &pending_pause, &pending_continue, &pending_snapshot, &pending_object, &pending_capture }) {
		if (*pending == p_request_id) {
			*pending = 0;
		}
	}
	if (retire_observation) {
		_retire_live_data();
		_publish_invalidated(runtime_session_id, revisions->get_runtime_event_seq(), "snapshot_cancelled");
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
	if (pending_pause) {
		transport->complete_request_error(p_request_id, "runtime_control_timeout", "A pause request is already pending.", true, _safe_coordinates());
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
	if (pending_continue) {
		transport->complete_request_error(p_request_id, "runtime_control_timeout", "A continue request is already pending.", true, _safe_coordinates());
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
	pending_object_generation = runtime_tree_generation;
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
	if (!EditorRun::request_screenshot(callable_mp(this, &RuntimeDebuggerAdapter::_on_screenshot), false)) {
		pending_capture = 0;
		pending_capture_params.clear();
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
		Array projected_tree;
		HashMap<String, uint64_t> projected_object_ids;
		HashMap<uint64_t, String> projected_opaque_by_object_id;
		if (!_project_tree(p_data[2], projected_tree, projected_object_ids, projected_opaque_by_object_id, truncated)) {
			const uint64_t request_id = pending_snapshot;
			pending_snapshot = 0;
			pending_snapshot_params.clear();
			pending_tree_correlation.clear();
			transport->complete_request_error(request_id, "runtime_request_timeout", "The runtime tree response was structurally invalid.", true, _safe_coordinates());
			return true;
		}
		const String projected_checksum = sha256_hex_utf8(JSON::stringify(projected_tree, "", true, true));
		if (projected_checksum.is_empty()) {
			const uint64_t request_id = pending_snapshot;
			pending_snapshot = 0;
			pending_snapshot_params.clear();
			pending_tree_correlation.clear();
			transport->complete_request_error(request_id, "runtime_request_timeout", "The runtime tree digest could not be produced.", true, _safe_coordinates());
			return true;
		}
		const bool changed = projected_checksum != runtime_tree_checksum;
		runtime_tree = projected_tree;
		runtime_tree_checksum = projected_checksum;
		object_ids = projected_object_ids;
		opaque_by_object_id = projected_opaque_by_object_id;
		runtime_tree_generation++;
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
		if (pending_object_generation != runtime_tree_generation) {
			pending_object_generation = 0;
			pending_object_params.clear();
			pending_object_correlation.clear();
			transport->complete_request_error(request_id, "runtime_object_stale", "The runtime tree changed while the object was being observed.", true, _safe_coordinates());
			return true;
		}
		pending_object_generation = 0;
		if (!(bool)p_data[1]) {
			pending_object_params.clear();
			pending_object_correlation.clear();
			transport->complete_request_error(request_id, "runtime_object_not_found", "The runtime object no longer exists.", true, _safe_coordinates());
			return true;
		}
		bool truncated = p_data[2];
		Dictionary result = _project_object(p_data[3], truncated, truncated);
		if (result.is_empty()) {
			pending_object_params.clear();
			pending_object_correlation.clear();
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
