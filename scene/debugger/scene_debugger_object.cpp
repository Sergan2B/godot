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
#include "core/io/json.h"
#include "core/io/marshalls.h"
#include "core/object/script_language.h"
#include "scene/main/node.h"

namespace {

struct CodexProjectionContext {
	HashMap<const void *, String> arrays;
	HashMap<const void *, String> dictionaries;
	uint64_t next_reference = 1;
};

static Dictionary codex_omitted(const String &p_type, const String &p_reason) {
	Dictionary omitted;
	omitted["type"] = p_type;
	omitted["omitted_reason"] = p_reason;
	return omitted;
}

static Variant codex_project_raw(const Variant &p_value, int p_depth, CodexProjectionContext &r_context, bool &r_truncated) {
	const String type = Variant::get_type_name(p_value.get_type());
	if (p_depth > 8) {
		r_truncated = true;
		return codex_omitted(type, "max_depth");
	}
	switch (p_value.get_type()) {
		case Variant::NIL:
		case Variant::BOOL:
		case Variant::INT:
		case Variant::FLOAT:
			return p_value;
		case Variant::STRING:
		case Variant::STRING_NAME:
		case Variant::NODE_PATH: {
			String value = p_value.stringify();
			if (p_value.get_type() == Variant::STRING) {
				value = p_value;
			} else if (p_value.get_type() == Variant::STRING_NAME) {
				value = String(StringName(p_value));
			} else {
				value = String(NodePath(p_value));
			}
			if (value.length() > 16384) {
				value = value.left(16384);
				r_truncated = true;
			}
			return value;
		}
		case Variant::ARRAY: {
			const Array source = p_value;
			const void *identity = source.id();
			if (const String *reference = r_context.arrays.getptr(identity)) {
				Dictionary result;
				result["type"] = "array";
				result["reference_id"] = *reference;
				result["reference"] = true;
				return result;
			}
			const String reference = "ref:" + String::num_uint64(r_context.next_reference++);
			r_context.arrays.insert(identity, reference);
			Array items;
			const int count = MIN(source.size(), 1000);
			for (int index = 0; index < count; index++) {
				items.push_back(codex_project_raw(source[index], p_depth + 1, r_context, r_truncated));
			}
			Dictionary result;
			result["type"] = "array";
			result["reference_id"] = reference;
			result["items"] = items;
			result["size"] = source.size();
			if (source.size() > count) {
				result["omitted_count"] = source.size() - count;
				r_truncated = true;
			}
			return result;
		}
		case Variant::DICTIONARY: {
			const Dictionary source = p_value;
			const void *identity = source.id();
			if (const String *reference = r_context.dictionaries.getptr(identity)) {
				Dictionary result;
				result["type"] = "dictionary";
				result["reference_id"] = *reference;
				result["reference"] = true;
				return result;
			}
			const String reference = "ref:" + String::num_uint64(r_context.next_reference++);
			r_context.dictionaries.insert(identity, reference);
			Array keys = source.keys();
			keys.sort();
			Array entries;
			const int count = MIN(keys.size(), 1000);
			for (int index = 0; index < count; index++) {
				String key = keys[index].stringify();
				if (key.length() > 16384) {
					key = key.left(16384);
					r_truncated = true;
				}
				Dictionary entry;
				entry["key"] = key;
				entry["value"] = codex_project_raw(source[keys[index]], p_depth + 1, r_context, r_truncated);
				entries.push_back(entry);
			}
			Dictionary result;
			result["type"] = "dictionary";
			result["reference_id"] = reference;
			result["entries"] = entries;
			result["size"] = keys.size();
			if (keys.size() > count) {
				result["omitted_count"] = keys.size() - count;
				r_truncated = true;
			}
			return result;
		}
		case Variant::OBJECT:
		case Variant::RID:
		case Variant::CALLABLE:
		case Variant::SIGNAL:
			r_truncated = true;
			return codex_omitted(type, "unsupported_handle");
		case Variant::PACKED_BYTE_ARRAY:
		case Variant::PACKED_INT32_ARRAY:
		case Variant::PACKED_INT64_ARRAY:
		case Variant::PACKED_FLOAT32_ARRAY:
		case Variant::PACKED_FLOAT64_ARRAY:
		case Variant::PACKED_STRING_ARRAY:
		case Variant::PACKED_VECTOR2_ARRAY:
		case Variant::PACKED_VECTOR3_ARRAY:
		case Variant::PACKED_COLOR_ARRAY:
		case Variant::PACKED_VECTOR4_ARRAY:
			r_truncated = true;
			return codex_omitted(type, "packed_array_omitted");
		default:
			return p_value.stringify().left(16384);
	}
}

static Dictionary codex_project_typed(const Variant &p_value, bool &r_truncated) {
	CodexProjectionContext context;
	Dictionary result;
	result["type"] = Variant::get_type_name(p_value.get_type());
	result["value"] = codex_project_raw(p_value, 0, context, r_truncated);
	result["truncated"] = r_truncated;
	if (JSON::stringify(result, "", true, true).utf8().length() > 65536) {
		result["value"] = codex_omitted(Variant::get_type_name(p_value.get_type()), "max_encoded_bytes");
		result["truncated"] = true;
		r_truncated = true;
	}
	return result;
}

} // namespace

SceneDebuggerObject::SceneDebuggerObject(Object *p_obj) {
	if (!p_obj) {
		return;
	}

	id = p_obj->get_instance_id();
	class_name = p_obj->get_class();

	if (ScriptInstance *si = p_obj->get_script_instance()) {
		// Read script instance constants and variables.
		if (!si->get_script().is_null()) {
			Script *s = si->get_script().ptr();
			_parse_script_properties(s, si);
		}
	}

	if (Node *node = Object::cast_to<Node>(p_obj)) {
		{
			PropertyInfo pi(Variant::STRING_NAME, "name", PROPERTY_HINT_NONE, "", PROPERTY_USAGE_NONE);
			properties.push_back(SceneDebuggerProperty(pi, node->get_name()));
		}

		// For debugging multiplayer.
		{
			PropertyInfo pi(Variant::INT, String("Node/multiplayer_authority"), PROPERTY_HINT_NONE, "", PROPERTY_USAGE_DEFAULT | PROPERTY_USAGE_READ_ONLY);
			properties.push_back(SceneDebuggerProperty(pi, node->get_multiplayer_authority()));
		}

		// Add specialized NodePath info (if inside tree).
		if (node->is_inside_tree()) {
			PropertyInfo pi(Variant::NODE_PATH, String("Node/path"));
			properties.push_back(SceneDebuggerProperty(pi, node->get_path()));
		} else { // Can't ask for path if a node is not in tree.
			PropertyInfo pi(Variant::STRING, String("Node/path"));
			properties.push_back(SceneDebuggerProperty(pi, "[Orphan]"));
		}
	} else if (Script *s = Object::cast_to<Script>(p_obj)) {
		// Add script constants (no instance).
		_parse_script_properties(s, nullptr);
	}

	// Add base object properties.
	List<PropertyInfo> pinfo;
	p_obj->get_property_list(&pinfo, true);
	for (PropertyInfo &E : pinfo) {
		const Variant &m = p_obj->get(E.name);

		if (!m.is_null() && E.type == Variant::OBJECT && E.hint == PROPERTY_HINT_NODE_TYPE && E.usage & PROPERTY_USAGE_EDITOR) {
			E.hint_string = DebuggerMarshalls::parse_type_from_variant(m);
		}

		if (E.usage & (PROPERTY_USAGE_EDITOR | PROPERTY_USAGE_GROUP | PROPERTY_USAGE_SUBGROUP | PROPERTY_USAGE_CATEGORY)) {
			properties.push_back(SceneDebuggerProperty(E, m));
		}
	}
}

void SceneDebuggerObject::_parse_script_properties(Script *p_script, ScriptInstance *p_instance) {
	typedef HashMap<const Script *, HashSet<StringName>> ScriptMemberMap;
	typedef HashMap<const Script *, HashMap<StringName, Variant>> ScriptConstantsMap;

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
		if (p_instance) {
			members[base.ptr()] = HashSet<StringName>();
			base->get_members(&(members[base.ptr()]));
		}

		constants[base.ptr()] = HashMap<StringName, Variant>();
		base->get_constants(&(constants[base.ptr()]));

		base = base->get_base_script();
	}

	HashSet<String> exported_members;
	HashMap<String, PropertyInfo> non_exported_members;

	if (p_instance) {
		List<PropertyInfo> pinfo;
		p_instance->get_property_list(&pinfo);
		for (const PropertyInfo &E : pinfo) {
			if (E.usage & (PROPERTY_USAGE_EDITOR | PROPERTY_USAGE_CATEGORY)) {
				exported_members.insert(E.name);
			} else {
				PropertyInfo pi = E;
				pi.usage |= PROPERTY_USAGE_EDITOR;
				non_exported_members.insert(E.name, pi);
			}
		}
	}

	// Members
	for (KeyValue<const Script *, HashSet<StringName>> sm : members) {
		for (const StringName &E : sm.value) {
			if (exported_members.has(E)) {
				continue; // Exported variables already show up in the inspector.
			}
			if (String(E).begins_with("@")) {
				continue; // Skip groups.
			}

			Variant m;
			if (p_instance->get(E, m)) {
				const String script_path = sm.key == p_script ? "" : sm.key->get_path().get_file() + "/";
				if (!m.is_null() && m.get_type() == Variant::OBJECT) {
					PropertyInfo pi(m.get_type(), "Members/" + script_path + E, PROPERTY_HINT_OBJECT_ID, DebuggerMarshalls::parse_type_from_variant(m));
					properties.push_back(SceneDebuggerProperty(pi, m));
				} else {
					PropertyInfo pi;
					const PropertyInfo *pi_ptr = non_exported_members.getptr(E);
					if (pi_ptr == nullptr) {
						pi.type = m.get_type();
					} else {
						pi = *pi_ptr;
					}
					pi.name = "Members/" + script_path + E;

					properties.push_back(SceneDebuggerProperty(pi, m));
				}
			}
		}
	}
	// Constants
	for (KeyValue<const Script *, HashMap<StringName, Variant>> &sc : constants) {
		for (const KeyValue<StringName, Variant> &E : sc.value) {
			const String script_path = sc.key == p_script ? "" : sc.key->get_path().get_file() + "/";
			if (!E.value.is_null() && E.value.get_type() == Variant::OBJECT) {
				PropertyInfo pi(E.value.get_type(), "Constants/" + E.key, PROPERTY_HINT_OBJECT_ID, DebuggerMarshalls::parse_type_from_variant(E.value), PROPERTY_USAGE_DEFAULT | PROPERTY_USAGE_READ_ONLY);
				properties.push_back(SceneDebuggerProperty(pi, E.value));
			} else {
				PropertyInfo pi(E.value.get_type(), "Constants/" + script_path + E.key);
				pi.usage |= PROPERTY_USAGE_READ_ONLY;

				properties.push_back(SceneDebuggerProperty(pi, E.value));
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
		if (source.get_type() == Variant::OBJECT) {
			Object *object = source;
			Resource *resource = Object::cast_to<Resource>(object);
			if (resource && resource->get_path().begins_with("res://")) {
				Dictionary value;
				value["type"] = "resource";
				Dictionary reference;
				reference["path"] = resource->get_path().get_slice("::", 0).left(1024);
				value["value"] = reference;
				value["truncated"] = false;
				projected = value;
			} else if (object) {
				projected = source;
				already_projected = false;
			} else {
				projected = codex_project_typed(source, truncated);
			}
		} else {
			bool value_truncated = false;
			projected = codex_project_typed(source, value_truncated);
			truncated = truncated || value_truncated;
		}
		int value_size = 0;
		encode_variant(projected, nullptr, value_size);
		if (value_size > p_max_size) {
			projected = codex_omitted(Variant::get_type_name(source.get_type()), "max_encoded_bytes");
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
	p_max_nodes = CLAMP(p_max_nodes, 1, 10000);
	p_max_depth = CLAMP(p_max_depth, 1, 256);
	// Flatten a deterministic depth-first prefix without queuing or serializing
	// nodes outside the negotiated runtime budgets.
	struct PendingNode {
		Node *node = nullptr;
		int depth = 0;
		int parent_index = -1;
	};
	Vector<PendingNode> stack;
	stack.push_back({ p_root, 0, -1 });
	Vector<RemoteNode> captured;
	const StringName &is_visible_sn = SNAME("is_visible");
	const StringName &is_visible_in_tree_sn = SNAME("is_visible_in_tree");
	while (!stack.is_empty() && captured.size() < p_max_nodes) {
		const PendingNode pending = stack[stack.size() - 1];
		stack.resize(stack.size() - 1);
		Node *n = pending.node;
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

				if (class_name.is_empty()) {
					// If there is no class_name in this script we just take the script path.
					class_name = script->get_path();
				}
			}
		}
		captured.push_back(RemoteNode(0, n->get_name(), class_name.is_empty() ? n->get_class() : class_name, n->get_instance_id(), n->get_scene_file_path(), view_flags));
		const int count = n->get_child_count();
		if (pending.depth + 1 < p_max_depth) {
			for (int i = count - 1; i >= 0; i--) {
				stack.push_back({ n->get_child(i), pending.depth + 1, captured_index });
			}
		} else if (count > 0) {
			truncated = true;
		}
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
