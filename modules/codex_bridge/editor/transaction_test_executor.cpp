/**************************************************************************/
/*  transaction_test_executor.cpp                                       */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "transaction_test_executor.h"

#ifdef CODEX_BRIDGE_TESTS_ENABLED

#include "core/os/os.h"
#include "editor/editor_undo_redo_manager.h"
#include "scene/2d/node_2d.h"

namespace {

static Node2D *payload_target(const TransactionExecutor::NativeActionPlan &p_plan) {
	if (p_plan.payload.get_type() != Variant::DICTIONARY) {
		return nullptr;
	}
	const Dictionary payload = p_plan.payload;
	Object *object = payload.get("target", Variant());
	return Object::cast_to<Node2D>(object);
}

static String fault_mode() {
	return OS::get_singleton()->get_environment("GODOT_CODEX_TRANSACTION_TEST_FAULT");
}

} // namespace

Error TransactionTestExecutor::final_preflight(const PreparedTransactionStore::Record &p_record, const TransactionSceneResolver::Job &p_resolver_job, const TransactionPreviewBuilder::Resolution &p_resolution, NativeActionPlan &r_plan, String &r_error_code, String &r_error_message) {
	(void)p_record;
	(void)p_resolution;
	r_plan = NativeActionPlan();
	if (fault_mode() == "preflight") {
		r_error_code = "transaction_apply_failed";
		r_error_message = "The test executor injected a precommit failure.";
		return ERR_BUG;
	}
	const Dictionary operation = p_resolver_job.operation;
	if (operation.get("kind", String()) != "set_property" || operation.get("property", String()) != "position" || operation.get("value", Variant()).get_type() != Variant::DICTIONARY) {
		r_error_code = "scene_operation_unsupported";
		r_error_message = "The test executor accepts only the fixture Node2D position operation.";
		return ERR_UNAVAILABLE;
	}
	const Dictionary wire_value = operation["value"];
	if (wire_value.get("type", String()) != "vector2" || wire_value.get("value", Variant()).get_type() != Variant::ARRAY) {
		r_error_code = "property_value_unsupported";
		r_error_message = "The test executor requires a bounded Vector2 value.";
		return ERR_INVALID_DATA;
	}
	const Array values = wire_value["value"];
	if (values.size() != 2 || (values[0].get_type() != Variant::FLOAT && values[0].get_type() != Variant::INT) || (values[1].get_type() != Variant::FLOAT && values[1].get_type() != Variant::INT) || !Math::is_finite((double)values[0]) || !Math::is_finite((double)values[1])) {
		r_error_code = "property_value_unsupported";
		r_error_message = "The test executor Vector2 value is invalid.";
		return ERR_INVALID_DATA;
	}
	const String node_id = operation["node_id"];
	const ObjectID *object_id = p_resolver_job.objects_by_node_id.getptr(node_id);
	Node2D *target = object_id ? Object::cast_to<Node2D>(ObjectDB::get_instance(*object_id)) : nullptr;
	if (!target) {
		r_error_code = "node_not_editable";
		r_error_message = "The test executor target disappeared before commit.";
		return ERR_DOES_NOT_EXIST;
	}
	Dictionary payload;
	payload["target"] = target;
	payload["before"] = target->get_position();
	payload["after"] = Vector2((double)values[0], (double)values[1]);
	r_plan.native_history_id = p_resolver_job.scene_evidence.native_history_id;
	r_plan.payload = payload;
	return OK;
}

Error TransactionTestExecutor::register_native_action(const NativeActionPlan &p_plan, EditorUndoRedoManager *p_undo_redo) {
	ERR_FAIL_NULL_V(p_undo_redo, ERR_UNAVAILABLE);
	if (fault_mode() == "registration") {
		return ERR_BUG;
	}
	Node2D *target = payload_target(p_plan);
	ERR_FAIL_NULL_V(target, ERR_DOES_NOT_EXIST);
	const Dictionary payload = p_plan.payload;
	p_undo_redo->add_do_property(target, SNAME("position"), payload["after"]);
	p_undo_redo->add_undo_property(target, SNAME("position"), payload["before"]);
	return OK;
}

bool TransactionTestExecutor::verify_postcondition(const PreparedTransactionStore::Record &p_record, const NativeActionPlan &p_plan) const {
	(void)p_record;
	if (fault_mode() == "postcondition" || fault_mode() == "rollback_proof") {
		return false;
	}
	Node2D *target = payload_target(p_plan);
	return target && target->get_position() == Vector2(Dictionary(p_plan.payload)["after"]);
}

bool TransactionTestExecutor::verify_prestate_after_rollback(const PreparedTransactionStore::Record &p_record, const NativeActionPlan &p_plan) const {
	(void)p_record;
	if (fault_mode() == "rollback_proof") {
		return false;
	}
	Node2D *target = payload_target(p_plan);
	return target && target->get_position() == Vector2(Dictionary(p_plan.payload)["before"]);
}

#endif // CODEX_BRIDGE_TESTS_ENABLED
