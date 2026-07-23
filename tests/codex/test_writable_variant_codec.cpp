/**************************************************************************/
/*  test_writable_variant_codec.cpp                                     */
/**************************************************************************/

#include "tests/test_macros.h"

TEST_FORCE_LINK(test_writable_variant_codec)

#include "modules/modules_enabled.gen.h"

#ifdef MODULE_CODEX_BRIDGE_ENABLED

#include "scene/main/node.h"

#include "modules/codex_bridge/editor/writable_variant_codec.h"

#include <limits>

namespace TestWritableVariantCodec {

static Dictionary scalar(const String &p_type, const Variant &p_value = Variant()) {
	Dictionary wire;
	wire["type"] = p_type;
	if (p_type != "nil") {
		wire["value"] = p_value;
	}
	return wire;
}

TEST_CASE("[CodexS9TransactionExecutor][CodexS9Variant] Safe values round-trip with stable commitments") {
	Node *root = memnew(Node);
	Node *origin = memnew(Node);
	Node *resolved = memnew(Node);
	root->set_name("Root");
	origin->set_name("Origin");
	resolved->set_name("Resolved");
	root->add_child(origin);
	origin->add_child(resolved);

	Array vector_components;
	vector_components.push_back(1.25);
	vector_components.push_back(-2.5);
	WritableVariantCodec::Result vector;
	String code;
	String message;
	REQUIRE(WritableVariantCodec::decode(scalar("vector2", vector_components), root, origin, vector, code, message) == OK);
	CHECK(vector.value.get_type() == Variant::VECTOR2);
	CHECK(Vector2(vector.value).is_equal_approx(Vector2(1.25, -2.5)));
	CHECK(vector.canonical_digest.begins_with("sha256:"));
	CHECK((bool)vector.redacted_summary["redacted"]);

	Dictionary dictionary_entry;
	dictionary_entry["key"] = "answer";
	dictionary_entry["value"] = scalar("int", 42);
	Array dictionary_entries;
	dictionary_entries.push_back(dictionary_entry);
	Dictionary dictionary_wire = scalar("dictionary", dictionary_entries);
	Array array_entries;
	array_entries.push_back(dictionary_wire);
	array_entries.push_back(scalar("node_path", "Resolved"));
	WritableVariantCodec::Result compound;
	REQUIRE(WritableVariantCodec::decode(scalar("array", array_entries), root, origin, compound, code, message) == OK);
	CHECK(compound.value.get_type() == Variant::ARRAY);
	CHECK(compound.container_items == 3);
	WritableVariantCodec::Result inspected;
	REQUIRE(WritableVariantCodec::inspect_native(compound.value, root, origin, inspected, code, message) == OK);
	CHECK(inspected.canonical_digest == compound.canonical_digest);
	CHECK(WritableVariantCodec::values_equal(inspected.value, compound.value));

	memdelete(root);
}

TEST_CASE("[CodexS9TransactionExecutor][CodexS9Variant] Scalar and every math wire type round-trip exactly") {
	Node *root = memnew(Node);
	Node *origin = memnew(Node);
	root->add_child(origin);
	String code;
	String message;
	for (const Dictionary &wire : {
				 scalar("nil"), scalar("bool", true), scalar("int", (int64_t)9007199254740991LL), scalar("float", 1.25), scalar("string", "redacted"), scalar("string_name", "semantic_name") }) {
		WritableVariantCodec::Result decoded;
		REQUIRE(WritableVariantCodec::decode(wire, root, origin, decoded, code, message) == OK);
		WritableVariantCodec::Result inspected;
		REQUIRE(WritableVariantCodec::inspect_native(decoded.value, root, origin, inspected, code, message) == OK);
		CHECK(inspected.canonical_digest == decoded.canonical_digest);
	}
	struct MathCase {
		const char *type;
		int arity;
		Variant::Type native_type;
	};
	const MathCase math_cases[] = {
		{ "vector2", 2, Variant::VECTOR2 },
		{ "vector2i", 2, Variant::VECTOR2I },
		{ "vector3", 3, Variant::VECTOR3 },
		{ "vector3i", 3, Variant::VECTOR3I },
		{ "vector4", 4, Variant::VECTOR4 },
		{ "vector4i", 4, Variant::VECTOR4I },
		{ "rect2", 4, Variant::RECT2 },
		{ "rect2i", 4, Variant::RECT2I },
		{ "transform2d", 6, Variant::TRANSFORM2D },
		{ "plane", 4, Variant::PLANE },
		{ "quaternion", 4, Variant::QUATERNION },
		{ "aabb", 6, Variant::AABB },
		{ "basis", 9, Variant::BASIS },
		{ "transform3d", 12, Variant::TRANSFORM3D },
		{ "projection", 16, Variant::PROJECTION },
		{ "color", 4, Variant::COLOR },
	};
	for (const MathCase &math_case : math_cases) {
		INFO("wire type: " << String(math_case.type));
		Array components;
		for (int index = 0; index < math_case.arity; index++) {
			components.push_back(String(math_case.type).ends_with("i") ? Variant(index + 1) : Variant((double)index + 0.25));
		}
		WritableVariantCodec::Result decoded;
		REQUIRE(WritableVariantCodec::decode(scalar(math_case.type, components), root, origin, decoded, code, message) == OK);
		CHECK(decoded.value.get_type() == math_case.native_type);
		WritableVariantCodec::Result inspected;
		REQUIRE(WritableVariantCodec::inspect_native(decoded.value, root, origin, inspected, code, message) == OK);
		CHECK(inspected.canonical_digest == decoded.canonical_digest);
		CHECK(WritableVariantCodec::values_equal(inspected.value, decoded.value));
	}
	memdelete(root);
}

TEST_CASE("[CodexS9TransactionExecutor][CodexS9Variant] Project resources and exact boundary values remain bounded") {
	Node *root = memnew(Node);
	Node *origin = memnew(Node);
	root->add_child(origin);
	String code;
	String message;
	WritableVariantCodec::Result result;
	Dictionary reference;
	reference["uid_missing"] = true;
	reference["path"] = "res://tests/codex/fixtures/live_editor_project/shared_resource.tres";
	REQUIRE(WritableVariantCodec::decode(scalar("resource", reference), root, origin, result, code, message) == OK);
	CHECK(result.value.get_type() == Variant::OBJECT);
	CHECK((bool)result.redacted_summary["redacted"]);

	const String maximum_string = String("x").repeat(16384);
	REQUIRE(WritableVariantCodec::decode(scalar("string", maximum_string), root, origin, result, code, message) == OK);
	CHECK((int64_t)result.redacted_summary["characters"] == 16384);
	CHECK(WritableVariantCodec::decode(scalar("string", maximum_string + "x"), root, origin, result, code, message) == ERR_OUT_OF_MEMORY);
	CHECK(code == "transaction_too_large");

	Array maximum_items;
	for (int index = 0; index < 1000; index++) {
		maximum_items.push_back(scalar("nil"));
	}
	REQUIRE(WritableVariantCodec::decode(scalar("array", maximum_items), root, origin, result, code, message) == OK);
	CHECK(result.container_items == 1000);
	Dictionary boundary_depth = scalar("nil");
	for (int depth = 0; depth < 7; depth++) {
		Array child;
		child.push_back(boundary_depth);
		boundary_depth = scalar("array", child);
	}
	REQUIRE(WritableVariantCodec::decode(boundary_depth, root, origin, result, code, message) == OK);
	memdelete(root);
}

TEST_CASE("[CodexS9TransactionExecutor][CodexS9Variant] Unsafe shapes and aggregate limits fail closed") {
	String code;
	String message;
	WritableVariantCodec::Result result;
	Array short_transform;
	for (int index = 0; index < 5; index++) {
		short_transform.push_back(index);
	}
	CHECK(WritableVariantCodec::decode(scalar("transform2d", short_transform), nullptr, nullptr, result, code, message) == ERR_INVALID_DATA);
	CHECK(code == "property_value_unsupported");
	CHECK(WritableVariantCodec::decode(scalar("node_path", "/root/Foreign"), nullptr, nullptr, result, code, message) == ERR_INVALID_DATA);
	CHECK(WritableVariantCodec::decode(scalar("float", std::numeric_limits<double>::infinity()), nullptr, nullptr, result, code, message) == ERR_INVALID_DATA);
	CHECK(WritableVariantCodec::decode(scalar("float", std::numeric_limits<double>::quiet_NaN()), nullptr, nullptr, result, code, message) == ERR_INVALID_DATA);

	Dictionary duplicate_a;
	duplicate_a["key"] = "same";
	duplicate_a["value"] = scalar("bool", true);
	Dictionary duplicate_b = duplicate_a.duplicate(true);
	Array duplicate_entries;
	duplicate_entries.push_back(duplicate_a);
	duplicate_entries.push_back(duplicate_b);
	CHECK(WritableVariantCodec::decode(scalar("dictionary", duplicate_entries), nullptr, nullptr, result, code, message) == ERR_INVALID_DATA);

	Array too_many;
	for (int index = 0; index < 1001; index++) {
		too_many.push_back(scalar("nil"));
	}
	CHECK(WritableVariantCodec::decode(scalar("array", too_many), nullptr, nullptr, result, code, message) == ERR_OUT_OF_MEMORY);
	CHECK(code == "transaction_too_large");

	Dictionary nested = scalar("nil");
	for (int depth = 0; depth < 9; depth++) {
		Array entry;
		entry.push_back(nested);
		nested = scalar("array", entry);
	}
	CHECK(WritableVariantCodec::decode(nested, nullptr, nullptr, result, code, message) == ERR_OUT_OF_MEMORY);
	CHECK(code == "transaction_too_large");

	Array cyclic;
	cyclic.push_back(cyclic);
	CHECK(WritableVariantCodec::inspect_native(cyclic, nullptr, nullptr, result, code, message) == ERR_INVALID_DATA);
	CHECK(code == "property_value_unsupported");
}

TEST_CASE("[CodexS9TransactionExecutor][CodexS9Variant] Property hints narrow writable values") {
	String code;
	String message;
	PropertyInfo enum_property(Variant::INT, "mode", PROPERTY_HINT_ENUM, "Idle:0,Run:2", PROPERTY_USAGE_EDITOR);
	CHECK(WritableVariantCodec::validate_property_compatibility(enum_property, 2, code, message) == OK);
	CHECK(WritableVariantCodec::validate_property_compatibility(enum_property, 1, code, message) == ERR_INVALID_DATA);
	PropertyInfo flags_property(Variant::INT, "mask", PROPERTY_HINT_FLAGS, "A:1,B:4", PROPERTY_USAGE_EDITOR);
	CHECK(WritableVariantCodec::validate_property_compatibility(flags_property, 5, code, message) == OK);
	CHECK(WritableVariantCodec::validate_property_compatibility(flags_property, 2, code, message) == ERR_INVALID_DATA);
	PropertyInfo path_property(Variant::STRING, "destination", PROPERTY_HINT_GLOBAL_SAVE_FILE, "", PROPERTY_USAGE_EDITOR);
	CHECK(WritableVariantCodec::validate_property_compatibility(path_property, String("safe-looking"), code, message) == ERR_INVALID_DATA);
	PropertyInfo float_property(Variant::FLOAT, "ratio", PROPERTY_HINT_NONE, "", PROPERTY_USAGE_EDITOR);
	CHECK(WritableVariantCodec::validate_property_compatibility(float_property, 2, code, message) == OK);
	CHECK(WritableVariantCodec::normalize_property_value(float_property, 2).get_type() == Variant::FLOAT);
	PropertyInfo resource_property(Variant::OBJECT, "texture", PROPERTY_HINT_RESOURCE_TYPE, "Texture2D", PROPERTY_USAGE_EDITOR);
	Ref<Resource> generic_resource;
	generic_resource.instantiate();
	CHECK(WritableVariantCodec::validate_property_compatibility(resource_property, generic_resource, code, message) == ERR_INVALID_DATA);
}

} // namespace TestWritableVariantCodec

#endif // MODULE_CODEX_BRIDGE_ENABLED
