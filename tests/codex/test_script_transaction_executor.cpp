/**************************************************************************/
/*  test_script_transaction_executor.cpp                                */
/**************************************************************************/

#include "tests/test_macros.h"

TEST_FORCE_LINK(test_script_transaction_executor)

#include "modules/modules_enabled.gen.h"

#ifdef MODULE_CODEX_BRIDGE_ENABLED

#include "core/io/resource_loader.h"
#include "core/object/script_language.h"
#include "core/object/undo_redo.h"
#include "scene/main/node.h"

#include "modules/codex_bridge/editor/script_transaction_executor.h"

#ifdef MODULE_GDSCRIPT_ENABLED
#include "modules/gdscript/gdscript.h"
#endif

namespace TestScriptTransactionExecutor {

#ifdef MODULE_GDSCRIPT_ENABLED
class GDScriptLanguageScope {
	GDScriptLanguage *language = nullptr;

public:
	GDScriptLanguageScope() {
		language = GDScriptLanguage::get_singleton();
		if (language) {
			language->init();
		}
	}
	~GDScriptLanguageScope() {
		if (language) {
			language->finish();
		}
	}
};
#endif

static const String EDITOR_ID = "editor:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
static const String SCENE_ID = "scene:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
static const String HISTORY_ID = "history:cccccccccccccccccccccccccccccccc";
static const String NODE_ID = "node:dddddddddddddddddddddddddddddddd";
static const String FIXTURE_SCRIPT = "res://tests/codex/fixtures/transaction_prepare_project/scripts/fixture_endpoint.gd";
static const String REPLACEMENT_SCRIPT = "res://tests/codex/fixtures/transaction_prepare_project/scripts/replacement_endpoint.gd";
static const String INCOMPATIBLE_SCRIPT = "res://tests/codex/fixtures/transaction_prepare_project/scripts/incompatible.gd";

static Dictionary script_ref(const String &p_path) {
	Dictionary reference;
	reference["uid_missing"] = true;
	reference["path"] = p_path;
	return reference;
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

static TransactionSceneResolver::Job job_for(Node *p_root, Node *p_target, const Dictionary &p_operation) {
	TransactionSceneResolver::Job job;
	job.editor_session_id = EDITOR_ID;
	job.scene_id = SCENE_ID;
	job.history_id = HISTORY_ID;
	job.operation = p_operation.duplicate(true);
	job.root_id = p_root->get_instance_id();
	job.scene_evidence.native_history_id = 920701;
	job.objects_by_node_id.insert(NODE_ID, p_target->get_instance_id());
	job.node_ids_by_object.insert(p_target->get_instance_id(), NODE_ID);
	return job;
}

static Ref<Script> load_script(const String &p_path) {
	return ResourceLoader::load(p_path, "Script", ResourceFormatLoader::CACHE_MODE_REUSE);
}

static void commit_plan(ScriptTransactionExecutor &p_executor, TransactionExecutor::NativeActionPlan &p_plan, UndoRedo &p_history) {
	p_history.create_action("Codex script executor test", UndoRedo::MERGE_DISABLE);
	REQUIRE(p_executor.register_native_action_on_history(p_plan, &p_history) == OK);
	p_history.commit_action(true);
}

TEST_CASE("[CodexS9TransactionExecutor][CodexS9TransactionBindings][CodexS9Script] Script attachment, detachment, and rejection preserve exact state") {
#ifdef MODULE_GDSCRIPT_ENABLED
	GDScriptLanguageScope language_scope;
#endif
	{
		Ref<Script> old_script = load_script(FIXTURE_SCRIPT);
		Ref<Script> replacement = load_script(REPLACEMENT_SCRIPT);
		REQUIRE(old_script.is_valid());
		REQUIRE(replacement.is_valid());
		Node *root = memnew(Node);
		Node *target = memnew(Node);
		root->add_child(target);
		target->set_script(old_script);
		target->set("custom_value", 19);
		REQUIRE((int64_t)target->get("custom_value") == 19);

		Dictionary operation;
		operation["kind"] = "attach_script";
		operation["node_id"] = NODE_ID;
		operation["script_ref"] = script_ref(REPLACEMENT_SCRIPT);
		String code;
		String message;
		TransactionPreviewBuilder::Resolution resolution;
		REQUIRE(ScriptTransactionExecutor::prepare_resolution(root, target, operation, resolution, code, message) == OK);
		PreparedTransactionStore::Record record = record_for("attach_script", resolution.precondition_digest);
		TransactionSceneResolver::Job job = job_for(root, target, operation);
		ScriptTransactionExecutor executor;
		TransactionExecutor::NativeActionPlan plan;
		REQUIRE(executor.final_preflight(record, job, resolution, plan, code, message) == OK);
		UndoRedo history;
		commit_plan(executor, plan, history);
		CHECK(Ref<Script>(target->get_script())->get_path() == REPLACEMENT_SCRIPT);
		CHECK(executor.verify_postcondition(record, plan));
		REQUIRE(history.undo());
		CHECK(Ref<Script>(target->get_script())->get_path() == FIXTURE_SCRIPT);
		CHECK((int64_t)target->get("custom_value") == 19);
		CHECK(executor.verify_prestate_after_rollback(record, plan));
		REQUIRE(history.redo());
		CHECK(Ref<Script>(target->get_script())->get_path() == REPLACEMENT_SCRIPT);
		REQUIRE(history.undo());
		plan.context.unref();
		history.clear_history();
		memdelete(root);
	}

	{
		Ref<Script> script = load_script(FIXTURE_SCRIPT);
		REQUIRE(script.is_valid());
		Node *root = memnew(Node);
		Node *target = memnew(Node);
		root->add_child(target);
		target->set_script(script);
		target->set("custom_value", 27);
		REQUIRE((int64_t)target->get("custom_value") == 27);
		Dictionary operation;
		operation["kind"] = "detach_script";
		operation["node_id"] = NODE_ID;
		String code;
		String message;
		TransactionPreviewBuilder::Resolution resolution;
		REQUIRE(ScriptTransactionExecutor::prepare_resolution(root, target, operation, resolution, code, message) == OK);
		PreparedTransactionStore::Record record = record_for("detach_script", resolution.precondition_digest);
		TransactionSceneResolver::Job job = job_for(root, target, operation);
		ScriptTransactionExecutor executor;
		TransactionExecutor::NativeActionPlan plan;
		REQUIRE(executor.final_preflight(record, job, resolution, plan, code, message) == OK);
		UndoRedo history;
		commit_plan(executor, plan, history);
		CHECK(target->get_script().get_type() == Variant::NIL);
		CHECK(executor.verify_postcondition(record, plan));
		REQUIRE(history.undo());
		CHECK(Ref<Script>(target->get_script())->get_path() == FIXTURE_SCRIPT);
		CHECK((int64_t)target->get("custom_value") == 27);
		CHECK(executor.verify_prestate_after_rollback(record, plan));
		REQUIRE(history.redo());
		CHECK(target->get_script().get_type() == Variant::NIL);
		REQUIRE(history.undo());
		plan.context.unref();
		history.clear_history();
		memdelete(root);
	}

	{
		Ref<Script> script = load_script(FIXTURE_SCRIPT);
		REQUIRE(script.is_valid());
		Node *root = memnew(Node);
		Node *target = memnew(Node);
		root->add_child(target);
		target->set_script(script);
		String code;
		String message;
		TransactionPreviewBuilder::Resolution resolution;
		Dictionary operation;
		operation["kind"] = "attach_script";
		operation["node_id"] = NODE_ID;
		operation["script_ref"] = script_ref(FIXTURE_SCRIPT);
		CHECK(ScriptTransactionExecutor::prepare_resolution(root, target, operation, resolution, code, message) != OK);
		CHECK(code == "script_incompatible");
		operation["script_ref"] = script_ref(INCOMPATIBLE_SCRIPT);
		CHECK(ScriptTransactionExecutor::prepare_resolution(root, target, operation, resolution, code, message) != OK);
		CHECK(code == "script_incompatible");
		operation["script_ref"] = script_ref("res://tests/codex/fixtures/transaction_prepare_project/scripts/missing.gd");
		CHECK(ScriptTransactionExecutor::prepare_resolution(root, target, operation, resolution, code, message) != OK);
		CHECK(code == "script_incompatible");
		memdelete(root);
	}
}

} // namespace TestScriptTransactionExecutor

#endif // MODULE_CODEX_BRIDGE_ENABLED
