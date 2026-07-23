/**************************************************************************/
/*  test_transaction_approval_verifier.cpp                              */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "tests/test_macros.h"

TEST_FORCE_LINK(test_transaction_approval_verifier)

#include "modules/modules_enabled.gen.h"

#ifdef MODULE_CODEX_BRIDGE_ENABLED

#include "modules/codex_bridge/editor/transaction_approval_verifier.h"
#include "modules/codex_bridge/protocol/bridge_crypto.h"

namespace TestTransactionApprovalVerifier {

static PackedByteArray sequential_token() {
	PackedByteArray token;
	token.resize(32);
	for (int index = 0; index < token.size(); index++) {
		token.ptrw()[index] = (uint8_t)index;
	}
	return token;
}

static TransactionApprovalVerifier::Binding binding() {
	TransactionApprovalVerifier::Binding value;
	value.project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	value.editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	value.scene_id = "scene:11111111111111111111111111111111";
	value.transaction_id = "transaction:22222222222222222222222222222222";
	value.preview_digest = "sha256:3333333333333333333333333333333333333333333333333333333333333333";
	value.scope = "scene.node.delete";
	value.risk = "destructive";
	value.scene_revision = 7;
	value.operation_seq = 11;
	return value;
}

static Dictionary receipt() {
	Dictionary value;
	value["kind"] = "mcp_form_v1";
	value["scope"] = "scene.node.delete";
	value["nonce"] = "ICEiIyQlJicoKSorLC0uLzAxMjM0NTY3ODk6Ozw9Pj8";
	value["issued_at_ms"] = (int64_t)1784690000000;
	value["expires_at_ms"] = (int64_t)1784690030000;
	value["mac"] = "UA22qIiXEf_HN0OEg83xEyJjdrSlNZBop2DYeAMaeEM";
	return value;
}

TEST_CASE("[CodexS9Approval] C++ reproduces the frozen approval receipt vector") {
	PackedByteArray approval_key;
	REQUIRE(TransactionApprovalVerifier::derive_approval_key(sequential_token(), approval_key) == OK);
	CHECK(BridgeCrypto::bytes_to_lower_hex(approval_key) == "bd5c8a753ee9611953024d9dc26ae4cbf6a5b7d451534f6429e150b23b201d6d");

	TransactionApprovalVerifier verifier;
	verifier.initialize(approval_key);
	const TransactionApprovalVerifier::Outcome outcome = verifier.verify(receipt(), binding(), 1784690010000);
	REQUIRE(outcome.valid);
	CHECK(outcome.receipt.nonce_hash.length() == 64);
	CHECK(outcome.receipt.receipt_hash.begins_with("sha256:"));
	String error_code;
	String error_message;
	CHECK(verifier.consume(outcome.receipt, 1784690010000, error_code, error_message) == OK);
	CHECK(verifier.get_consumed_nonce_count() == 1);
	CHECK(verifier.consume(outcome.receipt, 1784690010001, error_code, error_message) == ERR_ALREADY_IN_USE);
	CHECK(error_code == "approval_replayed");
	verifier.shutdown();
	CHECK_FALSE(verifier.is_available());
}

TEST_CASE("[CodexS9Approval] Binding, authentication, and lifetime failures are fail-closed") {
	PackedByteArray approval_key;
	REQUIRE(TransactionApprovalVerifier::derive_approval_key(sequential_token(), approval_key) == OK);
	TransactionApprovalVerifier verifier;
	verifier.initialize(approval_key);

	Dictionary wrong_scope = receipt();
	wrong_scope["scope"] = "scene.node.create";
	TransactionApprovalVerifier::Outcome outcome = verifier.verify(wrong_scope, binding(), 1784690010000);
	CHECK_FALSE(outcome.valid);
	CHECK(outcome.error_code == "approval_scope_mismatch");

	Dictionary wrong_mac = receipt();
	wrong_mac["mac"] = "AA22qIiXEf_HN0OEg83xEyJjdrSlNZBop2DYeAMaeEM";
	outcome = verifier.verify(wrong_mac, binding(), 1784690010000);
	CHECK_FALSE(outcome.valid);
	CHECK(outcome.error_code == "approval_invalid");

	outcome = verifier.verify(receipt(), binding(), 1784690032001);
	CHECK_FALSE(outcome.valid);
	CHECK(outcome.error_code == "approval_invalid");

	Dictionary long_lived = receipt();
	long_lived["expires_at_ms"] = (int64_t)1784690030001;
	outcome = verifier.verify(long_lived, binding(), 1784690010000);
	CHECK_FALSE(outcome.valid);
	CHECK(outcome.error_code == "approval_invalid");
}

TEST_CASE("[CodexS9Approval] Every receipt binding and encoding fails closed independently") {
	PackedByteArray approval_key;
	REQUIRE(TransactionApprovalVerifier::derive_approval_key(sequential_token(), approval_key) == OK);
	TransactionApprovalVerifier verifier;
	verifier.initialize(approval_key);

	Vector<TransactionApprovalVerifier::Binding> wrong_bindings;
	TransactionApprovalVerifier::Binding wrong = binding();
	wrong.editor_session_id = "editor:ffffffffffffffffffffffffffffffff";
	wrong_bindings.push_back(wrong);
	wrong = binding();
	wrong.scene_id = "scene:ffffffffffffffffffffffffffffffff";
	wrong_bindings.push_back(wrong);
	wrong = binding();
	wrong.transaction_id = "transaction:ffffffffffffffffffffffffffffffff";
	wrong_bindings.push_back(wrong);
	wrong = binding();
	wrong.preview_digest = "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
	wrong_bindings.push_back(wrong);
	wrong = binding();
	wrong.risk = "write";
	wrong_bindings.push_back(wrong);
	wrong = binding();
	wrong.scene_revision++;
	wrong_bindings.push_back(wrong);
	wrong = binding();
	wrong.operation_seq++;
	wrong_bindings.push_back(wrong);
	for (const TransactionApprovalVerifier::Binding &candidate : wrong_bindings) {
		const TransactionApprovalVerifier::Outcome outcome = verifier.verify(receipt(), candidate, 1784690010000);
		CHECK_FALSE(outcome.valid);
		CHECK(outcome.error_code == "approval_invalid");
	}

	Dictionary malformed = receipt();
	malformed["nonce"] = "not-base64url";
	CHECK(verifier.verify(malformed, binding(), 1784690010000).error_code == "approval_invalid");
	malformed = receipt();
	malformed["mac"] = "not-base64url";
	CHECK(verifier.verify(malformed, binding(), 1784690010000).error_code == "approval_invalid");
	CHECK(verifier.verify(receipt(), binding(), 1784689997999).error_code == "approval_invalid");
	CHECK(verifier.verify(receipt(), binding(), 1784689998000).valid);

	TransactionApprovalVerifier unavailable;
	CHECK(unavailable.verify(receipt(), binding(), 1784690010000).error_code == "approval_required");
}

TEST_CASE("[CodexS9Approval] Replay cache is bounded and only evicts expired entries") {
	PackedByteArray approval_key;
	REQUIRE(TransactionApprovalVerifier::derive_approval_key(sequential_token(), approval_key) == OK);
	TransactionApprovalVerifier verifier;
	verifier.initialize(approval_key);
	String error_code;
	String error_message;
	for (uint32_t index = 0; index < TransactionApprovalVerifier::MAX_CONSUMED_NONCES; index++) {
		TransactionApprovalVerifier::VerifiedReceipt verified;
		verified.nonce_hash = String::num_uint64(index, 16).lpad(64, "0");
		verified.replay_deadline_ms = 2000;
		REQUIRE(verifier.consume(verified, 1000, error_code, error_message) == OK);
	}
	TransactionApprovalVerifier::VerifiedReceipt overflow;
	overflow.nonce_hash = String("f").repeat(64);
	overflow.replay_deadline_ms = 3000;
	CHECK(verifier.consume(overflow, 1000, error_code, error_message) == ERR_OUT_OF_MEMORY);
	CHECK(error_code == "transaction_busy");
	CHECK(verifier.consume(overflow, 2001, error_code, error_message) == OK);
	CHECK(verifier.get_consumed_nonce_count() == 1);
}

} // namespace TestTransactionApprovalVerifier

#endif // MODULE_CODEX_BRIDGE_ENABLED
