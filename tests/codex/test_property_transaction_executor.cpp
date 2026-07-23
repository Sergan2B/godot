/**************************************************************************/
/*  test_property_transaction_executor.cpp                              */
/**************************************************************************/

#include "tests/test_macros.h"

TEST_FORCE_LINK(test_property_transaction_executor)

#include "modules/modules_enabled.gen.h"

#ifdef MODULE_CODEX_BRIDGE_ENABLED

#include "core/object/undo_redo.h"
#include "scene/2d/node_2d.h"

#include "modules/codex_bridge/editor/property_transaction_executor.h"
#include "modules/codex_bridge/editor/writable_variant_codec.h"

namespace TestPropertyTransactionExecutor {

static const String EDITOR_ID = "editor:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
static const String SCENE_ID = "scene:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
static const String HISTORY_ID = "history:cccccccccccccccccccccccccccccccc";
static const String NODE_ID = "node:dddddddddddddddddddddddddddddddd";

static Dictionary vector2_wire(double p_x, double p_y) {
	Array components;
	components.push_back(p_x);
	components.push_back(p_y);
	Dictionary wire;
	wire["type"] = "vector2";
	wire["value"] = components;
	return wire;
}

static PreparedTransactionStore::Record record_for(const String &p_digest) {
	PreparedTransactionStore::Record record;
	record.operation_kind = "set_property";
	record.binding.editor_session_id = EDITOR_ID;
	record.binding.scene_id = SCENE_ID;
	record.binding.history_id = HISTORY_ID;
	record.precondition_digest = p_digest;
	return record;
}

static TransactionSceneResolver::Job job_for(Node2D *p_root, Node2D *p_target, const Dictionary &p_operation) {
	TransactionSceneResolver::Job job;
	job.editor_session_id = EDITOR_ID;
	job.scene_id = SCENE_ID;
	job.history_id = HISTORY_ID;
	job.operation = p_operation.duplicate(true);
	job.root_id = p_root->get_instance_id();
	job.scene_evidence.native_history_id = 920601;
	job.objects_by_node_id.insert(NODE_ID, p_target->get_instance_id());
	job.node_ids_by_object.insert(p_target->get_instance_id(), NODE_ID);
	return job;
}

TEST_CASE("[CodexS9TransactionExecutor][CodexS9Property] Property action applies and restores exact values") {
	Node2D *root = memnew(Node2D);
	Node2D *target = memnew(Node2D);
	root->set_name("Root");
	target->set_name("Target");
	root->add_child(target);
	target->set_position(Vector2(3, 4));

	String code;
	String message;
	WritableVariantCodec::Result old_evidence;
	REQUIRE(WritableVariantCodec::inspect_native(target->get_position(), root, target, old_evidence, code, message) == OK);
	Dictionary operation;
	operation["kind"] = "set_property";
	operation["node_id"] = NODE_ID;
	operation["property"] = "position";
	operation["value"] = vector2_wire(10, 20);
	TransactionSceneResolver::Job job = job_for(root, target, operation);
	TransactionPreviewBuilder::Resolution resolution;
	resolution.precondition_digest = old_evidence.canonical_digest;
	PreparedTransactionStore::Record record = record_for(old_evidence.canonical_digest);
	PropertyTransactionExecutor executor;
	TransactionExecutor::NativeActionPlan plan;
	REQUIRE(executor.final_preflight(record, job, resolution, plan, code, message) == OK);

	UndoRedo history;
	history.create_action("Codex property executor test", UndoRedo::MERGE_DISABLE);
	REQUIRE(executor.register_native_action_on_history(plan, &history) == OK);
	history.commit_action(true);
	CHECK(target->get_position().is_equal_approx(Vector2(10, 20)));
	CHECK(executor.verify_postcondition(record, plan));
	REQUIRE(history.undo());
	CHECK(target->get_position().is_equal_approx(Vector2(3, 4)));
	CHECK(executor.verify_prestate_after_rollback(record, plan));
	REQUIRE(history.redo());
	CHECK(target->get_position().is_equal_approx(Vector2(10, 20)));
	CHECK(executor.verify_postcondition(record, plan));
	REQUIRE(history.undo());
	history.clear_history();
	memdelete(root);
}

TEST_CASE("[CodexS9TransactionExecutor][CodexS9Property] Stale and no-op values fail before action construction") {
	Node2D *root = memnew(Node2D);
	Node2D *target = memnew(Node2D);
	root->add_child(target);
	target->set_position(Vector2(1, 2));
	String code;
	String message;
	WritableVariantCodec::Result evidence;
	REQUIRE(WritableVariantCodec::inspect_native(target->get_position(), root, target, evidence, code, message) == OK);
	Dictionary operation;
	operation["kind"] = "set_property";
	operation["node_id"] = NODE_ID;
	operation["property"] = "position";
	operation["value"] = vector2_wire(7, 8);
	TransactionSceneResolver::Job job = job_for(root, target, operation);
	TransactionPreviewBuilder::Resolution resolution;
	resolution.precondition_digest = evidence.canonical_digest;
	PreparedTransactionStore::Record record = record_for(evidence.canonical_digest);
	PropertyTransactionExecutor executor;
	TransactionExecutor::NativeActionPlan plan;
	target->set_position(Vector2(2, 3));
	CHECK(executor.final_preflight(record, job, resolution, plan, code, message) == ERR_INVALID_DATA);
	CHECK(code == "stale_editor_state");

	WritableVariantCodec::Result current;
	REQUIRE(WritableVariantCodec::inspect_native(target->get_position(), root, target, current, code, message) == OK);
	record.precondition_digest = current.canonical_digest;
	resolution.precondition_digest = current.canonical_digest;
	operation["value"] = vector2_wire(2, 3);
	job = job_for(root, target, operation);
	CHECK(executor.final_preflight(record, job, resolution, plan, code, message) == ERR_INVALID_DATA);
	CHECK(code == "property_value_unsupported");
	memdelete(root);
}

TEST_CASE("[CodexS9TransactionExecutor][CodexS9Property] Safe int-to-float widening applies and detects canonical no-ops") {
	Node2D *root = memnew(Node2D);
	Node2D *target = memnew(Node2D);
	root->add_child(target);
	target->set_rotation(1.5);
	String code;
	String message;
	WritableVariantCodec::Result old_evidence;
	REQUIRE(WritableVariantCodec::inspect_native(target->get_rotation(), root, target, old_evidence, code, message) == OK);
	Dictionary operation;
	operation["kind"] = "set_property";
	operation["node_id"] = NODE_ID;
	operation["property"] = "rotation";
	Dictionary integer;
	integer["type"] = "int";
	integer["value"] = 2;
	operation["value"] = integer;
	TransactionSceneResolver::Job job = job_for(root, target, operation);
	TransactionPreviewBuilder::Resolution resolution;
	resolution.precondition_digest = old_evidence.canonical_digest;
	PreparedTransactionStore::Record record = record_for(old_evidence.canonical_digest);
	PropertyTransactionExecutor executor;
	TransactionExecutor::NativeActionPlan plan;
	REQUIRE(executor.final_preflight(record, job, resolution, plan, code, message) == OK);
	UndoRedo history;
	history.create_action("Codex float widening test", UndoRedo::MERGE_DISABLE);
	REQUIRE(executor.register_native_action_on_history(plan, &history) == OK);
	history.commit_action(true);
	CHECK(Math::is_equal_approx((double)target->get_rotation(), 2.0));
	CHECK(executor.verify_postcondition(record, plan));
	REQUIRE(history.undo());
	CHECK(Math::is_equal_approx((double)target->get_rotation(), 1.5));
	REQUIRE(history.redo());
	WritableVariantCodec::Result current_evidence;
	REQUIRE(WritableVariantCodec::inspect_native(target->get_rotation(), root, target, current_evidence, code, message) == OK);
	record.precondition_digest = current_evidence.canonical_digest;
	resolution.precondition_digest = current_evidence.canonical_digest;
	CHECK(executor.final_preflight(record, job, resolution, plan, code, message) == ERR_INVALID_DATA);
	CHECK(code == "property_value_unsupported");
	REQUIRE(history.undo());
	history.clear_history();
	memdelete(root);
}

} // namespace TestPropertyTransactionExecutor

#endif // MODULE_CODEX_BRIDGE_ENABLED
