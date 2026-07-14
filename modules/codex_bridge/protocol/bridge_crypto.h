/**************************************************************************/
/*  bridge_crypto.h                                                       */
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

#include "core/variant/variant.h"

class BridgeCrypto {
public:
	static constexpr int RANDOM_VALUE_BYTES = 32;

	static Error random_bytes(int p_size, PackedByteArray &r_bytes);
	static String bytes_to_lower_hex(const PackedByteArray &p_bytes);
	static Error project_id_from_canonical_root(const String &p_canonical_root, String &r_project_id);

	static Error base64url_encode_32(const PackedByteArray &p_bytes, String &r_encoded);
	static Error base64url_decode_32(const String &p_encoded, PackedByteArray &r_bytes);

	static Error build_handshake_transcript(const String &p_handshake_version, const PackedStringArray &p_supported_versions, const String &p_selected_version, const String &p_project_id, const String &p_editor_session_id, const PackedByteArray &p_client_nonce, const PackedByteArray &p_server_nonce, PackedByteArray &r_transcript);
	static Error hmac_sha256(const PackedByteArray &p_key, const PackedByteArray &p_message, PackedByteArray &r_digest);
	static Error handshake_proof(bool p_server, const PackedByteArray &p_token, const PackedByteArray &p_transcript, PackedByteArray &r_proof);
	static bool constant_time_equal(const PackedByteArray &p_trusted, const PackedByteArray &p_received);
};
