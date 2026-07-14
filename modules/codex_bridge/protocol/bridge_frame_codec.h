/**************************************************************************/
/*  bridge_frame_codec.h                                                  */
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

#include "core/templates/vector.h"
#include "core/variant/variant.h"

class BridgeJson {
public:
	static constexpr int MAX_NESTING_DEPTH = 64;
	static constexpr int MAX_CONTAINER_ENTRIES = 8192;

	static Error parse_strict_object(const PackedByteArray &p_payload, Dictionary &r_object);
};

class BridgeFrameCodec {
	uint8_t prefix[4] = {};
	int prefix_size = 0;
	uint32_t expected_payload_size = 0;
	uint32_t payload_size = 0;
	PackedByteArray payload;

public:
	static constexpr uint32_t MAX_PAYLOAD_BYTES = 1048576;

	Error feed(const uint8_t *p_bytes, int p_size, Vector<PackedByteArray> &r_frames);
	void reset();

	static Error encode_json(const Dictionary &p_object, PackedByteArray &r_frame);
};
