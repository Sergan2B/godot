/**************************************************************************/
/*  script_transaction_executor.cpp                                     */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "script_transaction_executor.h"

#include "writable_variant_codec.h"

#include "core/io/json.h"
#include "core/io/resource_loader.h"
#include "core/object/class_db.h"
#include "core/object/script_language.h"
#include "core/os/os.h"
#include "core/templates/hash_set.h"
#include "editor/editor_undo_redo_manager.h"
#include "scene/main/node.h"

#include "modules/codex_bridge/protocol/bridge_transaction_canonicalizer.h"
#include "modules/codex_bridge/protocol/bridge_transaction_profile.h"

namespace {

struct ScriptPropertySnapshot {
	StringName name;
	Variant value;
	String digest;
};

struct ScriptState {
	Ref<Script> script;
	Vector<ScriptPropertySnapshot> properties;
	String digest;
};

class ScriptPlanData : public RefCounted {
public:
	ObjectID root_id;
	ObjectID target_id;
	bool attach = false;
	ScriptState old_state;
	Ref<Script> new_script;
};

static ScriptPlanData *plan_data(const TransactionExecutor::NativeActionPlan &p_plan) {
	return p_plan.context.is_valid() ? static_cast<ScriptPlanData *>(p_plan.context.ptr()) : nullptr;
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

static bool inject_fault(const String &p_operation, const String &p_stage) {
#ifdef CODEX_BRIDGE_TESTS_ENABLED
	const String configured = OS::get_singleton()->get_environment("GODOT_CODEX_BINDING_TRANSACTION_TEST_FAULT");
	return configured == p_stage || configured == p_operation + ":" + p_stage;
#else
	(void)p_operation;
	(void)p_stage;
	return false;
#endif
}

static bool safe_script_path(const String &p_path) {
	if (!p_path.begins_with("res://") || p_path.length() <= 6 || p_path.contains("\\") || p_path.contains("::") || p_path.contains("?") || p_path.contains("#")) {
		return false;
	}
	for (const String &component : p_path.substr(6).split("/", true)) {
		if (component.is_empty() || component == "." || component == "..") {
			return false;
		}
	}
	return true;
}

static Error digest_text(const String &p_text, String &r_digest) {
	return BridgeTransactionCanonicalizer::sha256_utf8(p_text, r_digest, "godot-codex-script-state/v1\n");
}

static Variant snapshot_value(const Variant &p_value) {
	return p_value.get_type() == Variant::ARRAY || p_value.get_type() == Variant::DICTIONARY ? p_value.duplicate(true) : p_value;
}

struct PropertyInfoNameSort {
	bool operator()(const PropertyInfo &p_left, const PropertyInfo &p_right) const {
		return p_left.name < p_right.name;
	}
};

static Error load_project_script(const Dictionary &p_reference, Node *p_target, Ref<Script> &r_script, String &r_error_code, String &r_error_message) {
	const String path = WritableVariantCodec::resolve_project_resource_path(p_reference);
	if (path.is_empty() || !safe_script_path(path) || !ResourceLoader::exists(path) || !ClassDB::is_parent_class(ResourceLoader::get_resource_type(path), SNAME("Script"))) {
		return fail("script_incompatible", "The requested script is not an existing project Script resource.", r_error_code, r_error_message, ERR_DOES_NOT_EXIST);
	}
	r_script = ResourceLoader::load(path, "Script");
	if (r_script.is_null() || !r_script->is_script_valid() || !r_script->get_language() || r_script->get_instance_base_type().is_empty() || !ClassDB::is_parent_class(p_target->get_class_name(), r_script->get_instance_base_type())) {
		r_script.unref();
		return fail("script_incompatible", "The requested project script is invalid or incompatible with the target class.", r_error_code, r_error_message);
	}
	return OK;
}

static Error capture_script_state(Node *p_root, Node *p_target, ScriptState &r_state, String &r_error_code, String &r_error_message) {
	r_state = ScriptState();
	const Variant script_value = p_target->get_script();
	if (script_value.get_type() == Variant::NIL) {
		return digest_text("nil", r_state.digest);
	}
	if (script_value.get_type() != Variant::OBJECT) {
		return fail("script_incompatible", "The existing script property has an unsupported value.", r_error_code, r_error_message);
	}
	r_state.script = script_value;
	if (r_state.script.is_null() || !r_state.script->is_script_valid() || !r_state.script->get_language() || !safe_script_path(r_state.script->get_path())) {
		return fail("script_incompatible", "The existing script is not a valid project Script resource.", r_error_code, r_error_message);
	}
	List<PropertyInfo> property_list;
	r_state.script->get_script_property_list(&property_list);
	Vector<PropertyInfo> stored_properties;
	HashSet<StringName> names;
	for (const PropertyInfo &property : property_list) {
		if (!(property.usage & PROPERTY_USAGE_STORAGE) || names.has(property.name)) {
			continue;
		}
		names.insert(property.name);
		stored_properties.push_back(property);
	}
	stored_properties.sort_custom<PropertyInfoNameSort>();
	uint32_t aggregate_items = stored_properties.size();
	Array commitments;
	for (const PropertyInfo &property : stored_properties) {
		bool valid = false;
		const Variant value = p_target->get(property.name, &valid);
		if (!valid) {
			return fail("script_incompatible", "An exported script property could not be captured for exact Undo.", r_error_code, r_error_message);
		}
		WritableVariantCodec::Result evidence;
		String codec_code;
		String codec_message;
		const Error codec_error = WritableVariantCodec::inspect_native(value, p_root, p_target, evidence, codec_code, codec_message);
		if (codec_error != OK) {
			return fail(codec_error == ERR_OUT_OF_MEMORY ? "transaction_too_large" : "script_incompatible", codec_error == ERR_OUT_OF_MEMORY ? "The exported script state exceeds the bounded transaction limits." : "The exported script state contains an unsupported value.", r_error_code, r_error_message, codec_error);
		}
		aggregate_items += evidence.container_items;
		if (aggregate_items > BridgeTransactionProfile::MAX_CONTAINER_ITEMS) {
			return fail("transaction_too_large", "The exported script state exceeds the aggregate item limit.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
		}
		ScriptPropertySnapshot snapshot;
		snapshot.name = property.name;
		snapshot.value = snapshot_value(value);
		snapshot.digest = evidence.canonical_digest;
		r_state.properties.push_back(snapshot);
		Dictionary commitment;
		commitment["name"] = String(property.name);
		commitment["digest"] = evidence.canonical_digest;
		commitments.push_back(commitment);
	}
	Dictionary state;
	state["script_path"] = r_state.script->get_path();
	state["properties"] = commitments;
	const String canonical = JSON::stringify(state, "", true, true);
	if (canonical.utf8().length() > BridgeTransactionProfile::MAX_OPERATION_BYTES || digest_text(canonical, r_state.digest) != OK) {
		return fail("transaction_too_large", "The exported script state commitment exceeds the transaction limit.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
	}
	return OK;
}

static Dictionary script_summary(const Ref<Script> &p_script, const String &p_digest, const String &p_action) {
	Dictionary summary;
	summary["type"] = "script";
	summary["redacted"] = true;
	summary["digest"] = p_digest;
	summary["action"] = p_action;
	if (p_script.is_valid()) {
		summary["base_type"] = String(p_script->get_instance_base_type());
	}
	return summary;
}

static Error inspect_operation(Node *p_root, Node *p_target, const Dictionary &p_operation, ScriptState &r_old_state, Ref<Script> &r_new_script, TransactionPreviewBuilder::Resolution *r_resolution, String &r_error_code, String &r_error_message) {
	if (!p_root || !p_target || p_target->is_internal() || (p_target != p_root && !p_root->is_ancestor_of(p_target) && (p_target->get_parent() || p_target->get_owner()))) {
		return fail("node_not_editable", "The script target is outside the editable scene.", r_error_code, r_error_message, ERR_DOES_NOT_EXIST);
	}
	const String kind = p_operation.get("kind", String());
	if (kind != "attach_script" && kind != "detach_script") {
		return fail("script_incompatible", "The script executor received an unsupported operation.", r_error_code, r_error_message);
	}
	if (capture_script_state(p_root, p_target, r_old_state, r_error_code, r_error_message) != OK) {
		return ERR_INVALID_DATA;
	}
	String new_identity_digest;
	if (kind == "attach_script") {
		if (load_project_script(p_operation["script_ref"], p_target, r_new_script, r_error_code, r_error_message) != OK) {
			return ERR_INVALID_DATA;
		}
		if (r_old_state.script == r_new_script || (r_old_state.script.is_valid() && r_old_state.script->get_path() == r_new_script->get_path())) {
			return fail("script_incompatible", "The requested script is already attached to the target.", r_error_code, r_error_message);
		}
		if (digest_text(r_new_script->get_path(), new_identity_digest) != OK) {
			return fail("transaction_too_large", "The script identity commitment could not be created.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
		}
	} else {
		if (r_old_state.script.is_null()) {
			return fail("script_incompatible", "The requested target has no attached project script.", r_error_code, r_error_message);
		}
		new_identity_digest = r_old_state.digest;
	}
	if (r_resolution) {
		r_resolution->script_already_attached = r_old_state.script.is_valid();
		r_resolution->precondition_digest = r_old_state.digest;
		r_resolution->redacted_change = script_summary(kind == "attach_script" ? r_new_script : r_old_state.script, new_identity_digest, kind == "attach_script" ? "attach" : "detach");
	}
	return OK;
}

template <typename T>
static Error register_action(const TransactionExecutor::NativeActionPlan &p_plan, T *p_undo_redo) {
	ScriptPlanData *data = plan_data(p_plan);
	ERR_FAIL_NULL_V(data, ERR_INVALID_DATA);
	const String operation = data->attach ? "attach_script" : "detach_script";
	if (inject_fault(operation, "registration")) {
		return ERR_BUG;
	}
	Node *target = node_from_id(data->target_id);
	ERR_FAIL_NULL_V(target, ERR_DOES_NOT_EXIST);
	p_undo_redo->add_do_property(target, SNAME("script"), data->attach ? Variant(data->new_script) : Variant());
	p_undo_redo->add_undo_property(target, SNAME("script"), data->old_state.script.is_valid() ? Variant(data->old_state.script) : Variant());
	for (const ScriptPropertySnapshot &property : data->old_state.properties) {
		p_undo_redo->add_undo_property(target, property.name, property.value);
	}
	return OK;
}

static bool script_is(Node *p_target, const Ref<Script> &p_expected) {
	if (!p_target) {
		return false;
	}
	const Variant current = p_target->get_script();
	if (p_expected.is_null()) {
		return current.get_type() == Variant::NIL;
	}
	if (current.get_type() != Variant::OBJECT) {
		return false;
	}
	const Ref<Script> script = current;
	return script == p_expected || (script.is_valid() && script->get_path() == p_expected->get_path());
}

static bool verify_old_state(const ScriptPlanData *p_data) {
	if (!p_data) {
		return false;
	}
	Node *target = node_from_id(p_data->target_id);
	if (!script_is(target, p_data->old_state.script)) {
		return false;
	}
	for (const ScriptPropertySnapshot &property : p_data->old_state.properties) {
		bool valid = false;
		const Variant current = target->get(property.name, &valid);
		if (!valid || !WritableVariantCodec::values_equal(current, property.value)) {
			return false;
		}
	}
	return true;
}

} // namespace

Error ScriptTransactionExecutor::prepare_resolution(Node *p_scene_root, Node *p_target, const Dictionary &p_operation, TransactionPreviewBuilder::Resolution &r_resolution, String &r_error_code, String &r_error_message) {
	ScriptState old_state;
	Ref<Script> new_script;
	return inspect_operation(p_scene_root, p_target, p_operation, old_state, new_script, &r_resolution, r_error_code, r_error_message);
}

Error ScriptTransactionExecutor::final_preflight(const PreparedTransactionStore::Record &p_record, const TransactionSceneResolver::Job &p_resolver_job, const TransactionPreviewBuilder::Resolution &p_resolution, NativeActionPlan &r_plan, String &r_error_code, String &r_error_message) {
	r_plan = NativeActionPlan();
	r_error_code.clear();
	r_error_message.clear();
	const Dictionary operation = p_resolver_job.operation;
	const String kind = operation.get("kind", String());
	if ((kind != "attach_script" && kind != "detach_script") || p_record.operation_kind != kind || p_record.binding.editor_session_id != p_resolver_job.editor_session_id || p_record.binding.scene_id != p_resolver_job.scene_id || p_record.binding.history_id != p_resolver_job.history_id) {
		return fail("stale_editor_state", "The script transaction binding changed before final preflight.", r_error_code, r_error_message);
	}
	if (inject_fault(kind, "preflight")) {
		return fail("transaction_apply_failed", "The script executor injected a final-preflight failure.", r_error_code, r_error_message, ERR_BUG);
	}
	Node *root = node_from_id(p_resolver_job.root_id);
	Node *target = resolve_node(p_resolver_job, operation["node_id"]);
	ScriptState old_state;
	Ref<Script> new_script;
	if (inspect_operation(root, target, operation, old_state, new_script, nullptr, r_error_code, r_error_message) != OK) {
		return ERR_INVALID_DATA;
	}
	if (p_record.precondition_digest.is_empty() || p_record.precondition_digest != old_state.digest || p_resolution.precondition_digest != old_state.digest) {
		return fail("stale_editor_state", "The attached script state changed after the immutable preview.", r_error_code, r_error_message);
	}
	Ref<ScriptPlanData> data;
	data.instantiate();
	data->root_id = root->get_instance_id();
	data->target_id = target->get_instance_id();
	data->attach = kind == "attach_script";
	data->old_state = old_state;
	data->new_script = new_script;
	r_plan.native_history_id = p_resolver_job.scene_evidence.native_history_id;
	r_plan.context = data;
	return OK;
}

Error ScriptTransactionExecutor::register_native_action(const NativeActionPlan &p_plan, EditorUndoRedoManager *p_undo_redo) {
	ERR_FAIL_NULL_V(p_undo_redo, ERR_UNAVAILABLE);
	return register_action(p_plan, p_undo_redo);
}

Error ScriptTransactionExecutor::register_native_action_on_history(const NativeActionPlan &p_plan, UndoRedo *p_undo_redo) {
	ERR_FAIL_NULL_V(p_undo_redo, ERR_UNAVAILABLE);
	return register_action(p_plan, p_undo_redo);
}

bool ScriptTransactionExecutor::verify_postcondition(const PreparedTransactionStore::Record &p_record, const NativeActionPlan &p_plan) const {
	if (inject_fault(p_record.operation_kind, "postcondition") || inject_fault(p_record.operation_kind, "rollback_proof")) {
		return false;
	}
	ScriptPlanData *data = plan_data(p_plan);
	return data && script_is(node_from_id(data->target_id), data->attach ? data->new_script : Ref<Script>());
}

bool ScriptTransactionExecutor::verify_prestate_after_rollback(const PreparedTransactionStore::Record &p_record, const NativeActionPlan &p_plan) const {
	if (inject_fault(p_record.operation_kind, "rollback_proof")) {
		return false;
	}
	return verify_old_state(plan_data(p_plan));
}
