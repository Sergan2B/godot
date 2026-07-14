/**************************************************************************/
/*  bridge_crypto.cpp                                                     */
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

#include "bridge_crypto.h"

#include "core/crypto/crypto_core.h"

namespace {

static void append_bytes(PackedByteArray &r_target, const uint8_t *p_bytes, int p_size) {
	const int offset = r_target.size();
	r_target.resize(offset + p_size);
	if (p_size > 0) {
		memcpy(r_target.ptrw() + offset, p_bytes, p_size);
	}
}

static void append_utf8(PackedByteArray &r_target, const String &p_value) {
	const CharString utf8 = p_value.utf8();
	append_bytes(r_target, reinterpret_cast<const uint8_t *>(utf8.get_data()), utf8.length());
}

static void append_u32be(PackedByteArray &r_target, uint32_t p_value) {
	const uint8_t bytes[4] = {
		(uint8_t)(p_value >> 24),
		(uint8_t)(p_value >> 16),
		(uint8_t)(p_value >> 8),
		(uint8_t)p_value,
	};
	append_bytes(r_target, bytes, 4);
}

static void append_lp_utf8(PackedByteArray &r_target, const String &p_value) {
	const CharString utf8 = p_value.utf8();
	append_u32be(r_target, utf8.length());
	append_bytes(r_target, reinterpret_cast<const uint8_t *>(utf8.get_data()), utf8.length());
}

static void append_lp_bytes(PackedByteArray &r_target, const PackedByteArray &p_value) {
	append_u32be(r_target, p_value.size());
	append_bytes(r_target, p_value.ptr(), p_value.size());
}

static Error sha256(const PackedByteArray &p_input, PackedByteArray &r_digest) {
	r_digest.resize(32);
	const Error error = CryptoCore::sha256(p_input.ptr(), p_input.size(), r_digest.ptrw());
	if (error != OK) {
		r_digest.clear();
	}
	return error;
}

} // namespace

Error BridgeCrypto::random_bytes(int p_size, PackedByteArray &r_bytes) {
	ERR_FAIL_COND_V(p_size <= 0, ERR_INVALID_PARAMETER);
	r_bytes.resize(p_size);
	const Error error = CryptoCore::generate_random(r_bytes.ptrw(), p_size);
	if (error != OK) {
		r_bytes.clear();
	}
	return error;
}

String BridgeCrypto::bytes_to_lower_hex(const PackedByteArray &p_bytes) {
	static constexpr char HEX[] = "0123456789abcdef";
	CharString result;
	result.resize_uninitialized(p_bytes.size() * 2 + 1);
	for (int index = 0; index < p_bytes.size(); index++) {
		const uint8_t byte = p_bytes[index];
		result.ptrw()[index * 2] = HEX[byte >> 4];
		result.ptrw()[index * 2 + 1] = HEX[byte & 0x0f];
	}
	result.ptrw()[p_bytes.size() * 2] = '\0';
	return String::ascii(result.span());
}

Error BridgeCrypto::project_id_from_canonical_root(const String &p_canonical_root, String &r_project_id) {
	ERR_FAIL_COND_V(p_canonical_root.is_empty() || !p_canonical_root.is_absolute_path(), ERR_INVALID_PARAMETER);

	PackedByteArray input;
	static constexpr char PROJECT_ID_DOMAIN[] = "godot-codex-project-id/v1";
	append_bytes(input, reinterpret_cast<const uint8_t *>(PROJECT_ID_DOMAIN), sizeof(PROJECT_ID_DOMAIN));
	append_utf8(input, p_canonical_root);

	PackedByteArray digest;
	const Error error = sha256(input, digest);
	if (error != OK) {
		return error;
	}
	r_project_id = "project:sha256:" + bytes_to_lower_hex(digest);
	return OK;
}

Error BridgeCrypto::base64url_encode_32(const PackedByteArray &p_bytes, String &r_encoded) {
	ERR_FAIL_COND_V(p_bytes.size() != RANDOM_VALUE_BYTES, ERR_INVALID_PARAMETER);
	r_encoded = CryptoCore::b64_encode_str(p_bytes.ptr(), p_bytes.size()).replace("+", "-").replace("/", "_").trim_suffix("=");
	ERR_FAIL_COND_V(r_encoded.length() != 43, ERR_BUG);
	return OK;
}

Error BridgeCrypto::base64url_decode_32(const String &p_encoded, PackedByteArray &r_bytes) {
	if (p_encoded.length() != 43) {
		return ERR_INVALID_DATA;
	}
	for (int index = 0; index < p_encoded.length(); index++) {
		const char32_t c = p_encoded[index];
		const bool valid = (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '_' || c == '-';
		if (!valid) {
			return ERR_INVALID_DATA;
		}
	}

	const String padded = p_encoded.replace("-", "+").replace("_", "/") + "=";
	const CharString encoded = padded.ascii();
	r_bytes.resize(RANDOM_VALUE_BYTES);
	size_t decoded_size = 0;
	const Error error = CryptoCore::b64_decode(r_bytes.ptrw(), r_bytes.size(), &decoded_size, reinterpret_cast<const uint8_t *>(encoded.get_data()), encoded.length());
	if (error != OK || decoded_size != RANDOM_VALUE_BYTES) {
		r_bytes.clear();
		return ERR_INVALID_DATA;
	}
	String canonical;
	if (base64url_encode_32(r_bytes, canonical) != OK || canonical != p_encoded) {
		r_bytes.clear();
		return ERR_INVALID_DATA;
	}
	return OK;
}

Error BridgeCrypto::build_handshake_transcript(const String &p_handshake_version, const PackedStringArray &p_supported_versions, const String &p_selected_version, const String &p_project_id, const String &p_editor_session_id, const PackedByteArray &p_client_nonce, const PackedByteArray &p_server_nonce, PackedByteArray &r_transcript) {
	ERR_FAIL_COND_V(p_supported_versions.is_empty() || p_supported_versions.size() > 8, ERR_INVALID_PARAMETER);
	ERR_FAIL_COND_V(p_client_nonce.size() != RANDOM_VALUE_BYTES || p_server_nonce.size() != RANDOM_VALUE_BYTES, ERR_INVALID_PARAMETER);

	r_transcript.clear();
	static constexpr char TRANSCRIPT_DOMAIN[] = "godot-codex-bridge/handshake-transcript/v1";
	append_bytes(r_transcript, reinterpret_cast<const uint8_t *>(TRANSCRIPT_DOMAIN), sizeof(TRANSCRIPT_DOMAIN));
	append_lp_utf8(r_transcript, p_handshake_version);
	append_u32be(r_transcript, p_supported_versions.size());
	for (const String &version : p_supported_versions) {
		append_lp_utf8(r_transcript, version);
	}
	append_lp_utf8(r_transcript, p_selected_version);
	append_lp_utf8(r_transcript, p_project_id);
	append_lp_utf8(r_transcript, p_editor_session_id);
	append_lp_bytes(r_transcript, p_client_nonce);
	append_lp_bytes(r_transcript, p_server_nonce);
	return OK;
}

Error BridgeCrypto::hmac_sha256(const PackedByteArray &p_key, const PackedByteArray &p_message, PackedByteArray &r_digest) {
	ERR_FAIL_COND_V(p_key.is_empty(), ERR_INVALID_PARAMETER);
	static constexpr int BLOCK_BYTES = 64;

	PackedByteArray normalized_key;
	if (p_key.size() > BLOCK_BYTES) {
		const Error error = sha256(p_key, normalized_key);
		if (error != OK) {
			return error;
		}
	} else {
		normalized_key = p_key;
	}
	const int key_size = normalized_key.size();
	normalized_key.resize(BLOCK_BYTES);
	for (int index = key_size; index < BLOCK_BYTES; index++) {
		normalized_key.ptrw()[index] = 0;
	}

	PackedByteArray inner;
	inner.resize(BLOCK_BYTES);
	PackedByteArray outer;
	outer.resize(BLOCK_BYTES);
	for (int index = 0; index < BLOCK_BYTES; index++) {
		inner.ptrw()[index] = normalized_key[index] ^ 0x36;
		outer.ptrw()[index] = normalized_key[index] ^ 0x5c;
	}
	append_bytes(inner, p_message.ptr(), p_message.size());

	PackedByteArray inner_digest;
	Error error = sha256(inner, inner_digest);
	if (error != OK) {
		return error;
	}
	append_bytes(outer, inner_digest.ptr(), inner_digest.size());
	return sha256(outer, r_digest);
}

Error BridgeCrypto::handshake_proof(bool p_server, const PackedByteArray &p_token, const PackedByteArray &p_transcript, PackedByteArray &r_proof) {
	ERR_FAIL_COND_V(p_token.size() != RANDOM_VALUE_BYTES, ERR_INVALID_PARAMETER);
	PackedByteArray message;
	static constexpr char SERVER_DOMAIN[] = "godot-codex-bridge/server-proof/v1";
	static constexpr char CLIENT_DOMAIN[] = "godot-codex-bridge/client-proof/v1";
	if (p_server) {
		append_bytes(message, reinterpret_cast<const uint8_t *>(SERVER_DOMAIN), sizeof(SERVER_DOMAIN));
	} else {
		append_bytes(message, reinterpret_cast<const uint8_t *>(CLIENT_DOMAIN), sizeof(CLIENT_DOMAIN));
	}
	append_bytes(message, p_transcript.ptr(), p_transcript.size());
	return hmac_sha256(p_token, message, r_proof);
}

bool BridgeCrypto::constant_time_equal(const PackedByteArray &p_trusted, const PackedByteArray &p_received) {
	if (p_trusted.size() != p_received.size()) {
		return false;
	}
	uint8_t difference = 0;
	for (int index = 0; index < p_trusted.size(); index++) {
		difference |= p_trusted[index] ^ p_received[index];
	}
	return difference == 0;
}
