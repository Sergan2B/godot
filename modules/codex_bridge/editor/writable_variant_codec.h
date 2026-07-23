/**************************************************************************/
/*  writable_variant_codec.h                                            */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#pragma once

#include "core/object/property_info.h"
#include "core/variant/variant.h"

class Node;

class WritableVariantCodec {
public:
	struct Result {
		Variant value;
		String canonical_digest;
		Dictionary redacted_summary;
		uint32_t container_items = 0;
	};

	static Error decode(const Dictionary &p_wire_value, Node *p_scene_root, Node *p_node_path_origin, Result &r_result, String &r_error_code, String &r_error_message);
	static Error decode_binds(const Array &p_wire_binds, Node *p_scene_root, Node *p_node_path_origin, Array &r_values, String &r_digest, Dictionary &r_redacted_summary, String &r_error_code, String &r_error_message);
	static Error inspect_native(const Variant &p_value, Node *p_scene_root, Node *p_node_path_origin, Result &r_result, String &r_error_code, String &r_error_message);
	static Error validate_property_compatibility(const PropertyInfo &p_property, const Variant &p_value, String &r_error_code, String &r_error_message);
	static Variant normalize_property_value(const PropertyInfo &p_property, const Variant &p_value);
	static bool values_equal(const Variant &p_left, const Variant &p_right);
	static String resolve_project_resource_path(const Dictionary &p_reference);
};
