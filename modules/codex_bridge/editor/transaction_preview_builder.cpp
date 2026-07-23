/**************************************************************************/
/*  transaction_preview_builder.cpp                                      */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "transaction_preview_builder.h"

#include "core/io/json.h"

#include "modules/codex_bridge/protocol/bridge_transaction_canonicalizer.h"
#include "modules/codex_bridge/protocol/bridge_transaction_profile.h"

namespace {

static Error operation_policy(const Dictionary &p_operation, bool p_script_already_attached, String &r_risk, String &r_scope) {
	const String kind = p_operation.get("kind", String());
	if (kind == "create_node") {
		r_risk = "write";
		r_scope = "scene.node.create";
	} else if (kind == "delete_node") {
		r_risk = "destructive";
		r_scope = "scene.node.delete";
	} else if (kind == "reparent_node") {
		r_risk = "destructive";
		r_scope = "scene.node.reparent";
	} else if (kind == "set_property") {
		r_risk = "destructive";
		r_scope = "scene.property.set";
	} else if (kind == "attach_script") {
		r_risk = p_script_already_attached ? "destructive" : "write";
		r_scope = "scene.script.attach";
	} else if (kind == "detach_script") {
		r_risk = "destructive";
		r_scope = "scene.script.detach";
	} else if (kind == "connect_signal") {
		r_risk = "write";
		r_scope = "scene.signal.connect";
	} else if (kind == "disconnect_signal") {
		r_risk = "destructive";
		r_scope = "scene.signal.disconnect";
	} else {
		return ERR_INVALID_DATA;
	}
	return OK;
}

static String operation_summary(const Dictionary &p_operation, const TransactionPreviewBuilder::Resolution &p_resolution) {
	const String kind = p_operation["kind"];
	if (kind == "create_node") {
		return vformat("Create %s named %s under the resolved parent.", String(p_operation["godot_type"]), String(p_operation["name"]));
	}
	if (kind == "delete_node") {
		return vformat("Delete the resolved node and its bounded subtree (%d node%s).", p_resolution.structural_nodes, p_resolution.structural_nodes == 1 ? "" : "s");
	}
	if (kind == "reparent_node") {
		return "Move the resolved node under the resolved new parent.";
	}
	if (kind == "set_property") {
		return vformat("Replace property %s on the resolved node.", String(p_operation["property"]));
	}
	if (kind == "attach_script") {
		return p_resolution.script_already_attached ? "Replace the script attached to the resolved node." : "Attach a script to the resolved node.";
	}
	if (kind == "detach_script") {
		return "Detach the script from the resolved node.";
	}
	if (kind == "connect_signal") {
		return vformat("Connect signal %s to method %s on the resolved receiver.", String(p_operation["signal"]), String(p_operation["method"]));
	}
	return vformat("Disconnect signal %s from method %s on the resolved receiver.", String(p_operation["signal"]), String(p_operation["method"]));
}

static Array operation_preconditions(const String &p_kind) {
	Array preconditions;
	preconditions.push_back("scene and native history revisions remain unchanged");
	if (p_kind == "create_node") {
		preconditions.push_back("parent remains editable and the requested name remains available");
	} else if (p_kind == "delete_node") {
		preconditions.push_back("target remains editable with the same owner and bounded subtree");
	} else if (p_kind == "reparent_node") {
		preconditions.push_back("target and new parent remain editable and outside a cycle");
	} else if (p_kind == "set_property") {
		preconditions.push_back("native property remains editor-writable without a custom getter");
	} else if (p_kind == "attach_script") {
		preconditions.push_back("target script identity and compatible script resource remain unchanged");
	} else if (p_kind == "detach_script") {
		preconditions.push_back("target remains editable with the same attached script");
	} else {
		preconditions.push_back("signal endpoints and exact connection state remain unchanged");
	}
	return preconditions;
}

static Dictionary redacted_operation(const Dictionary &p_operation, const TransactionPreviewBuilder::Resolution &p_resolution, const String &p_operation_digest) {
	const Dictionary normalized = BridgeTransactionCanonicalizer::normalize_operation(p_operation);
	const String kind = normalized["kind"];
	if (kind != "set_property" && kind != "attach_script" && kind != "connect_signal" && kind != "disconnect_signal") {
		return normalized;
	}
	Dictionary redacted;
	redacted["kind"] = kind;
	Dictionary fallback;
	String summary_type = "binds";
	if (kind == "set_property") {
		summary_type = "value";
	} else if (kind == "attach_script") {
		summary_type = "script";
	}
	fallback["type"] = summary_type;
	fallback["redacted"] = true;
	fallback["digest"] = p_operation_digest;
	const Dictionary summary = p_resolution.redacted_change.is_empty() ? fallback : p_resolution.redacted_change;
	if (kind == "set_property") {
		redacted["node_id"] = normalized["node_id"];
		redacted["property"] = normalized["property"];
		redacted["value_summary"] = summary;
	} else if (kind == "attach_script") {
		redacted["node_id"] = normalized["node_id"];
		redacted["script_summary"] = summary;
	} else {
		redacted["emitter_node_id"] = normalized["emitter_node_id"];
		redacted["signal"] = normalized["signal"];
		redacted["receiver_node_id"] = normalized["receiver_node_id"];
		redacted["method"] = normalized["method"];
		redacted["flags"] = normalized["flags"];
		redacted["unbinds"] = normalized["unbinds"];
		redacted["binds_summary"] = summary;
	}
	return redacted;
}

} // namespace

Error TransactionPreviewBuilder::build(const String &p_transaction_id, const PreparedTransactionStore::Binding &p_binding, const Dictionary &p_operation, const Resolution &p_resolution, uint64_t p_created_at_ms, uint64_t p_expires_at_ms, Output &r_output) {
	r_output = Output();
	if (!BridgeTransactionProfile::validate_operation(p_operation) || p_resolution.affected_entities.is_empty() || p_resolution.affected_entities.size() > 16 || p_created_at_ms > p_expires_at_ms) {
		return ERR_INVALID_DATA;
	}
	String risk;
	String scope;
	if (operation_policy(p_operation, p_resolution.script_already_attached, risk, scope) != OK) {
		return ERR_INVALID_DATA;
	}
	const Dictionary normalized_operation = BridgeTransactionCanonicalizer::normalize_operation(p_operation);
	String operation_digest;
	if (BridgeTransactionCanonicalizer::sha256_utf8(JSON::stringify(normalized_operation, "", true, true), operation_digest, "godot-codex-preview-operation/v1\n") != OK) {
		return ERR_OUT_OF_MEMORY;
	}

	Dictionary coordinates;
	coordinates["transaction_id"] = p_transaction_id;
	coordinates["scene_id"] = p_binding.scene_id;
	coordinates["history_id"] = p_binding.history_id;
	coordinates["scene_revision"] = (int64_t)p_binding.scene_revision;
	coordinates["operation_seq"] = (int64_t)p_binding.operation_seq;
	coordinates["transaction_seq"] = (int64_t)2;

	Dictionary preview;
	preview["operation_kind"] = p_operation["kind"];
	preview["summary"] = operation_summary(p_operation, p_resolution);
	preview["dirty_effect"] = "marks_scene_dirty";
	preview["save_effect"] = "not_saved";
	preview["preconditions"] = operation_preconditions(p_operation["kind"]);
	preview["truncated"] = false;

	Dictionary payload;
	payload["schema_version"] = "canonical-transaction-preview/1.1";
	payload["coordinates"] = coordinates.duplicate(true);
	payload["operation"] = redacted_operation(normalized_operation, p_resolution, operation_digest);
	payload["operation_digest"] = operation_digest;
	payload["risk"] = risk;
	payload["scope"] = scope;
	payload["affected_entities"] = p_resolution.affected_entities.duplicate(true);
	payload["preview"] = preview.duplicate(true);
	payload["created_at_ms"] = (int64_t)p_created_at_ms;
	payload["expires_at_ms"] = (int64_t)p_expires_at_ms;
	payload["limits_applied"] = BridgeTransactionProfile::make_limits();
	if (BridgeTransactionCanonicalizer::make_preview_digest(payload, r_output.preview_payload_json, r_output.preview_digest) != OK) {
		return ERR_OUT_OF_MEMORY;
	}

	Dictionary result;
	result["schema_version"] = "transaction/1.0";
	result["coordinates"] = coordinates;
	result["state"] = "previewed";
	result["operation_kind"] = p_operation["kind"];
	result["risk"] = risk;
	result["scope"] = scope;
	result["affected_entities"] = p_resolution.affected_entities.duplicate(true);
	result["preview"] = preview;
	result["preview_payload_json"] = r_output.preview_payload_json;
	result["preview_digest"] = r_output.preview_digest;
	result["created_at_ms"] = (int64_t)p_created_at_ms;
	result["expires_at_ms"] = (int64_t)p_expires_at_ms;
	result["limits_applied"] = BridgeTransactionProfile::make_limits();
	if (!BridgeTransactionProfile::validate_prepare_result(result)) {
		return ERR_INVALID_DATA;
	}
	r_output.result_json = JSON::stringify(result, "", true, true);
	if (r_output.result_json.utf8().length() > BridgeTransactionProfile::MAX_STATUS_BYTES) {
		r_output = Output();
		return ERR_OUT_OF_MEMORY;
	}
	r_output.result = result.duplicate(true);
	return OK;
}
