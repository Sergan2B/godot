/**************************************************************************/
/*  bounded_variant_projector.cpp                                         */
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

#include "bounded_variant_projector.h"

#include "core/io/json.h"
#include "core/io/resource.h"
#include "core/object/object.h"

Dictionary BoundedVariantProjector::_omitted(const String &p_type, const String &p_reason, int64_t p_size_hint) {
	Dictionary omitted;
	omitted["type"] = p_type;
	omitted["opaque"] = true;
	omitted["omitted_reason"] = p_reason;
	if (p_size_hint >= 0) {
		omitted["size_hint"] = p_size_hint;
	}
	return omitted;
}

String BoundedVariantProjector::_next_reference(ProjectionContext &r_context) {
	return "ref:" + String::num_uint64(r_context.next_reference++);
}

String BoundedVariantProjector::type_token(Variant::Type p_type) {
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

Variant BoundedVariantProjector::_project_raw(const Variant &p_value, bool &r_truncated, int p_depth, ProjectionContext &r_context) {
	if (p_depth > MAX_DEPTH) {
		r_truncated = true;
		return _omitted(type_token(p_value.get_type()), "max_depth");
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
		case Variant::ARRAY: {
			const Array source = p_value;
			const void *identity = source.id();
			const String *known_reference = r_context.array_references.getptr(identity);
			if (known_reference) {
				Dictionary reference;
				reference["type"] = "array";
				reference["reference_id"] = *known_reference;
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
				r_truncated = true;
			}
			return result;
		}
		case Variant::DICTIONARY: {
			const Dictionary source = p_value;
			const void *identity = source.id();
			const String *known_reference = r_context.dictionary_references.getptr(identity);
			if (known_reference) {
				Dictionary reference;
				reference["type"] = "dictionary";
				reference["reference_id"] = *known_reference;
				reference["reference"] = true;
				return reference;
			}
			const String reference_id = _next_reference(r_context);
			r_context.dictionary_references.insert(identity, reference_id);
			Dictionary result;
			result["type"] = "dictionary";
			result["reference_id"] = reference_id;
			Array entries;
			Array keys = source.keys();
			keys.sort();
			const int count = MIN(keys.size(), MAX_CONTAINER_ITEMS);
			for (int index = 0; index < count; index++) {
				String key = keys[index].stringify();
				if (key.length() > MAX_STRING_CHARACTERS) {
					key = key.left(MAX_STRING_CHARACTERS);
					r_truncated = true;
				}
				Dictionary entry;
				entry["key"] = key;
				entry["value"] = _project_raw(source[keys[index]], r_truncated, p_depth + 1, r_context);
				entries.push_back(entry);
			}
			result["entries"] = entries;
			result["size"] = keys.size();
			if (keys.size() > count) {
				result["omitted_count"] = keys.size() - count;
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
			Dictionary projected;
			projected["godot_type"] = object->get_class();
			if (!resource) {
				projected["opaque"] = true;
				projected["omitted_reason"] = "unsupported_object";
				return projected;
			}
			projected["resource_ref"] = true;
			const String path = resource->get_path();
			if (path.begins_with("res://")) {
				projected["path"] = path.get_slice("::", 0);
			}
			if (!resource->get_scene_unique_id().is_empty()) {
				projected["scene_unique_id"] = resource->get_scene_unique_id();
			}
			return projected;
		}
		default: {
			return _omitted(type_token(p_value.get_type()), "unsupported_type");
		}
	}
}

Variant BoundedVariantProjector::project_raw(const Variant &p_value, bool &r_truncated, int p_depth) {
	ProjectionContext context;
	return _project_raw(p_value, r_truncated, p_depth, context);
}

Dictionary BoundedVariantProjector::project_typed(const Variant &p_value) {
	bool truncated = false;
	ProjectionContext context;
	Dictionary result;
	String projected_type = type_token(p_value.get_type());
	if (p_value.get_type() == Variant::OBJECT) {
		Object *object = p_value;
		if (object && Object::cast_to<Resource>(object)) {
			projected_type = "resource";
		}
	}
	result["type"] = projected_type;
	result["value"] = _project_raw(p_value, truncated, 0, context);
	result["truncated"] = truncated;
	if (JSON::stringify(result, "", true, true).utf8().length() > MAX_ENCODED_BYTES) {
		result["value"] = _omitted(projected_type, "max_encoded_bytes");
		result["truncated"] = true;
	}
	return result;
}
