/**************************************************************************/
/*  test_prepared_transaction_store.cpp                                  */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "tests/test_macros.h"

TEST_FORCE_LINK(test_prepared_transaction_store)

#include "modules/modules_enabled.gen.h"

#ifdef MODULE_CODEX_BRIDGE_ENABLED

#include "core/io/json.h"

#include "modules/codex_bridge/editor/prepared_transaction_store.h"
#include "modules/codex_bridge/protocol/bridge_transaction_canonicalizer.h"

namespace TestPreparedTransactionStore {

struct DeterministicIds {
	uint64_t next = 1;
	bool fail = false;
};

static Error generate_id(String &r_transaction_id, void *p_userdata) {
	DeterministicIds *ids = static_cast<DeterministicIds *>(p_userdata);
	if (ids->fail) {
		return ERR_CANT_CREATE;
	}
	r_transaction_id = "transaction:" + String::num_uint64(ids->next++, 16).lpad(32, "0");
	return OK;
}

static PreparedTransactionStore::Binding binding(uint64_t p_operation_seq = 11) {
	PreparedTransactionStore::Binding value;
	value.project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	value.editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	value.scene_id = "scene:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
	value.history_id = "history:cccccccccccccccccccccccccccccccc";
	value.scene_revision = 7;
	value.operation_seq = p_operation_seq;
	value.event_seq = 20;
	value.project_revision = 8;
	value.resource_revision = 4;
	value.scene_graph_revision = 6;
	value.script_graph_revision = 5;
	return value;
}

static Dictionary prepare_params(const String &p_idempotency_key = "idempotency:dddddddddddddddddddddddddddddddd", const String &p_name = "CreatedActor") {
	Dictionary coordinates;
	coordinates["scene_id"] = "scene:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
	coordinates["history_id"] = "history:cccccccccccccccccccccccccccccccc";
	coordinates["scene_revision"] = (int64_t)7;
	coordinates["operation_seq"] = (int64_t)11;
	Dictionary operation;
	operation["kind"] = "create_node";
	operation["parent_node_id"] = "node:11111111111111111111111111111111";
	operation["godot_type"] = "Node2D";
	operation["name"] = p_name;
	Dictionary params;
	params["idempotency_key"] = p_idempotency_key;
	params["coordinates"] = coordinates;
	params["operation"] = operation;
	return params;
}

static void canonical_request(const Dictionary &p_params, String &r_json, String &r_digest) {
	REQUIRE(BridgeTransactionCanonicalizer::make_request_digest(binding().project_id, binding().editor_session_id, p_params, r_json, r_digest) == OK);
}

TEST_CASE("[CodexS9PreparedStore] Canonical request digest binds editor scope and operation") {
	String canonical;
	String digest;
	canonical_request(prepare_params(), canonical, digest);
	CHECK(canonical == "{\"coordinates\":{\"history_id\":\"history:cccccccccccccccccccccccccccccccc\",\"operation_seq\":11,\"scene_id\":\"scene:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\",\"scene_revision\":7},\"editor_session_id\":\"editor:0123456789abcdef0123456789abcdef\",\"operation\":{\"godot_type\":\"Node2D\",\"kind\":\"create_node\",\"name\":\"CreatedActor\",\"parent_node_id\":\"node:11111111111111111111111111111111\"},\"project_id\":\"project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd\",\"schema_version\":\"transaction-request/1.0\"}");
	CHECK(digest.begins_with("sha256:"));
	CHECK(digest.length() == 71);
	String changed_canonical;
	String changed_digest;
	canonical_request(prepare_params("idempotency:dddddddddddddddddddddddddddddddd", "OtherActor"), changed_canonical, changed_digest);
	CHECK(changed_digest != digest);
	String other_scope_digest;
	REQUIRE(BridgeTransactionCanonicalizer::make_request_digest("project:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", binding().editor_session_id, prepare_params(), changed_canonical, other_scope_digest) == OK);
	CHECK(other_scope_digest != digest);
}

TEST_CASE("[CodexS9PreparedStore] Admission is bounded and idempotent") {
	DeterministicIds ids;
	PreparedTransactionStore store(generate_id, &ids);
	String canonical;
	String digest;
	const Dictionary params = prepare_params();
	canonical_request(params, canonical, digest);
	const String operation_json = JSON::stringify(params["operation"], "", true, true);
	PreparedTransactionStore::Admission first = store.admit(params["idempotency_key"], digest, canonical, operation_json, binding(), 1000, 2000);
	CHECK(first.kind == PreparedTransactionStore::ADMISSION_CREATED);
	CHECK(first.transaction_id == "transaction:00000000000000000000000000000001");
	CHECK(first.record.state == PreparedTransactionStore::STATE_PREPARING);
	CHECK(first.record.transaction_seq == 1);
	CHECK(first.record.expires_at_ms == 302000);
	CHECK(store.get_active_count() == 1);

	PreparedTransactionStore::Admission in_progress = store.admit(params["idempotency_key"], digest, canonical, operation_json, binding(), 2000, 3000);
	CHECK(in_progress.kind == PreparedTransactionStore::ADMISSION_IN_PROGRESS);
	CHECK(in_progress.transaction_id == first.transaction_id);
	CHECK(store.get_active_count() == 1);

	String other_canonical;
	String other_digest;
	canonical_request(prepare_params(params["idempotency_key"], "Different"), other_canonical, other_digest);
	PreparedTransactionStore::Admission conflict = store.admit(params["idempotency_key"], other_digest, other_canonical, operation_json, binding(), 3000, 4000);
	CHECK(conflict.kind == PreparedTransactionStore::ADMISSION_IDEMPOTENCY_CONFLICT);
	CHECK(store.get_active_count() == 1);
}

TEST_CASE("[CodexS9PreparedStore] Preview replay is immutable") {
	DeterministicIds ids;
	PreparedTransactionStore store(generate_id, &ids);
	String canonical;
	String digest;
	const Dictionary params = prepare_params();
	canonical_request(params, canonical, digest);
	const String operation_json = JSON::stringify(params["operation"], "", true, true);
	const PreparedTransactionStore::Admission admission = store.admit(params["idempotency_key"], digest, canonical, operation_json, binding(), 1000, 2000);
	Dictionary result;
	result["transaction_id"] = admission.transaction_id;
	result["state"] = "previewed";
	const String result_json = JSON::stringify(result, "", true, true);
	REQUIRE(store.publish_preview(admission.transaction_id, result_json, "{\"safe\":true}", "sha256:1111111111111111111111111111111111111111111111111111111111111111") == OK);
	PreparedTransactionStore::Admission replay = store.admit(params["idempotency_key"], digest, canonical, operation_json, binding(), 2000, 3000);
	CHECK(replay.kind == PreparedTransactionStore::ADMISSION_REPLAY);
	CHECK(replay.record.transaction_seq == 2);
	Dictionary returned;
	REQUIRE(store.get_prepare_result(admission.transaction_id, returned) == OK);
	returned["state"] = "tampered";
	Dictionary returned_again;
	REQUIRE(store.get_prepare_result(admission.transaction_id, returned_again) == OK);
	CHECK(returned_again["state"] == "previewed");
	CHECK(JSON::stringify(returned_again, "", true, true) == result_json);
}

TEST_CASE("[CodexS9PreparedStore] Expiry and conflict release active capacity but preserve bounded tombstones") {
	DeterministicIds ids;
	PreparedTransactionStore store(generate_id, &ids);
	String canonical;
	String digest;
	canonical_request(prepare_params(), canonical, digest);
	const String operation_json = JSON::stringify(prepare_params()["operation"], "", true, true);
	const PreparedTransactionStore::Admission first = store.admit("idempotency:00000000000000000000000000000001", digest, canonical, operation_json, binding(), 1000, 2000);
	store.expire(1000 + PreparedTransactionStore::PREPARED_TTL_MSEC * 1000);
	CHECK(store.get_active_count() == 0);
	CHECK(store.get_terminal_count() == 1);
	const PreparedTransactionStore::Admission expired = store.admit("idempotency:00000000000000000000000000000001", digest, canonical, operation_json, binding(), 1000 + PreparedTransactionStore::PREPARED_TTL_MSEC * 1000, 302000);
	CHECK(expired.kind == PreparedTransactionStore::ADMISSION_TRANSACTION_EXPIRED);

	const PreparedTransactionStore::Admission second = store.admit("idempotency:00000000000000000000000000000002", digest, canonical, operation_json, binding(), 2000, 3000);
	CHECK(store.conflict(second.transaction_id));
	CHECK(store.get_active_count() == 0);
	CHECK(store.get_terminal_count() == 2);
	const PreparedTransactionStore::Admission conflicted = store.admit("idempotency:00000000000000000000000000000002", digest, canonical, operation_json, binding(), 3000, 4000);
	CHECK(conflicted.kind == PreparedTransactionStore::ADMISSION_TRANSACTION_CONFLICTED);

	for (int index = 0; index < 70; index++) {
		const String key = "idempotency:" + String::num_int64(index + 100, 16).lpad(32, "0");
		const PreparedTransactionStore::Admission item = store.admit(key, digest, canonical, operation_json, binding(), 4000 + index, 5000 + index);
		REQUIRE(item.kind == PreparedTransactionStore::ADMISSION_CREATED);
		REQUIRE(store.conflict(item.transaction_id));
	}
	CHECK(store.get_active_count() == 0);
	CHECK(store.get_terminal_count() == PreparedTransactionStore::MAX_TERMINAL_TOMBSTONES);
	CHECK(store.get_total_count() == PreparedTransactionStore::MAX_TERMINAL_TOMBSTONES);
}

TEST_CASE("[CodexS9PreparedStore] Sixty-fifth active record and RNG failure fail closed") {
	DeterministicIds ids;
	PreparedTransactionStore store(generate_id, &ids);
	String canonical;
	String digest;
	canonical_request(prepare_params(), canonical, digest);
	const String operation_json = JSON::stringify(prepare_params()["operation"], "", true, true);
	for (uint32_t index = 0; index < PreparedTransactionStore::MAX_ACTIVE_RECORDS; index++) {
		const String key = "idempotency:" + String::num_uint64(index + 1, 16).lpad(32, "0");
		CHECK(store.admit(key, digest, canonical, operation_json, binding(), 1000, 2000).kind == PreparedTransactionStore::ADMISSION_CREATED);
	}
	CHECK(store.get_active_count() == PreparedTransactionStore::MAX_ACTIVE_RECORDS);
	CHECK(store.admit("idempotency:ffffffffffffffffffffffffffffffff", digest, canonical, operation_json, binding(), 1000, 2000).kind == PreparedTransactionStore::ADMISSION_BUSY);
	CHECK(store.admit("idempotency:00000000000000000000000000000001", digest, canonical, operation_json, binding(), 1000, 2000).kind == PreparedTransactionStore::ADMISSION_IN_PROGRESS);

	DeterministicIds failing_ids;
	failing_ids.fail = true;
	PreparedTransactionStore failing_store(generate_id, &failing_ids);
	CHECK(failing_store.admit("idempotency:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", digest, canonical, operation_json, binding(), 1000, 2000).kind == PreparedTransactionStore::ADMISSION_ID_FAILURE);
}

TEST_CASE("[CodexS9PreparedStore] Production IDs are opaque random values") {
	PreparedTransactionStore store;
	String canonical;
	String digest;
	canonical_request(prepare_params(), canonical, digest);
	const String operation_json = JSON::stringify(prepare_params()["operation"], "", true, true);
	const PreparedTransactionStore::Admission first = store.admit("idempotency:11111111111111111111111111111111", digest, canonical, operation_json, binding(), 1000, 2000);
	const PreparedTransactionStore::Admission second = store.admit("idempotency:22222222222222222222222222222222", digest, canonical, operation_json, binding(), 1000, 2000);
	REQUIRE(first.kind == PreparedTransactionStore::ADMISSION_CREATED);
	REQUIRE(second.kind == PreparedTransactionStore::ADMISSION_CREATED);
	CHECK(first.transaction_id.begins_with("transaction:"));
	CHECK(first.transaction_id.length() == 44);
	CHECK(first.transaction_id != second.transaction_id);
	CHECK_FALSE(first.transaction_id.contains("11111111111111111111111111111111"));
}

} // namespace TestPreparedTransactionStore

#endif // MODULE_CODEX_BRIDGE_ENABLED
