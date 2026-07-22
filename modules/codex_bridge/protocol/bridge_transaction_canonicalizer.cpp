/**************************************************************************/
/*  bridge_transaction_canonicalizer.cpp                                 */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "bridge_transaction_canonicalizer.h"

#include "bridge_crypto.h"
#include "bridge_transaction_profile.h"

#include "core/crypto/crypto_core.h"
#include "core/io/json.h"

namespace {

static Dictionary normalize_wire_variant(const Dictionary &p_value) {
	Dictionary normalized = p_value.duplicate(true);
	const String type = normalized.get("type", String());
	if (type == "int") {
		normalized["value"] = (int64_t)normalized["value"];
	} else if (type == "float") {
		normalized["value"] = (double)normalized["value"];
	} else if (type == "array") {
		Array values = normalized["value"];
		for (int index = 0; index < values.size(); index++) {
			values[index] = normalize_wire_variant(values[index]);
		}
		normalized["value"] = values;
	} else if (type == "dictionary") {
		Array entries = normalized["value"];
		for (int index = 0; index < entries.size(); index++) {
			Dictionary entry = entries[index];
			entry["value"] = normalize_wire_variant(entry["value"]);
			entries[index] = entry;
		}
		normalized["value"] = entries;
	} else if (type == "vector2" || type == "vector2i" || type == "vector3" || type == "vector3i" || type == "vector4" || type == "vector4i" || type == "rect2" || type == "rect2i" || type == "transform2d" || type == "plane" || type == "quaternion" || type == "aabb" || type == "basis" || type == "transform3d" || type == "projection" || type == "color") {
		Array values = normalized["value"];
		for (int index = 0; index < values.size(); index++) {
			values[index] = (double)values[index];
		}
		normalized["value"] = values;
	}
	return normalized;
}

} // namespace

Dictionary BridgeTransactionCanonicalizer::normalize_operation(const Dictionary &p_operation) {
	Dictionary normalized = p_operation.duplicate(true);
	const String kind = normalized.get("kind", String());
	if (normalized.has("insertion_index")) {
		normalized["insertion_index"] = (int64_t)normalized["insertion_index"];
	}
	if (kind == "set_property") {
		normalized["value"] = normalize_wire_variant(normalized["value"]);
	} else if (kind == "connect_signal" || kind == "disconnect_signal") {
		normalized["flags"] = (int64_t)normalized["flags"];
		normalized["unbinds"] = (int64_t)normalized["unbinds"];
		Array binds = normalized["binds"];
		for (int index = 0; index < binds.size(); index++) {
			binds[index] = normalize_wire_variant(binds[index]);
		}
		normalized["binds"] = binds;
	}
	return normalized;
}

Error BridgeTransactionCanonicalizer::sha256_utf8(const String &p_value, String &r_digest, const String &p_domain) {
	const String input = p_domain + p_value;
	const CharString bytes = input.utf8();
	PackedByteArray digest;
	digest.resize(32);
	const Error error = CryptoCore::sha256(reinterpret_cast<const uint8_t *>(bytes.get_data()), bytes.length(), digest.ptrw());
	if (error != OK) {
		r_digest.clear();
		return error;
	}
	r_digest = "sha256:" + BridgeCrypto::bytes_to_lower_hex(digest);
	return OK;
}

Error BridgeTransactionCanonicalizer::make_request_digest(const String &p_project_id, const String &p_editor_session_id, const Dictionary &p_prepare_params, String &r_canonical_json, String &r_digest) {
	if (!BridgeTransactionProfile::validate_prepare_params(p_prepare_params)) {
		return ERR_INVALID_DATA;
	}
	Dictionary request;
	request["schema_version"] = "transaction-request/1.0";
	request["project_id"] = p_project_id;
	request["editor_session_id"] = p_editor_session_id;
	Dictionary coordinates = Dictionary(p_prepare_params["coordinates"]).duplicate(true);
	coordinates["scene_revision"] = (int64_t)coordinates["scene_revision"];
	coordinates["operation_seq"] = (int64_t)coordinates["operation_seq"];
	request["coordinates"] = coordinates;
	request["operation"] = normalize_operation(p_prepare_params["operation"]);
	r_canonical_json = JSON::stringify(request, "", true, true);
	if (r_canonical_json.utf8().length() > BridgeTransactionProfile::MAX_OPERATION_BYTES) {
		r_canonical_json.clear();
		return ERR_OUT_OF_MEMORY;
	}
	return sha256_utf8(r_canonical_json, r_digest, "godot-codex-transaction-request/v1\n");
}

Error BridgeTransactionCanonicalizer::make_preview_digest(const Dictionary &p_payload, String &r_canonical_json, String &r_digest) {
	r_canonical_json = JSON::stringify(p_payload, "", true, true);
	if (r_canonical_json.utf8().length() > BridgeTransactionProfile::MAX_STATUS_BYTES) {
		r_canonical_json.clear();
		return ERR_OUT_OF_MEMORY;
	}
	return sha256_utf8(r_canonical_json, r_digest);
}
