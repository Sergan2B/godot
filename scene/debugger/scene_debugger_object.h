/**************************************************************************/
/*  scene_debugger_object.h                                               */
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

#ifdef DEBUG_ENABLED

#include "core/object/object.h"
#include "core/string/ustring.h"
#include "core/templates/hash_set.h"
#include "core/templates/list.h"
#include "core/templates/pair.h"
#include "scene/debugger/codex_runtime_limits.h"

class Node;
class Script;

class SceneDebuggerObject {
private:
	HashSet<StringName> codex_getter_failures;

	bool _has_property_capacity(int p_max_properties, bool *r_truncated) const;
	void _capture(Object *p_obj, int p_max_properties, bool *r_truncated);
	void _parse_script_properties(Script *p_script, ScriptInstance *p_instance, int p_max_properties, bool *r_truncated);

public:
	typedef Pair<PropertyInfo, Variant> SceneDebuggerProperty;
	ObjectID id;
	String class_name;
	List<SceneDebuggerProperty> properties;

	SceneDebuggerObject(ObjectID p_id);
	SceneDebuggerObject(Object *p_obj);
	SceneDebuggerObject(ObjectID p_id, int p_max_properties, bool *r_truncated);
	SceneDebuggerObject(Object *p_obj, int p_max_properties, bool *r_truncated);
	SceneDebuggerObject() {}

	void serialize(Array &r_arr, int p_max_size = 1 << 20, int p_max_properties = 2147483647, int p_max_total_size = 2147483647, bool *r_truncated = nullptr);
	void serialize_codex(Array &r_arr, int p_max_size, int p_max_properties, int p_max_total_size, bool *r_truncated = nullptr);
	void deserialize(const Array &p_arr);
	void deserialize(uint64_t p_id, const String &p_class_name, const Array &p_props);
};

class SceneDebuggerTree {
public:
	struct RemoteNode {
		int child_count = 0;
		String name;
		String type_name;
		ObjectID id;
		String scene_file_path;
		uint8_t view_flags = 0;

		enum ViewFlags {
			VIEW_HAS_VISIBLE_METHOD = 1 << 1,
			VIEW_VISIBLE = 1 << 2,
			VIEW_VISIBLE_IN_TREE = 1 << 3,
		};

		RemoteNode(int p_child, const String &p_name, const String &p_type, ObjectID p_id, const String p_scene_file_path, int p_view_flags) {
			child_count = p_child;
			name = p_name;
			type_name = p_type;
			id = p_id;

			scene_file_path = p_scene_file_path;
			view_flags = p_view_flags;
		}

		RemoteNode() {}
	};

	List<RemoteNode> nodes;
	bool truncated = false;

	void serialize(Array &r_arr);
	void deserialize(const Array &p_arr);
	SceneDebuggerTree(Node *p_root, int p_max_nodes = CodexRuntimeLimits::TREE_NODES, int p_max_depth = CodexRuntimeLimits::TREE_DEPTH);
	SceneDebuggerTree() {}
};
#endif // DEBUG_ENABLED
