/**************************************************************************/
/*  script_semantic_adapter.h                                             */
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

class ScriptSemanticAdapter {
public:
	static constexpr uint32_t MAX_DOCUMENTS = 250000;
	static constexpr uint32_t MAX_SYMBOLS = 2000000;
	static constexpr uint32_t MAX_RELATIONS = 4000000;
	static constexpr uint32_t MAX_DIAGNOSTICS = 2000000;
	static constexpr uint32_t MAX_SYMBOLS_PER_DOCUMENT = 65536;
	static constexpr uint32_t MAX_RELATIONS_PER_DOCUMENT = 262144;
	static constexpr uint32_t MAX_DIAGNOSTICS_PER_DOCUMENT = 4096;
	static constexpr uint32_t MAX_PATH_BYTES = 1024;
	static constexpr uint32_t MAX_NAME_BYTES = 1024;
	static constexpr uint32_t MAX_SIGNATURE_BYTES = 4096;
	static constexpr uint32_t MAX_DIAGNOSTIC_MESSAGE_BYTES = 2048;
	static constexpr uint64_t MAX_SOURCE_BYTES = 8 * 1024 * 1024;
	static constexpr uint64_t MAX_SAFE_INTEGER = 9007199254740991ULL;

	struct DocumentProjection {
		Dictionary bundle;
		String facts_checksum;
		uint64_t source_modified_time = 0;
		uint64_t source_bytes = 0;
	};

private:
	friend struct ScriptSemanticAdapterTestAccess;

	static bool _is_valid_script_path(const String &p_path);
	static bool _is_valid_script_ref(const Dictionary &p_script_ref, const String &p_path);
	static String _resource_id(const Dictionary &p_script_ref, const String &p_path, const String &p_content_sha256);
	static String _named_symbol_id(const String &p_script_id, const String &p_language, const String &p_kind, const String &p_qualified_key);
	static String _content_symbol_id(const String &p_script_id, const String &p_language, const String &p_content_sha256, const String &p_owner_qualified_key, const String &p_kind, uint64_t p_start_byte, uint64_t p_end_byte);
	static String _diagnostic_id(const String &p_script_id, const String &p_language, const String &p_content_sha256, const String &p_severity, const String &p_code, uint64_t p_start_byte, uint64_t p_end_byte, const String &p_message);
	static Error _project_source(const String &p_source, const String &p_path, const Dictionary &p_script_ref, uint64_t p_resource_revision, uint64_t p_script_graph_revision, DocumentProjection &r_projection);

public:
	static bool is_gdscript_available();
	static Array make_adapter_statuses();
	static Error project_saved_document(const String &p_path, const Dictionary &p_script_ref, uint64_t p_resource_revision, uint64_t p_script_graph_revision, DocumentProjection &r_projection);
};
