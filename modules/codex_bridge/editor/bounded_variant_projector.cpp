/**************************************************************************/
/*  bounded_variant_projector.cpp                                         */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
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
			return "resource";
		default:
			return "unsupported";
	}
}

Variant BoundedVariantProjector::project_raw(const Variant &p_value, bool &r_truncated, int p_depth) {
	if (p_depth > MAX_DEPTH) {
		r_truncated = true;
		return Variant();
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
			Array result;
			const int count = MIN(source.size(), MAX_CONTAINER_ITEMS);
			for (int index = 0; index < count; index++) {
				result.push_back(project_raw(source[index], r_truncated, p_depth + 1));
			}
			r_truncated = r_truncated || source.size() > count;
			return result;
		}
		case Variant::DICTIONARY: {
			const Dictionary source = p_value;
			Dictionary result;
			const Array keys = source.keys();
			const int count = MIN(keys.size(), MAX_CONTAINER_ITEMS);
			for (int index = 0; index < count; index++) {
				String key = keys[index].stringify();
				if (key.length() > MAX_STRING_CHARACTERS) {
					key = key.left(MAX_STRING_CHARACTERS);
					r_truncated = true;
				}
				result[key] = project_raw(source[keys[index]], r_truncated, p_depth + 1);
			}
			r_truncated = r_truncated || keys.size() > count;
			return result;
		}
		case Variant::OBJECT: {
			const Ref<Resource> resource = p_value;
			Dictionary projected;
			if (resource.is_valid()) {
				String path = resource->get_path();
				if (path.begins_with("res://")) {
					projected["path"] = path.get_slice("::", 0);
				}
				if (!resource->get_scene_unique_id().is_empty()) {
					projected["scene_unique_id"] = resource->get_scene_unique_id();
				}
				projected["godot_type"] = resource->get_class();
			}
			return projected;
		}
		default: {
			Dictionary opaque;
			opaque["type"] = Variant::get_type_name(p_value.get_type());
			opaque["opaque"] = true;
			return opaque;
		}
	}
}

Dictionary BoundedVariantProjector::project_typed(const Variant &p_value) {
	bool truncated = false;
	Dictionary result;
	result["type"] = type_token(p_value.get_type());
	result["value"] = project_raw(p_value, truncated);
	result["truncated"] = truncated;
	if (JSON::stringify(result, "", true, true).utf8().length() > MAX_ENCODED_BYTES) {
		result["value"] = Variant();
		result["truncated"] = true;
	}
	return result;
}
