/**************************************************************************/
/*  codex_runtime_limits.h                                                */
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

class CodexRuntimeLimits {
public:
	static constexpr int TREE_NODES = 10000;
	static constexpr int TREE_DEPTH = 256;
	static constexpr int NODE_STRING_CHARACTERS = 1024;
	static constexpr int SOURCE_SCENE_PATHS = 256;
	static constexpr int CORRELATION_CHARACTERS = 128;

	static constexpr int PROPERTIES = 512;
	static constexpr int VARIANT_DEPTH = 8;
	static constexpr int CONTAINER_ITEMS = 1000;
	static constexpr int STRING_CHARACTERS = 16384;
	static constexpr int PROJECTED_VALUE_BYTES = 65536;
	static constexpr int OBJECT_BYTES = 262144;

	static constexpr int SNAPSHOT_BYTES = 16777216;
	static constexpr int SNAPSHOT_CHUNK_BYTES = 524288;
	static constexpr int SNAPSHOT_WINDOW_BYTES = 33554432;
	static constexpr int SNAPSHOT_TIMEOUT_MS = 10000;
};
