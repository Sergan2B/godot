/**************************************************************************/
/*  transaction_approval_verifier.cpp                                   */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "transaction_approval_verifier.h"

#include "core/crypto/crypto_core.h"
#include "core/io/json.h"

#include "modules/codex_bridge/protocol/bridge_crypto.h"

namespace {

static void append_raw(PackedByteArray &r_bytes, const uint8_t *p_bytes, int p_size) {
	const int offset = r_bytes.size();
	r_bytes.resize(offset + p_size);
	if (p_size > 0) {
		memcpy(r_bytes.ptrw() + offset, p_bytes, p_size);
	}
}

static void append_ascii(PackedByteArray &r_bytes, const char *p_text, int p_size) {
	append_raw(r_bytes, reinterpret_cast<const uint8_t *>(p_text), p_size);
}

static void append_u32be(PackedByteArray &r_bytes, uint32_t p_value) {
	uint8_t encoded[4] = {
		(uint8_t)(p_value >> 24),
		(uint8_t)(p_value >> 16),
		(uint8_t)(p_value >> 8),
		(uint8_t)p_value,
	};
	append_raw(r_bytes, encoded, 4);
}

static void append_u64be(PackedByteArray &r_bytes, uint64_t p_value) {
	uint8_t encoded[8];
	for (int index = 0; index < 8; index++) {
		encoded[index] = (uint8_t)(p_value >> (56 - index * 8));
	}
	append_raw(r_bytes, encoded, 8);
}

static bool append_lp(PackedByteArray &r_bytes, const String &p_value) {
	const CharString utf8 = p_value.utf8();
	const int length = utf8.length();
	if (length < 0 || (uint64_t)length > UINT32_MAX) {
		return false;
	}
	append_u32be(r_bytes, (uint32_t)length);
	append_raw(r_bytes, reinterpret_cast<const uint8_t *>(utf8.get_data()), length);
	return true;
}

static String sha256_hex(const PackedByteArray &p_bytes) {
	PackedByteArray digest;
	digest.resize(32);
	if (CryptoCore::sha256(p_bytes.ptr(), p_bytes.size(), digest.ptrw()) != OK) {
		return String();
	}
	return BridgeCrypto::bytes_to_lower_hex(digest);
}

static TransactionApprovalVerifier::Outcome failure(const String &p_code, const String &p_message) {
	TransactionApprovalVerifier::Outcome outcome;
	outcome.error_code = p_code;
	outcome.error_message = p_message;
	return outcome;
}

static bool variant_to_u64(const Variant &p_value, uint64_t &r_value) {
	if (p_value.get_type() == Variant::INT) {
		const int64_t integer = p_value;
		if (integer < 0) {
			return false;
		}
		r_value = (uint64_t)integer;
		return true;
	}
	if (p_value.get_type() != Variant::FLOAT) {
		return false;
	}
	const double number = p_value;
	if (!Math::is_finite(number) || number < 0.0 || number > 9007199254740991.0 || Math::floor(number) != number) {
		return false;
	}
	r_value = (uint64_t)number;
	return true;
}

} // namespace

Error TransactionApprovalVerifier::derive_approval_key(const PackedByteArray &p_session_token, PackedByteArray &r_approval_key) {
	ERR_FAIL_COND_V(p_session_token.size() != BridgeCrypto::RANDOM_VALUE_BYTES, ERR_INVALID_PARAMETER);
	PackedByteArray domain;
	static constexpr char KEY_DOMAIN[] = "godot-codex/approval-key/v1";
	append_ascii(domain, KEY_DOMAIN, sizeof(KEY_DOMAIN) - 1);
	return BridgeCrypto::hmac_sha256(p_session_token, domain, r_approval_key);
}

void TransactionApprovalVerifier::initialize(const PackedByteArray &p_approval_key) {
	shutdown();
	if (p_approval_key.size() == BridgeCrypto::RANDOM_VALUE_BYTES) {
		approval_key = p_approval_key;
	}
}

void TransactionApprovalVerifier::shutdown() {
	if (!approval_key.is_empty()) {
		uint8_t *bytes = approval_key.ptrw();
		for (int index = 0; index < approval_key.size(); index++) {
			bytes[index] = 0;
		}
	}
	approval_key.clear();
	consumed_nonce_deadlines.clear();
}

bool TransactionApprovalVerifier::is_available() const {
	return approval_key.size() == BridgeCrypto::RANDOM_VALUE_BYTES;
}

TransactionApprovalVerifier::Outcome TransactionApprovalVerifier::verify(const Dictionary &p_receipt, const Binding &p_binding, uint64_t p_now_ms) const {
	if (!is_available()) {
		return failure("approval_required", "The transaction approval verifier is unavailable.");
	}
	if (p_receipt.get("kind", String()) != "mcp_form_v1") {
		return failure("approval_invalid", "The approval receipt kind is invalid.");
	}
	const String scope = p_receipt.get("scope", String());
	if (scope != p_binding.scope) {
		return failure("approval_scope_mismatch", "The approval receipt scope does not match the prepared transaction.");
	}
	const String nonce_encoded = p_receipt.get("nonce", String());
	const String mac_encoded = p_receipt.get("mac", String());
	PackedByteArray nonce;
	PackedByteArray received_mac;
	if (BridgeCrypto::base64url_decode_32(nonce_encoded, nonce) != OK || BridgeCrypto::base64url_decode_32(mac_encoded, received_mac) != OK) {
		return failure("approval_invalid", "The approval receipt encoding is invalid.");
	}
	uint64_t issued_at_ms = 0;
	uint64_t expires_at_ms = 0;
	if (!variant_to_u64(p_receipt.get("issued_at_ms", Variant()), issued_at_ms) || !variant_to_u64(p_receipt.get("expires_at_ms", Variant()), expires_at_ms)) {
		return failure("approval_invalid", "The approval receipt timestamps are invalid.");
	}
	const uint64_t latest_issued_at_ms = p_now_ms <= UINT64_MAX - CLOCK_SKEW_MSEC ? p_now_ms + CLOCK_SKEW_MSEC : UINT64_MAX;
	if (issued_at_ms > UINT64_MAX - RECEIPT_TTL_MSEC || expires_at_ms != issued_at_ms + RECEIPT_TTL_MSEC || issued_at_ms > latest_issued_at_ms || (p_now_ms > expires_at_ms && p_now_ms - expires_at_ms > CLOCK_SKEW_MSEC)) {
		return failure("approval_invalid", "The approval receipt is outside its permitted lifetime.");
	}

	PackedByteArray canonical;
	static constexpr char RECEIPT_DOMAIN[] = "godot-codex/approval-receipt/v1";
	append_ascii(canonical, RECEIPT_DOMAIN, sizeof(RECEIPT_DOMAIN) - 1);
	canonical.push_back(0);
	if (!append_lp(canonical, p_binding.project_id) || !append_lp(canonical, p_binding.editor_session_id) || !append_lp(canonical, p_binding.scene_id) || !append_lp(canonical, p_binding.transaction_id) || !append_lp(canonical, p_binding.preview_digest) || !append_lp(canonical, p_binding.scope) || !append_lp(canonical, p_binding.risk)) {
		return failure("approval_invalid", "The approval receipt binding is too large.");
	}
	append_u64be(canonical, p_binding.scene_revision);
	append_u64be(canonical, p_binding.operation_seq);
	append_u64be(canonical, issued_at_ms);
	append_u64be(canonical, expires_at_ms);
	append_raw(canonical, nonce.ptr(), nonce.size());

	PackedByteArray expected_mac;
	if (BridgeCrypto::hmac_sha256(approval_key, canonical, expected_mac) != OK || !BridgeCrypto::constant_time_equal(expected_mac, received_mac)) {
		return failure("approval_invalid", "The approval receipt authentication failed.");
	}

	const String nonce_hash = sha256_hex(nonce);
	const String canonical_hash = sha256_hex(canonical);
	if (nonce_hash.is_empty() || canonical_hash.is_empty()) {
		return failure("approval_invalid", "The approval receipt digest could not be computed.");
	}
	PackedByteArray receipt_hash_input = canonical;
	append_raw(receipt_hash_input, received_mac.ptr(), received_mac.size());
	const String receipt_hash = sha256_hex(receipt_hash_input);
	if (receipt_hash.is_empty()) {
		return failure("approval_invalid", "The approval receipt digest could not be computed.");
	}

	Outcome outcome;
	outcome.valid = true;
	outcome.receipt.nonce_hash = nonce_hash;
	outcome.receipt.receipt_hash = "sha256:" + receipt_hash;
	outcome.receipt.replay_deadline_ms = expires_at_ms <= UINT64_MAX - CLOCK_SKEW_MSEC ? expires_at_ms + CLOCK_SKEW_MSEC : UINT64_MAX;
	return outcome;
}

void TransactionApprovalVerifier::_purge_expired(uint64_t p_now_ms) {
	Vector<String> expired;
	for (const KeyValue<String, uint64_t> &entry : consumed_nonce_deadlines) {
		if (p_now_ms > entry.value) {
			expired.push_back(entry.key);
		}
	}
	for (const String &nonce_hash : expired) {
		consumed_nonce_deadlines.erase(nonce_hash);
	}
}

Error TransactionApprovalVerifier::consume(const VerifiedReceipt &p_receipt, uint64_t p_now_ms, String &r_error_code, String &r_error_message) {
	r_error_code.clear();
	r_error_message.clear();
	_purge_expired(p_now_ms);
	if (p_receipt.nonce_hash.is_empty() || p_now_ms > p_receipt.replay_deadline_ms) {
		r_error_code = "approval_invalid";
		r_error_message = "The approval receipt expired before the commit window.";
		return ERR_INVALID_DATA;
	}
	if (consumed_nonce_deadlines.has(p_receipt.nonce_hash)) {
		r_error_code = "approval_replayed";
		r_error_message = "The approval receipt nonce was already consumed.";
		return ERR_ALREADY_IN_USE;
	}
	if (consumed_nonce_deadlines.size() >= MAX_CONSUMED_NONCES) {
		r_error_code = "transaction_busy";
		r_error_message = "The bounded approval replay cache is full.";
		return ERR_OUT_OF_MEMORY;
	}
	consumed_nonce_deadlines.insert(p_receipt.nonce_hash, p_receipt.replay_deadline_ms);
	return OK;
}

uint32_t TransactionApprovalVerifier::get_consumed_nonce_count() const {
	return consumed_nonce_deadlines.size();
}
