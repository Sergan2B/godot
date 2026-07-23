/**************************************************************************/
/*  property_transaction_executor.cpp                                   */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "property_transaction_executor.h"

#include "writable_variant_codec.h"

#include "core/object/class_db.h"
#include "core/os/os.h"
#include "editor/editor_undo_redo_manager.h"
#include "scene/main/node.h"

namespace {

class PropertyPlanData : public RefCounted {
public:
	ObjectID root_id;
	ObjectID target_id;
	StringName property;
	PropertyInfo property_info;
	Variant old_value;
	Variant new_value;
	String old_digest;
	String new_digest;
};

static PropertyPlanData *plan_data(const TransactionExecutor::NativeActionPlan &p_plan) {
	return p_plan.context.is_valid() ? static_cast<PropertyPlanData *>(p_plan.context.ptr()) : nullptr;
}

static Node *node_from_id(ObjectID p_id) {
	return Object::cast_to<Node>(ObjectDB::get_instance(p_id));
}

static Node *resolve_node(const TransactionSceneResolver::Job &p_job, const String &p_node_id) {
	const ObjectID *object_id = p_job.objects_by_node_id.getptr(p_node_id);
	return object_id ? node_from_id(*object_id) : nullptr;
}

static Error fail(const String &p_code, const String &p_message, String &r_error_code, String &r_error_message, Error p_error = ERR_INVALID_DATA) {
	r_error_code = p_code;
	r_error_message = p_message;
	return p_error;
}

static bool inject_fault(const String &p_stage) {
#ifdef CODEX_BRIDGE_TESTS_ENABLED
	const String configured = OS::get_singleton()->get_environment("GODOT_CODEX_PROPERTY_TRANSACTION_TEST_FAULT");
	return configured == p_stage || configured == "set_property:" + p_stage;
#else
	(void)p_stage;
	return false;
#endif
}

static Variant snapshot_value(const Variant &p_value) {
	return p_value.get_type() == Variant::ARRAY || p_value.get_type() == Variant::DICTIONARY ? p_value.duplicate(true) : p_value;
}

static bool property_info_matches(const PropertyInfo &p_left, const PropertyInfo &p_right) {
	return p_left.type == p_right.type && p_left.class_name == p_right.class_name && p_left.hint == p_right.hint && p_left.hint_string == p_right.hint_string && p_left.usage == p_right.usage;
}

static Error resolve_property(Node *p_root, Node *p_target, const StringName &p_property, PropertyInfo &r_info, Variant &r_value, String &r_error_code, String &r_error_message) {
	if (!p_root || !p_target || (p_target != p_root && !p_root->is_ancestor_of(p_target)) || p_target->is_internal()) {
		return fail("node_not_editable", "The property target is no longer inside the editable scene.", r_error_code, r_error_message, ERR_DOES_NOT_EXIST);
	}
	if (p_property == SNAME("script") || !ClassDB::get_property_info(p_target->get_class_name(), p_property, &r_info) || ClassDB::get_property_setter(p_target->get_class_name(), p_property).is_empty() || ClassDB::get_property_getter(p_target->get_class_name(), p_property).is_empty() || !(r_info.usage & PROPERTY_USAGE_EDITOR) || (r_info.usage & (PROPERTY_USAGE_INTERNAL | PROPERTY_USAGE_READ_ONLY | PROPERTY_USAGE_SECRET))) {
		return fail("property_not_writable", "The property is no longer a proven native editor-writable property.", r_error_code, r_error_message);
	}
	bool valid = false;
	r_value = p_target->get(p_property, &valid);
	return valid ? OK : fail("property_not_writable", "The property value could not be read through its native getter.", r_error_code, r_error_message);
}

template <typename T>
static Error register_action(const TransactionExecutor::NativeActionPlan &p_plan, T *p_undo_redo) {
	PropertyPlanData *data = plan_data(p_plan);
	ERR_FAIL_NULL_V(data, ERR_INVALID_DATA);
	if (inject_fault("registration")) {
		return ERR_BUG;
	}
	Node *target = node_from_id(data->target_id);
	ERR_FAIL_NULL_V(target, ERR_DOES_NOT_EXIST);
	p_undo_redo->add_do_property(target, data->property, data->new_value);
	p_undo_redo->add_undo_property(target, data->property, data->old_value);
	return OK;
}

static bool verify_value(const PropertyPlanData *p_data, bool p_postcondition) {
	if (!p_data) {
		return false;
	}
	Node *root = node_from_id(p_data->root_id);
	Node *target = node_from_id(p_data->target_id);
	PropertyInfo current_info;
	Variant current_value;
	String error_code;
	String error_message;
	if (resolve_property(root, target, p_data->property, current_info, current_value, error_code, error_message) != OK || !property_info_matches(current_info, p_data->property_info)) {
		return false;
	}
	return WritableVariantCodec::values_equal(current_value, p_postcondition ? p_data->new_value : p_data->old_value);
}

} // namespace

Error PropertyTransactionExecutor::final_preflight(const PreparedTransactionStore::Record &p_record, const TransactionSceneResolver::Job &p_resolver_job, const TransactionPreviewBuilder::Resolution &p_resolution, NativeActionPlan &r_plan, String &r_error_code, String &r_error_message) {
	r_plan = NativeActionPlan();
	r_error_code.clear();
	r_error_message.clear();
	if (p_record.operation_kind != "set_property" || p_record.binding.editor_session_id != p_resolver_job.editor_session_id || p_record.binding.scene_id != p_resolver_job.scene_id || p_record.binding.history_id != p_resolver_job.history_id) {
		return fail("stale_editor_state", "The property transaction binding changed before final preflight.", r_error_code, r_error_message);
	}
	if (inject_fault("preflight")) {
		return fail("transaction_apply_failed", "The property executor injected a final-preflight failure.", r_error_code, r_error_message, ERR_BUG);
	}
	Node *root = node_from_id(p_resolver_job.root_id);
	const Dictionary operation = p_resolver_job.operation;
	Node *target = resolve_node(p_resolver_job, operation["node_id"]);
	PropertyInfo property_info;
	Variant old_value;
	if (resolve_property(root, target, operation["property"], property_info, old_value, r_error_code, r_error_message) != OK) {
		return ERR_INVALID_DATA;
	}
	WritableVariantCodec::Result decoded;
	if (WritableVariantCodec::decode(operation["value"], root, target, decoded, r_error_code, r_error_message) != OK || WritableVariantCodec::validate_property_compatibility(property_info, decoded.value, r_error_code, r_error_message) != OK) {
		return ERR_INVALID_DATA;
	}
	const Variant normalized_new_value = WritableVariantCodec::normalize_property_value(property_info, decoded.value);
	WritableVariantCodec::Result old_evidence;
	if (WritableVariantCodec::inspect_native(old_value, root, target, old_evidence, r_error_code, r_error_message) != OK) {
		return ERR_INVALID_DATA;
	}
	if (p_record.precondition_digest.is_empty() || p_record.precondition_digest != old_evidence.canonical_digest || p_resolution.precondition_digest != old_evidence.canonical_digest) {
		return fail("stale_editor_state", "The property value changed after the immutable preview.", r_error_code, r_error_message);
	}
	if (WritableVariantCodec::values_equal(old_value, normalized_new_value)) {
		return fail("property_value_unsupported", "The requested property write no longer changes the value.", r_error_code, r_error_message);
	}
	WritableVariantCodec::Result new_evidence;
	if (WritableVariantCodec::inspect_native(normalized_new_value, root, target, new_evidence, r_error_code, r_error_message) != OK) {
		return ERR_INVALID_DATA;
	}
	Ref<PropertyPlanData> data;
	data.instantiate();
	data->root_id = root->get_instance_id();
	data->target_id = target->get_instance_id();
	data->property = operation["property"];
	data->property_info = property_info;
	data->old_value = snapshot_value(old_value);
	data->new_value = snapshot_value(normalized_new_value);
	data->old_digest = old_evidence.canonical_digest;
	data->new_digest = new_evidence.canonical_digest;
	r_plan.native_history_id = p_resolver_job.scene_evidence.native_history_id;
	r_plan.context = data;
	return OK;
}

Error PropertyTransactionExecutor::register_native_action(const NativeActionPlan &p_plan, EditorUndoRedoManager *p_undo_redo) {
	ERR_FAIL_NULL_V(p_undo_redo, ERR_UNAVAILABLE);
	return register_action(p_plan, p_undo_redo);
}

Error PropertyTransactionExecutor::register_native_action_on_history(const NativeActionPlan &p_plan, UndoRedo *p_undo_redo) {
	ERR_FAIL_NULL_V(p_undo_redo, ERR_UNAVAILABLE);
	return register_action(p_plan, p_undo_redo);
}

bool PropertyTransactionExecutor::verify_postcondition(const PreparedTransactionStore::Record &p_record, const NativeActionPlan &p_plan) const {
	if (inject_fault("postcondition") || inject_fault("rollback_proof")) {
		return false;
	}
	return p_record.operation_kind == "set_property" && verify_value(plan_data(p_plan), true);
}

bool PropertyTransactionExecutor::verify_prestate_after_rollback(const PreparedTransactionStore::Record &p_record, const NativeActionPlan &p_plan) const {
	if (inject_fault("rollback_proof")) {
		return false;
	}
	return p_record.operation_kind == "set_property" && verify_value(plan_data(p_plan), false);
}
