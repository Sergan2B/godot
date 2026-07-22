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
	request["coordinates"] = Dictionary(p_prepare_params["coordinates"]).duplicate(true);
	request["operation"] = Dictionary(p_prepare_params["operation"]).duplicate(true);
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
