/**************************************************************************/
/*  bridge_rpc_session.cpp                                                */
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

#include "bridge_rpc_session.h"

#include "bridge_frame_codec.h"

#include "modules/codex_bridge/editor/main_thread_dispatcher.h"

namespace {

static bool get_string(const Dictionary &p_object, const StringName &p_key, String &r_value) {
	if (!p_object.has(p_key) || p_object[p_key].get_type() != Variant::STRING) {
		return false;
	}
	r_value = p_object[p_key];
	return true;
}

static bool get_bounded_integer(const Dictionary &p_object, const StringName &p_key, int64_t p_minimum, int64_t p_maximum, int64_t &r_value) {
	if (!p_object.has(p_key)) {
		return false;
	}
	const Variant value = p_object[p_key];
	if (value.get_type() == Variant::INT) {
		r_value = value;
		return r_value >= p_minimum && r_value <= p_maximum;
	}
	if (value.get_type() != Variant::FLOAT) {
		return false;
	}
	const double number = value;
	if (number < (double)p_minimum || number > (double)p_maximum) {
		return false;
	}
	r_value = (int64_t)number;
	return (double)r_value == number;
}

static bool is_request_id(const String &p_value) {
	if (p_value.length() < 5 || p_value.length() > 128 || !p_value.begins_with("req:")) {
		return false;
	}
	for (int index = 4; index < p_value.length(); index++) {
		const char32_t character = p_value[index];
		if (!((character >= 'A' && character <= 'Z') || (character >= 'a' && character <= 'z') || (character >= '0' && character <= '9') || character == '.' || character == '_' || character == '-')) {
			return false;
		}
	}
	return true;
}

static bool is_ack_id(const String &p_value) {
	if (p_value.length() < 5 || p_value.length() > 128 || !p_value.begins_with("ack:")) {
		return false;
	}
	for (int index = 4; index < p_value.length(); index++) {
		const char32_t character = p_value[index];
		if (!((character >= 'A' && character <= 'Z') || (character >= 'a' && character <= 'z') || (character >= '0' && character <= '9') || character == '.' || character == '_' || character == '-')) {
			return false;
		}
	}
	return true;
}

static bool is_snapshot_id(const String &p_value) {
	if (p_value.length() != 41 || !p_value.begins_with("snapshot:")) {
		return false;
	}
	for (int index = 9; index < p_value.length(); index++) {
		const char32_t character = p_value[index];
		if (!((character >= '0' && character <= '9') || (character >= 'a' && character <= 'f'))) {
			return false;
		}
	}
	return true;
}

static bool is_method_name(const String &p_value) {
	if (p_value.is_empty() || p_value.length() > 128 || p_value[0] < 'a' || p_value[0] > 'z') {
		return false;
	}
	for (int index = 1; index < p_value.length(); index++) {
		const char32_t character = p_value[index];
		if (!((character >= 'a' && character <= 'z') || (character >= '0' && character <= '9') || character == '.' || character == '_' || character == '-')) {
			return false;
		}
	}
	return true;
}

static bool is_capability_name(const String &p_value) {
	if (p_value.is_empty() || p_value.length() > 64 || p_value[0] < 'a' || p_value[0] > 'z') {
		return false;
	}
	for (int index = 1; index < p_value.length(); index++) {
		const char32_t character = p_value[index];
		if (!((character >= 'a' && character <= 'z') || (character >= '0' && character <= '9') || character == '.' || character == '_' || character == '-')) {
			return false;
		}
	}
	return true;
}

static bool is_client_name(const String &p_value) {
	if (p_value.is_empty() || p_value.length() > 64) {
		return false;
	}
	for (int index = 0; index < p_value.length(); index++) {
		const char32_t character = p_value[index];
		if (!((character >= 'A' && character <= 'Z') || (character >= 'a' && character <= 'z') || (character >= '0' && character <= '9') || character == '.' || character == '_' || character == '-')) {
			return false;
		}
	}
	return true;
}

} // namespace

Dictionary BridgeRpcSession::_make_context() const {
	Dictionary context;
	context["project_id"] = project_id;
	context["editor_session_id"] = editor_session_id;
	return context;
}

Dictionary BridgeRpcSession::_make_error(const String &p_code, const String &p_message, bool p_retryable) const {
	Dictionary error;
	error["code"] = p_code;
	error["message"] = p_message;
	error["retryable"] = p_retryable;
	return error;
}

Dictionary BridgeRpcSession::_make_error_response(const String &p_request_id, const String &p_code, const String &p_message, bool p_retryable) const {
	Dictionary response;
	response["protocol_version"] = protocol_version;
	response["kind"] = "response";
	response["request_id"] = p_request_id;
	response["context"] = _make_context();
	response["error"] = _make_error(p_code, p_message, p_retryable);
	return response;
}

Dictionary BridgeRpcSession::_make_result_response(const String &p_request_id, const Dictionary &p_result) const {
	Dictionary response;
	response["protocol_version"] = protocol_version;
	response["kind"] = "response";
	response["request_id"] = p_request_id;
	response["context"] = _make_context();
	response["result"] = p_result;
	return response;
}

Dictionary BridgeRpcSession::_make_capabilities() const {
	Array capabilities;
	Dictionary lifecycle;
	lifecycle["name"] = "bridge.lifecycle";
	lifecycle["version"] = "1.0";
	lifecycle["readiness"] = "ready";
	capabilities.push_back(lifecycle);
	Dictionary transport;
#ifdef WINDOWS_ENABLED
	transport["name"] = "transport.tcp_loopback";
#else
	transport["name"] = "transport.uds";
#endif
	transport["version"] = "1.0";
	transport["readiness"] = "ready";
	capabilities.push_back(transport);
	if (protocol_version == "1.1") {
		const char *names[] = {
			"editor.context",
			"editor.inspector",
			"sync.full_snapshot_v1",
			"sync.event_stream_v1",
		};
		for (const char *name : names) {
			Dictionary capability;
			capability["name"] = name;
			capability["version"] = "1.0";
			capability["readiness"] = "ready";
			capabilities.push_back(capability);
		}
	}
	Dictionary result;
	result["capabilities"] = capabilities;
	return result;
}

Dictionary BridgeRpcSession::_make_limits() const {
	Dictionary limits;
	limits["frame_bytes"] = (int64_t)BridgeFrameCodec::MAX_PAYLOAD_BYTES;
	limits["default_deadline_ms"] = (int64_t)DEFAULT_DEADLINE_MS;
	limits["max_deadline_ms"] = (int64_t)MAX_DEADLINE_MS;
	limits["max_in_flight_requests"] = (int64_t)MAX_IN_FLIGHT_REQUESTS;
	limits["main_thread_commands_per_frame"] = (int64_t)MainThreadDispatcher::MAX_COMMANDS_PER_FRAME;
	limits["main_thread_budget_us"] = (int64_t)MainThreadDispatcher::MAX_PROCESS_USEC_PER_FRAME;
	limits["ping_echo_bytes"] = (int64_t)MAX_PING_ECHO_BYTES;
	if (protocol_version == "1.1") {
		limits["hard_message_bytes"] = (int64_t)8388608;
		limits["snapshot_chunk_bytes"] = (int64_t)524288;
		limits["event_journal_entries"] = (int64_t)4096;
		limits["event_journal_bytes"] = (int64_t)16777216;
		limits["snapshot_window_bytes"] = (int64_t)33554432;
		limits["variant_depth"] = (int64_t)8;
		limits["container_items"] = (int64_t)1000;
	}
	return limits;
}

Dictionary BridgeRpcSession::_make_revisions() const {
	Dictionary revisions;
	revisions["editor_session_id"] = editor_session_id;
	revisions["event_seq"] = (int64_t)0;
	revisions["project_revision"] = (int64_t)0;
	revisions["operation_seq"] = (int64_t)0;
	revisions["scene_revisions"] = Dictionary();
	return revisions;
}

bool BridgeRpcSession::_validate_common_envelope(const Dictionary &p_message) const {
	String version;
	if (!get_string(p_message, "protocol_version", version) || version != protocol_version || !p_message.has("context") || p_message["context"].get_type() != Variant::DICTIONARY) {
		return false;
	}
	const Dictionary context = p_message["context"];
	String project;
	String session;
	return get_string(context, "project_id", project) && project == project_id && get_string(context, "editor_session_id", session) && session == editor_session_id;
}

bool BridgeRpcSession::_validate_initialize_params(const Dictionary &p_params) const {
	if (!p_params.has("client") || p_params["client"].get_type() != Variant::DICTIONARY) {
		return false;
	}
	const Dictionary client = p_params["client"];
	String client_name;
	String client_version;
	if (!get_string(client, "name", client_name) || !is_client_name(client_name) || !get_string(client, "version", client_version) || client_version.is_empty() || client_version.length() > 64) {
		return false;
	}
	if (!p_params.has("requested_capabilities")) {
		return true;
	}
	if (p_params["requested_capabilities"].get_type() != Variant::ARRAY) {
		return false;
	}
	const Array requested = p_params["requested_capabilities"];
	if (requested.size() > 64) {
		return false;
	}
	HashSet<String> unique;
	for (int index = 0; index < requested.size(); index++) {
		if (requested[index].get_type() != Variant::STRING) {
			return false;
		}
		const String name = requested[index];
		if (!is_capability_name(name) || unique.has(name)) {
			return false;
		}
		unique.insert(name);
	}
	return true;
}

bool BridgeRpcSession::_validate_ping_params(const Dictionary &p_params) const {
	if (!p_params.has("echo") || p_params["echo"].get_type() != Variant::STRING) {
		return false;
	}
	const String echo = p_params["echo"];
	return echo.utf8().length() <= (int)MAX_PING_ECHO_BYTES;
}

bool BridgeRpcSession::_validate_snapshot_params(const Dictionary &p_params) const {
	if (!p_params.has("domains")) {
		return true;
	}
	if (p_params["domains"].get_type() != Variant::ARRAY) {
		return false;
	}
	const Array domains = p_params["domains"];
	if (domains.is_empty() || domains.size() > 8) {
		return false;
	}
	HashSet<String> unique;
	for (int index = 0; index < domains.size(); index++) {
		if (domains[index].get_type() != Variant::STRING) {
			return false;
		}
		const String domain = domains[index];
		if ((domain != "editor_context" && domain != "editor_inspector") || unique.has(domain)) {
			return false;
		}
		unique.insert(domain);
	}
	return true;
}

bool BridgeRpcSession::_validate_shutdown_params(const Dictionary &p_params) const {
	if (!p_params.has("reason")) {
		return true;
	}
	if (p_params["reason"].get_type() != Variant::STRING) {
		return false;
	}
	return String(p_params["reason"]).length() <= 128;
}

void BridgeRpcSession::_remove_pending(uint64_t p_internal_request_id) {
	PendingRequest *pending = pending_by_internal_id.getptr(p_internal_request_id);
	if (!pending) {
		return;
	}
	const String request_id = pending->request_id;
	pending_by_request_id.erase(request_id);
	pending_by_internal_id.erase(p_internal_request_id);
	if (initialize_pending_id == p_internal_request_id) {
		initialize_pending_id = 0;
	}
	if (shutdown_pending_id == p_internal_request_id) {
		shutdown_pending_id = 0;
	}
}

void BridgeRpcSession::_set_error_outcome(const String &p_request_id, const String &p_code, const String &p_message, bool p_retryable, Outcome &r_outcome) const {
	r_outcome.has_response = true;
	r_outcome.response = _make_error_response(p_request_id, p_code, p_message, p_retryable);
}

Error BridgeRpcSession::_handle_request(const Dictionary &p_message, uint64_t p_now_usec, uint64_t p_internal_request_id, Outcome &r_outcome) {
	String request_id;
	if (!get_string(p_message, "request_id", request_id) || !is_request_id(request_id)) {
		return ERR_INVALID_DATA;
	}
	if (seen_request_ids.has(request_id)) {
		_set_error_outcome(request_id, "duplicate_request_id", "The request ID was already used on this connection.", true, r_outcome);
		return OK;
	}
	seen_request_ids.insert(request_id);

	if (!_validate_common_envelope(p_message)) {
		_set_error_outcome(request_id, "invalid_request", "The request envelope or connection context is invalid.", false, r_outcome);
		return OK;
	}
	String method_name;
	if (!get_string(p_message, "method", method_name) || !is_method_name(method_name) || !p_message.has("params") || p_message["params"].get_type() != Variant::DICTIONARY) {
		_set_error_outcome(request_id, "invalid_request", "The request envelope is invalid.", false, r_outcome);
		return OK;
	}
	int64_t deadline_ms = DEFAULT_DEADLINE_MS;
	if (p_message.has("deadline_ms") && !get_bounded_integer(p_message, "deadline_ms", 1, MAX_DEADLINE_MS, deadline_ms)) {
		_set_error_outcome(request_id, "invalid_request", "The request deadline is invalid.", false, r_outcome);
		return OK;
	}
	const Dictionary params = p_message["params"];

	if (closing || shutdown_pending_id != 0) {
		_set_error_outcome(request_id, "invalid_request", "The RPC connection is closing.", false, r_outcome);
		return OK;
	}

	Method method = METHOD_INITIALIZE;
	if (method_name == "bridge.initialize") {
		if (initialized || initialize_pending_id != 0) {
			_set_error_outcome(request_id, "already_initialized", "This RPC connection was already initialized.", false, r_outcome);
			return OK;
		}
		if (!_validate_initialize_params(params)) {
			_set_error_outcome(request_id, "invalid_request", "The initialize parameters are invalid.", false, r_outcome);
			return OK;
		}
		method = METHOD_INITIALIZE;
	} else {
		if (!initialized) {
			_set_error_outcome(request_id, "not_initialized", "Initialize the RPC connection before calling this method.", true, r_outcome);
			return OK;
		}
		if (method_name == "bridge.ping") {
			if (!_validate_ping_params(params)) {
				_set_error_outcome(request_id, "invalid_request", "The ping parameters are invalid.", false, r_outcome);
				return OK;
			}
			method = METHOD_PING;
		} else if (method_name == "bridge.capabilities") {
			method = METHOD_CAPABILITIES;
		} else if (method_name == "editor.snapshot.get" && protocol_version == "1.1") {
			if (!_validate_snapshot_params(params)) {
				_set_error_outcome(request_id, "invalid_request", "The snapshot parameters are invalid.", false, r_outcome);
				return OK;
			}
			method = METHOD_EDITOR_SNAPSHOT;
		} else if (method_name == "bridge.shutdown") {
			if (!_validate_shutdown_params(params)) {
				_set_error_outcome(request_id, "invalid_request", "The shutdown parameters are invalid.", false, r_outcome);
				return OK;
			}
			method = METHOD_SHUTDOWN;
		} else {
			_set_error_outcome(request_id, "method_not_found", "The requested bridge method is not supported.", false, r_outcome);
			return OK;
		}
	}

	if (pending_by_internal_id.size() >= MAX_IN_FLIGHT_REQUESTS) {
		_set_error_outcome(request_id, "overloaded", "The RPC connection has too many requests in flight.", true, r_outcome);
		return OK;
	}

	PendingRequest pending;
	pending.request_id = request_id;
	pending.method = method;
	pending.deadline_usec = p_now_usec + (uint64_t)deadline_ms * 1000;
	pending.params = params;
	pending_by_internal_id.insert(p_internal_request_id, pending);
	pending_by_request_id.insert(request_id, p_internal_request_id);
	if (method == METHOD_INITIALIZE) {
		initialize_pending_id = p_internal_request_id;
	} else if (method == METHOD_SHUTDOWN) {
		shutdown_pending_id = p_internal_request_id;
	}
	r_outcome.dispatch = true;
	r_outcome.method = method;
	r_outcome.internal_request_id = p_internal_request_id;
	r_outcome.deadline_usec = pending.deadline_usec;
	r_outcome.params = params;
	return OK;
}

Error BridgeRpcSession::_handle_cancel(const Dictionary &p_message, Outcome &r_outcome) {
	String request_id;
	if (!_validate_common_envelope(p_message) || !get_string(p_message, "request_id", request_id) || !is_request_id(request_id)) {
		return ERR_INVALID_DATA;
	}
	if (p_message.has("reason")) {
		String reason;
		if (!get_string(p_message, "reason", reason) || reason.is_empty() || reason.length() > 128) {
			return ERR_INVALID_DATA;
		}
	}
	const uint64_t *internal_id = pending_by_request_id.getptr(request_id);
	if (!internal_id) {
		return OK;
	}
	const uint64_t cancelled_id = *internal_id;
	_set_error_outcome(request_id, "cancelled", "The request was cancelled by the client.", true, r_outcome);
	r_outcome.cancel_dispatch = true;
	r_outcome.internal_request_id = cancelled_id;
	_remove_pending(cancelled_id);
	return OK;
}

Error BridgeRpcSession::_handle_ack(const Dictionary &p_message, Outcome &r_outcome) {
	if (protocol_version != "1.1" || !initialized || !_validate_common_envelope(p_message)) {
		return ERR_INVALID_DATA;
	}
	String ack_id;
	if (!get_string(p_message, "ack_id", ack_id) || !is_ack_id(ack_id) || !p_message.has("params") || p_message["params"].get_type() != Variant::DICTIONARY) {
		return ERR_INVALID_DATA;
	}
	const Dictionary params = p_message["params"];
	String snapshot_id;
	int64_t through_chunk = 0;
	if (params.size() != 2 || !get_string(params, "snapshot_id", snapshot_id) || !is_snapshot_id(snapshot_id) || !get_bounded_integer(params, "through_chunk", 0, 65535, through_chunk)) {
		return ERR_INVALID_DATA;
	}
	return OK;
}

BridgeRpcSession::BridgeRpcSession(const String &p_project_id, const String &p_editor_session_id) :
		project_id(p_project_id), editor_session_id(p_editor_session_id) {
}

Error BridgeRpcSession::handle_message(const Dictionary &p_message, uint64_t p_now_usec, uint64_t p_internal_request_id, Outcome &r_outcome) {
	r_outcome = Outcome();
	String kind;
	if (!get_string(p_message, "kind", kind)) {
		return ERR_INVALID_DATA;
	}
	if (kind == "request") {
		return _handle_request(p_message, p_now_usec, p_internal_request_id, r_outcome);
	}
	if (kind == "cancel") {
		return _handle_cancel(p_message, r_outcome);
	}
	if (kind == "ack") {
		return _handle_ack(p_message, r_outcome);
	}
	return ERR_INVALID_DATA;
}

Error BridgeRpcSession::complete(uint64_t p_internal_request_id, uint64_t p_now_usec, Outcome &r_outcome) {
	return complete(p_internal_request_id, p_now_usec, Dictionary(), r_outcome);
}

Error BridgeRpcSession::complete(uint64_t p_internal_request_id, uint64_t p_now_usec, const Dictionary &p_result_override, Outcome &r_outcome) {
	r_outcome = Outcome();
	PendingRequest *pending_pointer = pending_by_internal_id.getptr(p_internal_request_id);
	if (!pending_pointer) {
		return ERR_DOES_NOT_EXIST;
	}
	const PendingRequest pending = *pending_pointer;
	if (p_now_usec >= pending.deadline_usec) {
		_set_error_outcome(pending.request_id, "deadline_exceeded", "The request deadline elapsed before completion.", true, r_outcome);
		_remove_pending(p_internal_request_id);
		return OK;
	}

	Dictionary result;
	switch (pending.method) {
		case METHOD_INITIALIZE: {
			result = _make_capabilities();
			result["protocol_version"] = protocol_version;
			result["project_id"] = project_id;
			result["editor_session_id"] = editor_session_id;
			result["limits"] = _make_limits();
			result["revisions"] = _make_revisions();
			initialized = true;
		} break;
		case METHOD_PING: {
			result["echo"] = pending.params["echo"];
		} break;
		case METHOD_CAPABILITIES: {
			result = _make_capabilities();
			result["limits"] = _make_limits();
		} break;
		case METHOD_EDITOR_SNAPSHOT: {
			if (p_result_override.is_empty()) {
				_set_error_outcome(pending.request_id, "internal_error", "The editor snapshot was not produced.", true, r_outcome);
				_remove_pending(p_internal_request_id);
				return OK;
			}
			result = p_result_override;
		} break;
		case METHOD_SHUTDOWN: {
			result["closing"] = true;
			closing = true;
			r_outcome.close_after_response = true;
		} break;
	}
	if (pending.method != METHOD_EDITOR_SNAPSHOT && !p_result_override.is_empty()) {
		const Array keys = p_result_override.keys();
		for (int index = 0; index < keys.size(); index++) {
			result[keys[index]] = p_result_override[keys[index]];
		}
	}
	r_outcome.has_response = true;
	r_outcome.response = _make_result_response(pending.request_id, result);
	_remove_pending(p_internal_request_id);
	return OK;
}

Error BridgeRpcSession::reject_dispatch(uint64_t p_internal_request_id, Outcome &r_outcome) {
	r_outcome = Outcome();
	PendingRequest *pending = pending_by_internal_id.getptr(p_internal_request_id);
	if (!pending) {
		return ERR_DOES_NOT_EXIST;
	}
	_set_error_outcome(pending->request_id, "overloaded", "The bridge dispatcher is currently overloaded.", true, r_outcome);
	_remove_pending(p_internal_request_id);
	return OK;
}

void BridgeRpcSession::expire_requests(uint64_t p_now_usec, Vector<Outcome> &r_outcomes) {
	Vector<uint64_t> expired_ids;
	for (const KeyValue<uint64_t, PendingRequest> &entry : pending_by_internal_id) {
		if (p_now_usec >= entry.value.deadline_usec) {
			expired_ids.push_back(entry.key);
		}
	}
	for (uint64_t internal_id : expired_ids) {
		PendingRequest *pending = pending_by_internal_id.getptr(internal_id);
		if (!pending) {
			continue;
		}
		Outcome outcome;
		_set_error_outcome(pending->request_id, "deadline_exceeded", "The request deadline elapsed before completion.", true, outcome);
		outcome.cancel_dispatch = true;
		outcome.internal_request_id = internal_id;
		_remove_pending(internal_id);
		r_outcomes.push_back(outcome);
	}
}

void BridgeRpcSession::cancel_all(Vector<uint64_t> &r_internal_request_ids) {
	for (const KeyValue<uint64_t, PendingRequest> &entry : pending_by_internal_id) {
		r_internal_request_ids.push_back(entry.key);
	}
	pending_by_internal_id.clear();
	pending_by_request_id.clear();
	initialize_pending_id = 0;
	shutdown_pending_id = 0;
}

bool BridgeRpcSession::is_initialized() const {
	return initialized;
}

void BridgeRpcSession::set_protocol_version(const String &p_protocol_version) {
	ERR_FAIL_COND_MSG(initialized, "The negotiated protocol version cannot change after initialization.");
	ERR_FAIL_COND_MSG(p_protocol_version != "1.0" && p_protocol_version != "1.1", "Unsupported Bridge RPC protocol version.");
	protocol_version = p_protocol_version;
}

const String &BridgeRpcSession::get_protocol_version() const {
	return protocol_version;
}

uint32_t BridgeRpcSession::get_in_flight_count() const {
	return pending_by_internal_id.size();
}
