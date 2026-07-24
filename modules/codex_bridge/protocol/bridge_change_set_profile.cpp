/**************************************************************************/
/*  bridge_change_set_profile.cpp                                         */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#include "bridge_change_set_profile.h"

#include "modules/codex_bridge/editor/compound_change_set_planner.h"

namespace {

static constexpr int64_t MAX_SAFE_INTEGER = 9007199254740991;

static bool exact_keys(const Dictionary &p_value, std::initializer_list<const char *> p_keys) {
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

static bool prefixed_hex(const Variant &p_value, const String &p_prefix, int p_hex_length = 32) {
	if (p_value.get_type() != Variant::STRING) {
		return false;
	}
	const String value = p_value;
	if (!value.begins_with(p_prefix) || value.length() != p_prefix.length() + p_hex_length) {
		return false;
	}
	for (int index = p_prefix.length(); index < value.length(); index++) {
		const char32_t character = value[index];
		if (!((character >= '0' && character <= '9') || (character >= 'a' && character <= 'f'))) {
			return false;
		}
	}
	return true;
}

static bool safe_integer(const Variant &p_value, bool p_nonzero = false) {
	if (p_value.get_type() != Variant::INT) {
		return false;
	}
	const int64_t value = p_value;
	return value >= (p_nonzero ? 1 : 0) && value <= MAX_SAFE_INTEGER;
}

static bool digest(const Variant &p_value) {
	return prefixed_hex(p_value, "sha256:", 64);
}

static bool approval(const Variant &p_value) {
	if (p_value.get_type() != Variant::DICTIONARY) {
		return false;
	}
	const Dictionary value = p_value;
	if (!exact_keys(value, { "kind", "scope", "nonce", "issued_at_ms", "expires_at_ms", "mac" }) || String(value.get("kind", String())) != "mcp_form_v1" || String(value.get("scope", String())) != "change_set.atomic" || !safe_integer(value.get("issued_at_ms", Variant())) || !safe_integer(value.get("expires_at_ms", Variant()))) {
		return false;
	}
	const String nonce = value.get("nonce", String());
	const String mac = value.get("mac", String());
	return nonce.length() == 43 && mac.length() == 43;
}

} // namespace

Dictionary BridgeChangeSetProfile::make_limits() {
	Dictionary limits;
	limits["operations"] = MAX_OPERATIONS;
	limits["report_page_bytes"] = MAX_REPORT_PAGE_BYTES;
	limits["report_pages"] = MAX_REPORT_PAGES;
	limits["retained_report_bytes"] = MAX_RETAINED_REPORT_BYTES;
	return limits;
}

Dictionary BridgeChangeSetProfile::make_capability(const String &p_name, bool p_ready, const String &p_unavailable_reason) {
	Dictionary capability;
	capability["name"] = p_name;
	capability["version"] = "1.0";
	capability["readiness"] = p_ready ? "ready" : "unavailable";
	capability["reason"] = p_ready ? "ready" : p_unavailable_reason;
	capability["limits"] = make_limits();
	return capability;
}

void BridgeChangeSetProfile::append_global_limits(Dictionary &r_limits) {
	const Dictionary limits = make_limits();
	const Array keys = limits.keys();
	for (int index = 0; index < keys.size(); index++) {
		r_limits["change_set_" + String(keys[index])] = limits[keys[index]];
	}
}

bool BridgeChangeSetProfile::validate_prepare_params(const Dictionary &p_params) {
	return CompoundChangeSetPlanner::validate_params(p_params);
}

bool BridgeChangeSetProfile::validate_apply_params(const Dictionary &p_params) {
	return exact_keys(p_params, { "change_set_id", "preview_digest", "expected_scene_revision", "expected_operation_seq", "approval" }) &&
			prefixed_hex(p_params.get("change_set_id", Variant()), "change-set:") &&
			digest(p_params.get("preview_digest", Variant())) &&
			safe_integer(p_params.get("expected_scene_revision", Variant())) &&
			safe_integer(p_params.get("expected_operation_seq", Variant())) &&
			approval(p_params.get("approval", Variant()));
}

bool BridgeChangeSetProfile::validate_status_params(const Dictionary &p_params) {
	return exact_keys(p_params, { "change_set_id" }) && prefixed_hex(p_params.get("change_set_id", Variant()), "change-set:");
}

bool BridgeChangeSetProfile::validate_undo_params(const Dictionary &p_params) {
	return exact_keys(p_params, { "change_set_id", "expected_transaction_seq" }) &&
			prefixed_hex(p_params.get("change_set_id", Variant()), "change-set:") &&
			safe_integer(p_params.get("expected_transaction_seq", Variant()), true);
}

bool BridgeChangeSetProfile::validate_validation_complete_params(const Dictionary &p_params) {
	if (!exact_keys(p_params, { "change_set_id", "validation_report_id", "report_digest", "outcome" }) ||
			!prefixed_hex(p_params.get("change_set_id", Variant()), "change-set:") ||
			!prefixed_hex(p_params.get("validation_report_id", Variant()), "validation-report:") ||
			!digest(p_params.get("report_digest", Variant())) ||
			p_params.get("outcome", Variant()).get_type() != Variant::STRING) {
		return false;
	}
	const String outcome = p_params["outcome"];
	return outcome == "passed" || outcome == "failed" || outcome == "inconclusive" || outcome == "timed_out";
}

bool BridgeChangeSetProfile::validate_rollback_params(const Dictionary &p_params) {
	return exact_keys(p_params, { "change_set_id", "expected_transaction_seq", "expected_postimage_digest" }) &&
			prefixed_hex(p_params.get("change_set_id", Variant()), "change-set:") &&
			safe_integer(p_params.get("expected_transaction_seq", Variant()), true) &&
			digest(p_params.get("expected_postimage_digest", Variant()));
}
