/**************************************************************************/
/*  bridge_rpc_session.h                                                  */
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

#pragma once

#include "core/templates/hash_map.h"
#include "core/templates/hash_set.h"
#include "core/templates/vector.h"
#include "core/variant/variant.h"

class BridgeRpcSession {
public:
	enum Method {
		METHOD_INITIALIZE,
		METHOD_PING,
		METHOD_CAPABILITIES,
		METHOD_EDITOR_SNAPSHOT,
		METHOD_RESOURCE_SNAPSHOT,
		METHOD_RESOURCE_DELTA,
		METHOD_SHUTDOWN,
	};

	struct Outcome {
		bool has_response = false;
		Dictionary response;
		bool dispatch = false;
		Method method = METHOD_INITIALIZE;
		uint64_t internal_request_id = 0;
		uint64_t deadline_usec = 0;
		Dictionary params;
		bool cancel_dispatch = false;
		bool cancel_stream = false;
		String stream_request_id;
		bool ack_received = false;
		String ack_snapshot_id;
		int64_t ack_through_chunk = -1;
		bool close_after_response = false;
	};

	static constexpr uint32_t MAX_IN_FLIGHT_REQUESTS = 64;
	static constexpr uint32_t DEFAULT_DEADLINE_MS = 5000;
	static constexpr uint32_t MAX_DEADLINE_MS = 30000;
	static constexpr uint32_t MAX_PING_ECHO_BYTES = 256;

private:
	struct PendingRequest {
		String request_id;
		Method method = METHOD_INITIALIZE;
		uint64_t deadline_usec = 0;
		Dictionary params;
	};

	String project_id;
	String editor_session_id;
	String protocol_version = "1.0";
	HashSet<String> seen_request_ids;
	HashMap<uint64_t, PendingRequest> pending_by_internal_id;
	HashMap<String, uint64_t> pending_by_request_id;
	bool initialized = false;
	bool closing = false;
	uint64_t initialize_pending_id = 0;
	uint64_t shutdown_pending_id = 0;

	Dictionary _make_context() const;
	Dictionary _make_error(const String &p_code, const String &p_message, bool p_retryable, const Dictionary &p_data = Dictionary()) const;
	Dictionary _make_error_response(const String &p_request_id, const String &p_code, const String &p_message, bool p_retryable, const Dictionary &p_data = Dictionary()) const;
	Dictionary _make_result_response(const String &p_request_id, const Dictionary &p_result) const;
	Dictionary _make_capabilities() const;
	Dictionary _make_limits() const;
	Dictionary _make_revisions() const;

	bool _validate_common_envelope(const Dictionary &p_message) const;
	bool _validate_initialize_params(const Dictionary &p_params) const;
	bool _validate_ping_params(const Dictionary &p_params) const;
	bool _validate_snapshot_params(const Dictionary &p_params) const;
	bool _validate_resource_snapshot_params(const Dictionary &p_params) const;
	bool _validate_resource_delta_params(const Dictionary &p_params) const;
	bool _validate_shutdown_params(const Dictionary &p_params) const;
	void _remove_pending(uint64_t p_internal_request_id);
	void _set_error_outcome(const String &p_request_id, const String &p_code, const String &p_message, bool p_retryable, Outcome &r_outcome) const;
	Error _handle_request(const Dictionary &p_message, uint64_t p_now_usec, uint64_t p_internal_request_id, Outcome &r_outcome);
	Error _handle_cancel(const Dictionary &p_message, Outcome &r_outcome);
	Error _handle_ack(const Dictionary &p_message, Outcome &r_outcome);

public:
	BridgeRpcSession(const String &p_project_id, const String &p_editor_session_id);

	Error handle_message(const Dictionary &p_message, uint64_t p_now_usec, uint64_t p_internal_request_id, Outcome &r_outcome);
	Error complete(uint64_t p_internal_request_id, uint64_t p_now_usec, Outcome &r_outcome);
	Error complete(uint64_t p_internal_request_id, uint64_t p_now_usec, const Dictionary &p_result_override, Outcome &r_outcome);
	Error complete_error(uint64_t p_internal_request_id, const String &p_code, const String &p_message, bool p_retryable, const Dictionary &p_data, Outcome &r_outcome);
	Error reject_dispatch(uint64_t p_internal_request_id, Outcome &r_outcome);
	void expire_requests(uint64_t p_now_usec, Vector<Outcome> &r_outcomes);
	void cancel_all(Vector<uint64_t> &r_internal_request_ids);

	bool is_initialized() const;
	void set_protocol_version(const String &p_protocol_version);
	const String &get_protocol_version() const;
	uint32_t get_in_flight_count() const;
};
