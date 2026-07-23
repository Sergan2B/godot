/**************************************************************************/
/*  transaction_approval_verifier.h                                     */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#pragma once

#include "core/templates/hash_map.h"
#include "core/variant/variant.h"

class TransactionApprovalVerifier {
public:
	struct Binding {
		String project_id;
		String editor_session_id;
		String scene_id;
		String transaction_id;
		String preview_digest;
		String scope;
		String risk;
		uint64_t scene_revision = 0;
		uint64_t operation_seq = 0;
	};

	struct VerifiedReceipt {
		String nonce_hash;
		String receipt_hash;
		uint64_t replay_deadline_ms = 0;
	};

	struct Outcome {
		bool valid = false;
		String error_code;
		String error_message;
		VerifiedReceipt receipt;
	};

	static constexpr uint64_t RECEIPT_TTL_MSEC = 30000;
	static constexpr uint64_t CLOCK_SKEW_MSEC = 2000;
	static constexpr uint32_t MAX_CONSUMED_NONCES = 1024;

private:
	PackedByteArray approval_key;
	HashMap<String, uint64_t> consumed_nonce_deadlines;

	void _purge_expired(uint64_t p_now_ms);

public:
	static Error derive_approval_key(const PackedByteArray &p_session_token, PackedByteArray &r_approval_key);

	void initialize(const PackedByteArray &p_approval_key);
	void shutdown();
	bool is_available() const;
	Outcome verify(const Dictionary &p_receipt, const Binding &p_binding, uint64_t p_now_ms) const;
	Error consume(const VerifiedReceipt &p_receipt, uint64_t p_now_ms, String &r_error_code, String &r_error_message);
	uint32_t get_consumed_nonce_count() const;
};
