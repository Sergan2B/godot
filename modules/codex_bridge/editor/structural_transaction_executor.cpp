/**************************************************************************/
/*  structural_transaction_executor.cpp                                 */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "structural_transaction_executor.h"

#include "bridge_editor_identity.h"

#include "core/object/class_db.h"
#include "core/os/os.h"
#include "editor/editor_undo_redo_manager.h"
#include "scene/2d/node_2d.h"
#include "scene/3d/node_3d.h"
#include "scene/main/node.h"

namespace {

enum StructuralKind {
	STRUCTURAL_CREATE,
	STRUCTURAL_REPARENT,
	STRUCTURAL_DELETE,
};

enum TransformKind {
	TRANSFORM_NONE,
	TRANSFORM_2D,
	TRANSFORM_3D,
};

struct NodeSnapshot {
	ObjectID node_id;
	ObjectID parent_id;
	ObjectID owner_id;
	int sibling_index = -1;
};

class StructuralPlanData : public RefCounted {
public:
	StructuralKind kind = STRUCTURAL_CREATE;
	TransformKind transform_kind = TRANSFORM_NONE;
	ObjectID root_id;
	ObjectID parent_id;
	ObjectID target_id;
	ObjectID new_parent_id;
	ObjectID created_id;
	String editor_session_id;
	String scene_id;
	StringName requested_type;
	StringName requested_name;
	int old_index = -1;
	int new_index = -1;
	int parent_child_count_before = 0;
	bool same_parent = false;
	bool keep_global_transform = false;
	bool owns_created_node = false;
	bool created_reference_transferred = false;
	Variant old_local_transform;
	Variant old_global_transform;
	Vector<NodeSnapshot> subtree;

	~StructuralPlanData() override {
		if (!owns_created_node || created_reference_transferred) {
			return;
		}
		Node *created = Object::cast_to<Node>(ObjectDB::get_instance(created_id));
		if (created && !created->get_parent()) {
			memdelete(created);
		}
	}
};

static Node *node_from_id(ObjectID p_id) {
	return Object::cast_to<Node>(ObjectDB::get_instance(p_id));
}

static StructuralPlanData *plan_data(const TransactionExecutor::NativeActionPlan &p_plan) {
	return p_plan.context.is_valid() ? static_cast<StructuralPlanData *>(p_plan.context.ptr()) : nullptr;
}

static Error fail(const String &p_code, const String &p_message, String &r_error_code, String &r_error_message, Error p_error = ERR_INVALID_DATA) {
	r_error_code = p_code;
	r_error_message = p_message;
	return p_error;
}

static bool inject_fault(const String &p_operation_kind, const String &p_stage) {
#ifdef CODEX_BRIDGE_TESTS_ENABLED
	const String configured = OS::get_singleton()->get_environment("GODOT_CODEX_STRUCTURAL_TRANSACTION_TEST_FAULT");
	return configured == p_stage || configured == p_operation_kind + ":" + p_stage;
#else
	(void)p_operation_kind;
	(void)p_stage;
	return false;
#endif
}

static bool is_editable(Node *p_root, Node *p_node) {
	if (!p_root || !p_node || p_node->is_internal()) {
		return false;
	}
	if (p_node != p_root && !p_root->is_ancestor_of(p_node)) {
		return !p_node->get_parent() && !p_node->get_owner();
	}
	if (p_node == p_root || p_node->get_owner() == p_root) {
		return true;
	}
	Node *owner = p_node->get_owner();
	return owner && p_root->is_ancestor_of(owner) && p_root->is_editable_instance(owner);
}

static Node *resolve_node(const TransactionSceneResolver::Job &p_job, const String &p_node_id) {
	const ObjectID *object_id = p_job.objects_by_node_id.getptr(p_node_id);
	return object_id ? node_from_id(*object_id) : nullptr;
}

static bool direct_child_named(Node *p_parent, const StringName &p_name, Node *p_except = nullptr) {
	for (int index = 0; index < p_parent->get_child_count(false); index++) {
		Node *child = p_parent->get_child(index, false);
		if (child != p_except && child->get_name() == p_name) {
			return true;
		}
	}
	return false;
}

static bool owner_is_in_scene(Node *p_root, Node *p_owner) {
	return p_owner && (p_owner == p_root || p_root->is_ancestor_of(p_owner));
}

static Error capture_subtree(Node *p_root, Node *p_target, Vector<NodeSnapshot> &r_subtree, String &r_error_code, String &r_error_message) {
	Vector<Node *> pending;
	pending.push_back(p_target);
	while (!pending.is_empty()) {
		Node *node = pending[pending.size() - 1];
		pending.resize(pending.size() - 1);
		if (r_subtree.size() >= (int)TransactionSceneResolver::MAX_STRUCTURAL_NODES) {
			return fail("transaction_too_large", "The requested structural subtree exceeds the bounded node limit.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
		}
		Node *owner = node->get_owner();
		if (!owner_is_in_scene(p_root, owner) || !owner->is_ancestor_of(node)) {
			return fail("node_ownership_invalid", "The structural subtree contains an invalid scene owner.", r_error_code, r_error_message);
		}
		NodeSnapshot snapshot;
		snapshot.node_id = node->get_instance_id();
		snapshot.parent_id = node->get_parent() ? node->get_parent()->get_instance_id() : ObjectID();
		snapshot.owner_id = owner->get_instance_id();
		snapshot.sibling_index = node->get_index(false);
		r_subtree.push_back(snapshot);
		for (int child_index = node->get_child_count(false) - 1; child_index >= 0; child_index--) {
			pending.push_back(node->get_child(child_index, false));
		}
	}
	return OK;
}

static bool owners_remain_valid_after_reparent(Node *p_root, Node *p_target, Node *p_new_parent, const Vector<NodeSnapshot> &p_subtree) {
	for (const NodeSnapshot &snapshot : p_subtree) {
		Node *owner = node_from_id(snapshot.owner_id);
		if (!owner_is_in_scene(p_root, owner)) {
			return false;
		}
		if (owner == p_root || owner == p_new_parent || owner->is_ancestor_of(p_new_parent) || owner == p_target || p_target->is_ancestor_of(owner)) {
			continue;
		}
		return false;
	}
	return true;
}

static void capture_transform(Node *p_target, StructuralPlanData &r_data) {
	if (Node2D *node_2d = Object::cast_to<Node2D>(p_target)) {
		r_data.transform_kind = TRANSFORM_2D;
		r_data.old_local_transform = node_2d->get_transform();
		if (r_data.keep_global_transform) {
			r_data.old_global_transform = node_2d->get_global_transform();
		}
	} else if (Node3D *node_3d = Object::cast_to<Node3D>(p_target)) {
		r_data.transform_kind = TRANSFORM_3D;
		r_data.old_local_transform = node_3d->get_transform();
		if (r_data.keep_global_transform) {
			r_data.old_global_transform = node_3d->get_global_transform();
		}
	}
}

static bool transform_matches(Node *p_target, const StructuralPlanData &p_data, bool p_postcondition) {
	if (p_data.transform_kind == TRANSFORM_2D) {
		Node2D *node = Object::cast_to<Node2D>(p_target);
		if (!node) {
			return false;
		}
		const Transform2D expected = p_postcondition && p_data.keep_global_transform ? Transform2D(p_data.old_global_transform) : Transform2D(p_data.old_local_transform);
		const Transform2D actual = p_postcondition && p_data.keep_global_transform ? node->get_global_transform() : node->get_transform();
		return actual.is_equal_approx(expected);
	}
	if (p_data.transform_kind == TRANSFORM_3D) {
		Node3D *node = Object::cast_to<Node3D>(p_target);
		if (!node) {
			return false;
		}
		const Transform3D expected = p_postcondition && p_data.keep_global_transform ? Transform3D(p_data.old_global_transform) : Transform3D(p_data.old_local_transform);
		const Transform3D actual = p_postcondition && p_data.keep_global_transform ? node->get_global_transform() : node->get_transform();
		return actual.is_equal_approx(expected);
	}
	return true;
}

static bool subtree_matches(const StructuralPlanData &p_data, Node *p_target, Node *p_expected_target_parent, int p_expected_target_index, bool p_check_owners) {
	for (int index = 0; index < p_data.subtree.size(); index++) {
		const NodeSnapshot &snapshot = p_data.subtree[index];
		Node *node = node_from_id(snapshot.node_id);
		if (!node) {
			return false;
		}
		Node *expected_parent = index == 0 ? p_expected_target_parent : node_from_id(snapshot.parent_id);
		const int expected_index = index == 0 ? p_expected_target_index : snapshot.sibling_index;
		if (node->get_parent() != expected_parent || (expected_parent && node->get_index(false) != expected_index)) {
			return false;
		}
		if (p_check_owners && node->get_owner() != node_from_id(snapshot.owner_id)) {
			return false;
		}
	}
	return !p_data.subtree.is_empty() && node_from_id(p_data.subtree[0].node_id) == p_target;
}

class NativeActionRegistrar {
	EditorUndoRedoManager *manager = nullptr;
	UndoRedo *history = nullptr;

public:
	explicit NativeActionRegistrar(EditorUndoRedoManager *p_manager) :
			manager(p_manager) {}
	explicit NativeActionRegistrar(UndoRedo *p_history) :
			history(p_history) {}

	template <typename... VarArgs>
	void add_do_method(Object *p_object, const StringName &p_method, VarArgs... p_args) {
		if (manager) {
			manager->add_do_method(p_object, p_method, p_args...);
		} else {
			history->add_do_method(Callable(p_object, p_method).bind(p_args...));
		}
	}

	template <typename... VarArgs>
	void add_undo_method(Object *p_object, const StringName &p_method, VarArgs... p_args) {
		if (manager) {
			manager->add_undo_method(p_object, p_method, p_args...);
		} else {
			history->add_undo_method(Callable(p_object, p_method).bind(p_args...));
		}
	}

	void add_do_reference(Object *p_object) {
		if (manager) {
			manager->add_do_reference(p_object);
		} else {
			history->add_do_reference(p_object);
		}
	}

	void add_undo_reference(Object *p_object) {
		if (manager) {
			manager->add_undo_reference(p_object);
		} else {
			history->add_undo_reference(p_object);
		}
	}
};

static void register_owner_methods(NativeActionRegistrar &p_registrar, const Vector<NodeSnapshot> &p_subtree, bool p_do) {
	for (const NodeSnapshot &snapshot : p_subtree) {
		Node *node = node_from_id(snapshot.node_id);
		Node *owner = node_from_id(snapshot.owner_id);
		if (p_do) {
			p_registrar.add_do_method(node, SNAME("set_owner"), owner);
		} else {
			p_registrar.add_undo_method(node, SNAME("set_owner"), owner);
		}
	}
}

static void register_transform_method(NativeActionRegistrar &p_registrar, StructuralPlanData *p_data, bool p_do, bool p_postcondition) {
	Node *target = node_from_id(p_data->target_id);
	const Variant transform = p_postcondition && p_data->keep_global_transform ? p_data->old_global_transform : p_data->old_local_transform;
	const StringName method = p_postcondition && p_data->keep_global_transform ? SNAME("set_global_transform") : SNAME("set_transform");
	if (p_do) {
		p_registrar.add_do_method(target, method, transform);
	} else {
		p_registrar.add_undo_method(target, method, transform);
	}
}

static Error register_action(const TransactionExecutor::NativeActionPlan &p_plan, NativeActionRegistrar &p_registrar) {
	StructuralPlanData *data = plan_data(p_plan);
	ERR_FAIL_NULL_V(data, ERR_INVALID_DATA);
	const String operation_kind = data->kind == STRUCTURAL_CREATE ? "create_node" : data->kind == STRUCTURAL_REPARENT ? "reparent_node"
																													  : "delete_node";
	if (inject_fault(operation_kind, "registration")) {
		return ERR_BUG;
	}
	Node *root = node_from_id(data->root_id);
	ERR_FAIL_NULL_V(root, ERR_DOES_NOT_EXIST);

	if (data->kind == STRUCTURAL_CREATE) {
		Node *parent = node_from_id(data->parent_id);
		Node *created = node_from_id(data->created_id);
		ERR_FAIL_NULL_V(parent, ERR_DOES_NOT_EXIST);
		ERR_FAIL_NULL_V(created, ERR_DOES_NOT_EXIST);
		p_registrar.add_do_reference(created);
		data->created_reference_transferred = true;
		p_registrar.add_do_method(parent, SNAME("add_child"), created, false);
		if (data->new_index != data->parent_child_count_before) {
			p_registrar.add_do_method(parent, SNAME("move_child"), created, data->new_index);
		}
		p_registrar.add_do_method(created, SNAME("set_owner"), root);
		p_registrar.add_undo_method(parent, SNAME("remove_child"), created);
		return OK;
	}

	Node *target = node_from_id(data->target_id);
	Node *parent = node_from_id(data->parent_id);
	ERR_FAIL_NULL_V(target, ERR_DOES_NOT_EXIST);
	ERR_FAIL_NULL_V(parent, ERR_DOES_NOT_EXIST);
	if (data->kind == STRUCTURAL_REPARENT) {
		Node *new_parent = node_from_id(data->new_parent_id);
		ERR_FAIL_NULL_V(new_parent, ERR_DOES_NOT_EXIST);
		if (data->same_parent) {
			p_registrar.add_do_method(parent, SNAME("move_child"), target, data->new_index);
			p_registrar.add_undo_method(parent, SNAME("move_child"), target, data->old_index);
			return OK;
		}
		p_registrar.add_do_method(target, SNAME("reparent"), new_parent, data->keep_global_transform);
		p_registrar.add_do_method(new_parent, SNAME("move_child"), target, data->new_index);
		register_owner_methods(p_registrar, data->subtree, true);
		if (data->transform_kind != TRANSFORM_NONE) {
			register_transform_method(p_registrar, data, true, true);
		}
		p_registrar.add_undo_method(target, SNAME("reparent"), parent, false);
		p_registrar.add_undo_method(parent, SNAME("move_child"), target, data->old_index);
		register_owner_methods(p_registrar, data->subtree, false);
		if (data->transform_kind != TRANSFORM_NONE) {
			register_transform_method(p_registrar, data, false, false);
		}
		return OK;
	}

	p_registrar.add_do_method(parent, SNAME("remove_child"), target);
	p_registrar.add_undo_method(parent, SNAME("add_child"), target, false);
	p_registrar.add_undo_method(parent, SNAME("move_child"), target, data->old_index);
	register_owner_methods(p_registrar, data->subtree, false);
	if (data->transform_kind != TRANSFORM_NONE) {
		register_transform_method(p_registrar, data, false, false);
	}
	p_registrar.add_undo_reference(target);
	return OK;
}

} // namespace

Error StructuralTransactionExecutor::final_preflight(const PreparedTransactionStore::Record &p_record, const TransactionSceneResolver::Job &p_resolver_job, const TransactionPreviewBuilder::Resolution &p_resolution, NativeActionPlan &r_plan, String &r_error_code, String &r_error_message) {
	r_plan = NativeActionPlan();
	r_error_code.clear();
	r_error_message.clear();
	if (p_record.binding.editor_session_id != p_resolver_job.editor_session_id || p_record.binding.scene_id != p_resolver_job.scene_id || p_record.binding.history_id != p_resolver_job.history_id || p_record.operation_kind != String(p_resolver_job.operation.get("kind", String()))) {
		return fail("stale_editor_state", "The structural transaction binding changed before final preflight.", r_error_code, r_error_message);
	}
	Node *root = node_from_id(p_resolver_job.root_id);
	if (!root) {
		return fail("stale_editor_state", "The edited scene root disappeared before final preflight.", r_error_code, r_error_message, ERR_DOES_NOT_EXIST);
	}
	const Dictionary operation = p_resolver_job.operation;
	const String kind = operation["kind"];
	if (inject_fault(kind, "preflight")) {
		return fail("transaction_apply_failed", "The structural executor injected a final-preflight failure.", r_error_code, r_error_message, ERR_BUG);
	}
	Ref<StructuralPlanData> data;
	data.instantiate();
	data->root_id = root->get_instance_id();
	data->editor_session_id = p_resolver_job.editor_session_id;
	data->scene_id = p_resolver_job.scene_id;

	if (kind == "create_node") {
		Node *parent = resolve_node(p_resolver_job, operation["parent_node_id"]);
		if (!is_editable(root, parent)) {
			return fail("node_not_editable", "The requested create parent is no longer editable.", r_error_code, r_error_message, ERR_DOES_NOT_EXIST);
		}
		const StringName type = operation["godot_type"];
		if (!ClassDB::class_exists(type) || !ClassDB::can_instantiate(type) || !ClassDB::is_parent_class(type, SNAME("Node"))) {
			return fail("scene_operation_unsupported", "The requested Godot node type cannot be instantiated.", r_error_code, r_error_message);
		}
		const StringName name = operation["name"];
		if (direct_child_named(parent, name)) {
			return fail("scene_operation_unsupported", "The requested child name is already in use.", r_error_code, r_error_message);
		}
		const int insertion_index = operation.has("insertion_index") ? (int)(int64_t)operation["insertion_index"] : parent->get_child_count(false);
		if (insertion_index < 0 || insertion_index > parent->get_child_count(false)) {
			return fail("scene_operation_unsupported", "The requested insertion index is outside the parent bounds.", r_error_code, r_error_message);
		}
		Object *instance = ClassDB::instantiate(type);
		Node *created = Object::cast_to<Node>(instance);
		if (!created) {
			if (instance && !instance->is_ref_counted()) {
				memdelete(instance);
			}
			return fail("scene_operation_unsupported", "The requested Godot type did not produce a Node instance.", r_error_code, r_error_message, ERR_CANT_CREATE);
		}
		created->set_name(name);
		if (created->get_name() != name) {
			memdelete(created);
			return fail("scene_operation_unsupported", "Godot did not accept the requested exact node name.", r_error_code, r_error_message);
		}
		data->kind = STRUCTURAL_CREATE;
		data->parent_id = parent->get_instance_id();
		data->created_id = created->get_instance_id();
		data->requested_type = type;
		data->requested_name = name;
		data->new_index = insertion_index;
		data->parent_child_count_before = parent->get_child_count(false);
		data->owns_created_node = true;
		if (p_resolution.structural_nodes != 1) {
			return fail("stale_editor_state", "The create structural count changed before commit.", r_error_code, r_error_message);
		}
	} else if (kind == "reparent_node") {
		Node *target = resolve_node(p_resolver_job, operation["node_id"]);
		Node *new_parent = resolve_node(p_resolver_job, operation["new_parent_node_id"]);
		if (!target || !new_parent || !is_editable(root, target) || !is_editable(root, new_parent)) {
			return fail("node_not_editable", "A reparent target is no longer editable.", r_error_code, r_error_message, ERR_DOES_NOT_EXIST);
		}
		if (target == root || target == new_parent || target->is_ancestor_of(new_parent)) {
			return fail("scene_operation_unsupported", "The requested reparent operation targets the root or would create a cycle.", r_error_code, r_error_message);
		}
		Node *old_parent = target->get_parent();
		if (!old_parent) {
			return fail("stale_editor_state", "The requested reparent target no longer has a parent.", r_error_code, r_error_message);
		}
		const bool same_parent = old_parent == new_parent;
		const int insertion_index = (int)(int64_t)operation["insertion_index"];
		const int maximum_index = same_parent ? new_parent->get_child_count(false) - 1 : new_parent->get_child_count(false);
		if (insertion_index < 0 || insertion_index > maximum_index || (same_parent && insertion_index == target->get_index(false))) {
			return fail("scene_operation_unsupported", "The requested reparent index is invalid or would not change the scene.", r_error_code, r_error_message);
		}
		if (direct_child_named(new_parent, target->get_name(), target)) {
			return fail("scene_operation_unsupported", "The target name is already in use under the new parent.", r_error_code, r_error_message);
		}
		const bool keep_global = operation["keep_global_transform"];
		if (keep_global && !Object::cast_to<Node2D>(target) && !Object::cast_to<Node3D>(target)) {
			return fail("scene_operation_unsupported", "Global transform retention is supported only for Node2D and Node3D targets.", r_error_code, r_error_message);
		}
		data->kind = STRUCTURAL_REPARENT;
		data->parent_id = old_parent->get_instance_id();
		data->target_id = target->get_instance_id();
		data->new_parent_id = new_parent->get_instance_id();
		data->old_index = target->get_index(false);
		data->new_index = insertion_index;
		data->same_parent = same_parent;
		data->keep_global_transform = keep_global;
		if (capture_subtree(root, target, data->subtree, r_error_code, r_error_message) != OK) {
			return ERR_INVALID_DATA;
		}
		if (data->subtree.size() != (int)p_resolution.structural_nodes) {
			return fail("stale_editor_state", "The reparent subtree changed before commit.", r_error_code, r_error_message);
		}
		if (!owners_remain_valid_after_reparent(root, target, new_parent, data->subtree)) {
			return fail("node_ownership_invalid", "The requested reparent would invalidate a subtree owner relationship.", r_error_code, r_error_message);
		}
		capture_transform(target, **data);
	} else if (kind == "delete_node") {
		Node *target = resolve_node(p_resolver_job, operation["node_id"]);
		if (!target || !is_editable(root, target)) {
			return fail("node_not_editable", "The requested delete target is no longer editable.", r_error_code, r_error_message, ERR_DOES_NOT_EXIST);
		}
		if (target == root || !target->get_parent()) {
			return fail("scene_operation_unsupported", "The root of an open scene cannot be deleted.", r_error_code, r_error_message);
		}
		data->kind = STRUCTURAL_DELETE;
		data->parent_id = target->get_parent()->get_instance_id();
		data->target_id = target->get_instance_id();
		data->old_index = target->get_index(false);
		if (capture_subtree(root, target, data->subtree, r_error_code, r_error_message) != OK) {
			return ERR_INVALID_DATA;
		}
		if (data->subtree.size() != (int)p_resolution.structural_nodes) {
			return fail("stale_editor_state", "The delete subtree changed before commit.", r_error_code, r_error_message);
		}
		capture_transform(target, **data);
	} else {
		return fail("scene_operation_unsupported", "The structural executor does not support this operation.", r_error_code, r_error_message, ERR_UNAVAILABLE);
	}

	r_plan.native_history_id = p_resolver_job.scene_evidence.native_history_id;
	r_plan.context = data;
	return OK;
}

Error StructuralTransactionExecutor::register_native_action(const NativeActionPlan &p_plan, EditorUndoRedoManager *p_undo_redo) {
	ERR_FAIL_NULL_V(p_undo_redo, ERR_UNAVAILABLE);
	NativeActionRegistrar registrar(p_undo_redo);
	return register_action(p_plan, registrar);
}

Error StructuralTransactionExecutor::register_native_action_on_history(const NativeActionPlan &p_plan, UndoRedo *p_undo_redo) {
	ERR_FAIL_NULL_V(p_undo_redo, ERR_UNAVAILABLE);
	NativeActionRegistrar registrar(p_undo_redo);
	return register_action(p_plan, registrar);
}

ObjectID StructuralTransactionExecutor::get_created_node_id(const NativeActionPlan &p_plan) const {
	StructuralPlanData *data = plan_data(p_plan);
	return data && data->kind == STRUCTURAL_CREATE ? data->created_id : ObjectID();
}

bool StructuralTransactionExecutor::verify_postcondition(const PreparedTransactionStore::Record &p_record, const NativeActionPlan &p_plan) const {
	if (inject_fault(p_record.operation_kind, "postcondition") || inject_fault(p_record.operation_kind, "rollback_proof")) {
		return false;
	}
	StructuralPlanData *data = plan_data(p_plan);
	if (!data) {
		return false;
	}
	Node *root = node_from_id(data->root_id);
	if (!root) {
		return false;
	}
	if (data->kind == STRUCTURAL_CREATE) {
		Node *parent = node_from_id(data->parent_id);
		Node *created = node_from_id(data->created_id);
		return parent && created && created->get_parent() == parent && created->get_index(false) == data->new_index && created->get_owner() == root && created->get_name() == data->requested_name && created->is_class(data->requested_type);
	}
	Node *target = node_from_id(data->target_id);
	if (!target) {
		return false;
	}
	if (data->kind == STRUCTURAL_REPARENT) {
		Node *new_parent = node_from_id(data->new_parent_id);
		return new_parent && subtree_matches(*data, target, new_parent, data->new_index, true) && transform_matches(target, *data, true);
	}
	return !target->get_parent() && subtree_matches(*data, target, nullptr, -1, false) && transform_matches(target, *data, true);
}

bool StructuralTransactionExecutor::verify_prestate_after_rollback(const PreparedTransactionStore::Record &p_record, const NativeActionPlan &p_plan) const {
	if (inject_fault(p_record.operation_kind, "rollback_proof")) {
		return false;
	}
	StructuralPlanData *data = plan_data(p_plan);
	if (!data) {
		return false;
	}
	if (data->kind == STRUCTURAL_CREATE) {
		Node *created = node_from_id(data->created_id);
		Node *parent = node_from_id(data->parent_id);
		if (!created || !parent || created->get_parent()) {
			return false;
		}
		for (int index = 0; index < parent->get_child_count(false); index++) {
			if (parent->get_child(index, false) == created) {
				return false;
			}
		}
		return true;
	}
	Node *target = node_from_id(data->target_id);
	Node *parent = node_from_id(data->parent_id);
	return target && parent && subtree_matches(*data, target, parent, data->old_index, true) && transform_matches(target, *data, false);
}

Array StructuralTransactionExecutor::collect_committed_entities(const PreparedTransactionStore::Record &p_record, const NativeActionPlan &p_plan) const {
	Array entities;
	StructuralPlanData *data = plan_data(p_plan);
	if (!data || (data->kind != STRUCTURAL_CREATE && data->kind != STRUCTURAL_REPARENT)) {
		return entities;
	}
	Node *root = node_from_id(data->root_id);
	Node *node = node_from_id(data->kind == STRUCTURAL_CREATE ? data->created_id : data->target_id);
	if (!root || !node || !root->is_inside_tree() || !node->is_inside_tree() || (node != root && !root->is_ancestor_of(node))) {
		return entities;
	}
	Dictionary entity;
	entity["node_id"] = BridgeEditorIdentity::make_node_id(p_record.binding.editor_session_id, p_record.binding.scene_id, String(root->get_path_to(node)));
	entity["role"] = data->kind == STRUCTURAL_CREATE ? "created" : "target";
	entities.push_back(entity);
	return entities;
}
