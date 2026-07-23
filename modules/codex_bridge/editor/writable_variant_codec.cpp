/**************************************************************************/
/*  writable_variant_codec.cpp                                          */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "writable_variant_codec.h"

#include "core/io/json.h"
#include "core/io/resource_loader.h"
#include "core/io/resource_uid.h"
#include "core/math/aabb.h"
#include "core/math/basis.h"
#include "core/math/color.h"
#include "core/math/plane.h"
#include "core/math/projection.h"
#include "core/math/quaternion.h"
#include "core/math/rect2.h"
#include "core/math/transform_2d.h"
#include "core/math/transform_3d.h"
#include "core/math/vector2.h"
#include "core/math/vector3.h"
#include "core/math/vector4.h"
#include "core/object/class_db.h"
#include "core/templates/hash_set.h"
#include "scene/main/node.h"

#include "modules/codex_bridge/protocol/bridge_transaction_canonicalizer.h"
#include "modules/codex_bridge/protocol/bridge_transaction_profile.h"

#include <initializer_list>

namespace {

struct CodecState {
	uint32_t items = 0;
	HashSet<const void *> active_containers;
};

static Error fail(const String &p_code, const String &p_message, String &r_error_code, String &r_error_message, Error p_error = ERR_INVALID_DATA) {
	r_error_code = p_code;
	r_error_message = p_message;
	return p_error;
}

static bool safe_resource_path(const String &p_path) {
	if (!p_path.begins_with("res://") || p_path.length() <= 6 || p_path.contains("\\") || p_path.contains("::") || p_path.contains("?") || p_path.contains("#")) {
		return false;
	}
	const PackedStringArray components = p_path.substr(6).split("/", true);
	for (const String &component : components) {
		if (component.is_empty() || component == "." || component == "..") {
			return false;
		}
	}
	return true;
}

static int math_arity(const String &p_type) {
	if (p_type == "vector2" || p_type == "vector2i") {
		return 2;
	}
	if (p_type == "vector3" || p_type == "vector3i") {
		return 3;
	}
	if (p_type == "vector4" || p_type == "vector4i" || p_type == "rect2" || p_type == "rect2i" || p_type == "plane" || p_type == "quaternion" || p_type == "color") {
		return 4;
	}
	if (p_type == "transform2d" || p_type == "aabb") {
		return 6;
	}
	if (p_type == "basis") {
		return 9;
	}
	if (p_type == "transform3d") {
		return 12;
	}
	if (p_type == "projection") {
		return 16;
	}
	return -1;
}

static Dictionary scalar_wire(const String &p_type, const Variant &p_value = Variant()) {
	Dictionary wire;
	wire["type"] = p_type;
	if (p_type != "nil") {
		wire["value"] = p_value;
	}
	return wire;
}

static Array make_array(std::initializer_list<Variant> p_values) {
	Array result;
	for (const Variant &value : p_values) {
		result.push_back(value);
	}
	return result;
}

static Error digest_wire(const Dictionary &p_wire, String &r_digest) {
	const String canonical = JSON::stringify(p_wire, "", true, true);
	return BridgeTransactionCanonicalizer::sha256_utf8(canonical, r_digest, "godot-codex-writable-variant/v1\n");
}

static Dictionary make_summary(const String &p_type, const String &p_digest, uint32_t p_items = 0, int p_characters = -1, int p_arity = -1, const Variant &p_safe_value = Variant()) {
	Dictionary summary;
	summary["type"] = p_type;
	summary["redacted"] = p_safe_value.get_type() == Variant::NIL;
	summary["digest"] = p_digest;
	if (p_items > 0 || p_type == "array" || p_type == "dictionary" || p_type == "binds") {
		summary["items"] = (int64_t)p_items;
	}
	if (p_characters >= 0) {
		summary["characters"] = (int64_t)p_characters;
	}
	if (p_arity >= 0) {
		summary["arity"] = (int64_t)p_arity;
	}
	if (p_safe_value.get_type() != Variant::NIL) {
		summary["value"] = p_safe_value;
	}
	return summary;
}

static bool node_path_is_safe(const NodePath &p_path, Node *p_scene_root, Node *p_origin) {
	if (p_path.is_absolute()) {
		return false;
	}
	if (p_path.is_empty()) {
		return true;
	}
	if (!p_scene_root || !p_origin) {
		return false;
	}
	Node *resolved = p_origin->get_node_or_null(p_path);
	return resolved && (resolved == p_scene_root || p_scene_root->is_ancestor_of(resolved));
}

static Error decode_value(const Dictionary &p_wire, Node *p_scene_root, Node *p_origin, int p_depth, CodecState &r_state, Variant &r_value, String &r_error_code, String &r_error_message);

static Error decode_math(const String &p_type, const Array &p_values, Variant &r_value, String &r_error_code, String &r_error_message) {
	const int arity = math_arity(p_type);
	if (arity < 0 || p_values.size() != arity) {
		return fail("property_value_unsupported", "The writable math value has an invalid component count.", r_error_code, r_error_message);
	}
	Vector<double> values;
	values.resize(arity);
	for (int index = 0; index < arity; index++) {
		if (p_values[index].get_type() != Variant::INT && p_values[index].get_type() != Variant::FLOAT) {
			return fail("property_value_unsupported", "The writable math value contains a non-numeric component.", r_error_code, r_error_message);
		}
		const double number = p_values[index];
		if (!Math::is_finite(number)) {
			return fail("property_value_unsupported", "The writable math value contains a non-finite component.", r_error_code, r_error_message);
		}
		if (p_type.ends_with("i") && (double)(int64_t)number != number) {
			return fail("property_value_unsupported", "The writable integer vector contains a fractional component.", r_error_code, r_error_message);
		}
		values.write[index] = number;
	}
	if (p_type == "vector2") {
		r_value = Vector2(values[0], values[1]);
	} else if (p_type == "vector2i") {
		r_value = Vector2i((int64_t)values[0], (int64_t)values[1]);
	} else if (p_type == "vector3") {
		r_value = Vector3(values[0], values[1], values[2]);
	} else if (p_type == "vector3i") {
		r_value = Vector3i((int64_t)values[0], (int64_t)values[1], (int64_t)values[2]);
	} else if (p_type == "vector4") {
		r_value = Vector4(values[0], values[1], values[2], values[3]);
	} else if (p_type == "vector4i") {
		r_value = Vector4i((int64_t)values[0], (int64_t)values[1], (int64_t)values[2], (int64_t)values[3]);
	} else if (p_type == "rect2") {
		r_value = Rect2(values[0], values[1], values[2], values[3]);
	} else if (p_type == "rect2i") {
		r_value = Rect2i((int64_t)values[0], (int64_t)values[1], (int64_t)values[2], (int64_t)values[3]);
	} else if (p_type == "transform2d") {
		Transform2D transform;
		transform.columns[0] = Vector2(values[0], values[1]);
		transform.columns[1] = Vector2(values[2], values[3]);
		transform.columns[2] = Vector2(values[4], values[5]);
		r_value = transform;
	} else if (p_type == "plane") {
		r_value = Plane(values[0], values[1], values[2], values[3]);
	} else if (p_type == "quaternion") {
		r_value = Quaternion(values[0], values[1], values[2], values[3]);
	} else if (p_type == "aabb") {
		r_value = AABB(Vector3(values[0], values[1], values[2]), Vector3(values[3], values[4], values[5]));
	} else if (p_type == "basis") {
		Basis basis;
		basis.rows[0] = Vector3(values[0], values[1], values[2]);
		basis.rows[1] = Vector3(values[3], values[4], values[5]);
		basis.rows[2] = Vector3(values[6], values[7], values[8]);
		r_value = basis;
	} else if (p_type == "transform3d") {
		Transform3D transform;
		transform.basis.rows[0] = Vector3(values[0], values[1], values[2]);
		transform.basis.rows[1] = Vector3(values[3], values[4], values[5]);
		transform.basis.rows[2] = Vector3(values[6], values[7], values[8]);
		transform.origin = Vector3(values[9], values[10], values[11]);
		r_value = transform;
	} else if (p_type == "projection") {
		Projection projection;
		projection.columns[0] = Vector4(values[0], values[1], values[2], values[3]);
		projection.columns[1] = Vector4(values[4], values[5], values[6], values[7]);
		projection.columns[2] = Vector4(values[8], values[9], values[10], values[11]);
		projection.columns[3] = Vector4(values[12], values[13], values[14], values[15]);
		r_value = projection;
	} else if (p_type == "color") {
		r_value = Color(values[0], values[1], values[2], values[3]);
	} else {
		return fail("property_value_unsupported", "The writable math value type is unsupported.", r_error_code, r_error_message);
	}
	return OK;
}

static Error decode_value(const Dictionary &p_wire, Node *p_scene_root, Node *p_origin, int p_depth, CodecState &r_state, Variant &r_value, String &r_error_code, String &r_error_message) {
	if (p_depth > (int)BridgeTransactionProfile::MAX_VARIANT_DEPTH) {
		return fail("transaction_too_large", "The writable Variant exceeds the depth limit.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
	}
	const String type = p_wire.get("type", String());
	if (type == "nil") {
		r_value = Variant();
		return OK;
	}
	if (!p_wire.has("value")) {
		return fail("property_value_unsupported", "The writable Variant is missing its value.", r_error_code, r_error_message);
	}
	const Variant wire_value = p_wire["value"];
	if (type == "bool" && wire_value.get_type() == Variant::BOOL) {
		r_value = wire_value;
		return OK;
	}
	if (type == "int" && (wire_value.get_type() == Variant::INT || wire_value.get_type() == Variant::FLOAT)) {
		const double number = wire_value;
		if (!Math::is_finite(number) || number < -9007199254740991.0 || number > 9007199254740991.0) {
			return fail("property_value_unsupported", "The writable integer exceeds the safe JSON range.", r_error_code, r_error_message);
		}
		const int64_t integer = (int64_t)number;
		if (number != (double)integer) {
			return fail("property_value_unsupported", "The writable integer exceeds the safe JSON range.", r_error_code, r_error_message);
		}
		r_value = integer;
		return OK;
	}
	if (type == "float" && (wire_value.get_type() == Variant::INT || wire_value.get_type() == Variant::FLOAT)) {
		const double number = wire_value;
		if (!Math::is_finite(number)) {
			return fail("property_value_unsupported", "The writable float is not finite.", r_error_code, r_error_message);
		}
		r_value = number;
		return OK;
	}
	if ((type == "string" || type == "string_name" || type == "node_path") && wire_value.get_type() == Variant::STRING) {
		const String string = wire_value;
		if (string.length() > (int)BridgeTransactionProfile::MAX_STRING_CHARACTERS) {
			return fail("transaction_too_large", "The writable string exceeds the character limit.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
		}
		if (type == "string") {
			r_value = string;
		} else if (type == "string_name") {
			r_value = StringName(string);
		} else {
			const NodePath path(string);
			if (!node_path_is_safe(path, p_scene_root, p_origin)) {
				return fail("property_value_unsupported", "The writable NodePath is not a relative same-scene path.", r_error_code, r_error_message);
			}
			r_value = path;
		}
		return OK;
	}
	const int arity = math_arity(type);
	if (arity >= 0 && wire_value.get_type() == Variant::ARRAY) {
		return decode_math(type, wire_value, r_value, r_error_code, r_error_message);
	}
	if (type == "resource" && wire_value.get_type() == Variant::DICTIONARY) {
		const String path = WritableVariantCodec::resolve_project_resource_path(wire_value);
		if (path.is_empty() || !ResourceLoader::exists(path)) {
			return fail("property_value_unsupported", "The writable resource reference is not an existing project resource.", r_error_code, r_error_message);
		}
		Ref<Resource> resource = ResourceLoader::load(path);
		if (resource.is_null()) {
			return fail("property_value_unsupported", "The writable project resource could not be loaded.", r_error_code, r_error_message);
		}
		r_value = resource;
		return OK;
	}
	if ((type == "array" || type == "dictionary") && wire_value.get_type() == Variant::ARRAY) {
		const Array entries = wire_value;
		r_state.items += entries.size();
		if (r_state.items > BridgeTransactionProfile::MAX_CONTAINER_ITEMS) {
			return fail("transaction_too_large", "The writable Variant exceeds the aggregate container limit.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
		}
		if (type == "array") {
			Array array;
			array.resize(entries.size());
			for (int index = 0; index < entries.size(); index++) {
				if (entries[index].get_type() != Variant::DICTIONARY) {
					return fail("property_value_unsupported", "The writable Array contains an invalid entry.", r_error_code, r_error_message);
				}
				Variant child;
				const Error error = decode_value(entries[index], p_scene_root, p_origin, p_depth + 1, r_state, child, r_error_code, r_error_message);
				if (error != OK) {
					return error;
				}
				array[index] = child;
			}
			r_value = array;
			return OK;
		}
		Dictionary dictionary;
		HashSet<String> keys;
		for (int index = 0; index < entries.size(); index++) {
			if (entries[index].get_type() != Variant::DICTIONARY) {
				return fail("property_value_unsupported", "The writable Dictionary contains an invalid entry.", r_error_code, r_error_message);
			}
			const Dictionary entry = entries[index];
			if (entry.get("key", Variant()).get_type() != Variant::STRING || entry.get("value", Variant()).get_type() != Variant::DICTIONARY) {
				return fail("property_value_unsupported", "The writable Dictionary entry is malformed.", r_error_code, r_error_message);
			}
			const String key = entry["key"];
			if (key.length() > (int)BridgeTransactionProfile::MAX_STRING_CHARACTERS) {
				return fail("transaction_too_large", "The writable Dictionary key exceeds the character limit.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
			}
			if (keys.has(key)) {
				return fail("property_value_unsupported", "The writable Dictionary contains a duplicate key.", r_error_code, r_error_message);
			}
			keys.insert(key);
			Variant child;
			const Error error = decode_value(entry["value"], p_scene_root, p_origin, p_depth + 1, r_state, child, r_error_code, r_error_message);
			if (error != OK) {
				return error;
			}
			dictionary[key] = child;
		}
		r_value = dictionary;
		return OK;
	}
	return fail("property_value_unsupported", "The writable Variant type or shape is unsupported.", r_error_code, r_error_message);
}

static Error encode_native(const Variant &p_value, Node *p_scene_root, Node *p_origin, int p_depth, CodecState &r_state, Dictionary &r_wire, String &r_error_code, String &r_error_message) {
	if (p_depth > (int)BridgeTransactionProfile::MAX_VARIANT_DEPTH) {
		return fail("transaction_too_large", "The existing Variant exceeds the depth limit.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
	}
	switch (p_value.get_type()) {
		case Variant::NIL:
			r_wire = scalar_wire("nil");
			return OK;
		case Variant::BOOL:
			r_wire = scalar_wire("bool", p_value);
			return OK;
		case Variant::INT: {
			const int64_t integer = p_value;
			if (integer < -9007199254740991LL || integer > 9007199254740991LL) {
				return fail("property_value_unsupported", "The existing integer exceeds the safe JSON range.", r_error_code, r_error_message);
			}
			r_wire = scalar_wire("int", integer);
			return OK;
		}
		case Variant::FLOAT: {
			const double number = p_value;
			if (!Math::is_finite(number)) {
				return fail("property_value_unsupported", "The existing float is not finite.", r_error_code, r_error_message);
			}
			r_wire = scalar_wire("float", number);
			return OK;
		}
		case Variant::STRING: {
			const String string = p_value;
			if (string.length() > (int)BridgeTransactionProfile::MAX_STRING_CHARACTERS) {
				return fail("transaction_too_large", "The existing string exceeds the character limit.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
			}
			r_wire = scalar_wire("string", string);
			return OK;
		}
		case Variant::STRING_NAME: {
			const String string = StringName(p_value);
			if (string.length() > (int)BridgeTransactionProfile::MAX_STRING_CHARACTERS) {
				return fail("transaction_too_large", "The existing StringName exceeds the character limit.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
			}
			r_wire = scalar_wire("string_name", string);
			return OK;
		}
		case Variant::NODE_PATH: {
			const NodePath path = p_value;
			if (!node_path_is_safe(path, p_scene_root, p_origin)) {
				return fail("property_value_unsupported", "The existing NodePath is not a relative same-scene path.", r_error_code, r_error_message);
			}
			r_wire = scalar_wire("node_path", String(path));
			return OK;
		}
		case Variant::VECTOR2: {
			const Vector2 value = p_value;
			Array a = make_array({ value.x, value.y });
			r_wire = scalar_wire("vector2", a);
			return OK;
		}
		case Variant::VECTOR2I: {
			const Vector2i value = p_value;
			Array a = make_array({ value.x, value.y });
			r_wire = scalar_wire("vector2i", a);
			return OK;
		}
		case Variant::VECTOR3: {
			const Vector3 value = p_value;
			Array a = make_array({ value.x, value.y, value.z });
			r_wire = scalar_wire("vector3", a);
			return OK;
		}
		case Variant::VECTOR3I: {
			const Vector3i value = p_value;
			Array a = make_array({ value.x, value.y, value.z });
			r_wire = scalar_wire("vector3i", a);
			return OK;
		}
		case Variant::VECTOR4: {
			const Vector4 value = p_value;
			Array a = make_array({ value.x, value.y, value.z, value.w });
			r_wire = scalar_wire("vector4", a);
			return OK;
		}
		case Variant::VECTOR4I: {
			const Vector4i value = p_value;
			Array a = make_array({ value.x, value.y, value.z, value.w });
			r_wire = scalar_wire("vector4i", a);
			return OK;
		}
		case Variant::RECT2: {
			const Rect2 value = p_value;
			Array a = make_array({ value.position.x, value.position.y, value.size.x, value.size.y });
			r_wire = scalar_wire("rect2", a);
			return OK;
		}
		case Variant::RECT2I: {
			const Rect2i value = p_value;
			Array a = make_array({ value.position.x, value.position.y, value.size.x, value.size.y });
			r_wire = scalar_wire("rect2i", a);
			return OK;
		}
		case Variant::TRANSFORM2D: {
			const Transform2D value = p_value;
			Array a = make_array({ value.columns[0].x, value.columns[0].y, value.columns[1].x, value.columns[1].y, value.columns[2].x, value.columns[2].y });
			r_wire = scalar_wire("transform2d", a);
			return OK;
		}
		case Variant::PLANE: {
			const Plane value = p_value;
			Array a = make_array({ value.normal.x, value.normal.y, value.normal.z, value.d });
			r_wire = scalar_wire("plane", a);
			return OK;
		}
		case Variant::QUATERNION: {
			const Quaternion value = p_value;
			Array a = make_array({ value.x, value.y, value.z, value.w });
			r_wire = scalar_wire("quaternion", a);
			return OK;
		}
		case Variant::AABB: {
			const AABB value = p_value;
			Array a = make_array({ value.position.x, value.position.y, value.position.z, value.size.x, value.size.y, value.size.z });
			r_wire = scalar_wire("aabb", a);
			return OK;
		}
		case Variant::BASIS: {
			const Basis value = p_value;
			Array a = make_array({ value.rows[0].x, value.rows[0].y, value.rows[0].z, value.rows[1].x, value.rows[1].y, value.rows[1].z, value.rows[2].x, value.rows[2].y, value.rows[2].z });
			r_wire = scalar_wire("basis", a);
			return OK;
		}
		case Variant::TRANSFORM3D: {
			const Transform3D value = p_value;
			Array a = make_array({ value.basis.rows[0].x, value.basis.rows[0].y, value.basis.rows[0].z, value.basis.rows[1].x, value.basis.rows[1].y, value.basis.rows[1].z, value.basis.rows[2].x, value.basis.rows[2].y, value.basis.rows[2].z, value.origin.x, value.origin.y, value.origin.z });
			r_wire = scalar_wire("transform3d", a);
			return OK;
		}
		case Variant::PROJECTION: {
			const Projection value = p_value;
			Array a;
			for (int column = 0; column < 4; column++) {
				a.push_back(value.columns[column].x);
				a.push_back(value.columns[column].y);
				a.push_back(value.columns[column].z);
				a.push_back(value.columns[column].w);
			}
			r_wire = scalar_wire("projection", a);
			return OK;
		}
		case Variant::COLOR: {
			const Color value = p_value;
			Array a = make_array({ value.r, value.g, value.b, value.a });
			r_wire = scalar_wire("color", a);
			return OK;
		}
		case Variant::OBJECT: {
			const Ref<Resource> resource = p_value;
			if (resource.is_null() || !safe_resource_path(resource->get_path())) {
				return fail("property_value_unsupported", "The existing Object is not a project-bound Resource.", r_error_code, r_error_message);
			}
			Dictionary reference;
			reference["uid_missing"] = true;
			reference["path"] = resource->get_path();
			r_wire = scalar_wire("resource", reference);
			return OK;
		}
		case Variant::ARRAY: {
			const Array value = p_value;
			if (value.is_typed() || r_state.active_containers.has(value.id())) {
				return fail("property_value_unsupported", "Typed or cyclic existing Arrays are unsupported.", r_error_code, r_error_message);
			}
			r_state.items += value.size();
			if (r_state.items > BridgeTransactionProfile::MAX_CONTAINER_ITEMS) {
				return fail("transaction_too_large", "The existing Array exceeds the aggregate container limit.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
			}
			r_state.active_containers.insert(value.id());
			Array entries;
			for (int index = 0; index < value.size(); index++) {
				Dictionary child;
				const Error error = encode_native(value[index], p_scene_root, p_origin, p_depth + 1, r_state, child, r_error_code, r_error_message);
				if (error != OK) {
					r_state.active_containers.erase(value.id());
					return error;
				}
				entries.push_back(child);
			}
			r_state.active_containers.erase(value.id());
			r_wire = scalar_wire("array", entries);
			return OK;
		}
		case Variant::DICTIONARY: {
			const Dictionary value = p_value;
			if (value.is_typed() || r_state.active_containers.has(value.id())) {
				return fail("property_value_unsupported", "Typed or cyclic existing Dictionaries are unsupported.", r_error_code, r_error_message);
			}
			r_state.items += value.size();
			if (r_state.items > BridgeTransactionProfile::MAX_CONTAINER_ITEMS) {
				return fail("transaction_too_large", "The existing Dictionary exceeds the aggregate container limit.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
			}
			const Array keys = value.keys();
			Vector<String> sorted_keys;
			for (int index = 0; index < keys.size(); index++) {
				if (keys[index].get_type() != Variant::STRING && keys[index].get_type() != Variant::STRING_NAME) {
					return fail("property_value_unsupported", "The existing Dictionary has a non-string key.", r_error_code, r_error_message);
				}
				const String key = keys[index];
				if (key.length() > (int)BridgeTransactionProfile::MAX_STRING_CHARACTERS) {
					return fail("transaction_too_large", "The existing Dictionary key exceeds the character limit.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
				}
				sorted_keys.push_back(key);
			}
			sorted_keys.sort();
			r_state.active_containers.insert(value.id());
			Array entries;
			for (const String &key : sorted_keys) {
				Dictionary child;
				const Error error = encode_native(value[key], p_scene_root, p_origin, p_depth + 1, r_state, child, r_error_code, r_error_message);
				if (error != OK) {
					r_state.active_containers.erase(value.id());
					return error;
				}
				Dictionary entry;
				entry["key"] = key;
				entry["value"] = child;
				entries.push_back(entry);
			}
			r_state.active_containers.erase(value.id());
			r_wire = scalar_wire("dictionary", entries);
			return OK;
		}
		default:
			return fail("property_value_unsupported", "The existing Variant type is unsupported.", r_error_code, r_error_message);
	}
}

static Dictionary summary_for_wire(const Dictionary &p_wire, const String &p_digest, uint32_t p_items) {
	const String type = p_wire.get("type", String("unknown"));
	if (type == "bool" || type == "int" || type == "float") {
		return make_summary(type, p_digest, 0, -1, -1, p_wire.get("value", Variant()));
	}
	if (type == "string" || type == "string_name" || type == "node_path") {
		return make_summary(type, p_digest, 0, String(p_wire.get("value", String())).length());
	}
	const int arity = math_arity(type);
	if (arity >= 0) {
		return make_summary(type, p_digest, 0, -1, arity);
	}
	if (type == "array" || type == "dictionary") {
		return make_summary(type, p_digest, p_items);
	}
	return make_summary(type, p_digest);
}

static bool is_path_writing_hint(PropertyHint p_hint) {
	return p_hint == PROPERTY_HINT_FILE || p_hint == PROPERTY_HINT_DIR || p_hint == PROPERTY_HINT_GLOBAL_FILE || p_hint == PROPERTY_HINT_GLOBAL_DIR || p_hint == PROPERTY_HINT_SAVE_FILE || p_hint == PROPERTY_HINT_GLOBAL_SAVE_FILE || p_hint == PROPERTY_HINT_FILE_PATH;
}

static bool enum_contains(const String &p_hint, int64_t p_value) {
	const PackedStringArray entries = p_hint.split(",", false);
	int64_t implicit = 0;
	for (const String &entry : entries) {
		const int separator = entry.rfind(":");
		int64_t candidate = implicit;
		if (separator >= 0) {
			const String encoded = entry.substr(separator + 1).strip_edges();
			if (!encoded.is_valid_int()) {
				return false;
			}
			candidate = encoded.to_int();
		}
		if (candidate == p_value) {
			return true;
		}
		implicit = candidate + 1;
	}
	return false;
}

static bool flags_contain_only_declared_bits(const String &p_hint, int64_t p_value) {
	if (p_value < 0) {
		return false;
	}
	const PackedStringArray entries = p_hint.split(",", false);
	uint64_t allowed = 0;
	for (int index = 0; index < entries.size(); index++) {
		const String entry = entries[index];
		const int separator = entry.rfind(":");
		int64_t bit = index < 63 ? (int64_t(1) << index) : 0;
		if (separator >= 0) {
			const String encoded = entry.substr(separator + 1).strip_edges();
			if (!encoded.is_valid_int()) {
				return false;
			}
			bit = encoded.to_int();
		}
		if (bit < 0) {
			return false;
		}
		allowed |= (uint64_t)bit;
	}
	return ((uint64_t)p_value & ~allowed) == 0;
}

static bool resource_matches_hint(const Ref<Resource> &p_resource, const String &p_hint) {
	if (p_resource.is_null() || p_hint.is_empty()) {
		return p_resource.is_valid();
	}
	const PackedStringArray classes = p_hint.split(",", false);
	bool positive_match = false;
	for (const String &entry : classes) {
		const String type = entry.strip_edges();
		if (type.is_empty()) {
			continue;
		}
		if (type.begins_with("-")) {
			if (p_resource->is_class(type.substr(1))) {
				return false;
			}
		} else if (p_resource->is_class(type)) {
			positive_match = true;
		}
	}
	return positive_match;
}

} // namespace

String WritableVariantCodec::resolve_project_resource_path(const Dictionary &p_reference) {
	String path;
	if (p_reference.has("uid")) {
		if (!ResourceUID::get_singleton()) {
			return String();
		}
		const ResourceUID::ID uid = ResourceUID::get_singleton()->text_to_id(p_reference["uid"]);
		if (uid == ResourceUID::INVALID_ID || !ResourceUID::get_singleton()->has_id(uid)) {
			return String();
		}
		path = ResourceUID::get_singleton()->get_id_path(uid);
	} else if ((bool)p_reference.get("uid_missing", false)) {
		path = p_reference.get("path", String());
	}
	return safe_resource_path(path) ? path : String();
}

Error WritableVariantCodec::decode(const Dictionary &p_wire_value, Node *p_scene_root, Node *p_node_path_origin, Result &r_result, String &r_error_code, String &r_error_message) {
	r_result = Result();
	r_error_code.clear();
	r_error_message.clear();
	CodecState state;
	const Error error = decode_value(p_wire_value, p_scene_root, p_node_path_origin, 1, state, r_result.value, r_error_code, r_error_message);
	if (error != OK) {
		return error;
	}
	const Dictionary normalized = BridgeTransactionCanonicalizer::normalize_wire_variant(p_wire_value);
	if (digest_wire(normalized, r_result.canonical_digest) != OK) {
		return fail("transaction_too_large", "The writable Variant commitment could not be created.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
	}
	r_result.container_items = state.items;
	r_result.redacted_summary = summary_for_wire(normalized, r_result.canonical_digest, state.items);
	return OK;
}

Error WritableVariantCodec::decode_binds(const Array &p_wire_binds, Node *p_scene_root, Node *p_node_path_origin, Array &r_values, String &r_digest, Dictionary &r_redacted_summary, String &r_error_code, String &r_error_message) {
	r_values.clear();
	r_digest.clear();
	r_redacted_summary.clear();
	CodecState state;
	state.items = p_wire_binds.size();
	if (state.items > BridgeTransactionProfile::MAX_CONTAINER_ITEMS) {
		return fail("transaction_too_large", "The signal binds exceed the aggregate container limit.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
	}
	Array normalized;
	for (int index = 0; index < p_wire_binds.size(); index++) {
		if (p_wire_binds[index].get_type() != Variant::DICTIONARY) {
			return fail("property_value_unsupported", "A signal bind is not a writable Variant.", r_error_code, r_error_message);
		}
		Variant value;
		const Error error = decode_value(p_wire_binds[index], p_scene_root, p_node_path_origin, 1, state, value, r_error_code, r_error_message);
		if (error != OK) {
			return error;
		}
		r_values.push_back(value);
		normalized.push_back(BridgeTransactionCanonicalizer::normalize_wire_variant(p_wire_binds[index]));
	}
	Dictionary wire;
	wire["type"] = "binds";
	wire["value"] = normalized;
	if (digest_wire(wire, r_digest) != OK) {
		return fail("transaction_too_large", "The signal bind commitment could not be created.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
	}
	r_redacted_summary = make_summary("binds", r_digest, state.items);
	return OK;
}

Error WritableVariantCodec::inspect_native(const Variant &p_value, Node *p_scene_root, Node *p_node_path_origin, Result &r_result, String &r_error_code, String &r_error_message) {
	r_result = Result();
	r_error_code.clear();
	r_error_message.clear();
	CodecState state;
	Dictionary wire;
	const Error error = encode_native(p_value, p_scene_root, p_node_path_origin, 1, state, wire, r_error_code, r_error_message);
	if (error != OK) {
		return error;
	}
	const Dictionary normalized = BridgeTransactionCanonicalizer::normalize_wire_variant(wire);
	if (digest_wire(normalized, r_result.canonical_digest) != OK) {
		return fail("transaction_too_large", "The existing Variant commitment could not be created.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
	}
	r_result.value = p_value.duplicate(true);
	r_result.container_items = state.items;
	r_result.redacted_summary = summary_for_wire(normalized, r_result.canonical_digest, state.items);
	return OK;
}

bool WritableVariantCodec::values_equal(const Variant &p_left, const Variant &p_right) {
	return p_left.hash_compare(p_right, 0, false);
}

Variant WritableVariantCodec::normalize_property_value(const PropertyInfo &p_property, const Variant &p_value) {
	if (p_property.type == Variant::FLOAT && p_value.get_type() == Variant::INT) {
		return (double)(int64_t)p_value;
	}
	return p_value.get_type() == Variant::ARRAY || p_value.get_type() == Variant::DICTIONARY ? p_value.duplicate(true) : p_value;
}

Error WritableVariantCodec::validate_property_compatibility(const PropertyInfo &p_property, const Variant &p_value, String &r_error_code, String &r_error_message) {
	if (is_path_writing_hint(p_property.hint)) {
		return fail("property_not_writable", "Path-writing editor properties are outside the safe property profile.", r_error_code, r_error_message);
	}
	if (p_value.get_type() == Variant::NIL) {
		if (p_property.type == Variant::OBJECT || p_property.type == Variant::NIL || (p_property.usage & PROPERTY_USAGE_NIL_IS_VARIANT)) {
			return OK;
		}
		return fail("property_value_unsupported", "The writable property is not nullable.", r_error_code, r_error_message);
	}
	if (p_property.type == Variant::FLOAT && p_value.get_type() == Variant::INT) {
		return OK;
	}
	if (p_property.type != p_value.get_type()) {
		return fail("property_value_unsupported", "The writable Variant type does not match the property type.", r_error_code, r_error_message);
	}
	if (p_property.hint == PROPERTY_HINT_ENUM && p_value.get_type() == Variant::INT && !enum_contains(p_property.hint_string, p_value)) {
		return fail("property_value_unsupported", "The requested enum value is not declared by the property.", r_error_code, r_error_message);
	}
	if (p_property.hint == PROPERTY_HINT_FLAGS && p_value.get_type() == Variant::INT && !flags_contain_only_declared_bits(p_property.hint_string, p_value)) {
		return fail("property_value_unsupported", "The requested flags contain undeclared bits.", r_error_code, r_error_message);
	}
	if (p_property.type == Variant::OBJECT) {
		const Ref<Resource> resource = p_value;
		if (resource.is_null() || (p_property.hint == PROPERTY_HINT_RESOURCE_TYPE && !resource_matches_hint(resource, p_property.hint_string)) || (!p_property.class_name.is_empty() && !resource->is_class(p_property.class_name))) {
			return fail("property_value_unsupported", "The project Resource is incompatible with the property class.", r_error_code, r_error_message);
		}
	}
	if ((p_property.type == Variant::ARRAY && p_property.hint == PROPERTY_HINT_ARRAY_TYPE) || (p_property.type == Variant::DICTIONARY && p_property.hint == PROPERTY_HINT_DICTIONARY_TYPE)) {
		return fail("property_value_unsupported", "Typed containers are outside the safe property profile.", r_error_code, r_error_message);
	}
	return OK;
}
