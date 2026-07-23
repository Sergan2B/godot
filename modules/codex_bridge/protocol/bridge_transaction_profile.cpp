/**************************************************************************/
/*  bridge_transaction_profile.cpp                                        */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "bridge_transaction_profile.h"

#include "bridge_crypto.h"
#include "bridge_frame_codec.h"

#include "core/crypto/crypto_core.h"
#include "core/io/json.h"
#include "core/math/math_funcs.h"
#include "core/templates/hash_set.h"

#include <initializer_list>

namespace {

static constexpr int64_t MAX_SAFE_INTEGER = 9007199254740991LL;

static bool has_exact_keys(const Dictionary &p_value, std::initializer_list<const char *> p_keys) {
	if (p_value.size() != (int)p_keys.size()) {
		return false;
	}
	for (const char *key : p_keys) {
		if (!p_value.has(key)) {
			return false;
		}
	}
	return true;
}

static bool has_only_keys(const Dictionary &p_value, std::initializer_list<const char *> p_keys) {
	HashSet<String> allowed;
	for (const char *key : p_keys) {
		allowed.insert(key);
	}
	const Array keys = p_value.keys();
	for (int index = 0; index < keys.size(); index++) {
		if (keys[index].get_type() != Variant::STRING && keys[index].get_type() != Variant::STRING_NAME) {
			return false;
		}
		if (!allowed.has(String(keys[index]))) {
			return false;
		}
	}
	return true;
}

static bool get_string(const Dictionary &p_value, const StringName &p_key, String &r_value) {
	if (!p_value.has(p_key) || p_value[p_key].get_type() != Variant::STRING) {
		return false;
	}
	r_value = p_value[p_key];
	return true;
}

static bool get_integer(const Dictionary &p_value, const StringName &p_key, int64_t p_minimum, int64_t p_maximum, int64_t &r_value) {
	if (!p_value.has(p_key)) {
		return false;
	}
	const Variant value = p_value[p_key];
	if (value.get_type() == Variant::INT) {
		r_value = value;
		return r_value >= p_minimum && r_value <= p_maximum;
	}
	if (value.get_type() != Variant::FLOAT) {
		return false;
	}
	const double number = value;
	if (!Math::is_finite(number) || number < (double)p_minimum || number > (double)p_maximum) {
		return false;
	}
	r_value = (int64_t)number;
	return (double)r_value == number;
}

static bool is_prefixed_lower_hex_id(const String &p_value, const String &p_prefix) {
	if (!p_value.begins_with(p_prefix) || p_value.length() != p_prefix.length() + 32) {
		return false;
	}
	for (int index = p_prefix.length(); index < p_value.length(); index++) {
		const char32_t character = p_value[index];
		if (!((character >= '0' && character <= '9') || (character >= 'a' && character <= 'f'))) {
			return false;
		}
	}
	return true;
}

static bool is_digest(const String &p_value) {
	if (!p_value.begins_with("sha256:") || p_value.length() != 71) {
		return false;
	}
	for (int index = 7; index < p_value.length(); index++) {
		const char32_t character = p_value[index];
		if (!((character >= '0' && character <= '9') || (character >= 'a' && character <= 'f'))) {
			return false;
		}
	}
	return true;
}

static bool is_base64url_32(const String &p_value) {
	if (p_value.length() != 43) {
		return false;
	}
	for (int index = 0; index < p_value.length(); index++) {
		const char32_t character = p_value[index];
		if (!((character >= 'A' && character <= 'Z') || (character >= 'a' && character <= 'z') || (character >= '0' && character <= '9') || character == '_' || character == '-')) {
			return false;
		}
	}
	return true;
}

static bool is_bounded_string(const String &p_value, int p_minimum, int p_maximum, bool p_reject_controls = true) {
	if (p_value.length() < p_minimum || p_value.length() > p_maximum) {
		return false;
	}
	if (p_reject_controls) {
		for (int index = 0; index < p_value.length(); index++) {
			if (p_value[index] <= 0x1f) {
				return false;
			}
		}
	}
	return true;
}

static bool is_godot_type(const String &p_value) {
	if (!is_bounded_string(p_value, 1, 256) || !((p_value[0] >= 'A' && p_value[0] <= 'Z') || (p_value[0] >= 'a' && p_value[0] <= 'z') || p_value[0] == '_')) {
		return false;
	}
	for (int index = 1; index < p_value.length(); index++) {
		const char32_t character = p_value[index];
		if (!((character >= 'A' && character <= 'Z') || (character >= 'a' && character <= 'z') || (character >= '0' && character <= '9') || character == '_')) {
			return false;
		}
	}
	return true;
}

static bool is_node_name(const String &p_value) {
	if (!is_bounded_string(p_value, 1, 512)) {
		return false;
	}
	return !p_value.contains("/") && !p_value.contains("\\") && !p_value.contains(":");
}

static bool is_resource_ref(const Dictionary &p_value) {
	if (has_exact_keys(p_value, { "uid" })) {
		String uid;
		if (!get_string(p_value, "uid", uid) || !uid.begins_with("uid://") || uid.length() > 128 || uid.length() == 6) {
			return false;
		}
		for (int index = 6; index < uid.length(); index++) {
			const char32_t character = uid[index];
			if (!((character >= 'A' && character <= 'Z') || (character >= 'a' && character <= 'z') || (character >= '0' && character <= '9') || character == '_' || character == '-')) {
				return false;
			}
		}
		return true;
	}
	if (!has_exact_keys(p_value, { "uid_missing", "path" }) || p_value["uid_missing"].get_type() != Variant::BOOL || !(bool)p_value["uid_missing"]) {
		return false;
	}
	String path;
	if (!get_string(p_value, "path", path) || !path.begins_with("res://") || path.length() > 1024 || path.length() == 6 || path.contains("\\") || path.contains("?") || path.contains("#")) {
		return false;
	}
	const PackedStringArray components = path.substr(6).split("/", true);
	for (const String &component : components) {
		if (component.is_empty() || component == "." || component == ".." || !is_bounded_string(component, 1, 1024)) {
			return false;
		}
	}
	return true;
}

static bool encoded_within(const Dictionary &p_value, int64_t p_limit) {
	return JSON::stringify(p_value, "", true, true).utf8().length() <= p_limit;
}

static bool is_operation_kind(const String &p_value) {
	return p_value == "create_node" || p_value == "delete_node" || p_value == "reparent_node" || p_value == "set_property" || p_value == "attach_script" || p_value == "detach_script" || p_value == "connect_signal" || p_value == "disconnect_signal";
}

static bool is_scope(const String &p_value) {
	return p_value == "scene.node.create" || p_value == "scene.node.delete" || p_value == "scene.node.reparent" || p_value == "scene.property.set" || p_value == "scene.script.attach" || p_value == "scene.script.detach" || p_value == "scene.signal.connect" || p_value == "scene.signal.disconnect";
}

static bool is_state(const String &p_value) {
	return p_value == "preparing" || p_value == "previewed" || p_value == "awaiting_approval" || p_value == "applying" || p_value == "applied" || p_value == "validating" || p_value == "committed" || p_value == "undone" || p_value == "conflicted" || p_value == "expired" || p_value == "rejected" || p_value == "failed" || p_value == "failed_rolled_back" || p_value == "in_doubt";
}

static bool is_event_reason(const String &p_value) {
	return p_value == "prepared" || p_value == "approval_requested" || p_value == "approval_cancelled" || p_value == "approval_declined" || p_value == "apply_started" || p_value == "native_action_committed" || p_value == "validation_started" || p_value == "validation_succeeded" || p_value == "native_undo_observed" || p_value == "native_redo_observed" || p_value == "revision_conflict" || p_value == "prepared_expired" || p_value == "precommit_failed" || p_value == "postcondition_failed_rolled_back" || p_value == "reconciliation_required" || p_value == "reconciliation_committed" || p_value == "reconciliation_undone" || p_value == "reconciliation_rolled_back";
}

static bool is_safe_error_code(const String &p_value) {
	return p_value == "approval_required" || p_value == "approval_host_unsupported" || p_value == "approval_declined" || p_value == "approval_cancelled" || p_value == "approval_timeout" || p_value == "approval_invalid" || p_value == "approval_scope_mismatch" || p_value == "approval_replayed" || p_value == "preview_mismatch" || p_value == "transaction_not_found" || p_value == "transaction_expired" || p_value == "transaction_conflicted" || p_value == "transaction_busy" || p_value == "transaction_too_large" || p_value == "transaction_in_doubt" || p_value == "transaction_not_undoable" || p_value == "idempotency_conflict" || p_value == "stale_editor_state" || p_value == "stale_scene_revision" || p_value == "scene_not_open" || p_value == "scene_operation_unsupported" || p_value == "node_not_editable" || p_value == "node_ownership_invalid" || p_value == "property_not_writable" || p_value == "property_value_unsupported" || p_value == "script_incompatible" || p_value == "signal_connection_invalid" || p_value == "transaction_apply_failed" || p_value == "transaction_undo_failed";
}

static bool validate_affected_entity(const Dictionary &p_value) {
	String node_id;
	String role;
	return has_exact_keys(p_value, { "node_id", "role" }) && get_string(p_value, "node_id", node_id) && is_prefixed_lower_hex_id(node_id, "node:") && get_string(p_value, "role", role) && (role == "target" || role == "parent" || role == "new_parent" || role == "emitter" || role == "receiver" || role == "created");
}

static bool validate_preview(const Dictionary &p_value) {
	if (!has_exact_keys(p_value, { "operation_kind", "summary", "dirty_effect", "save_effect", "preconditions", "truncated" }) || p_value["preconditions"].get_type() != Variant::ARRAY || p_value["truncated"].get_type() != Variant::BOOL) {
		return false;
	}
	String text;
	if (!get_string(p_value, "operation_kind", text) || !is_operation_kind(text) || !get_string(p_value, "summary", text) || !is_bounded_string(text, 1, 8192, false) || !get_string(p_value, "dirty_effect", text) || text != "marks_scene_dirty" || !get_string(p_value, "save_effect", text) || text != "not_saved") {
		return false;
	}
	const Array preconditions = p_value["preconditions"];
	if (preconditions.size() > 64) {
		return false;
	}
	for (int index = 0; index < preconditions.size(); index++) {
		if (preconditions[index].get_type() != Variant::STRING || !is_bounded_string(preconditions[index], 1, 512, false)) {
			return false;
		}
	}
	return true;
}

static bool validate_undo_eligibility(const Dictionary &p_value) {
	String reason;
	return has_exact_keys(p_value, { "eligible", "reason" }) && p_value["eligible"].get_type() == Variant::BOOL && get_string(p_value, "reason", reason) && (reason == "eligible" || reason == "not_committed" || reason == "not_newest_action" || reason == "history_unavailable" || reason == "revision_mismatch");
}

static bool validate_safe_error(const Dictionary &p_value) {
	String code;
	String message;
	return has_exact_keys(p_value, { "code", "message", "retryable" }) && get_string(p_value, "code", code) && is_safe_error_code(code) && get_string(p_value, "message", message) && is_bounded_string(message, 1, 256, false) && p_value["retryable"].get_type() == Variant::BOOL;
}

static bool preview_digest_matches(const String &p_payload, const String &p_expected_digest) {
	Dictionary parsed;
	if (BridgeJson::parse_strict_object(p_payload.to_utf8_buffer(), parsed) != OK) {
		return false;
	}
	const CharString bytes = p_payload.utf8();
	PackedByteArray digest;
	digest.resize(32);
	return CryptoCore::sha256(reinterpret_cast<const uint8_t *>(bytes.get_data()), bytes.length(), digest.ptrw()) == OK && "sha256:" + BridgeCrypto::bytes_to_lower_hex(digest) == p_expected_digest;
}

static bool validate_writable_variant(const Dictionary &p_value, int p_depth, int &r_items) {
	if (p_depth > BridgeTransactionProfile::MAX_VARIANT_DEPTH) {
		return false;
	}
	String type;
	if (!get_string(p_value, "type", type)) {
		return false;
	}
	if (type == "nil") {
		return has_exact_keys(p_value, { "type" });
	}
	if (!has_exact_keys(p_value, { "type", "value" })) {
		return false;
	}
	const Variant value = p_value["value"];
	if (type == "bool") {
		return value.get_type() == Variant::BOOL;
	}
	if (type == "int") {
		if (value.get_type() == Variant::INT) {
			return (int64_t)value >= -MAX_SAFE_INTEGER && (int64_t)value <= MAX_SAFE_INTEGER;
		}
		if (value.get_type() != Variant::FLOAT) {
			return false;
		}
		const double number = value;
		if (!Math::is_finite(number) || number < (double)-MAX_SAFE_INTEGER || number > (double)MAX_SAFE_INTEGER) {
			return false;
		}
		return (double)(int64_t)number == number;
	}
	if (type == "float") {
		return (value.get_type() == Variant::FLOAT || value.get_type() == Variant::INT) && Math::is_finite((double)value);
	}
	if (type == "string" || type == "string_name" || type == "node_path") {
		if (value.get_type() != Variant::STRING || String(value).length() > BridgeTransactionProfile::MAX_STRING_CHARACTERS) {
			return false;
		}
		return type != "node_path" || !NodePath(String(value)).is_absolute();
	}
	if (type == "resource") {
		return value.get_type() == Variant::DICTIONARY && is_resource_ref(value);
	}
	int math_arity = -1;
	if (type == "vector2" || type == "vector2i") {
		math_arity = 2;
	} else if (type == "vector3" || type == "vector3i") {
		math_arity = 3;
	} else if (type == "vector4" || type == "vector4i" || type == "rect2" || type == "rect2i" || type == "plane" || type == "quaternion" || type == "color") {
		math_arity = 4;
	} else if (type == "transform2d" || type == "aabb") {
		math_arity = 6;
	} else if (type == "basis") {
		math_arity = 9;
	} else if (type == "transform3d") {
		math_arity = 12;
	} else if (type == "projection") {
		math_arity = 16;
	}
	if (math_arity >= 0) {
		if (value.get_type() != Variant::ARRAY) {
			return false;
		}
		const Array values = value;
		if (values.size() != math_arity) {
			return false;
		}
		for (int index = 0; index < values.size(); index++) {
			if ((values[index].get_type() != Variant::INT && values[index].get_type() != Variant::FLOAT) || !Math::is_finite((double)values[index])) {
				return false;
			}
			if ((type == "vector2i" || type == "vector3i" || type == "vector4i" || type == "rect2i") && (double)(int64_t)(double)values[index] != (double)values[index]) {
				return false;
			}
		}
		return true;
	}
	if (type != "array" && type != "dictionary") {
		return false;
	}
	if (value.get_type() != Variant::ARRAY) {
		return false;
	}
	const Array values = value;
	r_items += values.size();
	if (r_items > BridgeTransactionProfile::MAX_CONTAINER_ITEMS) {
		return false;
	}
	HashSet<String> dictionary_keys;
	for (int index = 0; index < values.size(); index++) {
		if (values[index].get_type() != Variant::DICTIONARY) {
			return false;
		}
		Dictionary child = values[index];
		if (type == "dictionary") {
			String key;
			if (!has_exact_keys(child, { "key", "value" }) || !get_string(child, "key", key) || key.length() > BridgeTransactionProfile::MAX_STRING_CHARACTERS || child["value"].get_type() != Variant::DICTIONARY) {
				return false;
			}
			if (dictionary_keys.has(key)) {
				return false;
			}
			dictionary_keys.insert(key);
			child = child["value"];
		}
		if (!validate_writable_variant(child, p_depth + 1, r_items)) {
			return false;
		}
	}
	return true;
}

static bool validate_revision_coordinates(const Dictionary &p_value, bool p_transaction) {
	if (p_transaction ? !has_exact_keys(p_value, { "transaction_id", "scene_id", "history_id", "scene_revision", "operation_seq", "transaction_seq" }) : !has_exact_keys(p_value, { "scene_id", "history_id", "scene_revision", "operation_seq" })) {
		return false;
	}
	String scene_id;
	String history_id;
	int64_t number = 0;
	if (!get_string(p_value, "scene_id", scene_id) || !is_prefixed_lower_hex_id(scene_id, "scene:") || !get_string(p_value, "history_id", history_id) || !is_prefixed_lower_hex_id(history_id, "history:") || !get_integer(p_value, "scene_revision", 0, MAX_SAFE_INTEGER, number) || !get_integer(p_value, "operation_seq", 0, MAX_SAFE_INTEGER, number)) {
		return false;
	}
	if (!p_transaction) {
		return true;
	}
	String transaction_id;
	return get_string(p_value, "transaction_id", transaction_id) && is_prefixed_lower_hex_id(transaction_id, "transaction:") && get_integer(p_value, "transaction_seq", 1, MAX_SAFE_INTEGER, number);
}

static bool validate_limits(const Dictionary &p_value) {
	if (!has_exact_keys(p_value, { "operations", "prepared_records", "applying_per_history", "prepared_ttl_ms", "approval_timeout_ms", "receipt_ttl_ms", "clock_skew_ms", "approval_message_bytes", "preview_bytes", "operation_bytes", "variant_depth", "container_items", "string_characters", "structural_nodes", "journal_records", "journal_bytes", "status_bytes", "request_deadline_ms" })) {
		return false;
	}
	const Dictionary expected = BridgeTransactionProfile::make_limits();
	const Array keys = expected.keys();
	for (int index = 0; index < keys.size(); index++) {
		int64_t actual = 0;
		if (!get_integer(p_value, keys[index], (int64_t)expected[keys[index]], (int64_t)expected[keys[index]], actual)) {
			return false;
		}
	}
	return true;
}

static bool validate_revision_vector(const Dictionary &p_value) {
	if (!p_value.has("editor_session_id") || !p_value.has("event_seq") || !p_value.has("project_revision") || !p_value.has("operation_seq") || !p_value.has("scene_revisions") || p_value["scene_revisions"].get_type() != Variant::DICTIONARY) {
		return false;
	}
	String editor_session_id;
	int64_t number = 0;
	if (!get_string(p_value, "editor_session_id", editor_session_id) || !is_prefixed_lower_hex_id(editor_session_id, "editor:") || !get_integer(p_value, "event_seq", 0, MAX_SAFE_INTEGER, number) || !get_integer(p_value, "project_revision", 0, MAX_SAFE_INTEGER, number) || !get_integer(p_value, "operation_seq", 0, MAX_SAFE_INTEGER, number)) {
		return false;
	}
	for (const char *optional_revision : { "resource_revision", "scene_graph_revision", "script_graph_revision" }) {
		if (p_value.has(optional_revision) && !get_integer(p_value, optional_revision, 0, MAX_SAFE_INTEGER, number)) {
			return false;
		}
	}
	if (p_value.has("runtime_event_seq") && !get_integer(p_value, "runtime_event_seq", 1, MAX_SAFE_INTEGER, number)) {
		return false;
	}
	if (p_value.has("runtime_session_id")) {
		String runtime_session_id;
		if (!get_string(p_value, "runtime_session_id", runtime_session_id) || !is_prefixed_lower_hex_id(runtime_session_id, "runtime:")) {
			return false;
		}
	}
	const Dictionary scene_revisions = p_value["scene_revisions"];
	const Array scene_ids = scene_revisions.keys();
	for (int index = 0; index < scene_ids.size(); index++) {
		const String scene_id = scene_ids[index];
		if (!is_prefixed_lower_hex_id(scene_id, "scene:") || !get_integer(scene_revisions, scene_id, 0, MAX_SAFE_INTEGER, number)) {
			return false;
		}
	}
	return true;
}

} // namespace

Dictionary BridgeTransactionProfile::make_limits() {
	Dictionary limits;
	limits["operations"] = (int64_t)1;
	limits["prepared_records"] = (int64_t)64;
	limits["applying_per_history"] = (int64_t)1;
	limits["prepared_ttl_ms"] = (int64_t)300000;
	limits["approval_timeout_ms"] = (int64_t)120000;
	limits["receipt_ttl_ms"] = (int64_t)30000;
	limits["clock_skew_ms"] = (int64_t)2000;
	limits["approval_message_bytes"] = (int64_t)8192;
	limits["preview_bytes"] = (int64_t)65536;
	limits["operation_bytes"] = (int64_t)65536;
	limits["variant_depth"] = (int64_t)8;
	limits["container_items"] = (int64_t)1000;
	limits["string_characters"] = (int64_t)16384;
	limits["structural_nodes"] = (int64_t)1000;
	limits["journal_records"] = (int64_t)1024;
	limits["journal_bytes"] = (int64_t)8388608;
	limits["status_bytes"] = (int64_t)65536;
	limits["request_deadline_ms"] = (int64_t)5000;
	return limits;
}

Dictionary BridgeTransactionProfile::make_unavailable_capability() {
	Dictionary readiness;
	readiness["scene_state"] = "not_evaluated";
	readiness["approval_state"] = "unavailable";
	readiness["busy"] = false;
	readiness["reason"] = "approval_unavailable";
	Dictionary capability;
	capability["name"] = "transaction.scene_v1";
	capability["version"] = "1.0";
	capability["readiness"] = "unavailable";
	capability["transaction_readiness"] = readiness;
	capability["limits"] = make_limits();
	return capability;
}

Dictionary BridgeTransactionProfile::make_ready_capability(bool p_busy, bool p_scene_available, bool p_approval_available, bool p_coordinator_available) {
	String reason = "ready";
	if (!p_coordinator_available) {
		reason = "transaction_coordinator_unavailable";
	} else if (!p_approval_available) {
		reason = "approval_unavailable";
	} else if (!p_scene_available) {
		reason = "scene_not_open";
	} else if (p_busy) {
		reason = "transaction_busy";
	}
	const bool ready = reason == "ready";
	Dictionary readiness;
	readiness["scene_state"] = p_coordinator_available ? (p_scene_available ? "available" : "unavailable") : "not_evaluated";
	readiness["approval_state"] = p_approval_available ? "available" : "unavailable";
	readiness["busy"] = p_busy;
	readiness["reason"] = reason;
	Dictionary capability;
	capability["name"] = "transaction.scene_v1";
	capability["version"] = "1.0";
	capability["readiness"] = ready ? "ready" : "unavailable";
	capability["transaction_readiness"] = readiness;
	capability["limits"] = make_limits();
	return capability;
}

void BridgeTransactionProfile::append_global_limits(Dictionary &r_limits) {
	const Dictionary limits = make_limits();
	const Array keys = limits.keys();
	for (int index = 0; index < keys.size(); index++) {
		r_limits["transaction_" + String(keys[index])] = limits[keys[index]];
	}
}

bool BridgeTransactionProfile::validate_operation(const Dictionary &p_operation) {
	if (!encoded_within(p_operation, MAX_OPERATION_BYTES)) {
		return false;
	}
	String kind;
	if (!get_string(p_operation, "kind", kind) || !is_operation_kind(kind)) {
		return false;
	}
	String first;
	String second;
	int64_t number = 0;
	if (kind == "create_node") {
		if (!has_only_keys(p_operation, { "kind", "parent_node_id", "godot_type", "name", "insertion_index" }) || p_operation.size() < 4) {
			return false;
		}
		if (!get_string(p_operation, "parent_node_id", first) || !is_prefixed_lower_hex_id(first, "node:") || !get_string(p_operation, "godot_type", first) || !is_godot_type(first) || !get_string(p_operation, "name", first) || !is_node_name(first)) {
			return false;
		}
		return !p_operation.has("insertion_index") || get_integer(p_operation, "insertion_index", 0, INT32_MAX, number);
	}
	if (kind == "delete_node" || kind == "detach_script") {
		return has_exact_keys(p_operation, { "kind", "node_id" }) && get_string(p_operation, "node_id", first) && is_prefixed_lower_hex_id(first, "node:");
	}
	if (kind == "reparent_node") {
		return has_exact_keys(p_operation, { "kind", "node_id", "new_parent_node_id", "insertion_index", "keep_global_transform" }) && get_string(p_operation, "node_id", first) && is_prefixed_lower_hex_id(first, "node:") && get_string(p_operation, "new_parent_node_id", second) && is_prefixed_lower_hex_id(second, "node:") && get_integer(p_operation, "insertion_index", 0, INT32_MAX, number) && p_operation["keep_global_transform"].get_type() == Variant::BOOL;
	}
	if (kind == "set_property") {
		int items = 0;
		return has_exact_keys(p_operation, { "kind", "node_id", "property", "value" }) && get_string(p_operation, "node_id", first) && is_prefixed_lower_hex_id(first, "node:") && get_string(p_operation, "property", first) && is_bounded_string(first, 1, 512) && p_operation["value"].get_type() == Variant::DICTIONARY && validate_writable_variant(p_operation["value"], 1, items);
	}
	if (kind == "attach_script") {
		return has_exact_keys(p_operation, { "kind", "node_id", "script_ref" }) && get_string(p_operation, "node_id", first) && is_prefixed_lower_hex_id(first, "node:") && p_operation["script_ref"].get_type() == Variant::DICTIONARY && is_resource_ref(p_operation["script_ref"]);
	}
	if (!has_exact_keys(p_operation, { "kind", "emitter_node_id", "signal", "receiver_node_id", "method", "flags", "unbinds", "binds" }) || !get_string(p_operation, "emitter_node_id", first) || !is_prefixed_lower_hex_id(first, "node:") || !get_string(p_operation, "receiver_node_id", second) || !is_prefixed_lower_hex_id(second, "node:") || !get_string(p_operation, "signal", first) || !is_bounded_string(first, 1, 512) || !get_string(p_operation, "method", first) || !is_bounded_string(first, 1, 512) || !get_integer(p_operation, "flags", 2, 7, number) || (number & 2) == 0 || (number & ~7) != 0 || !get_integer(p_operation, "unbinds", 0, MAX_CONTAINER_ITEMS, number) || p_operation["binds"].get_type() != Variant::ARRAY) {
		return false;
	}
	const Array binds = p_operation["binds"];
	int items = binds.size();
	if (items > MAX_CONTAINER_ITEMS) {
		return false;
	}
	for (int index = 0; index < binds.size(); index++) {
		if (binds[index].get_type() != Variant::DICTIONARY || !validate_writable_variant(binds[index], 1, items)) {
			return false;
		}
	}
	return true;
}

bool BridgeTransactionProfile::validate_prepare_params(const Dictionary &p_params) {
	if (!has_exact_keys(p_params, { "idempotency_key", "coordinates", "operation" }) || !encoded_within(p_params, MAX_OPERATION_BYTES)) {
		return false;
	}
	String idempotency_key;
	return get_string(p_params, "idempotency_key", idempotency_key) && is_prefixed_lower_hex_id(idempotency_key, "idempotency:") && p_params["coordinates"].get_type() == Variant::DICTIONARY && validate_revision_coordinates(p_params["coordinates"], false) && p_params["operation"].get_type() == Variant::DICTIONARY && validate_operation(p_params["operation"]);
}

bool BridgeTransactionProfile::validate_apply_params(const Dictionary &p_params) {
	if (!has_exact_keys(p_params, { "transaction_id", "preview_digest", "expected_scene_revision", "expected_operation_seq", "approval" }) || !encoded_within(p_params, MAX_OPERATION_BYTES) || p_params["approval"].get_type() != Variant::DICTIONARY) {
		return false;
	}
	String transaction_id;
	String digest;
	int64_t number = 0;
	const Dictionary approval = p_params["approval"];
	String kind;
	String scope;
	String nonce;
	String mac;
	int64_t issued_at = 0;
	int64_t expires_at = 0;
	return get_string(p_params, "transaction_id", transaction_id) && is_prefixed_lower_hex_id(transaction_id, "transaction:") && get_string(p_params, "preview_digest", digest) && is_digest(digest) && get_integer(p_params, "expected_scene_revision", 0, MAX_SAFE_INTEGER, number) && get_integer(p_params, "expected_operation_seq", 0, MAX_SAFE_INTEGER, number) && has_exact_keys(approval, { "kind", "scope", "nonce", "issued_at_ms", "expires_at_ms", "mac" }) && get_string(approval, "kind", kind) && kind == "mcp_form_v1" && get_string(approval, "scope", scope) && is_scope(scope) && get_string(approval, "nonce", nonce) && is_base64url_32(nonce) && get_integer(approval, "issued_at_ms", 0, MAX_SAFE_INTEGER, issued_at) && get_integer(approval, "expires_at_ms", 0, MAX_SAFE_INTEGER, expires_at) && expires_at >= issued_at && expires_at - issued_at <= 30000 && get_string(approval, "mac", mac) && is_base64url_32(mac);
}

bool BridgeTransactionProfile::validate_status_params(const Dictionary &p_params) {
	String transaction_id;
	return has_exact_keys(p_params, { "transaction_id" }) && get_string(p_params, "transaction_id", transaction_id) && is_prefixed_lower_hex_id(transaction_id, "transaction:");
}

bool BridgeTransactionProfile::validate_undo_params(const Dictionary &p_params) {
	if (!has_exact_keys(p_params, { "transaction_id", "expected_transaction_seq", "expected_scene_revision", "expected_operation_seq" })) {
		return false;
	}
	String transaction_id;
	int64_t number = 0;
	return get_string(p_params, "transaction_id", transaction_id) && is_prefixed_lower_hex_id(transaction_id, "transaction:") && get_integer(p_params, "expected_transaction_seq", 1, MAX_SAFE_INTEGER, number) && get_integer(p_params, "expected_scene_revision", 0, MAX_SAFE_INTEGER, number) && get_integer(p_params, "expected_operation_seq", 0, MAX_SAFE_INTEGER, number);
}

bool BridgeTransactionProfile::validate_prepare_result(const Dictionary &p_result) {
	if (!has_exact_keys(p_result, { "schema_version", "coordinates", "state", "operation_kind", "risk", "scope", "affected_entities", "preview", "preview_payload_json", "preview_digest", "created_at_ms", "expires_at_ms", "limits_applied" }) || !encoded_within(p_result, MAX_STATUS_BYTES)) {
		return false;
	}
	String value;
	String preview_payload;
	String preview_digest;
	int64_t created_at = 0;
	int64_t expires_at = 0;
	if (!get_string(p_result, "schema_version", value) || value != "transaction/1.0" || p_result["coordinates"].get_type() != Variant::DICTIONARY || !validate_revision_coordinates(p_result["coordinates"], true) || !get_string(p_result, "state", value) || value != "previewed" || !get_string(p_result, "operation_kind", value) || !is_operation_kind(value) || !get_string(p_result, "risk", value) || (value != "write" && value != "destructive") || !get_string(p_result, "scope", value) || !is_scope(value) || p_result["affected_entities"].get_type() != Variant::ARRAY || p_result["preview"].get_type() != Variant::DICTIONARY || !validate_preview(p_result["preview"]) || !get_string(p_result, "preview_payload_json", preview_payload) || preview_payload.utf8().length() < 2 || preview_payload.utf8().length() > MAX_STATUS_BYTES || !get_string(p_result, "preview_digest", preview_digest) || !is_digest(preview_digest) || !preview_digest_matches(preview_payload, preview_digest) || !get_integer(p_result, "created_at_ms", 0, MAX_SAFE_INTEGER, created_at) || !get_integer(p_result, "expires_at_ms", 0, MAX_SAFE_INTEGER, expires_at) || expires_at < created_at || p_result["limits_applied"].get_type() != Variant::DICTIONARY || !validate_limits(p_result["limits_applied"])) {
		return false;
	}
	return validate_affected_entities(p_result["affected_entities"]);
}

bool BridgeTransactionProfile::validate_status_result(const Dictionary &p_result) {
	if (!has_only_keys(p_result, { "schema_version", "coordinates", "state", "operation_kind", "risk", "scope", "preview_digest", "current_scene_revision", "current_operation_seq", "outcome", "error", "undo_eligibility", "committed_entities", "updated_at_ms", "limits_applied", "truncated" }) || p_result.size() < 13 || !encoded_within(p_result, MAX_STATUS_BYTES)) {
		return false;
	}
	String value;
	int64_t number = 0;
	if (!get_string(p_result, "schema_version", value) || value != "transaction/1.0" || p_result["coordinates"].get_type() != Variant::DICTIONARY || !validate_revision_coordinates(p_result["coordinates"], true) || !get_string(p_result, "state", value) || !is_state(value) || !get_string(p_result, "operation_kind", value) || !is_operation_kind(value) || !get_string(p_result, "risk", value) || (value != "write" && value != "destructive") || !get_string(p_result, "scope", value) || !is_scope(value) || !get_string(p_result, "preview_digest", value) || !is_digest(value) || !get_integer(p_result, "current_scene_revision", 0, MAX_SAFE_INTEGER, number) || !get_integer(p_result, "current_operation_seq", 0, MAX_SAFE_INTEGER, number) || p_result["undo_eligibility"].get_type() != Variant::DICTIONARY || !validate_undo_eligibility(p_result["undo_eligibility"]) || !get_integer(p_result, "updated_at_ms", 0, MAX_SAFE_INTEGER, number) || p_result["limits_applied"].get_type() != Variant::DICTIONARY || !validate_limits(p_result["limits_applied"]) || p_result["truncated"].get_type() != Variant::BOOL) {
		return false;
	}
	if (p_result.has("outcome")) {
		if (!get_string(p_result, "outcome", value) || (value != "none" && value != "applied" && value != "committed" && value != "undone" && value != "rolled_back" && value != "unknown")) {
			return false;
		}
	}
	if (p_result.has("committed_entities")) {
		if (p_result["committed_entities"].get_type() != Variant::ARRAY || !validate_affected_entities(p_result["committed_entities"])) {
			return false;
		}
		const String state = p_result["state"];
		if (state != "committed" && state != "undone") {
			return false;
		}
	}
	return !p_result.has("error") || (p_result["error"].get_type() == Variant::DICTIONARY && validate_safe_error(p_result["error"]));
}

bool BridgeTransactionProfile::validate_affected_entities(const Array &p_entities, bool p_allow_empty) {
	if ((!p_allow_empty && p_entities.is_empty()) || p_entities.size() > 16) {
		return false;
	}
	for (int index = 0; index < p_entities.size(); index++) {
		if (p_entities[index].get_type() != Variant::DICTIONARY || !validate_affected_entity(p_entities[index])) {
			return false;
		}
	}
	return true;
}

bool BridgeTransactionProfile::validate_event_params(const Dictionary &p_params) {
	if (!has_exact_keys(p_params, { "coordinates", "previous_state", "state", "reason", "revisions", "timestamp_ms" }) || p_params["coordinates"].get_type() != Variant::DICTIONARY || !validate_revision_coordinates(p_params["coordinates"], true) || p_params["revisions"].get_type() != Variant::DICTIONARY || !validate_revision_vector(p_params["revisions"])) {
		return false;
	}
	String previous;
	String state;
	String reason;
	int64_t timestamp = 0;
	const bool previous_valid = p_params["previous_state"].get_type() == Variant::NIL || (get_string(p_params, "previous_state", previous) && is_state(previous));
	return previous_valid && get_string(p_params, "state", state) && is_state(state) && get_string(p_params, "reason", reason) && is_event_reason(reason) && get_integer(p_params, "timestamp_ms", 0, MAX_SAFE_INTEGER, timestamp) && (p_params["previous_state"].get_type() == Variant::NIL || is_legal_transition(previous, state));
}

bool BridgeTransactionProfile::is_legal_transition(const String &p_previous_state, const String &p_state) {
	if (p_previous_state == "preparing") {
		return p_state == "previewed" || p_state == "failed";
	}
	if (p_previous_state == "previewed") {
		return p_state == "awaiting_approval" || p_state == "conflicted" || p_state == "expired" || p_state == "rejected";
	}
	if (p_previous_state == "awaiting_approval") {
		return p_state == "applying" || p_state == "conflicted" || p_state == "expired" || p_state == "rejected";
	}
	if (p_previous_state == "applying") {
		return p_state == "applied" || p_state == "failed" || p_state == "failed_rolled_back" || p_state == "in_doubt";
	}
	if (p_previous_state == "applied") {
		return p_state == "validating";
	}
	if (p_previous_state == "validating") {
		return p_state == "committed" || p_state == "failed_rolled_back" || p_state == "in_doubt";
	}
	if (p_previous_state == "committed") {
		return p_state == "undone";
	}
	if (p_previous_state == "undone") {
		return p_state == "committed";
	}
	if (p_previous_state == "in_doubt") {
		return p_state == "committed" || p_state == "undone" || p_state == "failed_rolled_back";
	}
	return false;
}
