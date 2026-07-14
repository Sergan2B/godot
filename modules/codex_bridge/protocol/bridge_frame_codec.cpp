/**************************************************************************/
/*  bridge_frame_codec.cpp                                                */
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

#include "bridge_frame_codec.h"

#include "core/io/json.h"
#include "core/templates/hash_set.h"

namespace {

static bool is_json_whitespace(char32_t p_character) {
	return p_character == 0x20 || p_character == '\t' || p_character == '\n' || p_character == '\r';
}

static bool validate_strict_utf8(const PackedByteArray &p_payload) {
	if (p_payload.is_empty()) {
		return false;
	}
	if (p_payload.size() >= 3 && p_payload[0] == 0xef && p_payload[1] == 0xbb && p_payload[2] == 0xbf) {
		return false;
	}

	int index = 0;
	while (index < p_payload.size()) {
		const uint8_t first = p_payload[index++];
		if (first == 0) {
			return false;
		}
		if (first <= 0x7f) {
			continue;
		}

		int continuation_count = 0;
		uint32_t codepoint = 0;
		uint32_t minimum = 0;
		if (first >= 0xc2 && first <= 0xdf) {
			continuation_count = 1;
			codepoint = first & 0x1f;
			minimum = 0x80;
		} else if (first >= 0xe0 && first <= 0xef) {
			continuation_count = 2;
			codepoint = first & 0x0f;
			minimum = 0x800;
		} else if (first >= 0xf0 && first <= 0xf4) {
			continuation_count = 3;
			codepoint = first & 0x07;
			minimum = 0x10000;
		} else {
			return false;
		}
		if (index + continuation_count > p_payload.size()) {
			return false;
		}
		for (int continuation = 0; continuation < continuation_count; continuation++) {
			const uint8_t byte = p_payload[index++];
			if ((byte & 0xc0) != 0x80) {
				return false;
			}
			codepoint = (codepoint << 6) | (byte & 0x3f);
		}
		if (codepoint < minimum || codepoint > 0x10ffff || (codepoint >= 0xd800 && codepoint <= 0xdfff)) {
			return false;
		}
	}
	return true;
}

class StrictJsonScanner {
	const String &source;
	int index = 0;
	int entries = 0;

	void skip_whitespace() {
		while (index < source.length() && is_json_whitespace(source[index])) {
			index++;
		}
	}

	Error scan_string(String *r_decoded = nullptr) {
		ERR_FAIL_COND_V(index >= source.length() || source[index] != '"', ERR_INVALID_DATA);
		const int start = index++;
		bool escaped = false;
		while (index < source.length()) {
			const char32_t character = source[index++];
			if (escaped) {
				escaped = false;
				continue;
			}
			if (character == '\\') {
				escaped = true;
				continue;
			}
			if (character == '"') {
				if (r_decoded) {
					Ref<JSON> parser;
					parser.instantiate();
					const Error error = parser->parse(source.substr(start, index - start));
					if (error != OK || parser->get_data().get_type() != Variant::STRING) {
						return ERR_INVALID_DATA;
					}
					*r_decoded = parser->get_data();
				}
				return OK;
			}
		}
		return ERR_INVALID_DATA;
	}

	Error scan_value(int p_depth) {
		if (p_depth > BridgeJson::MAX_NESTING_DEPTH) {
			return ERR_OUT_OF_MEMORY;
		}
		skip_whitespace();
		if (index >= source.length()) {
			return ERR_INVALID_DATA;
		}

		if (source[index] == '"') {
			return scan_string();
		}
		if (source[index] == '{') {
			return scan_object(p_depth + 1);
		}
		if (source[index] == '[') {
			return scan_array(p_depth + 1);
		}

		const int start = index;
		while (index < source.length() && source[index] != ',' && source[index] != ']' && source[index] != '}' && !is_json_whitespace(source[index])) {
			index++;
		}
		return index > start ? OK : ERR_INVALID_DATA;
	}

	Error scan_object(int p_depth) {
		index++;
		skip_whitespace();
		HashSet<String> keys;
		if (index < source.length() && source[index] == '}') {
			index++;
			return OK;
		}
		while (index < source.length()) {
			String key;
			if (scan_string(&key) != OK || keys.has(key)) {
				return ERR_INVALID_DATA;
			}
			keys.insert(key);
			if (++entries > BridgeJson::MAX_CONTAINER_ENTRIES) {
				return ERR_OUT_OF_MEMORY;
			}
			skip_whitespace();
			if (index >= source.length() || source[index++] != ':') {
				return ERR_INVALID_DATA;
			}
			const Error error = scan_value(p_depth);
			if (error != OK) {
				return error;
			}
			skip_whitespace();
			if (index < source.length() && source[index] == '}') {
				index++;
				return OK;
			}
			if (index >= source.length() || source[index++] != ',') {
				return ERR_INVALID_DATA;
			}
			skip_whitespace();
		}
		return ERR_INVALID_DATA;
	}

	Error scan_array(int p_depth) {
		index++;
		skip_whitespace();
		if (index < source.length() && source[index] == ']') {
			index++;
			return OK;
		}
		while (index < source.length()) {
			if (++entries > BridgeJson::MAX_CONTAINER_ENTRIES) {
				return ERR_OUT_OF_MEMORY;
			}
			const Error error = scan_value(p_depth);
			if (error != OK) {
				return error;
			}
			skip_whitespace();
			if (index < source.length() && source[index] == ']') {
				index++;
				return OK;
			}
			if (index >= source.length() || source[index++] != ',') {
				return ERR_INVALID_DATA;
			}
			skip_whitespace();
		}
		return ERR_INVALID_DATA;
	}

public:
	explicit StrictJsonScanner(const String &p_source) : source(p_source) {}

	Error scan() {
		skip_whitespace();
		const Error error = scan_value(0);
		if (error != OK) {
			return error;
		}
		skip_whitespace();
		return index == source.length() ? OK : ERR_INVALID_DATA;
	}
};

} // namespace

Error BridgeJson::parse_strict_object(const PackedByteArray &p_payload, Dictionary &r_object) {
	if (!validate_strict_utf8(p_payload)) {
		return ERR_INVALID_DATA;
	}
	const String source = String::utf8(reinterpret_cast<const char *>(p_payload.ptr()), p_payload.size());
	StrictJsonScanner scanner(source);
	const Error scan_error = scanner.scan();
	if (scan_error != OK) {
		return scan_error;
	}
	Ref<JSON> parser;
	parser.instantiate();
	const Error parse_error = parser->parse(source);
	if (parse_error != OK || parser->get_data().get_type() != Variant::DICTIONARY) {
		return ERR_INVALID_DATA;
	}
	r_object = parser->get_data();
	return OK;
}

Error BridgeFrameCodec::feed(const uint8_t *p_bytes, int p_size, Vector<PackedByteArray> &r_frames) {
	ERR_FAIL_COND_V(p_size < 0 || (p_size > 0 && p_bytes == nullptr), ERR_INVALID_PARAMETER);
	int offset = 0;
	while (offset < p_size) {
		if (prefix_size < 4) {
			const int copy_size = MIN(4 - prefix_size, p_size - offset);
			memcpy(prefix + prefix_size, p_bytes + offset, copy_size);
			prefix_size += copy_size;
			offset += copy_size;
			if (prefix_size < 4) {
				continue;
			}
			expected_payload_size = ((uint32_t)prefix[0] << 24) | ((uint32_t)prefix[1] << 16) | ((uint32_t)prefix[2] << 8) | prefix[3];
			if (expected_payload_size == 0 || expected_payload_size > MAX_PAYLOAD_BYTES) {
				reset();
				return ERR_INVALID_DATA;
			}
			payload.resize(expected_payload_size);
			payload_size = 0;
		}

		const int copy_size = MIN((int)(expected_payload_size - payload_size), p_size - offset);
		memcpy(payload.ptrw() + payload_size, p_bytes + offset, copy_size);
		payload_size += copy_size;
		offset += copy_size;
		if (payload_size == expected_payload_size) {
			r_frames.push_back(payload);
			prefix_size = 0;
			expected_payload_size = 0;
			payload_size = 0;
			payload.clear();
		}
	}
	return OK;
}

void BridgeFrameCodec::reset() {
	prefix_size = 0;
	expected_payload_size = 0;
	payload_size = 0;
	payload.clear();
}

Error BridgeFrameCodec::encode_json(const Dictionary &p_object, PackedByteArray &r_frame) {
	// Snapshot chunks carry both a structured payload and the exact canonical
	// JSON used for their checksum. Preserve full double precision in the outer
	// frame as well, otherwise values such as float-backed Godot properties can
	// round differently and fail the payload/payload_json equality check.
	const CharString json = JSON::stringify(p_object, "", true, true).utf8();
	if (json.length() == 0 || json.length() > (int)MAX_PAYLOAD_BYTES) {
		return ERR_INVALID_DATA;
	}
	r_frame.resize(json.length() + 4);
	uint8_t *write = r_frame.ptrw();
	write[0] = (uint8_t)(json.length() >> 24);
	write[1] = (uint8_t)(json.length() >> 16);
	write[2] = (uint8_t)(json.length() >> 8);
	write[3] = (uint8_t)json.length();
	memcpy(write + 4, json.get_data(), json.length());
	return OK;
}
