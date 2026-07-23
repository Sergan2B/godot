/**************************************************************************/
/*  test_signal_transaction_executor.cpp                                */
/**************************************************************************/

#include "tests/test_macros.h"

TEST_FORCE_LINK(test_signal_transaction_executor)

#include "modules/modules_enabled.gen.h"

#ifdef MODULE_CODEX_BRIDGE_ENABLED

#include "core/object/object.h"
#include "core/object/undo_redo.h"
#include "scene/main/node.h"

#include "modules/codex_bridge/editor/signal_transaction_executor.h"

namespace TestSignalTransactionExecutor {

static const String EDITOR_ID = "editor:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
static const String SCENE_ID = "scene:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
static const String HISTORY_ID = "history:cccccccccccccccccccccccccccccccc";
static const String EMITTER_ID = "node:dddddddddddddddddddddddddddddddd";
static const String RECEIVER_ID = "node:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

static Dictionary bool_wire(bool p_value) {
	Dictionary wire;
	wire["type"] = "bool";
	wire["value"] = p_value;
	return wire;
}

static Dictionary operation_for(const String &p_kind, uint32_t p_flags) {
	Array binds;
	binds.push_back(bool_wire(true));
	Dictionary operation;
	operation["kind"] = p_kind;
	operation["emitter_node_id"] = EMITTER_ID;
	operation["signal"] = "pulse";
	operation["receiver_node_id"] = RECEIVER_ID;
	operation["method"] = "set_process";
	operation["flags"] = (int64_t)p_flags;
	operation["unbinds"] = (int64_t)0;
	operation["binds"] = binds;
	return operation;
}

static PreparedTransactionStore::Record record_for(const String &p_kind, const String &p_digest) {
	PreparedTransactionStore::Record record;
	record.operation_kind = p_kind;
	record.binding.editor_session_id = EDITOR_ID;
	record.binding.scene_id = SCENE_ID;
	record.binding.history_id = HISTORY_ID;
	record.precondition_digest = p_digest;
	return record;
}

static TransactionSceneResolver::Job job_for(Node *p_root, Node *p_emitter, Node *p_receiver, const Dictionary &p_operation) {
	TransactionSceneResolver::Job job;
	job.editor_session_id = EDITOR_ID;
	job.scene_id = SCENE_ID;
	job.history_id = HISTORY_ID;
	job.operation = p_operation.duplicate(true);
	job.root_id = p_root->get_instance_id();
	job.scene_evidence.native_history_id = 920702;
	job.objects_by_node_id.insert(EMITTER_ID, p_emitter->get_instance_id());
	job.objects_by_node_id.insert(RECEIVER_ID, p_receiver->get_instance_id());
	job.node_ids_by_object.insert(p_emitter->get_instance_id(), EMITTER_ID);
	job.node_ids_by_object.insert(p_receiver->get_instance_id(), RECEIVER_ID);
	return job;
}

static bool exact_connection(Node *p_emitter, Node *p_receiver, uint32_t p_flags) {
	List<Object::Connection> connections;
	p_emitter->get_signal_connection_list(SNAME("pulse"), &connections);
	for (const Object::Connection &connection : connections) {
		const Array binds = connection.callable.get_bound_arguments();
		if (connection.flags == p_flags && connection.callable.get_object_id() == p_receiver->get_instance_id() && connection.callable.get_method() == SNAME("set_process") && connection.callable.get_unbound_arguments_count() == 0 && binds.size() == 1 && binds[0].get_type() == Variant::BOOL && (bool)binds[0]) {
			return true;
		}
	}
	return false;
}

static void commit_plan(SignalTransactionExecutor &p_executor, TransactionExecutor::NativeActionPlan &p_plan, UndoRedo &p_history) {
	p_history.create_action("Codex signal executor test", UndoRedo::MERGE_DISABLE);
	REQUIRE(p_executor.register_native_action_on_history(p_plan, &p_history) == OK);
	p_history.commit_action(true);
}

TEST_CASE("[CodexS9TransactionExecutor][CodexS9TransactionBindings][CodexS9Signal] Connect and disconnect preserve exact callable, binds, flags, and Undo") {
	const uint32_t flags = Object::CONNECT_PERSIST | Object::CONNECT_DEFERRED;
	Node *root = memnew(Node);
	Node *emitter = memnew(Node);
	Node *receiver = memnew(Node);
	root->add_child(emitter);
	root->add_child(receiver);
	emitter->set_owner(root);
	receiver->set_owner(root);
	emitter->add_user_signal(MethodInfo("pulse"));
	SignalTransactionExecutor executor;
	UndoRedo history;
	String code;
	String message;

	Dictionary connect_operation = operation_for("connect_signal", flags);
	TransactionPreviewBuilder::Resolution connect_resolution;
	REQUIRE(SignalTransactionExecutor::prepare_resolution(root, emitter, receiver, connect_operation, connect_resolution, code, message) == OK);
	PreparedTransactionStore::Record connect_record = record_for("connect_signal", connect_resolution.precondition_digest);
	TransactionSceneResolver::Job connect_job = job_for(root, emitter, receiver, connect_operation);
	TransactionExecutor::NativeActionPlan connect_plan;
	REQUIRE(executor.final_preflight(connect_record, connect_job, connect_resolution, connect_plan, code, message) == OK);
	commit_plan(executor, connect_plan, history);
	CHECK(exact_connection(emitter, receiver, flags));
	CHECK(executor.verify_postcondition(connect_record, connect_plan));
	REQUIRE(history.undo());
	CHECK_FALSE(exact_connection(emitter, receiver, flags));
	CHECK(executor.verify_prestate_after_rollback(connect_record, connect_plan));
	REQUIRE(history.redo());
	CHECK(exact_connection(emitter, receiver, flags));
	connect_plan.context.unref();
	history.clear_history(false);

	Dictionary disconnect_operation = operation_for("disconnect_signal", flags);
	TransactionPreviewBuilder::Resolution disconnect_resolution;
	REQUIRE(SignalTransactionExecutor::prepare_resolution(root, emitter, receiver, disconnect_operation, disconnect_resolution, code, message) == OK);
	PreparedTransactionStore::Record disconnect_record = record_for("disconnect_signal", disconnect_resolution.precondition_digest);
	TransactionSceneResolver::Job disconnect_job = job_for(root, emitter, receiver, disconnect_operation);
	TransactionExecutor::NativeActionPlan disconnect_plan;
	REQUIRE(executor.final_preflight(disconnect_record, disconnect_job, disconnect_resolution, disconnect_plan, code, message) == OK);
	commit_plan(executor, disconnect_plan, history);
	CHECK_FALSE(exact_connection(emitter, receiver, flags));
	CHECK(executor.verify_postcondition(disconnect_record, disconnect_plan));
	REQUIRE(history.undo());
	CHECK(exact_connection(emitter, receiver, flags));
	CHECK(executor.verify_prestate_after_rollback(disconnect_record, disconnect_plan));
	REQUIRE(history.redo());
	CHECK_FALSE(exact_connection(emitter, receiver, flags));
	disconnect_plan.context.unref();
	history.clear_history();
	memdelete(root);
}

TEST_CASE("[CodexS9TransactionExecutor][CodexS9TransactionBindings][CodexS9Signal] Non-persistent, reference-counted, incompatible, and duplicate connections fail closed") {
	Node *root = memnew(Node);
	Node *emitter = memnew(Node);
	Node *receiver = memnew(Node);
	root->add_child(emitter);
	root->add_child(receiver);
	emitter->set_owner(root);
	receiver->set_owner(root);
	emitter->add_user_signal(MethodInfo("pulse"));
	String code;
	String message;
	TransactionPreviewBuilder::Resolution resolution;
	Dictionary operation = operation_for("connect_signal", 0);
	CHECK(SignalTransactionExecutor::prepare_resolution(root, emitter, receiver, operation, resolution, code, message) != OK);
	CHECK(code == "signal_connection_invalid");
	operation["flags"] = (int64_t)(Object::CONNECT_PERSIST | Object::CONNECT_REFERENCE_COUNTED);
	CHECK(SignalTransactionExecutor::prepare_resolution(root, emitter, receiver, operation, resolution, code, message) != OK);
	CHECK(code == "signal_connection_invalid");
	operation["flags"] = (int64_t)Object::CONNECT_PERSIST;
	operation["binds"] = Array();
	CHECK(SignalTransactionExecutor::prepare_resolution(root, emitter, receiver, operation, resolution, code, message) != OK);
	CHECK(code == "signal_connection_invalid");
	operation = operation_for("connect_signal", Object::CONNECT_PERSIST);
	Callable callable(receiver, SNAME("set_process"));
	callable = callable.bind(true);
	REQUIRE(emitter->connect(SNAME("pulse"), callable, Object::CONNECT_PERSIST) == OK);
	CHECK(SignalTransactionExecutor::prepare_resolution(root, emitter, receiver, operation, resolution, code, message) != OK);
	CHECK(code == "signal_connection_invalid");
	memdelete(root);
}

} // namespace TestSignalTransactionExecutor

#endif // MODULE_CODEX_BRIDGE_ENABLED
