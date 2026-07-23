/**************************************************************************/
/*  test_transaction_preview_builder.cpp                                 */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "tests/test_macros.h"

TEST_FORCE_LINK(test_transaction_preview_builder)

#include "modules/modules_enabled.gen.h"

#ifdef MODULE_CODEX_BRIDGE_ENABLED

#include "core/io/file_access.h"
#include "core/io/json.h"
#include "core/templates/hash_set.h"
#include "tests/test_utils.h"

#include "modules/codex_bridge/editor/bridge_editor_identity.h"
#include "modules/codex_bridge/editor/transaction_preview_builder.h"
#include "modules/codex_bridge/protocol/bridge_frame_codec.h"
#include "modules/codex_bridge/protocol/bridge_transaction_profile.h"

namespace TestTransactionPreviewBuilder {

static PreparedTransactionStore::Binding binding() {
	PreparedTransactionStore::Binding value;
	value.project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	value.editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	value.scene_id = "scene:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
	value.history_id = "history:cccccccccccccccccccccccccccccccc";
	value.scene_revision = 7;
	value.operation_seq = 11;
	return value;
}

static Dictionary entity(const String &p_node_id, const String &p_role) {
	Dictionary value;
	value["node_id"] = p_node_id;
	value["role"] = p_role;
	return value;
}

static Dictionary scalar(const String &p_type, const Variant &p_value = Variant()) {
	Dictionary wire;
	wire["type"] = p_type;
	if (p_type != "nil") {
		wire["value"] = p_value;
	}
	return wire;
}

static Array operations() {
	const String node_a = "node:11111111111111111111111111111111";
	const String node_b = "node:22222222222222222222222222222222";
	const String node_c = "node:33333333333333333333333333333333";
	Array values;
	Dictionary create;
	create["kind"] = "create_node";
	create["parent_node_id"] = node_a;
	create["godot_type"] = "Node2D";
	create["name"] = "CreatedActor";
	create["insertion_index"] = 1;
	values.push_back(create);
	Dictionary remove;
	remove["kind"] = "delete_node";
	remove["node_id"] = node_b;
	values.push_back(remove);
	Dictionary reparent;
	reparent["kind"] = "reparent_node";
	reparent["node_id"] = node_b;
	reparent["new_parent_node_id"] = node_c;
	reparent["insertion_index"] = 0;
	reparent["keep_global_transform"] = true;
	values.push_back(reparent);
	Dictionary property;
	property["kind"] = "set_property";
	property["node_id"] = node_b;
	property["property"] = "position";
	Array vector;
	vector.push_back(12.5);
	vector.push_back(-3.0);
	property["value"] = scalar("vector2", vector);
	values.push_back(property);
	Dictionary attach;
	attach["kind"] = "attach_script";
	attach["node_id"] = node_b;
	Dictionary script_ref;
	script_ref["uid_missing"] = true;
	script_ref["path"] = "res://scripts/existing_actor.gd";
	attach["script_ref"] = script_ref;
	values.push_back(attach);
	Dictionary detach;
	detach["kind"] = "detach_script";
	detach["node_id"] = node_b;
	values.push_back(detach);
	for (const String &kind : { String("connect_signal"), String("disconnect_signal") }) {
		Dictionary signal;
		signal["kind"] = kind;
		signal["emitter_node_id"] = node_b;
		signal["signal"] = "health_changed";
		signal["receiver_node_id"] = node_c;
		signal["method"] = "_on_health_changed";
		signal["flags"] = 3;
		signal["unbinds"] = 0;
		Array binds;
		binds.push_back(scalar("string", "fixture"));
		signal["binds"] = binds;
		values.push_back(signal);
	}
	return values;
}

static TransactionPreviewBuilder::Resolution resolution_for(const Dictionary &p_operation) {
	TransactionPreviewBuilder::Resolution resolution;
	const String kind = p_operation["kind"];
	if (kind == "create_node") {
		resolution.affected_entities.push_back(entity(p_operation["parent_node_id"], "parent"));
	} else if (kind == "reparent_node") {
		resolution.affected_entities.push_back(entity(p_operation["node_id"], "target"));
		resolution.affected_entities.push_back(entity(p_operation["new_parent_node_id"], "new_parent"));
	} else if (kind == "connect_signal" || kind == "disconnect_signal") {
		resolution.affected_entities.push_back(entity(p_operation["emitter_node_id"], "emitter"));
		resolution.affected_entities.push_back(entity(p_operation["receiver_node_id"], "receiver"));
	} else {
		resolution.affected_entities.push_back(entity(p_operation["node_id"], "target"));
	}
	resolution.structural_nodes = 3;
	resolution.script_already_attached = kind == "attach_script";
	return resolution;
}

TEST_CASE("[CodexS9Preview] All operations produce bounded canonical immutable previews") {
	HashSet<String> digests;
	const Array operation_values = operations();
	for (int index = 0; index < operation_values.size(); index++) {
		const Dictionary operation = operation_values[index];
		TransactionPreviewBuilder::Output output;
		REQUIRE(TransactionPreviewBuilder::build("transaction:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", binding(), operation, resolution_for(operation), 1784690000000, 1784690300000, output) == OK);
		CHECK(BridgeTransactionProfile::validate_prepare_result(output.result));
		CHECK(output.result_json == JSON::stringify(output.result, "", true, true));
		CHECK(output.preview_payload_json.utf8().length() <= BridgeTransactionProfile::MAX_STATUS_BYTES);
		CHECK_FALSE(digests.has(output.preview_digest));
		digests.insert(output.preview_digest);
		Dictionary payload;
		REQUIRE(BridgeJson::parse_strict_object(output.preview_payload_json.to_utf8_buffer(), payload) == OK);
		CHECK(Dictionary(payload["operation"])["kind"] == operation["kind"]);
		CHECK(payload["schema_version"] == "canonical-transaction-preview/1.1");
		CHECK(String(payload["operation_digest"]).begins_with("sha256:"));
		const String encoded_operation = JSON::stringify(payload["operation"], "", true, true);
		if (operation["kind"] == "set_property") {
			CHECK_FALSE(encoded_operation.contains("12.5"));
		} else if (operation["kind"] == "attach_script") {
			CHECK_FALSE(encoded_operation.contains("existing_actor.gd"));
		} else if (operation["kind"] == "connect_signal" || operation["kind"] == "disconnect_signal") {
			CHECK_FALSE(encoded_operation.contains("fixture"));
		}
		if (operation["kind"] == "create_node") {
			const Array affected_entities = output.result["affected_entities"];
			REQUIRE(affected_entities.size() == 1);
			CHECK(Dictionary(affected_entities[0])["role"] == "parent");
		}
		Dictionary exposed = output.result.duplicate(true);
		Dictionary exposed_preview = exposed["preview"];
		exposed_preview["summary"] = "tampered";
		exposed["preview"] = exposed_preview;
		CHECK(output.result_json != JSON::stringify(exposed, "", true, true));
	}
	CHECK(digests.size() == 8);
}

TEST_CASE("[CodexS9Preview] Digest binds operation, target, value, coordinates, and expiry") {
	Dictionary operation = operations()[3];
	TransactionPreviewBuilder::Output baseline;
	REQUIRE(TransactionPreviewBuilder::build("transaction:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", binding(), operation, resolution_for(operation), 1000, 301000, baseline) == OK);
	HashSet<String> changed;
	for (int variant = 0; variant < 5; variant++) {
		Dictionary candidate = operation.duplicate(true);
		PreparedTransactionStore::Binding candidate_binding = binding();
		uint64_t expiry = 301000;
		if (variant == 0) {
			candidate["node_id"] = "node:99999999999999999999999999999999";
		} else if (variant == 1) {
			candidate["property"] = "rotation";
		} else if (variant == 2) {
			Array changed_vector;
			changed_vector.push_back(1.0);
			changed_vector.push_back(2.0);
			candidate["value"] = scalar("vector2", changed_vector);
		} else if (variant == 3) {
			candidate_binding.scene_revision++;
		} else {
			expiry++;
		}
		TransactionPreviewBuilder::Output output;
		REQUIRE(TransactionPreviewBuilder::build("transaction:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", candidate_binding, candidate, resolution_for(candidate), 1000, expiry, output) == OK);
		CHECK(output.preview_digest != baseline.preview_digest);
		changed.insert(output.preview_digest);
	}
	CHECK(changed.size() == 5);
}

TEST_CASE("[CodexS9Preview] Shared identity helper is stable and scoped") {
	const String editor_a = "editor:0123456789abcdef0123456789abcdef";
	const String editor_b = "editor:ffffffffffffffffffffffffffffffff";
	const String scene = "scene:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
	const String first = BridgeEditorIdentity::make_node_id(editor_a, scene, "Root/Child");
	CHECK(first.begins_with("node:"));
	CHECK(first.length() == 37);
	CHECK(first == BridgeEditorIdentity::make_node_id(editor_a, scene, "Root/Child"));
	CHECK(first != BridgeEditorIdentity::make_node_id(editor_a, scene, "Root/Other"));
	CHECK(first != BridgeEditorIdentity::make_node_id(editor_b, scene, "Root/Child"));
	CHECK(BridgeEditorIdentity::make_history_id(editor_a, 17) != BridgeEditorIdentity::make_history_id(editor_a, 18));
}

TEST_CASE("[CodexS9Preview] C++ reproduces all cross-language canonical vectors") {
	const String path = TestUtils::get_executable_dir().path_join("../schemas/codex_bridge/v1/fixtures/test-vectors/transaction.json").simplify_path();
	Error error = OK;
	const Variant parsed = JSON::parse_string(FileAccess::get_file_as_string(path, &error));
	REQUIRE(error == OK);
	REQUIRE(parsed.get_type() == Variant::DICTIONARY);
	const Dictionary vector = parsed;
	const Array operation_values = vector["operations"];
	const Array expected_values = vector["canonical_preview_digests"];
	REQUIRE(operation_values.size() == 8);
	REQUIRE(expected_values.size() == operation_values.size());
	for (int index = 0; index < operation_values.size(); index++) {
		const Dictionary operation = operation_values[index];
		const Dictionary expected = expected_values[index];
		TransactionPreviewBuilder::Output output;
		REQUIRE(TransactionPreviewBuilder::build("transaction:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", binding(), operation, resolution_for(operation), 1784690000000, 1784690300000, output) == OK);
		CHECK(output.result["operation_kind"] == expected["operation_kind"]);
		CHECK(output.preview_payload_json.utf8().length() == (int64_t)expected["preview_payload_bytes"]);
		CHECK(output.preview_digest == expected["preview_digest"]);
	}
}

} // namespace TestTransactionPreviewBuilder

#endif // MODULE_CODEX_BRIDGE_ENABLED
