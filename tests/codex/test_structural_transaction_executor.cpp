/**************************************************************************/
/*  test_structural_transaction_executor.cpp                            */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "tests/test_macros.h"

TEST_FORCE_LINK(test_structural_transaction_executor)

#include "modules/modules_enabled.gen.h"

#ifdef MODULE_CODEX_BRIDGE_ENABLED

#include "core/object/undo_redo.h"
#include "scene/2d/node_2d.h"
#include "scene/3d/node_3d.h"
#include "scene/main/node.h"

#include "modules/codex_bridge/editor/bridge_editor_identity.h"
#include "modules/codex_bridge/editor/structural_transaction_executor.h"

namespace TestStructuralTransactionExecutor {

static const String EDITOR_SESSION_ID = "editor:0123456789abcdef0123456789abcdef";
static const String SCENE_ID = "scene:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
static const String HISTORY_ID = "history:cccccccccccccccccccccccccccccccc";

static void add_owned(Node *p_parent, Node *p_child, Node *p_owner) {
	p_parent->add_child(p_child, false);
	p_child->set_owner(p_owner);
}

static PreparedTransactionStore::Record record_for(const String &p_kind) {
	PreparedTransactionStore::Record record;
	record.operation_kind = p_kind;
	record.binding.editor_session_id = EDITOR_SESSION_ID;
	record.binding.scene_id = SCENE_ID;
	record.binding.history_id = HISTORY_ID;
	return record;
}

static TransactionSceneResolver::Job job_for(Node *p_root, const Dictionary &p_operation, int p_history_id) {
	TransactionSceneResolver::Job job;
	job.editor_session_id = EDITOR_SESSION_ID;
	job.scene_id = SCENE_ID;
	job.history_id = HISTORY_ID;
	job.operation = p_operation.duplicate(true);
	job.root_id = p_root->get_instance_id();
	job.scene_evidence.native_history_id = p_history_id;
	return job;
}

static void bind_node(TransactionSceneResolver::Job &r_job, const String &p_node_id, Node *p_node) {
	r_job.objects_by_node_id.insert(p_node_id, p_node->get_instance_id());
	r_job.node_ids_by_object.insert(p_node->get_instance_id(), p_node_id);
}

static void commit_plan(StructuralTransactionExecutor &p_executor, TransactionExecutor::NativeActionPlan &p_plan, UndoRedo &p_history) {
	p_history.create_action("Codex structural executor test", UndoRedo::MERGE_DISABLE);
	REQUIRE(p_executor.register_native_action_on_history(p_plan, &p_history) == OK);
	p_history.commit_action(true);
}

TEST_CASE("[CodexS9TransactionExecutor] Create is one native action with exact Undo and Redo identity") {
	constexpr int history_id = 910501;
	UndoRedo history;
	Node *root = memnew(Node);
	root->set_name("Root");
	Node *parent = memnew(Node);
	parent->set_name("Parent");
	add_owned(root, parent, root);
	Node *tail = memnew(Node);
	tail->set_name("Tail");
	add_owned(parent, tail, root);

	Dictionary operation;
	operation["kind"] = "create_node";
	operation["parent_node_id"] = "node:11111111111111111111111111111111";
	operation["godot_type"] = "Node2D";
	operation["name"] = "Created";
	operation["insertion_index"] = (int64_t)0;
	TransactionSceneResolver::Job job = job_for(root, operation, history_id);
	bind_node(job, operation["parent_node_id"], parent);
	TransactionPreviewBuilder::Resolution resolution;
	resolution.structural_nodes = 1;
	StructuralTransactionExecutor executor;
	TransactionExecutor::NativeActionPlan plan;
	String error_code;
	String error_message;
	PreparedTransactionStore::Record record = record_for("create_node");
	REQUIRE(executor.final_preflight(record, job, resolution, plan, error_code, error_message) == OK);
	commit_plan(executor, plan, history);
	REQUIRE(parent->get_child_count(false) == 2);
	Node *created = parent->get_child(0, false);
	const ObjectID created_id = created->get_instance_id();
	CHECK(created->get_name() == "Created");
	CHECK(created->get_owner() == root);
	CHECK(executor.verify_postcondition(record, plan));
	const Array committed_entities = executor.collect_committed_entities(record, plan);
	REQUIRE(committed_entities.size() == 1);
	CHECK(Dictionary(committed_entities[0])["role"] == "created");
	CHECK(Dictionary(committed_entities[0])["node_id"] == BridgeEditorIdentity::make_node_id(EDITOR_SESSION_ID, SCENE_ID, "Parent/Created"));

	REQUIRE(history.undo());
	CHECK(executor.verify_prestate_after_rollback(record, plan));
	REQUIRE(history.redo());
	CHECK(executor.verify_postcondition(record, plan));
	CHECK(parent->get_child(0, false)->get_instance_id() == created_id);
	REQUIRE(history.undo());
	plan.context.unref();
	history.clear_history();
	memdelete(root);
}

TEST_CASE("[CodexS9TransactionExecutor] Reparent preserves subtree owners and Node2D global transform") {
	constexpr int history_id = 910502;
	UndoRedo history;
	Node2D *root = memnew(Node2D);
	root->set_name("Root");
	Node2D *old_parent = memnew(Node2D);
	old_parent->set_name("OldParent");
	old_parent->set_position(Vector2(20, 30));
	add_owned(root, old_parent, root);
	Node2D *new_parent = memnew(Node2D);
	new_parent->set_name("NewParent");
	new_parent->set_position(Vector2(-11, 7));
	add_owned(root, new_parent, root);
	Node2D *target = memnew(Node2D);
	target->set_name("Target");
	target->set_position(Vector2(4, 5));
	add_owned(old_parent, target, root);
	Node *descendant = memnew(Node);
	descendant->set_name("Descendant");
	add_owned(target, descendant, target);
	const Transform2D old_local = target->get_transform();
	const Transform2D old_global = target->get_global_transform();
	const ObjectID target_id = target->get_instance_id();

	Dictionary operation;
	operation["kind"] = "reparent_node";
	operation["node_id"] = "node:22222222222222222222222222222222";
	operation["new_parent_node_id"] = "node:33333333333333333333333333333333";
	operation["insertion_index"] = (int64_t)0;
	operation["keep_global_transform"] = true;
	TransactionSceneResolver::Job job = job_for(root, operation, history_id);
	bind_node(job, operation["node_id"], target);
	bind_node(job, operation["new_parent_node_id"], new_parent);
	TransactionPreviewBuilder::Resolution resolution;
	resolution.structural_nodes = 2;
	StructuralTransactionExecutor executor;
	TransactionExecutor::NativeActionPlan plan;
	String error_code;
	String error_message;
	PreparedTransactionStore::Record record = record_for("reparent_node");
	REQUIRE(executor.final_preflight(record, job, resolution, plan, error_code, error_message) == OK);
	commit_plan(executor, plan, history);
	CHECK(executor.verify_postcondition(record, plan));
	CHECK(target->get_parent() == new_parent);
	CHECK(target->get_global_transform().is_equal_approx(old_global));
	CHECK(target->get_owner() == root);
	CHECK(descendant->get_owner() == target);
	const Array committed_entities = executor.collect_committed_entities(record, plan);
	REQUIRE(committed_entities.size() == 1);
	CHECK(Dictionary(committed_entities[0])["node_id"] == BridgeEditorIdentity::make_node_id(EDITOR_SESSION_ID, SCENE_ID, "NewParent/Target"));

	REQUIRE(history.undo());
	CHECK(executor.verify_prestate_after_rollback(record, plan));
	CHECK(target->get_parent() == old_parent);
	CHECK(target->get_transform().is_equal_approx(old_local));
	REQUIRE(history.redo());
	CHECK(executor.verify_postcondition(record, plan));
	CHECK(target->get_instance_id() == target_id);
	REQUIRE(history.undo());
	plan.context.unref();
	history.clear_history();
	memdelete(root);
}

TEST_CASE("[CodexS9TransactionExecutor] Same-parent reorder is exact in both directions and keeps local transform") {
	constexpr int history_id = 910504;
	UndoRedo history;
	Node2D *root = memnew(Node2D);
	root->set_name("Root");
	Node2D *parent = memnew(Node2D);
	parent->set_name("Parent");
	add_owned(root, parent, root);
	Node *before = memnew(Node);
	before->set_name("Before");
	add_owned(parent, before, root);
	Node2D *target = memnew(Node2D);
	target->set_name("Target");
	target->set_position(Vector2(13, 17));
	add_owned(parent, target, root);
	Node *after = memnew(Node);
	after->set_name("After");
	add_owned(parent, after, root);
	const Transform2D local_transform = target->get_transform();
	const ObjectID target_id = target->get_instance_id();

	StructuralTransactionExecutor executor;
	PreparedTransactionStore::Record record = record_for("reparent_node");
	for (int destination : { 2, 0 }) {
		Dictionary operation;
		operation["kind"] = "reparent_node";
		operation["node_id"] = "node:22222222222222222222222222222222";
		operation["new_parent_node_id"] = "node:33333333333333333333333333333333";
		operation["insertion_index"] = (int64_t)destination;
		operation["keep_global_transform"] = false;
		TransactionSceneResolver::Job job = job_for(root, operation, history_id);
		bind_node(job, operation["node_id"], target);
		bind_node(job, operation["new_parent_node_id"], parent);
		TransactionPreviewBuilder::Resolution resolution;
		resolution.structural_nodes = 1;
		TransactionExecutor::NativeActionPlan plan;
		String error_code;
		String error_message;
		REQUIRE(executor.final_preflight(record, job, resolution, plan, error_code, error_message) == OK);
		commit_plan(executor, plan, history);
		CHECK(target->get_index(false) == destination);
		CHECK(target->get_transform().is_equal_approx(local_transform));
		CHECK(executor.verify_postcondition(record, plan));
		REQUIRE(history.undo());
		CHECK(target->get_index(false) == 1);
		CHECK(executor.verify_prestate_after_rollback(record, plan));
		REQUIRE(history.redo());
		CHECK(target->get_index(false) == destination);
		CHECK(target->get_instance_id() == target_id);
		REQUIRE(history.undo());
		plan.context.unref();
		history.clear_history();
	}
	memdelete(root);
}

TEST_CASE("[CodexS9TransactionExecutor] Reparent preserves Node3D local transform when requested") {
	constexpr int history_id = 910505;
	UndoRedo history;
	Node3D *root = memnew(Node3D);
	root->set_name("Root");
	Node3D *old_parent = memnew(Node3D);
	old_parent->set_name("OldParent");
	old_parent->set_position(Vector3(20, 30, 40));
	add_owned(root, old_parent, root);
	Node3D *new_parent = memnew(Node3D);
	new_parent->set_name("NewParent");
	new_parent->set_position(Vector3(-11, 7, 5));
	add_owned(root, new_parent, root);
	Node3D *target = memnew(Node3D);
	target->set_name("Target");
	target->set_position(Vector3(4, 5, 6));
	add_owned(old_parent, target, root);
	const Transform3D old_local = target->get_transform();

	Dictionary operation;
	operation["kind"] = "reparent_node";
	operation["node_id"] = "node:22222222222222222222222222222222";
	operation["new_parent_node_id"] = "node:33333333333333333333333333333333";
	operation["insertion_index"] = (int64_t)0;
	operation["keep_global_transform"] = false;
	TransactionSceneResolver::Job job = job_for(root, operation, history_id);
	bind_node(job, operation["node_id"], target);
	bind_node(job, operation["new_parent_node_id"], new_parent);
	TransactionPreviewBuilder::Resolution resolution;
	resolution.structural_nodes = 1;
	StructuralTransactionExecutor executor;
	TransactionExecutor::NativeActionPlan plan;
	String error_code;
	String error_message;
	PreparedTransactionStore::Record record = record_for("reparent_node");
	REQUIRE(executor.final_preflight(record, job, resolution, plan, error_code, error_message) == OK);
	commit_plan(executor, plan, history);
	CHECK(target->get_transform().is_equal_approx(old_local));
	CHECK(executor.verify_postcondition(record, plan));
	REQUIRE(history.undo());
	CHECK(target->get_transform().is_equal_approx(old_local));
	CHECK(executor.verify_prestate_after_rollback(record, plan));
	REQUIRE(history.redo());
	CHECK(target->get_transform().is_equal_approx(old_local));
	REQUIRE(history.undo());
	plan.context.unref();
	history.clear_history();
	memdelete(root);
}

TEST_CASE("[CodexS9TransactionExecutor] Editable instance children support exact bounded reorder") {
	constexpr int history_id = 910506;
	UndoRedo history;
	Node *root = memnew(Node);
	root->set_name("Root");
	Node *instance = memnew(Node);
	instance->set_name("EditableInstance");
	add_owned(root, instance, root);
	root->set_editable_instance(instance, true);
	Node *target = memnew(Node);
	target->set_name("Target");
	add_owned(instance, target, instance);
	Node *sibling = memnew(Node);
	sibling->set_name("Sibling");
	add_owned(instance, sibling, instance);

	Dictionary operation;
	operation["kind"] = "reparent_node";
	operation["node_id"] = "node:22222222222222222222222222222222";
	operation["new_parent_node_id"] = "node:33333333333333333333333333333333";
	operation["insertion_index"] = (int64_t)1;
	operation["keep_global_transform"] = false;
	TransactionSceneResolver::Job job = job_for(root, operation, history_id);
	bind_node(job, operation["node_id"], target);
	bind_node(job, operation["new_parent_node_id"], instance);
	TransactionPreviewBuilder::Resolution resolution;
	resolution.structural_nodes = 1;
	StructuralTransactionExecutor executor;
	TransactionExecutor::NativeActionPlan plan;
	String error_code;
	String error_message;
	PreparedTransactionStore::Record record = record_for("reparent_node");
	REQUIRE(executor.final_preflight(record, job, resolution, plan, error_code, error_message) == OK);
	commit_plan(executor, plan, history);
	CHECK(target->get_index(false) == 1);
	CHECK(target->get_owner() == instance);
	REQUIRE(history.undo());
	CHECK(target->get_index(false) == 0);
	CHECK(executor.verify_prestate_after_rollback(record, plan));
	plan.context.unref();
	history.clear_history();
	memdelete(root);
}

TEST_CASE("[CodexS9TransactionExecutor] Delete retains the live subtree and restores exact topology") {
	constexpr int history_id = 910503;
	UndoRedo history;
	Node *root = memnew(Node);
	root->set_name("Root");
	Node *before = memnew(Node);
	before->set_name("Before");
	add_owned(root, before, root);
	Node *target = memnew(Node);
	target->set_name("Target");
	add_owned(root, target, root);
	Node *descendant = memnew(Node);
	descendant->set_name("Descendant");
	add_owned(target, descendant, target);
	Node *after = memnew(Node);
	after->set_name("After");
	add_owned(root, after, root);
	REQUIRE(target->connect(SNAME("tree_entered"), Callable(after, SNAME("get_name"))) == OK);
	const ObjectID target_id = target->get_instance_id();
	const ObjectID descendant_id = descendant->get_instance_id();

	Dictionary operation;
	operation["kind"] = "delete_node";
	operation["node_id"] = "node:22222222222222222222222222222222";
	TransactionSceneResolver::Job job = job_for(root, operation, history_id);
	bind_node(job, operation["node_id"], target);
	TransactionPreviewBuilder::Resolution resolution;
	resolution.structural_nodes = 2;
	StructuralTransactionExecutor executor;
	TransactionExecutor::NativeActionPlan plan;
	String error_code;
	String error_message;
	PreparedTransactionStore::Record record = record_for("delete_node");
	REQUIRE(executor.final_preflight(record, job, resolution, plan, error_code, error_message) == OK);
	commit_plan(executor, plan, history);
	CHECK(executor.verify_postcondition(record, plan));
	CHECK(target->get_parent() == nullptr);
	CHECK(ObjectDB::get_instance(descendant_id) == descendant);
	CHECK(target->is_connected(SNAME("tree_entered"), Callable(after, SNAME("get_name"))));

	REQUIRE(history.undo());
	CHECK(executor.verify_prestate_after_rollback(record, plan));
	CHECK(root->get_child(1, false)->get_instance_id() == target_id);
	CHECK(target->get_owner() == root);
	CHECK(descendant->get_owner() == target);
	CHECK(target->is_connected(SNAME("tree_entered"), Callable(after, SNAME("get_name"))));
	REQUIRE(history.redo());
	CHECK(executor.verify_postcondition(record, plan));
	CHECK(ObjectDB::get_instance(target_id) == target);
	REQUIRE(history.undo());
	plan.context.unref();
	history.clear_history();
	memdelete(root);
}

TEST_CASE("[CodexS9TransactionExecutor] Structural validation rejects collisions, no-ops, roots, and oversized subtrees") {
	Node *root = memnew(Node);
	root->set_name("Root");
	Node *parent = memnew(Node);
	parent->set_name("Parent");
	add_owned(root, parent, root);
	Node *existing = memnew(Node);
	existing->set_name("Existing");
	add_owned(parent, existing, root);
	StructuralTransactionExecutor executor;
	TransactionExecutor::NativeActionPlan plan;
	String error_code;
	String error_message;

	Dictionary create;
	create["kind"] = "create_node";
	create["parent_node_id"] = "node:11111111111111111111111111111111";
	create["godot_type"] = "Node";
	create["name"] = "Existing";
	TransactionSceneResolver::Job create_job = job_for(root, create, 910504);
	bind_node(create_job, create["parent_node_id"], parent);
	TransactionPreviewBuilder::Resolution create_resolution;
	create_resolution.structural_nodes = 1;
	CHECK(executor.final_preflight(record_for("create_node"), create_job, create_resolution, plan, error_code, error_message) != OK);
	CHECK(error_code == "scene_operation_unsupported");

	Node *new_parent = memnew(Node);
	new_parent->set_name("NewParent");
	add_owned(root, new_parent, root);
	Node *collision = memnew(Node);
	collision->set_name("Existing");
	add_owned(new_parent, collision, root);
	Dictionary reparent;
	reparent["kind"] = "reparent_node";
	reparent["node_id"] = "node:22222222222222222222222222222222";
	reparent["new_parent_node_id"] = "node:33333333333333333333333333333333";
	reparent["insertion_index"] = (int64_t)0;
	reparent["keep_global_transform"] = false;
	TransactionSceneResolver::Job collision_job = job_for(root, reparent, 910507);
	bind_node(collision_job, reparent["node_id"], existing);
	bind_node(collision_job, reparent["new_parent_node_id"], new_parent);
	TransactionPreviewBuilder::Resolution reparent_resolution;
	reparent_resolution.structural_nodes = 1;
	CHECK(executor.final_preflight(record_for("reparent_node"), collision_job, reparent_resolution, plan, error_code, error_message) != OK);
	CHECK(error_code == "scene_operation_unsupported");

	TransactionSceneResolver::Job no_op_job = job_for(root, reparent, 910507);
	bind_node(no_op_job, reparent["node_id"], existing);
	bind_node(no_op_job, reparent["new_parent_node_id"], parent);
	CHECK(executor.final_preflight(record_for("reparent_node"), no_op_job, reparent_resolution, plan, error_code, error_message) != OK);
	CHECK(error_code == "scene_operation_unsupported");

	Node *foreign_parent = memnew(Node);
	foreign_parent->set_name("ForeignParent");
	add_owned(root, foreign_parent, root);
	root->set_editable_instance(foreign_parent, true);
	Node *foreign_target = memnew(Node);
	foreign_target->set_name("ForeignTarget");
	add_owned(foreign_parent, foreign_target, foreign_parent);
	TransactionSceneResolver::Job owner_job = job_for(root, reparent, 910507);
	bind_node(owner_job, reparent["node_id"], foreign_target);
	bind_node(owner_job, reparent["new_parent_node_id"], new_parent);
	CHECK(executor.final_preflight(record_for("reparent_node"), owner_job, reparent_resolution, plan, error_code, error_message) != OK);
	CHECK(error_code == "node_ownership_invalid");

	Dictionary remove;
	remove["kind"] = "delete_node";
	remove["node_id"] = "node:22222222222222222222222222222222";
	TransactionSceneResolver::Job remove_root_job = job_for(root, remove, 910504);
	bind_node(remove_root_job, remove["node_id"], root);
	TransactionPreviewBuilder::Resolution remove_resolution;
	remove_resolution.structural_nodes = 1;
	CHECK(executor.final_preflight(record_for("delete_node"), remove_root_job, remove_resolution, plan, error_code, error_message) != OK);
	CHECK(error_code == "scene_operation_unsupported");

	Node *bounded_target = memnew(Node);
	bounded_target->set_name("Bounded");
	add_owned(root, bounded_target, root);
	for (int index = 0; index < 999; index++) {
		Node *child = memnew(Node);
		child->set_name(vformat("BoundedChild%d", index));
		add_owned(bounded_target, child, root);
	}
	TransactionSceneResolver::Job bounded_job = job_for(root, remove, 910508);
	bind_node(bounded_job, remove["node_id"], bounded_target);
	remove_resolution.structural_nodes = 1000;
	CHECK(executor.final_preflight(record_for("delete_node"), bounded_job, remove_resolution, plan, error_code, error_message) == OK);
	plan.context.unref();

	Node *large_target = memnew(Node);
	large_target->set_name("Large");
	add_owned(root, large_target, root);
	for (int index = 0; index < 1000; index++) {
		Node *child = memnew(Node);
		child->set_name(vformat("Child%d", index));
		add_owned(large_target, child, root);
	}
	TransactionSceneResolver::Job large_job = job_for(root, remove, 910509);
	bind_node(large_job, remove["node_id"], large_target);
	remove_resolution.structural_nodes = 1001;
	CHECK(executor.final_preflight(record_for("delete_node"), large_job, remove_resolution, plan, error_code, error_message) != OK);
	CHECK(error_code == "transaction_too_large");
	memdelete(root);
}

} // namespace TestStructuralTransactionExecutor

#endif // MODULE_CODEX_BRIDGE_ENABLED
