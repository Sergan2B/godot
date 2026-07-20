/**************************************************************************/
/*  editor_context_adapter.h                                              */
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

class Node;
class Object;

class EditorContextAdapter {
public:
	static constexpr int MAX_VARIANT_DEPTH = 8;
	static constexpr int MAX_CONTAINER_ITEMS = 1000;
	static constexpr int MAX_SCENE_NODES = 1000;
	static constexpr int MAX_OPEN_SCENES = 64;
	static constexpr int MAX_SELECTED_NODES = 256;
	static constexpr int MAX_INSPECTOR_PROPERTIES = 512;
	static constexpr int MAX_STRING_CHARACTERS = 16384;
	static constexpr int MAX_IDENTITY_CHARACTERS = 1024;
	static constexpr int MAX_PROJECTED_VALUE_BYTES = 65536;
	static constexpr int MAX_INSPECTOR_BYTES_PER_NODE = 262144;
	static constexpr int MAX_TOTAL_INSPECTOR_BYTES = 4194304;

	static String make_scene_id(const String &p_editor_session_id, const Node *p_scene_root);
	static Error capture(const String &p_project_id, const String &p_editor_session_id, const Dictionary &p_revisions, Dictionary &r_snapshot, bool p_full_live_context = false);

private:
	static String _make_opaque_id(const String &p_prefix, const String &p_domain, const String &p_value);
	static String _bounded_identity(const String &p_value, bool &r_truncated);
	static Variant _project_variant(const Variant &p_value, int p_depth, bool &r_truncated);
	static String _make_node_id(const String &p_editor_session_id, const String &p_scene_id, const String &p_node_path);
	static String _make_history_id(const String &p_editor_session_id, int p_native_history_id);
	static Array _capture_properties(Object *p_object, int p_limit, const String &p_scene_id, const Dictionary &p_revisions, bool &r_truncated, int &r_total_bytes);
};
