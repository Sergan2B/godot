/**************************************************************************/
/*  codex_runtime_value_projector.h                                       */
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
#include "core/variant/variant.h"
#include "scene/debugger/codex_runtime_limits.h"

class CodexRuntimeValueProjector {
private:
	struct ProjectionContext {
		HashMap<const void *, String> array_references;
		HashMap<const void *, String> dictionary_references;
		uint64_t next_reference = 1;
	};

	struct DictionaryKey {
		Variant value;
		String label;
		int original_index = 0;

		bool operator<(const DictionaryKey &p_other) const {
			return label == p_other.label ? original_index < p_other.original_index : label < p_other.label;
		}
	};

	static Variant _project_raw(const Variant &p_value, bool &r_truncated, int p_depth, ProjectionContext &r_context);
	static Dictionary _omitted(const String &p_type, const String &p_reason, int64_t p_size_hint = -1);
	static String _next_reference(ProjectionContext &r_context);
	static String _dictionary_key_label(const Variant &p_key, bool &r_truncated);
	static bool _safe_resource_path(const String &p_path);
	static bool _looks_like_absolute_path(const String &p_value);

public:
	static constexpr int MAX_DEPTH = CodexRuntimeLimits::VARIANT_DEPTH;
	static constexpr int MAX_CONTAINER_ITEMS = CodexRuntimeLimits::CONTAINER_ITEMS;
	static constexpr int MAX_STRING_CHARACTERS = CodexRuntimeLimits::STRING_CHARACTERS;
	static constexpr int MAX_ENCODED_BYTES = CodexRuntimeLimits::PROJECTED_VALUE_BYTES;

	static Variant project_raw(const Variant &p_value, bool &r_truncated, int p_depth = 0);
	static Dictionary project_typed(const Variant &p_value);
	static Dictionary omitted_typed(Variant::Type p_type, const String &p_reason);
	static String type_token(Variant::Type p_type);
};
