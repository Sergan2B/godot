/**************************************************************************/
/*  scene_debugger_object.cpp                                             */
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

#ifdef DEBUG_ENABLED

#include "scene_debugger_object.h"

#include "core/debugger/debugger_marshalls.h"
#include "core/io/marshalls.h"
#include "core/object/script_language.h"
#include "scene/debugger/codex_runtime_value_projector.h"
#include "scene/main/node.h"

namespace {

bool codex_safe_res_path(const String &p_path) {
	if (!p_path.begins_with("res://") || p_path.contains("\\") || p_path.length() > CodexRuntimeLimits::NODE_STRING_CHARACTERS) {
		return false;
	}
	const PackedStringArray components = p_path.trim_prefix("res://").split("/", false);
	for (const String &component : components) {
		if (component == "..") {
			return false;
		}
	}
	return true;
}

} // namespace

bool SceneDebuggerObject::_has_property_capacity(int p_max_properties, bool *r_truncated) const {
	if (properties.size() < p_max_properties) {
		return true;
	}
	if (r_truncated) {
		*r_truncated = true;
	}
	return false;
}

void SceneDebuggerObject::_capture(Object *p_obj, int p_max_properties, bool *r_truncated) {
	if (!p_obj) {
		return;
	}
	p_max_properties = MAX(1, p_max_properties);

	id = p_obj->get_instance_id();
	class_name = p_obj->get_class();

	if (ScriptInstance *si = p_obj->get_script_instance()) {
		// Read script instance constants and variables.
		if (!si->get_script().is_null()) {
			Script *s = si->get_script().ptr();
			_parse_script_properties(s, si, p_max_properties, r_truncated);
		}
	}

	if (Node *node = Object::cast_to<Node>(p_obj)) {
		if (_has_property_capacity(p_max_properties, r_truncated)) {
			PropertyInfo pi(Variant::STRING_NAME, "name", PROPERTY_HINT_NONE, "", PROPERTY_USAGE_NONE);
			properties.push_back(SceneDebuggerProperty(pi, node->get_name()));
		}

		// For debugging multiplayer.
		if (_has_property_capacity(p_max_properties, r_truncated)) {
			PropertyInfo pi(Variant::INT, String("Node/multiplayer_authority"), PROPERTY_HINT_NONE, "", PROPERTY_USAGE_DEFAULT | PROPERTY_USAGE_READ_ONLY);
			properties.push_back(SceneDebuggerProperty(pi, node->get_multiplayer_authority()));
		}

		// Add specialized NodePath info (if inside tree).
		if (_has_property_capacity(p_max_properties, r_truncated)) {
			if (node->is_inside_tree()) {
				PropertyInfo pi(Variant::NODE_PATH, String("Node/path"));
				properties.push_back(SceneDebuggerProperty(pi, node->get_path()));
			} else { // Can't ask for path if a node is not in tree.
				PropertyInfo pi(Variant::STRING, String("Node/path"));
				properties.push_back(SceneDebuggerProperty(pi, "[Orphan]"));
			}
		}
	} else if (Script *s = Object::cast_to<Script>(p_obj)) {
		// Add script constants (no instance).
		_parse_script_properties(s, nullptr, p_max_properties, r_truncated);
	}

	// Add base object properties.
	List<PropertyInfo> pinfo;
	p_obj->get_property_list(&pinfo, true);
	for (PropertyInfo &E : pinfo) {
		if (!(E.usage & (PROPERTY_USAGE_EDITOR | PROPERTY_USAGE_GROUP | PROPERTY_USAGE_SUBGROUP | PROPERTY_USAGE_CATEGORY))) {
			continue;
		}
		if (!_has_property_capacity(p_max_properties, r_truncated)) {
			break;
		}
		Variant value;
		bool valid = true;
		if (!(E.usage & (PROPERTY_USAGE_GROUP | PROPERTY_USAGE_SUBGROUP | PROPERTY_USAGE_CATEGORY))) {
			value = p_obj->get(E.name, &valid);
		}
		if (!valid) {
			codex_getter_failures.insert(E.name);
			if (r_truncated) {
				*r_truncated = true;
			}
		}

		if (valid && !value.is_null() && E.type == Variant::OBJECT && E.hint == PROPERTY_HINT_NODE_TYPE && E.usage & PROPERTY_USAGE_EDITOR) {
			E.hint_string = DebuggerMarshalls::parse_type_from_variant(value);
		}

		properties.push_back(SceneDebuggerProperty(E, value));
	}
}

SceneDebuggerObject::SceneDebuggerObject(Object *p_obj) {
	_capture(p_obj, 2147483647, nullptr);
}

SceneDebuggerObject::SceneDebuggerObject(Object *p_obj, int p_max_properties, bool *r_truncated) {
	_capture(p_obj, p_max_properties, r_truncated);
}

SceneDebuggerObject::SceneDebuggerObject(ObjectID p_id, int p_max_properties, bool *r_truncated) :
		SceneDebuggerObject(ObjectDB::get_instance(p_id), p_max_properties, r_truncated) {
}

void SceneDebuggerObject::_parse_script_properties(Script *p_script, ScriptInstance *p_instance, int p_max_properties, bool *r_truncated) {
	typedef HashMap<const Script *, HashSet<StringName>> ScriptMemberMap;
	typedef HashMap<const Script *, HashMap<StringName, Variant>> ScriptConstantsMap;

	Vector<const Script *> script_chain;
	script_chain.push_back(p_script);
	ScriptMemberMap members;
	if (p_instance) {
		members[p_script] = HashSet<StringName>();
		p_script->get_members(&(members[p_script]));
	}

	ScriptConstantsMap constants;
	constants[p_script] = HashMap<StringName, Variant>();
	p_script->get_constants(&(constants[p_script]));

	Ref<Script> base = p_script->get_base_script();
	while (base.is_valid()) {
		script_chain.push_back(base.ptr());
		if (p_instance) {
			members[base.ptr()] = HashSet<StringName>();
			base->get_members(&(members[base.ptr()]));
		}

		constants[base.ptr()] = HashMap<StringName, Variant>();
		base->get_constants(&(constants[base.ptr()]));

		base = base->get_base_script();
	}

	HashSet<StringName> exported_members;
	HashMap<StringName, PropertyInfo> non_exported_members;
	Vector<PropertyInfo> ordered_non_exported;

	if (p_instance) {
		List<PropertyInfo> pinfo;
		p_instance->get_property_list(&pinfo);
		for (const PropertyInfo &E : pinfo) {
			if (E.usage & (PROPERTY_USAGE_EDITOR | PROPERTY_USAGE_CATEGORY)) {
				exported_members.insert(E.name);
			} else if (!(E.usage & (PROPERTY_USAGE_GROUP | PROPERTY_USAGE_SUBGROUP))) {
				PropertyInfo pi = E;
				pi.usage |= PROPERTY_USAGE_EDITOR;
				non_exported_members.insert(E.name, pi);
				ordered_non_exported.push_back(pi);
			}
		}
	}

	auto script_member_path = [p_script](const Script *p_owner) {
		return p_owner == p_script ? String() : p_owner->get_path().get_file() + "/";
	};
	auto member_owner = [&script_chain, &members, p_script](const StringName &p_name) -> const Script * {
		for (const Script *script : script_chain) {
			const HashSet<StringName> *script_members = members.getptr(script);
			if (script_members && script_members->has(p_name)) {
				return script;
			}
		}
		return p_script;
	};
	auto append_member = [&](const StringName &p_name, PropertyInfo p_info, const Script *p_owner) -> bool {
		if (!_has_property_capacity(p_max_properties, r_truncated)) {
			return false;
		}
		Variant value;
		const bool valid = p_instance->get(p_name, value);
		p_info.name = "Members/" + script_member_path(p_owner) + p_name;
		if (valid && !value.is_null() && value.get_type() == Variant::OBJECT) {
			p_info.type = value.get_type();
			p_info.hint = PROPERTY_HINT_OBJECT_ID;
			p_info.hint_string = DebuggerMarshalls::parse_type_from_variant(value);
		} else if (p_info.type == Variant::NIL && valid) {
			p_info.type = value.get_type();
		}
		if (!valid) {
			codex_getter_failures.insert(p_info.name);
			if (r_truncated) {
				*r_truncated = true;
			}
		}
		properties.push_back(SceneDebuggerProperty(p_info, value));
		return true;
	};

	HashSet<StringName> emitted_members;
	for (const PropertyInfo &property : ordered_non_exported) {
		if (String(property.name).begins_with("@")) {
			continue;
		}
		if (!append_member(property.name, property, member_owner(property.name))) {
			return;
		}
		emitted_members.insert(property.name);
	}

	// Some languages expose members outside ScriptInstance::get_property_list().
	// Normalize those unordered sets by inheritance and name.
	for (const Script *script : script_chain) {
		const HashSet<StringName> *script_members = members.getptr(script);
		if (!script_members) {
			continue;
		}
		Vector<StringName> ordered_members;
		for (const StringName &member : *script_members) {
			ordered_members.push_back(member);
		}
		ordered_members.sort();
		for (const StringName &member : ordered_members) {
			if (exported_members.has(member) || emitted_members.has(member) || String(member).begins_with("@")) {
				continue;
			}
			PropertyInfo info;
			if (const PropertyInfo *known = non_exported_members.getptr(member)) {
				info = *known;
			}
			if (!append_member(member, info, script)) {
				return;
			}
			emitted_members.insert(member);
		}
	}

	// Constants are not ordered by the Script API; use derived-to-base and
	// lexical name order to keep repeated captures byte-identical.
	for (const Script *script : script_chain) {
		HashMap<StringName, Variant> *script_constants = constants.getptr(script);
		if (!script_constants) {
			continue;
		}
		Vector<StringName> ordered_constants;
		for (const KeyValue<StringName, Variant> &constant : *script_constants) {
			ordered_constants.push_back(constant.key);
		}
		ordered_constants.sort();
		for (const StringName &constant_name : ordered_constants) {
			if (!_has_property_capacity(p_max_properties, r_truncated)) {
				return;
			}
			const Variant value = (*script_constants)[constant_name];
			const String constant_path = script_member_path(script);
			if (!value.is_null() && value.get_type() == Variant::OBJECT) {
				PropertyInfo pi(value.get_type(), "Constants/" + constant_path + constant_name, PROPERTY_HINT_OBJECT_ID, DebuggerMarshalls::parse_type_from_variant(value), PROPERTY_USAGE_DEFAULT | PROPERTY_USAGE_READ_ONLY);
				properties.push_back(SceneDebuggerProperty(pi, value));
			} else {
				PropertyInfo pi(value.get_type(), "Constants/" + constant_path + constant_name);
				pi.usage |= PROPERTY_USAGE_READ_ONLY;
				properties.push_back(SceneDebuggerProperty(pi, value));
			}
		}
	}
}

void SceneDebuggerObject::serialize(Array &r_arr, int p_max_size, int p_max_properties, int p_max_total_size, bool *r_truncated) {
	Array send_props;
	int total_size = 0;
	bool truncated = false;
	for (SceneDebuggerProperty &property : properties) {
		if (send_props.size() >= p_max_properties) {
			truncated = true;
			break;
		}
		const PropertyInfo &pi = property.first;
		Variant &var = property.second;

		Ref<Resource> res = var;

		Array prop = { pi.name, pi.type };
		PropertyHint hint = pi.hint;
		String hint_string = pi.hint_string;
		if (res.is_valid() && !res->get_path().is_empty()) {
			// HACK: Overwrite `PropertyInfo` with the current runtime type.
			// This allows untyped variables to be displayed correctly.
			prop[1] = Variant::OBJECT;

			var = res->get_path();
		} else { //only send information that can be sent..
			int len = 0; //test how big is this to encode
			encode_variant(var, nullptr, len);
			if (len > p_max_size) { //limit to max size
				hint = PROPERTY_HINT_OBJECT_TOO_BIG;
				hint_string = "";
				var = Variant();
				truncated = true;
			}
		}
		prop.push_back(hint);
		prop.push_back(hint_string);
		prop.push_back(pi.usage);
		prop.push_back(var);
		int prop_size = 0;
		encode_variant(prop, nullptr, prop_size);
		if (prop_size > p_max_total_size - total_size) {
			truncated = true;
			break;
		}
		total_size += prop_size;
		send_props.push_back(prop);
	}
	if (r_truncated) {
		*r_truncated = truncated;
	}
	r_arr.push_back(uint64_t(id));
	r_arr.push_back(class_name);
	r_arr.push_back(send_props);
}

void SceneDebuggerObject::serialize_codex(Array &r_arr, int p_max_size, int p_max_properties, int p_max_total_size, bool *r_truncated) {
	Array send_props;
	int total_size = 0;
	bool truncated = false;
	for (const SceneDebuggerProperty &property : properties) {
		if (send_props.size() >= p_max_properties) {
			truncated = true;
			break;
		}
		const PropertyInfo &pi = property.first;
		const Variant &source = property.second;
		Array prop = { pi.name, pi.type, pi.hint, pi.hint_string, pi.usage };
		bool already_projected = true;
		Variant projected;
		if (codex_getter_failures.has(pi.name)) {
			projected = CodexRuntimeValueProjector::omitted_typed(pi.type, "getter_failed");
			truncated = true;
		} else if (source.get_type() == Variant::OBJECT) {
			Object *object = source;
			Resource *resource = Object::cast_to<Resource>(object);
			if (resource) {
				projected = CodexRuntimeValueProjector::project_typed(source);
				truncated = truncated || (bool)Dictionary(projected)["truncated"];
			} else if (object) {
				projected = source;
				already_projected = false;
			} else {
				projected = CodexRuntimeValueProjector::project_typed(source);
			}
		} else {
			projected = CodexRuntimeValueProjector::project_typed(source);
			truncated = truncated || (bool)Dictionary(projected)["truncated"];
		}
		int value_size = 0;
		encode_variant(projected, nullptr, value_size);
		if (value_size > p_max_size) {
			projected = CodexRuntimeValueProjector::omitted_typed(pi.type, "max_encoded_bytes");
			already_projected = true;
			truncated = true;
		}
		prop.push_back(projected);
		prop.push_back(already_projected);
		int prop_size = 0;
		encode_variant(prop, nullptr, prop_size);
		if (prop_size > p_max_total_size - total_size) {
			truncated = true;
			break;
		}
		total_size += prop_size;
		send_props.push_back(prop);
	}
	if (r_truncated) {
		*r_truncated = truncated;
	}
	r_arr.push_back(uint64_t(id));
	r_arr.push_back(class_name);
	r_arr.push_back(send_props);
}

#define CHECK_TYPE(p_what, p_type) ERR_FAIL_COND(p_what.get_type() != Variant::p_type)

void SceneDebuggerObject::deserialize(const Array &p_arr) {
	ERR_FAIL_COND(p_arr.size() < 3);
	CHECK_TYPE(p_arr[0], INT);
	CHECK_TYPE(p_arr[1], STRING);
	CHECK_TYPE(p_arr[2], ARRAY);

	deserialize(uint64_t(p_arr[0]), p_arr[1], p_arr[2]);
}

void SceneDebuggerObject::deserialize(uint64_t p_id, const String &p_class_name, const Array &p_props) {
	id = p_id;
	class_name = p_class_name;

	for (int i = 0; i < p_props.size(); i++) {
		CHECK_TYPE(p_props[i], ARRAY);
		Array prop = p_props[i];

		ERR_FAIL_COND(prop.size() != 6);
		CHECK_TYPE(prop[0], STRING);
		CHECK_TYPE(prop[1], INT);
		CHECK_TYPE(prop[2], INT);
		CHECK_TYPE(prop[3], STRING);
		CHECK_TYPE(prop[4], INT);

		PropertyInfo pinfo;
		pinfo.name = prop[0];
		pinfo.type = Variant::Type(int(prop[1]));
		pinfo.hint = PropertyHint(int(prop[2]));
		pinfo.hint_string = prop[3];
		pinfo.usage = PropertyUsageFlags(int(prop[4]));
		Variant var = prop[5];

		if (pinfo.type == Variant::OBJECT) {
			if (var.is_zero()) {
				var = Ref<Resource>();
			} else if (var.get_type() == Variant::OBJECT) {
				if (((Object *)var)->is_class("EncodedObjectAsID")) {
					var = Object::cast_to<EncodedObjectAsID>(var)->get_object_id();
					pinfo.type = var.get_type();
					pinfo.hint = PROPERTY_HINT_OBJECT_ID;
					if (pinfo.hint_string.is_empty()) {
						pinfo.hint_string = "Object";
					}
				}
			}
		}
		properties.push_back(SceneDebuggerProperty(pinfo, var));
	}
}

SceneDebuggerTree::SceneDebuggerTree(Node *p_root, int p_max_nodes, int p_max_depth) {
	ERR_FAIL_NULL(p_root);
	p_max_nodes = CLAMP(p_max_nodes, 1, CodexRuntimeLimits::TREE_NODES);
	p_max_depth = CLAMP(p_max_depth, 1, CodexRuntimeLimits::TREE_DEPTH);
	// Flatten a deterministic depth-first prefix without queuing or serializing
	// nodes outside the negotiated runtime budgets.
	struct PendingNode {
		Node *node = nullptr;
		int depth = 0;
		int parent_index = -1;
		int next_child = 0;
		int captured_index = -1;
	};
	Vector<PendingNode> stack;
	stack.push_back({ p_root, 0, -1 });
	Vector<RemoteNode> captured;
	const StringName &is_visible_sn = SNAME("is_visible");
	const StringName &is_visible_in_tree_sn = SNAME("is_visible_in_tree");
	while (!stack.is_empty() && captured.size() < p_max_nodes) {
		const PendingNode pending = stack[stack.size() - 1];
		Node *n = pending.node;
		if (pending.captured_index >= 0) {
			const int child_count = n->get_child_count();
			if (pending.depth + 1 >= p_max_depth) {
				if (child_count > 0) {
					truncated = true;
				}
				stack.resize(stack.size() - 1);
				continue;
			}
			if (pending.next_child < child_count) {
				stack.write[stack.size() - 1].next_child++;
				stack.push_back({ n->get_child(pending.next_child), pending.depth + 1, pending.captured_index, 0, -1 });
			} else {
				stack.resize(stack.size() - 1);
			}
			continue;
		}
		const int captured_index = captured.size();
		if (pending.parent_index >= 0) {
			captured.write[pending.parent_index].child_count++;
		}

		int view_flags = 0;
		if (pending.depth == 0) {
			// Prevent root window visibility from being changed.
		} else if (n->has_method(is_visible_sn)) {
			const Variant visible = n->call(is_visible_sn);
			if (visible.get_type() == Variant::BOOL) {
				view_flags = RemoteNode::VIEW_HAS_VISIBLE_METHOD;
				view_flags |= uint8_t(visible) * RemoteNode::VIEW_VISIBLE;
			}
			if (n->has_method(is_visible_in_tree_sn)) {
				const Variant visible_in_tree = n->call(is_visible_in_tree_sn);
				if (visible_in_tree.get_type() == Variant::BOOL) {
					view_flags |= uint8_t(visible_in_tree) * RemoteNode::VIEW_VISIBLE_IN_TREE;
				}
			}
		}

		String class_name;
		ScriptInstance *script_instance = n->get_script_instance();
		if (script_instance) {
			Ref<Script> script = script_instance->get_script();
			if (script.is_valid()) {
				class_name = script->get_global_name();

				if (class_name.is_empty() && codex_safe_res_path(script->get_path().get_slice("::", 0))) {
					// If there is no class_name in this script we just take the script path.
					class_name = script->get_path();
				}
			}
		}
		String name = n->get_name();
		if (name.length() > CodexRuntimeLimits::NODE_STRING_CHARACTERS) {
			name = name.left(CodexRuntimeLimits::NODE_STRING_CHARACTERS);
			truncated = true;
		}
		String type_name = class_name.is_empty() ? n->get_class() : class_name;
		if (type_name.length() > CodexRuntimeLimits::NODE_STRING_CHARACTERS) {
			type_name = type_name.left(CodexRuntimeLimits::NODE_STRING_CHARACTERS);
			truncated = true;
		}
		String scene_file_path = n->get_scene_file_path().get_slice("::", 0);
		if (!scene_file_path.is_empty() && !codex_safe_res_path(scene_file_path)) {
			scene_file_path.clear();
			truncated = true;
		}
		captured.push_back(RemoteNode(0, name, type_name, n->get_instance_id(), scene_file_path, view_flags));
		stack.write[stack.size() - 1].captured_index = captured_index;
	}
	truncated = truncated || !stack.is_empty();
	for (const RemoteNode &node : captured) {
		nodes.push_back(node);
	}
}

void SceneDebuggerTree::serialize(Array &p_arr) {
	for (const RemoteNode &n : nodes) {
		p_arr.push_back(n.child_count);
		p_arr.push_back(n.name);
		p_arr.push_back(n.type_name);
		p_arr.push_back(n.id);
		p_arr.push_back(n.scene_file_path);
		p_arr.push_back(n.view_flags);
	}
}

void SceneDebuggerTree::deserialize(const Array &p_arr) {
	int idx = 0;
	while (p_arr.size() > idx) {
		ERR_FAIL_COND(p_arr.size() < 6);
		CHECK_TYPE(p_arr[idx], INT); // child_count.
		CHECK_TYPE(p_arr[idx + 1], STRING); // name.
		CHECK_TYPE(p_arr[idx + 2], STRING); // type_name.
		CHECK_TYPE(p_arr[idx + 3], INT); // id.
		CHECK_TYPE(p_arr[idx + 4], STRING); // scene_file_path.
		CHECK_TYPE(p_arr[idx + 5], INT); // view_flags.
		nodes.push_back(RemoteNode(p_arr[idx], p_arr[idx + 1], p_arr[idx + 2], p_arr[idx + 3], p_arr[idx + 4], p_arr[idx + 5]));
		idx += 6;
	}
}

#undef CHECK_TYPE

#endif // DEBUG_ENABLED
