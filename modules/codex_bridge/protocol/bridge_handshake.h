/**************************************************************************/
/*  bridge_handshake.h                                                    */
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

#include "core/templates/hash_set.h"
#include "core/variant/variant.h"

class BridgeHandshakeSession {
public:
	enum State {
		STATE_WAITING_FOR_HELLO,
		STATE_WAITING_FOR_AUTHENTICATION,
		STATE_AUTHENTICATED,
		STATE_CLOSED,
	};

	struct Outcome {
		Dictionary response;
		bool has_response = false;
		bool close_after_response = false;
		bool authentication_failed = false;
	};

	static constexpr uint64_t HANDSHAKE_TIMEOUT_USEC = 3000000;

private:
	State state = STATE_WAITING_FOR_HELLO;
	uint64_t deadline_usec = 0;
	PackedByteArray token;
	String project_id;
	String editor_session_id;
	PackedStringArray supported_versions;
	PackedStringArray offered_versions;
	String selected_version;
	PackedByteArray client_nonce;
	PackedByteArray server_nonce;
	PackedByteArray transcript;
	HashSet<String> *seen_client_nonces = nullptr;

	static bool _parse_protocol_version(const String &p_version, uint32_t &r_major, uint32_t &r_minor);
	static Dictionary _make_error(const String &p_code, const String &p_message, bool p_retryable);
	void _set_error_outcome(const String &p_code, const String &p_message, bool p_retryable, Outcome &r_outcome, bool p_authentication_failed = false);
	Error _handle_client_hello(const Dictionary &p_message, Outcome &r_outcome);
	Error _handle_client_authenticate(const Dictionary &p_message, Outcome &r_outcome);

public:
	BridgeHandshakeSession(const PackedByteArray &p_token, const String &p_project_id, const String &p_editor_session_id, uint64_t p_accepted_at_usec, HashSet<String> *p_seen_client_nonces = nullptr);

	Error handle_message(const Dictionary &p_message, uint64_t p_now_usec, Outcome &r_outcome);
	bool has_timed_out(uint64_t p_now_usec) const;
	State get_state() const;
	const String &get_selected_protocol_version() const;
};
