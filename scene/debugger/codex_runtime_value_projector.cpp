/**************************************************************************/
/*  codex_runtime_value_projector.cpp                                     */
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

#include "codex_runtime_value_projector.h"

#include "core/io/json.h"
#include "core/io/resource.h"
#include "core/object/object.h"

Dictionary CodexRuntimeValueProjector::_omitted(const String &p_type, const String &p_reason, int64_t p_size_hint) {
	Dictionary omitted;
	omitted["type"] = p_type;
	omitted["opaque"] = true;
	omitted["omitted_reason"] = p_reason;
	if (p_size_hint >= 0) {
		omitted["size_hint"] = p_size_hint;
	}
	return omitted;
}

String CodexRuntimeValueProjector::_next_reference(ProjectionContext &r_context) {
	return "ref:" + String::num_uint64(r_context.next_reference++);
}

bool CodexRuntimeValueProjector::_safe_resource_path(const String &p_path) {
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

bool CodexRuntimeValueProjector::_looks_like_absolute_path(const String &p_value) {
	if (p_value.begins_with("/") || p_value.begins_with("file://")) {
		return true;
	}
	return p_value.length() >= 3 && p_value[1] == ':' && (p_value[2] == '/' || p_value[2] == '\\') && ((p_value[0] >= 'A' && p_value[0] <= 'Z') || (p_value[0] >= 'a' && p_value[0] <= 'z'));
}

String CodexRuntimeValueProjector::_dictionary_key_label(const Variant &p_key, bool &r_truncated) {
	String label;
	switch (p_key.get_type()) {
		case Variant::STRING:
			if (_looks_like_absolute_path(p_key)) {
				label = "<unsafe_string>";
				r_truncated = true;
			} else {
				label = p_key;
			}
			break;
		case Variant::STRING_NAME:
			label = String(StringName(p_key));
			if (_looks_like_absolute_path(label)) {
				label = "<unsafe_string_name>";
				r_truncated = true;
			}
			break;
		case Variant::NIL:
		case Variant::BOOL:
		case Variant::INT:
		case Variant::FLOAT:
			label = type_token(p_key.get_type()) + ":" + p_key.stringify();
			break;
		default:
			label = "<" + type_token(p_key.get_type()) + ">";
			r_truncated = true;
			break;
	}
	if (label.length() > MAX_STRING_CHARACTERS) {
		label = label.left(MAX_STRING_CHARACTERS);
		r_truncated = true;
	}
	return label;
}

String CodexRuntimeValueProjector::type_token(Variant::Type p_type) {
	switch (p_type) {
		case Variant::NIL:
			return "nil";
		case Variant::BOOL:
			return "bool";
		case Variant::INT:
			return "int";
		case Variant::FLOAT:
			return "float";
		case Variant::STRING:
			return "string";
		case Variant::STRING_NAME:
			return "string_name";
		case Variant::NODE_PATH:
			return "node_path";
		case Variant::VECTOR2:
			return "vector2";
		case Variant::VECTOR2I:
			return "vector2i";
		case Variant::VECTOR3:
			return "vector3";
		case Variant::VECTOR3I:
			return "vector3i";
		case Variant::VECTOR4:
			return "vector4";
		case Variant::VECTOR4I:
			return "vector4i";
		case Variant::RECT2:
			return "rect2";
		case Variant::RECT2I:
			return "rect2i";
		case Variant::TRANSFORM2D:
			return "transform2d";
		case Variant::PLANE:
			return "plane";
		case Variant::QUATERNION:
			return "quaternion";
		case Variant::AABB:
			return "aabb";
		case Variant::BASIS:
			return "basis";
		case Variant::TRANSFORM3D:
			return "transform3d";
		case Variant::PROJECTION:
			return "projection";
		case Variant::COLOR:
			return "color";
		case Variant::RID:
			return "rid";
		case Variant::CALLABLE:
			return "callable";
		case Variant::SIGNAL:
			return "signal";
		case Variant::DICTIONARY:
			return "dictionary";
		case Variant::ARRAY:
			return "array";
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
			return "packed_array";
		case Variant::OBJECT:
			return "object";
		default:
			return "unsupported";
	}
}

Variant CodexRuntimeValueProjector::_project_raw(const Variant &p_value, bool &r_truncated, int p_depth, ProjectionContext &r_context) {
	const String type = type_token(p_value.get_type());
	if (p_depth > MAX_DEPTH) {
		r_truncated = true;
		return _omitted(type, "max_depth");
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
			String value;
			if (p_value.get_type() == Variant::STRING) {
				value = p_value;
			} else if (p_value.get_type() == Variant::STRING_NAME) {
				value = String(StringName(p_value));
			} else {
				value = String(NodePath(p_value));
			}
			if ((p_value.get_type() == Variant::STRING || p_value.get_type() == Variant::STRING_NAME) && _looks_like_absolute_path(value)) {
				r_truncated = true;
				return _omitted("string", "unsafe_absolute_path");
			}
			if (value.length() > MAX_STRING_CHARACTERS) {
				value = value.left(MAX_STRING_CHARACTERS);
				r_truncated = true;
			}
			return value;
		}
		case Variant::VECTOR2: {
			const Vector2 value = p_value;
			Dictionary result;
			result["type"] = "Vector2";
			result["x"] = value.x;
			result["y"] = value.y;
			return result;
		}
		case Variant::VECTOR2I: {
			const Vector2i value = p_value;
			Dictionary result;
			result["type"] = "Vector2i";
			result["x"] = value.x;
			result["y"] = value.y;
			return result;
		}
		case Variant::VECTOR3: {
			const Vector3 value = p_value;
			Dictionary result;
			result["type"] = "Vector3";
			result["x"] = value.x;
			result["y"] = value.y;
			result["z"] = value.z;
			return result;
		}
		case Variant::VECTOR3I: {
			const Vector3i value = p_value;
			Dictionary result;
			result["type"] = "Vector3i";
			result["x"] = value.x;
			result["y"] = value.y;
			result["z"] = value.z;
			return result;
		}
		case Variant::COLOR: {
			const Color value = p_value;
			Dictionary result;
			result["type"] = "Color";
			result["r"] = value.r;
			result["g"] = value.g;
			result["b"] = value.b;
			result["a"] = value.a;
			return result;
		}
		case Variant::VECTOR4:
		case Variant::VECTOR4I:
		case Variant::RECT2:
		case Variant::RECT2I:
		case Variant::TRANSFORM2D:
		case Variant::PLANE:
		case Variant::QUATERNION:
		case Variant::AABB:
		case Variant::BASIS:
		case Variant::TRANSFORM3D:
		case Variant::PROJECTION: {
			String value = p_value.stringify();
			if (value.length() > MAX_STRING_CHARACTERS) {
				value = value.left(MAX_STRING_CHARACTERS);
				r_truncated = true;
			}
			return value;
		}
		case Variant::ARRAY: {
			const Array source = p_value;
			const void *identity = source.id();
			if (const String *known = r_context.array_references.getptr(identity)) {
				Dictionary reference;
				reference["type"] = "array";
				reference["reference_id"] = *known;
				reference["reference"] = true;
				return reference;
			}
			const String reference_id = _next_reference(r_context);
			r_context.array_references.insert(identity, reference_id);
			Array items;
			const int count = MIN(source.size(), MAX_CONTAINER_ITEMS);
			for (int index = 0; index < count; index++) {
				items.push_back(_project_raw(source[index], r_truncated, p_depth + 1, r_context));
			}
			Dictionary result;
			result["type"] = "array";
			result["reference_id"] = reference_id;
			result["items"] = items;
			result["size"] = source.size();
			if (source.size() > count) {
				result["omitted_count"] = source.size() - count;
				result["omitted_reason"] = "max_items";
				r_truncated = true;
			}
			return result;
		}
		case Variant::DICTIONARY: {
			const Dictionary source = p_value;
			const void *identity = source.id();
			if (const String *known = r_context.dictionary_references.getptr(identity)) {
				Dictionary reference;
				reference["type"] = "dictionary";
				reference["reference_id"] = *known;
				reference["reference"] = true;
				return reference;
			}
			const String reference_id = _next_reference(r_context);
			r_context.dictionary_references.insert(identity, reference_id);
			Array keys = source.keys();
			Vector<DictionaryKey> ordered_keys;
			ordered_keys.resize(keys.size());
			for (int index = 0; index < keys.size(); index++) {
				ordered_keys.write[index].value = keys[index];
				ordered_keys.write[index].label = _dictionary_key_label(keys[index], r_truncated);
				ordered_keys.write[index].original_index = index;
			}
			ordered_keys.sort();
			Array entries;
			const int count = MIN(ordered_keys.size(), MAX_CONTAINER_ITEMS);
			for (int index = 0; index < count; index++) {
				Dictionary entry;
				entry["key"] = ordered_keys[index].label;
				entry["value"] = _project_raw(source[ordered_keys[index].value], r_truncated, p_depth + 1, r_context);
				entries.push_back(entry);
			}
			Dictionary result;
			result["type"] = "dictionary";
			result["reference_id"] = reference_id;
			result["entries"] = entries;
			result["size"] = ordered_keys.size();
			if (ordered_keys.size() > count) {
				result["omitted_count"] = ordered_keys.size() - count;
				result["omitted_reason"] = "max_items";
				r_truncated = true;
			}
			return result;
		}
		case Variant::OBJECT: {
			Object *object = p_value;
			if (!object) {
				return Variant();
			}
			Resource *resource = Object::cast_to<Resource>(object);
			if (!resource) {
				return _omitted("object", "unsupported_object");
			}
			const String path = resource->get_path().get_slice("::", 0);
			if (!_safe_resource_path(path)) {
				r_truncated = true;
				return _omitted("resource", "unsafe_resource_path");
			}
			Dictionary reference;
			reference["path"] = path;
			if (!resource->get_scene_unique_id().is_empty()) {
				reference["scene_unique_id"] = resource->get_scene_unique_id().left(128);
			}
			return reference;
		}
		case Variant::RID:
		case Variant::CALLABLE:
		case Variant::SIGNAL:
			r_truncated = true;
			return _omitted(type, "unsupported_handle");
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
			return _omitted("packed_array", "packed_array_omitted");
		default:
			r_truncated = true;
			return _omitted(type, "unsupported_type");
	}
}

Variant CodexRuntimeValueProjector::project_raw(const Variant &p_value, bool &r_truncated, int p_depth) {
	ProjectionContext context;
	return _project_raw(p_value, r_truncated, p_depth, context);
}

Dictionary CodexRuntimeValueProjector::project_typed(const Variant &p_value) {
	bool truncated = false;
	ProjectionContext context;
	String projected_type = type_token(p_value.get_type());
	if (p_value.get_type() == Variant::OBJECT) {
		Object *object = p_value;
		if (object && Object::cast_to<Resource>(object)) {
			projected_type = "resource";
		}
	}
	Dictionary result;
	result["type"] = projected_type;
	result["value"] = _project_raw(p_value, truncated, 0, context);
	result["truncated"] = truncated;
	if ((p_value.get_type() == Variant::STRING || p_value.get_type() == Variant::STRING_NAME || p_value.get_type() == Variant::NODE_PATH) && String(p_value).length() > MAX_STRING_CHARACTERS) {
		result["omitted_reason"] = "max_string";
	}
	if (JSON::stringify(result, "", true, true).utf8().length() > MAX_ENCODED_BYTES) {
		result["value"] = _omitted(projected_type, "max_encoded_bytes");
		result["truncated"] = true;
	}
	return result;
}

Dictionary CodexRuntimeValueProjector::omitted_typed(Variant::Type p_type, const String &p_reason) {
	const String type = type_token(p_type);
	Dictionary result;
	result["type"] = type;
	result["value"] = _omitted(type, p_reason);
	result["truncated"] = true;
	return result;
}
