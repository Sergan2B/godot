/**************************************************************************/
/*  bridge_handshake.cpp                                                  */
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

#include "bridge_handshake.h"

#include "bridge_crypto.h"

namespace {

static bool get_required_string(const Dictionary &p_message, const StringName &p_key, String &r_value) {
	if (!p_message.has(p_key)) {
		return false;
	}
	const Variant value = p_message[p_key];
	if (value.get_type() != Variant::STRING) {
		return false;
	}
	r_value = value;
	return true;
}

} // namespace

bool BridgeHandshakeSession::_parse_protocol_version(const String &p_version, uint32_t &r_major, uint32_t &r_minor) {
	const int separator = p_version.find_char('.');
	if (separator <= 0 || separator == p_version.length() - 1 || p_version.find_char('.', separator + 1) != -1) {
		return false;
	}
	const String major = p_version.left(separator);
	const String minor = p_version.substr(separator + 1);
	if (!major.is_valid_int() || !minor.is_valid_int() || major[0] == '0') {
		return false;
	}
	for (int index = 0; index < major.length(); index++) {
		if (major[index] < '0' || major[index] > '9') {
			return false;
		}
	}
	for (int index = 0; index < minor.length(); index++) {
		if (minor[index] < '0' || minor[index] > '9') {
			return false;
		}
	}
	const int64_t parsed_major = major.to_int();
	const int64_t parsed_minor = minor.to_int();
	if (parsed_major <= 0 || parsed_major > UINT32_MAX || parsed_minor < 0 || parsed_minor > UINT32_MAX) {
		return false;
	}
	r_major = parsed_major;
	r_minor = parsed_minor;
	return true;
}

Dictionary BridgeHandshakeSession::_make_error(const String &p_code, const String &p_message, bool p_retryable) {
	Dictionary error;
	error["code"] = p_code;
	error["message"] = p_message;
	error["retryable"] = p_retryable;
	return error;
}

void BridgeHandshakeSession::_set_error_outcome(const String &p_code, const String &p_message, bool p_retryable, Outcome &r_outcome, bool p_authentication_failed) {
	r_outcome.response["handshake_version"] = "1.0";
	r_outcome.response["kind"] = "handshake.error";
	r_outcome.response["error"] = _make_error(p_code, p_message, p_retryable);
	if (p_code == "protocol_mismatch") {
		Array versions;
		for (const String &version : supported_versions) {
			versions.push_back(version);
		}
		r_outcome.response["supported_protocol_versions"] = versions;
	}
	r_outcome.has_response = true;
	r_outcome.close_after_response = true;
	r_outcome.authentication_failed = p_authentication_failed;
	state = STATE_CLOSED;
}

Error BridgeHandshakeSession::_handle_client_hello(const Dictionary &p_message, Outcome &r_outcome) {
	String handshake_version;
	String project;
	String editor_session;
	String nonce_encoded;
	if (!get_required_string(p_message, "handshake_version", handshake_version) || handshake_version != "1.0" ||
			!get_required_string(p_message, "project_id", project) ||
			!get_required_string(p_message, "editor_session_id", editor_session) ||
			!get_required_string(p_message, "client_nonce", nonce_encoded) ||
			!p_message.has("supported_protocol_versions") || p_message["supported_protocol_versions"].get_type() != Variant::ARRAY) {
		state = STATE_CLOSED;
		return ERR_INVALID_DATA;
	}
	if (project != project_id) {
		_set_error_outcome("project_not_bound", "Project binding failed.", false, r_outcome, true);
		return OK;
	}
	if (editor_session != editor_session_id) {
		_set_error_outcome("session_mismatch", "Editor session binding failed.", false, r_outcome, true);
		return OK;
	}

	const Array versions = p_message["supported_protocol_versions"];
	if (versions.is_empty() || versions.size() > 8) {
		state = STATE_CLOSED;
		return ERR_INVALID_DATA;
	}
	HashSet<String> unique_versions;
	HashSet<uint32_t> unique_majors;
	offered_versions.clear();
	bool supports_major_one = false;
	for (int index = 0; index < versions.size(); index++) {
		if (versions[index].get_type() != Variant::STRING) {
			state = STATE_CLOSED;
			return ERR_INVALID_DATA;
		}
		const String version = versions[index];
		uint32_t major = 0;
		uint32_t minor = 0;
		if (!_parse_protocol_version(version, major, minor) || unique_versions.has(version) || unique_majors.has(major)) {
			state = STATE_CLOSED;
			return ERR_INVALID_DATA;
		}
		unique_versions.insert(version);
		unique_majors.insert(major);
		offered_versions.push_back(version);
		if (major == 1) {
			supports_major_one = true;
		}
	}
	if (!supports_major_one) {
		_set_error_outcome("protocol_mismatch", "No compatible protocol version.", false, r_outcome);
		return OK;
	}

	if (BridgeCrypto::base64url_decode_32(nonce_encoded, client_nonce) != OK) {
		state = STATE_CLOSED;
		return ERR_INVALID_DATA;
	}
	if (seen_client_nonces) {
		if (seen_client_nonces->has(nonce_encoded)) {
			state = STATE_CLOSED;
			return ERR_INVALID_DATA;
		}
		seen_client_nonces->insert(nonce_encoded);
	}

	selected_version = "1.0";
	Error error = BridgeCrypto::random_bytes(BridgeCrypto::RANDOM_VALUE_BYTES, server_nonce);
	if (error != OK) {
		state = STATE_CLOSED;
		return error;
	}
	error = BridgeCrypto::build_handshake_transcript(handshake_version, offered_versions, selected_version, project_id, editor_session_id, client_nonce, server_nonce, transcript);
	if (error != OK) {
		state = STATE_CLOSED;
		return error;
	}
	PackedByteArray proof;
	error = BridgeCrypto::handshake_proof(true, token, transcript, proof);
	if (error != OK) {
		state = STATE_CLOSED;
		return error;
	}
	String server_nonce_encoded;
	String proof_encoded;
	BridgeCrypto::base64url_encode_32(server_nonce, server_nonce_encoded);
	BridgeCrypto::base64url_encode_32(proof, proof_encoded);

	r_outcome.response["handshake_version"] = "1.0";
	r_outcome.response["kind"] = "handshake.server_challenge";
	r_outcome.response["selected_protocol_version"] = selected_version;
	r_outcome.response["project_id"] = project_id;
	r_outcome.response["editor_session_id"] = editor_session_id;
	r_outcome.response["server_nonce"] = server_nonce_encoded;
	r_outcome.response["server_proof"] = proof_encoded;
	r_outcome.has_response = true;
	state = STATE_WAITING_FOR_AUTHENTICATION;
	return OK;
}

Error BridgeHandshakeSession::_handle_client_authenticate(const Dictionary &p_message, Outcome &r_outcome) {
	String handshake_version;
	String selected;
	String project;
	String editor_session;
	String proof_encoded;
	if (!get_required_string(p_message, "handshake_version", handshake_version) || handshake_version != "1.0" ||
			!get_required_string(p_message, "selected_protocol_version", selected) ||
			!get_required_string(p_message, "project_id", project) ||
			!get_required_string(p_message, "editor_session_id", editor_session) ||
			!get_required_string(p_message, "client_proof", proof_encoded)) {
		state = STATE_CLOSED;
		return ERR_INVALID_DATA;
	}
	if (project != project_id) {
		_set_error_outcome("project_not_bound", "Project binding failed.", false, r_outcome, true);
		return OK;
	}
	if (editor_session != editor_session_id) {
		_set_error_outcome("session_mismatch", "Editor session binding failed.", false, r_outcome, true);
		return OK;
	}
	if (selected != selected_version) {
		state = STATE_CLOSED;
		return ERR_INVALID_DATA;
	}

	PackedByteArray received_proof;
	PackedByteArray expected_proof;
	if (BridgeCrypto::base64url_decode_32(proof_encoded, received_proof) != OK || BridgeCrypto::handshake_proof(false, token, transcript, expected_proof) != OK || !BridgeCrypto::constant_time_equal(expected_proof, received_proof)) {
		_set_error_outcome("unauthenticated", "Authentication failed.", false, r_outcome, true);
		return OK;
	}

	r_outcome.response["handshake_version"] = "1.0";
	r_outcome.response["kind"] = "handshake.server_ready";
	r_outcome.response["selected_protocol_version"] = selected_version;
	r_outcome.response["project_id"] = project_id;
	r_outcome.response["editor_session_id"] = editor_session_id;
	r_outcome.has_response = true;
	state = STATE_AUTHENTICATED;
	return OK;
}

BridgeHandshakeSession::BridgeHandshakeSession(const PackedByteArray &p_token, const String &p_project_id, const String &p_editor_session_id, uint64_t p_accepted_at_usec, HashSet<String> *p_seen_client_nonces) :
		deadline_usec(p_accepted_at_usec + HANDSHAKE_TIMEOUT_USEC), token(p_token), project_id(p_project_id), editor_session_id(p_editor_session_id), seen_client_nonces(p_seen_client_nonces) {
	supported_versions.push_back("1.0");
}

Error BridgeHandshakeSession::handle_message(const Dictionary &p_message, uint64_t p_now_usec, Outcome &r_outcome) {
	r_outcome = Outcome();
	if (has_timed_out(p_now_usec)) {
		state = STATE_CLOSED;
		return ERR_TIMEOUT;
	}
	String kind;
	if (!get_required_string(p_message, "kind", kind)) {
		state = STATE_CLOSED;
		return ERR_INVALID_DATA;
	}
	if (state == STATE_WAITING_FOR_HELLO && kind == "handshake.client_hello") {
		return _handle_client_hello(p_message, r_outcome);
	}
	if (state == STATE_WAITING_FOR_AUTHENTICATION && kind == "handshake.client_authenticate") {
		return _handle_client_authenticate(p_message, r_outcome);
	}
	state = STATE_CLOSED;
	return ERR_INVALID_DATA;
}

bool BridgeHandshakeSession::has_timed_out(uint64_t p_now_usec) const {
	return state != STATE_AUTHENTICATED && state != STATE_CLOSED && p_now_usec >= deadline_usec;
}

BridgeHandshakeSession::State BridgeHandshakeSession::get_state() const {
	return state;
}
