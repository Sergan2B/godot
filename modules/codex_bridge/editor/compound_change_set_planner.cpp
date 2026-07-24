/**************************************************************************/
/*  compound_change_set_planner.cpp                                       */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#include "compound_change_set_planner.h"

#include "core/io/json.h"
#include "core/templates/hash_map.h"
#include "core/templates/hash_set.h"

#include "modules/codex_bridge/protocol/bridge_crypto.h"
#include "modules/codex_bridge/protocol/bridge_transaction_canonicalizer.h"
#include "modules/codex_bridge/protocol/bridge_transaction_profile.h"

namespace {

static bool has_only_keys(const Dictionary &p_value, std::initializer_list<const char *> p_keys) {
	HashSet<String> allowed;
	for (const char *key : p_keys) {
		allowed.insert(key);
	}
	const Array keys = p_value.keys();
	for (int index = 0; index < keys.size(); index++) {
		if (keys[index].get_type() != Variant::STRING || !allowed.has(keys[index])) {
			return false;
		}
	}
	return true;
}

static bool valid_project_path(const String &p_path, const String &p_extension = String()) {
	if (!p_path.begins_with("res://") || p_path.length() < 7 || p_path.length() > 1024 || p_path.contains("\\") || p_path.contains_char('\0') || p_path.contains(":///")) {
		return false;
	}
	const PackedStringArray segments = p_path.trim_prefix("res://").split("/");
	for (const String &segment : segments) {
		if (segment.is_empty() || segment == "." || segment == "..") {
			return false;
		}
	}
	return p_extension.is_empty() || p_path.ends_with(p_extension);
}

static bool valid_alias(const String &p_alias) {
	if (!p_alias.begins_with("alias:")) {
		return false;
	}
	const String name = p_alias.trim_prefix("alias:");
	if (name.is_empty() || name.length() > 64 || name[0] < 'a' || name[0] > 'z') {
		return false;
	}
	for (int index = 1; index < name.length(); index++) {
		const char32_t character = name[index];
		if (!((character >= 'a' && character <= 'z') || (character >= '0' && character <= '9') || character == '_')) {
			return false;
		}
	}
	return true;
}

static bool valid_digest(const String &p_digest) {
	if (!p_digest.begins_with("sha256:") || p_digest.length() != 71) {
		return false;
	}
	for (int index = 7; index < p_digest.length(); index++) {
		const char32_t character = p_digest[index];
		if (!((character >= '0' && character <= '9') || (character >= 'a' && character <= 'f'))) {
			return false;
		}
	}
	return true;
}

static bool valid_safe_integer(const Variant &p_value, int64_t &r_value) {
	if (p_value.get_type() == Variant::INT) {
		r_value = p_value;
		return r_value >= 0 && r_value <= 9007199254740991LL;
	}
	if (p_value.get_type() != Variant::FLOAT) {
		return false;
	}
	const double number = p_value;
	if (!Math::is_finite(number) || number < 0.0 || number > 9007199254740991.0) {
		return false;
	}
	r_value = (int64_t)number;
	return (double)r_value == number;
}

static bool valid_resource_class(const String &p_class) {
	return p_class == "Gradient" || p_class == "Curve" || p_class == "Curve2D" || p_class == "Curve3D" || p_class == "Animation" || p_class == "CanvasItemMaterial" || p_class == "StandardMaterial3D";
}

static bool valid_stored_properties(const Array &p_properties) {
	if (p_properties.size() > 64) {
		return false;
	}
	HashSet<String> names;
	for (int index = 0; index < p_properties.size(); index++) {
		if (p_properties[index].get_type() != Variant::DICTIONARY) {
			return false;
		}
		const Dictionary property = p_properties[index];
		if (!has_only_keys(property, { "name", "value" }) || property.size() != 2 || !property.has("name") || property["name"].get_type() != Variant::STRING || !property.has("value") || property["value"].get_type() != Variant::DICTIONARY) {
			return false;
		}
		const String name = property["name"];
		if (name.is_empty() || name.length() > 128 || names.has(name)) {
			return false;
		}
		names.insert(name);
		const Dictionary value = property["value"];
		Dictionary probe;
		probe["kind"] = "set_property";
		probe["node_id"] = "node:00000000000000000000000000000000";
		probe["property"] = name;
		probe["value"] = value;
		if (!BridgeTransactionProfile::validate_operation(probe)) {
			return false;
		}
	}
	return true;
}

static Dictionary sanitize_compound_scene_operation(const Dictionary &p_operation) {
	Dictionary sanitized = p_operation.duplicate(true);
	sanitized.erase("alias");
	for (const char *key : { "node_id", "parent_node_id", "new_parent_node_id", "emitter_node_id", "receiver_node_id" }) {
		if (sanitized.has(key) && sanitized[key].get_type() == Variant::STRING && valid_alias(sanitized[key])) {
			sanitized[key] = "node:00000000000000000000000000000000";
		}
	}
	return sanitized;
}

static bool valid_compound_scene_operation(const Dictionary &p_operation) {
	const String kind = p_operation.get("kind", String());
	if (kind != "create_node" && kind != "reparent_node" && kind != "delete_node" && kind != "set_property" && kind != "attach_script" && kind != "detach_script" && kind != "connect_signal" && kind != "disconnect_signal") {
		return false;
	}
	if (p_operation.has("alias") && (kind != "create_node" || p_operation["alias"].get_type() != Variant::STRING || !valid_alias(p_operation["alias"]))) {
		return false;
	}
	bool has_plan_alias = p_operation.has("alias");
	for (const char *key : { "node_id", "parent_node_id", "new_parent_node_id", "emitter_node_id", "receiver_node_id" }) {
		if (p_operation.has(key) && p_operation[key].get_type() == Variant::STRING && valid_alias(p_operation[key])) {
			has_plan_alias = true;
		}
	}
	return has_plan_alias && BridgeTransactionProfile::validate_operation(sanitize_compound_scene_operation(p_operation));
}

static bool valid_new_operation(const Dictionary &p_operation) {
	if (!p_operation.has("kind") || p_operation["kind"].get_type() != Variant::STRING) {
		return false;
	}
	const String kind = p_operation["kind"];
	if (valid_compound_scene_operation(p_operation)) {
		return true;
	}
	if (kind == "create_resource") {
		return has_only_keys(p_operation, { "kind", "alias", "path", "resource_class", "properties" }) && p_operation.size() == 5 &&
				p_operation["alias"].get_type() == Variant::STRING && valid_alias(p_operation["alias"]) &&
				p_operation["path"].get_type() == Variant::STRING && valid_project_path(p_operation["path"], ".tres") &&
				p_operation["resource_class"].get_type() == Variant::STRING && valid_resource_class(p_operation["resource_class"]) &&
				p_operation["properties"].get_type() == Variant::ARRAY && valid_stored_properties(p_operation["properties"]);
	}
	if (kind == "update_resource") {
		if (!has_only_keys(p_operation, { "kind", "resource", "resolved_path", "expected_hash", "properties" }) || (p_operation.size() != 4 && p_operation.size() != 5) || p_operation["resource"].get_type() != Variant::STRING || p_operation["expected_hash"].get_type() != Variant::STRING || !valid_digest(p_operation["expected_hash"]) || p_operation["properties"].get_type() != Variant::ARRAY || Array(p_operation["properties"]).is_empty() || !valid_stored_properties(p_operation["properties"])) {
			return false;
		}
		const String resource = p_operation["resource"];
		if (valid_alias(resource)) {
			return !p_operation.has("resolved_path");
		}
		return (resource.begins_with("godot:resource:uid:v1:") || resource.begins_with("godot:resource:path-content:v1:")) &&
				p_operation.has("resolved_path") && p_operation["resolved_path"].get_type() == Variant::STRING && valid_project_path(p_operation["resolved_path"], ".tres");
	}
	if (kind == "update_gdscript") {
		if (!has_only_keys(p_operation, { "kind", "path", "expected_hash", "edits" }) || p_operation.size() != 4 || p_operation["path"].get_type() != Variant::STRING || !valid_project_path(p_operation["path"], ".gd") || p_operation["expected_hash"].get_type() != Variant::STRING || !valid_digest(p_operation["expected_hash"]) || p_operation["edits"].get_type() != Variant::ARRAY) {
			return false;
		}
		const Array edits = p_operation["edits"];
		if (edits.is_empty() || edits.size() > 64) {
			return false;
		}
		int64_t previous_end = 0;
		for (int index = 0; index < edits.size(); index++) {
			if (edits[index].get_type() != Variant::DICTIONARY) {
				return false;
			}
			const Dictionary edit = edits[index];
			if (!has_only_keys(edit, { "start_byte", "end_byte", "replacement" }) || edit.size() != 3 || edit["replacement"].get_type() != Variant::STRING) {
				return false;
			}
			int64_t start = 0;
			int64_t end = 0;
			if (!valid_safe_integer(edit["start_byte"], start) || !valid_safe_integer(edit["end_byte"], end) || start < previous_end || end < start || String(edit["replacement"]).utf8().length() > 65536) {
				return false;
			}
			previous_end = end;
		}
		return true;
	}
	return false;
}

static Dictionary normalize_operation(const Dictionary &p_operation) {
	if (BridgeTransactionProfile::validate_operation(p_operation)) {
		return BridgeTransactionCanonicalizer::normalize_operation(p_operation);
	}
	if (valid_compound_scene_operation(p_operation)) {
		Dictionary normalized = BridgeTransactionCanonicalizer::normalize_operation(sanitize_compound_scene_operation(p_operation));
		if (p_operation.has("alias")) {
			normalized["alias"] = p_operation["alias"];
		}
		for (const char *key : { "node_id", "parent_node_id", "new_parent_node_id", "emitter_node_id", "receiver_node_id" }) {
			if (p_operation.has(key) && p_operation[key].get_type() == Variant::STRING && valid_alias(p_operation[key])) {
				normalized[key] = p_operation[key];
			}
		}
		return normalized;
	}
	Dictionary normalized = p_operation.duplicate(true);
	const String kind = normalized["kind"];
	if (kind == "create_resource" || kind == "update_resource") {
		Array properties = normalized["properties"];
		for (int index = 0; index < properties.size(); index++) {
			Dictionary property = properties[index];
			property["value"] = BridgeTransactionCanonicalizer::normalize_wire_variant(property["value"]);
			properties[index] = property;
		}
		normalized["properties"] = properties;
	} else if (kind == "update_gdscript") {
		Array edits = normalized["edits"];
		for (int index = 0; index < edits.size(); index++) {
			Dictionary edit = edits[index];
			edit["start_byte"] = (int64_t)edit["start_byte"];
			edit["end_byte"] = (int64_t)edit["end_byte"];
			edits[index] = edit;
		}
		normalized["edits"] = edits;
	}
	return normalized;
}

static String write_key(const Dictionary &p_operation) {
	const String kind = p_operation["kind"];
	if (kind == "set_property") {
		return "property:" + String(p_operation["node_id"]) + ":" + String(p_operation["property"]);
	}
	if (kind == "create_resource") {
		return "file:" + String(p_operation["path"]);
	}
	if (kind == "update_resource") {
		return "resource:" + String(p_operation["resource"]);
	}
	if (kind == "update_gdscript") {
		return "file:" + String(p_operation["path"]);
	}
	if (kind == "create_node") {
		return "children:" + String(p_operation["parent_node_id"]) + ":" + String(p_operation["name"]);
	}
	if (kind == "reparent_node" || kind == "delete_node" || kind == "attach_script" || kind == "detach_script") {
		return "node:" + String(p_operation["node_id"]);
	}
	if (kind == "connect_signal" || kind == "disconnect_signal") {
		return vformat("signal:%s:%s:%s:%s", String(p_operation["emitter_node_id"]), String(p_operation["signal"]), String(p_operation["receiver_node_id"]), String(p_operation["method"]));
	}
	return String();
}

static Dictionary redacted_operation(const Dictionary &p_operation) {
	Dictionary redacted;
	const String kind = p_operation["kind"];
	redacted["kind"] = kind;
	for (const char *key : { "alias", "path", "resolved_path", "resource_class", "resource", "node_id", "parent_node_id", "new_parent_node_id", "property", "emitter_node_id", "signal", "receiver_node_id", "method", "name", "godot_type" }) {
		if (p_operation.has(key)) {
			redacted[key] = p_operation[key];
		}
	}
	String digest;
	BridgeTransactionCanonicalizer::sha256_utf8(JSON::stringify(p_operation, "", true, true), digest, "godot-codex-change-set-operation/v1\n");
	redacted["operation_digest"] = digest;
	if (kind == "create_resource" || kind == "update_resource") {
		redacted["property_count"] = Array(p_operation["properties"]).size();
	} else if (kind == "update_gdscript") {
		redacted["edit_count"] = Array(p_operation["edits"]).size();
	}
	return redacted;
}

static bool validation_policy_valid(const Dictionary &p_policy) {
	if (!has_only_keys(p_policy, { "rollback", "warnings", "runtime", "runtime_timeout_ms" }) || p_policy.size() < 3 || p_policy.size() > 4 || p_policy["rollback"].get_type() != Variant::STRING || p_policy["warnings"].get_type() != Variant::STRING || p_policy["runtime"].get_type() != Variant::STRING) {
		return false;
	}
	const String rollback = p_policy["rollback"];
	const String warnings = p_policy["warnings"];
	const String runtime = p_policy["runtime"];
	if (rollback != "never" && rollback != "on_required_failure" && rollback != "on_any_failure") {
		return false;
	}
	if (warnings != "allow" && warnings != "fail_on_introduced") {
		return false;
	}
	if (runtime != "skip" && runtime != "run_current_scene" && runtime != "run_project") {
		return false;
	}
	if (p_policy.has("runtime_timeout_ms")) {
		int64_t runtime_timeout_ms = 0;
		if (!valid_safe_integer(p_policy["runtime_timeout_ms"], runtime_timeout_ms) || runtime_timeout_ms < 250 || runtime_timeout_ms > 30000) {
			return false;
		}
	}
	return true;
}

} // namespace

bool CompoundChangeSetPlanner::validate_params(const Dictionary &p_params) {
	if (!has_only_keys(p_params, { "idempotency_key", "coordinates", "operations", "save_scope", "validation_policy" }) || p_params.size() != 5 || p_params["idempotency_key"].get_type() != Variant::STRING || p_params["coordinates"].get_type() != Variant::DICTIONARY || p_params["operations"].get_type() != Variant::ARRAY || p_params["save_scope"].get_type() != Variant::DICTIONARY || p_params["validation_policy"].get_type() != Variant::DICTIONARY) {
		return false;
	}
	const String idempotency = p_params["idempotency_key"];
	if (!idempotency.begins_with("idempotency:") || idempotency.length() != 44) {
		return false;
	}
	const Dictionary coordinates = p_params["coordinates"];
	for (const char *key : { "editor_session_id", "scene_id", "scene_revision", "operation_seq", "resource_revision", "script_graph_revision" }) {
		if (!coordinates.has(key)) {
			return false;
		}
	}
	if (coordinates.size() != 6 || coordinates["editor_session_id"].get_type() != Variant::STRING || coordinates["scene_id"].get_type() != Variant::STRING) {
		return false;
	}
	for (const char *key : { "scene_revision", "operation_seq", "resource_revision", "script_graph_revision" }) {
		int64_t coordinate = 0;
		if (!valid_safe_integer(coordinates[key], coordinate)) {
			return false;
		}
	}
	const Array operations = p_params["operations"];
	if (operations.is_empty() || operations.size() > MAX_OPERATIONS) {
		return false;
	}
	for (int index = 0; index < operations.size(); index++) {
		if (operations[index].get_type() != Variant::DICTIONARY) {
			return false;
		}
		const Dictionary operation = operations[index];
		if (!BridgeTransactionProfile::validate_operation(operation) && !valid_new_operation(operation)) {
			return false;
		}
	}
	const Dictionary save_scope = p_params["save_scope"];
	if (!has_only_keys(save_scope, { "paths" }) || save_scope.size() != 1 || save_scope["paths"].get_type() != Variant::ARRAY) {
		return false;
	}
	const Array paths = save_scope["paths"];
	if (paths.size() > MAX_SAVE_PATHS) {
		return false;
	}
	HashSet<String> unique_paths;
	for (int index = 0; index < paths.size(); index++) {
		if (paths[index].get_type() != Variant::STRING || !valid_project_path(paths[index]) || unique_paths.has(paths[index])) {
			return false;
		}
		unique_paths.insert(paths[index]);
	}
	bool has_persistence_operation = false;
	bool has_scene_operation = false;
	for (int index = 0; index < operations.size(); index++) {
		const String kind = Dictionary(operations[index])["kind"];
		if (kind == "create_resource" || kind == "update_resource" || kind == "update_gdscript") {
			has_persistence_operation = true;
		} else {
			has_scene_operation = true;
		}
	}
	int scene_path_count = 0;
	for (int index = 0; index < paths.size(); index++) {
		if (String(paths[index]).ends_with(".tscn")) {
			scene_path_count++;
		}
	}
	if (scene_path_count > (has_scene_operation ? 1 : 0) ||
			(!has_persistence_operation && !paths.is_empty() && !(has_scene_operation && scene_path_count == paths.size())) ||
			(has_persistence_operation && paths.is_empty())) {
		return false;
	}
	return validation_policy_valid(p_params["validation_policy"]);
}

Error CompoundChangeSetPlanner::build(const String &p_project_id, const String &p_editor_session_id, const Dictionary &p_params, uint64_t p_now_ms, Plan &r_plan, String &r_error_code, String &r_error_message) {
	r_plan = Plan();
	r_error_code.clear();
	r_error_message.clear();
	if (!validate_params(p_params)) {
		r_error_code = "invalid_request";
		r_error_message = "The compound change-set parameters are invalid.";
		return ERR_INVALID_DATA;
	}
	const Dictionary coordinates = p_params["coordinates"];
	if (String(coordinates["editor_session_id"]) != p_editor_session_id) {
		r_error_code = "stale_editor_state";
		r_error_message = "The change set is bound to another editor session.";
		return ERR_INVALID_DATA;
	}

	const Array operations = p_params["operations"];
	HashMap<String, int> alias_producers;
	for (int index = 0; index < operations.size(); index++) {
		const Dictionary operation = operations[index];
		if (operation.has("alias")) {
			const String alias = operation["alias"];
			if (alias_producers.has(alias)) {
				r_error_code = "alias_conflict";
				r_error_message = "A plan-local alias is produced more than once.";
				return ERR_ALREADY_EXISTS;
			}
			alias_producers.insert(alias, index);
		}
	}

	Vector<HashSet<int>> dependencies;
	dependencies.resize(operations.size());
	HashSet<String> writes;
	HashSet<String> deleted_nodes;
	for (int index = 0; index < operations.size(); index++) {
		const Dictionary operation = operations[index];
		if (operation.has("resource") && operation["resource"].get_type() == Variant::STRING && String(operation["resource"]).begins_with("alias:")) {
			const String alias = operation["resource"];
			const int *producer = alias_producers.getptr(alias);
			if (!producer) {
				r_error_code = "alias_not_found";
				r_error_message = "A plan-local alias has no producer.";
				return ERR_DOES_NOT_EXIST;
			}
			if (String(Dictionary(operations[*producer])["kind"]) != "create_resource") {
				r_error_code = "alias_type_mismatch";
				r_error_message = "A resource reference must use an alias produced by create_resource.";
				return ERR_INVALID_DATA;
			}
			if (*producer == index) {
				r_error_code = "dependency_cycle";
				r_error_message = "An operation cannot reference its own plan-local alias.";
				return ERR_CYCLIC_LINK;
			}
			dependencies.write[index].insert(*producer);
		}
		for (const char *field : { "node_id", "parent_node_id", "new_parent_node_id", "emitter_node_id", "receiver_node_id" }) {
			if (!operation.has(field) || operation[field].get_type() != Variant::STRING) {
				continue;
			}
			const String reference = operation[field];
			if (deleted_nodes.has(reference)) {
				r_error_code = "deleted_node_referenced";
				r_error_message = "A later operation references a node already deleted by this change set.";
				return ERR_DOES_NOT_EXIST;
			}
			if (valid_alias(reference)) {
				const int *producer = alias_producers.getptr(reference);
				if (!producer) {
					r_error_code = "alias_not_found";
					r_error_message = "A plan-local node alias has no producer.";
					return ERR_DOES_NOT_EXIST;
				}
				if (String(Dictionary(operations[*producer])["kind"]) != "create_node") {
					r_error_code = "alias_type_mismatch";
					r_error_message = "A node reference must use an alias produced by create_node.";
					return ERR_INVALID_DATA;
				}
				if (*producer == index) {
					r_error_code = "dependency_cycle";
					r_error_message = "An operation cannot reference its own plan-local alias.";
					return ERR_CYCLIC_LINK;
				}
				dependencies.write[index].insert(*producer);
			}
		}
		if (String(operation["kind"]) == "delete_node") {
			deleted_nodes.insert(operation["node_id"]);
		}
		const String key = write_key(operation);
		if (!key.is_empty() && writes.has(key)) {
			r_error_code = "conflicting_writes";
			r_error_message = "Two operations target the same bounded write location.";
			return ERR_ALREADY_EXISTS;
		}
		writes.insert(key);
	}

	Vector<int> ordered_indices;
	HashSet<int> emitted;
	while (ordered_indices.size() < operations.size()) {
		bool progressed = false;
		for (int index = 0; index < operations.size(); index++) {
			if (emitted.has(index)) {
				continue;
			}
			bool ready = true;
			for (const int dependency : dependencies[index]) {
				if (!emitted.has(dependency)) {
					ready = false;
					break;
				}
			}
			if (ready) {
				emitted.insert(index);
				ordered_indices.push_back(index);
				progressed = true;
			}
		}
		if (!progressed) {
			r_error_code = "dependency_cycle";
			r_error_message = "The change-set dependency graph contains a cycle.";
			return ERR_CYCLIC_LINK;
		}
	}

	Array normalized_operations;
	Array original_order;
	Array preview_operations;
	String risk = "low";
	for (const int index : ordered_indices) {
		const Dictionary operation = normalize_operation(operations[index]);
		normalized_operations.push_back(operation);
		original_order.push_back(index);
		preview_operations.push_back(redacted_operation(operation));
		const String kind = operation["kind"];
		if (kind == "delete_node" || kind == "reparent_node" || kind == "set_property" || kind == "detach_script" || kind == "disconnect_signal" || kind == "update_resource" || kind == "update_gdscript") {
			risk = "destructive";
		}
	}

	Dictionary canonical_request;
	canonical_request["schema_version"] = "compound-change-set-request/1.0";
	canonical_request["project_id"] = p_project_id;
	canonical_request["editor_session_id"] = p_editor_session_id;
	canonical_request["coordinates"] = coordinates.duplicate(true);
	canonical_request["operations"] = normalized_operations;
	canonical_request["save_scope"] = Dictionary(p_params["save_scope"]).duplicate(true);
	canonical_request["validation_policy"] = Dictionary(p_params["validation_policy"]).duplicate(true);
	const String canonical_request_json = JSON::stringify(canonical_request, "", true, true);
	String request_digest;
	if (BridgeTransactionCanonicalizer::sha256_utf8(canonical_request_json, request_digest, "godot-codex-change-set-request/v1\n") != OK) {
		r_error_code = "internal_error";
		r_error_message = "The immutable change-set request digest could not be created.";
		return ERR_CANT_CREATE;
	}

	PackedByteArray id_bytes;
	if (BridgeCrypto::random_bytes(16, id_bytes) != OK) {
		r_error_code = "internal_error";
		r_error_message = "A change-set identifier could not be created.";
		return ERR_CANT_CREATE;
	}
	const String change_set_id = "change-set:" + BridgeCrypto::bytes_to_lower_hex(id_bytes);
	Array preconditions;
	preconditions.push_back("editor session and revision coordinates remain unchanged");
	preconditions.push_back("all operation preimages and exact file hashes remain unchanged");
	preconditions.push_back("save_scope remains explicit and writable without symlinks");
	preconditions.push_back("approval binding matches this immutable preview digest");
	Dictionary preview;
	preview["schema_version"] = "canonical-change-set-preview/1.0";
	preview["change_set_id"] = change_set_id;
	preview["request_digest"] = request_digest;
	preview["operations"] = preview_operations;
	preview["original_order"] = original_order;
	preview["preconditions"] = preconditions;
	preview["save_scope"] = Dictionary(p_params["save_scope"])["paths"];
	preview["validation_policy"] = Dictionary(p_params["validation_policy"]).duplicate(true);
	preview["risk"] = risk;
	preview["created_at_ms"] = (int64_t)p_now_ms;
	preview["expires_at_ms"] = (int64_t)(p_now_ms + PREPARED_TTL_MS);
	const String canonical_preview_json = JSON::stringify(preview, "", true, true);
	if (canonical_preview_json.utf8().length() > MAX_PREVIEW_BYTES) {
		r_error_code = "change_set_too_large";
		r_error_message = "The bounded redacted preview exceeds 64 KiB.";
		return ERR_OUT_OF_MEMORY;
	}
	String preview_digest;
	if (BridgeTransactionCanonicalizer::sha256_utf8(canonical_preview_json, preview_digest, "godot-codex-change-set-preview/v1\n") != OK) {
		r_error_code = "internal_error";
		r_error_message = "The immutable preview digest could not be created.";
		return ERR_CANT_CREATE;
	}

	r_plan.change_set_id = change_set_id;
	r_plan.idempotency_key = p_params["idempotency_key"];
	r_plan.request_digest = request_digest;
	r_plan.preview_digest = preview_digest;
	r_plan.canonical_request_json = canonical_request_json;
	r_plan.canonical_preview_json = canonical_preview_json;
	r_plan.ordered_operations = normalized_operations;
	r_plan.original_order = original_order;
	r_plan.preconditions = preconditions;
	r_plan.save_scope = Dictionary(p_params["save_scope"])["paths"];
	r_plan.validation_policy = Dictionary(p_params["validation_policy"]).duplicate(true);
	r_plan.risk = risk;
	r_plan.created_at_ms = p_now_ms;
	r_plan.expires_at_ms = p_now_ms + PREPARED_TTL_MS;
	return OK;
}
