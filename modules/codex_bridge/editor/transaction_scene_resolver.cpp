/**************************************************************************/
/*  transaction_scene_resolver.cpp                                      */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "transaction_scene_resolver.h"

#include "script_transaction_executor.h"
#include "signal_transaction_executor.h"
#include "writable_variant_codec.h"

#include "bridge_editor_identity.h"

#include "core/object/class_db.h"
#include "core/object/object.h"
#include "core/os/os.h"
#include "core/os/thread.h"
#include "editor/editor_data.h"
#include "editor/editor_node.h"
#include "editor/editor_undo_redo_manager.h"
#include "scene/2d/node_2d.h"
#include "scene/3d/node_3d.h"
#include "scene/main/node.h"

namespace {

static TransactionSceneResolver::ProcessOutcome failure(const String &p_code, const String &p_message) {
	TransactionSceneResolver::ProcessOutcome outcome;
	outcome.failed = true;
	outcome.error_code = p_code;
	outcome.error_message = p_message;
	return outcome;
}

static Node *node_from_id(ObjectID p_id) {
	return Object::cast_to<Node>(ObjectDB::get_instance(p_id));
}

static bool is_editable(Node *p_root, Node *p_node) {
	if (!p_root || !p_node) {
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

static bool owner_is_valid(Node *p_root, Node *p_node) {
	if (p_node == p_root) {
		return true;
	}
	if (p_node && p_node != p_root && !p_root->is_ancestor_of(p_node) && !p_node->get_parent() && !p_node->get_owner()) {
		return true;
	}
	Node *owner = p_node ? p_node->get_owner() : nullptr;
	return owner && (owner == p_root || p_root->is_ancestor_of(owner));
}

static Dictionary affected(const String &p_node_id, const String &p_role) {
	Dictionary entity;
	entity["node_id"] = p_node_id;
	entity["role"] = p_role;
	return entity;
}

static ObjectID resolve_node(const TransactionSceneResolver::Job &p_job, const String &p_node_id) {
	const ObjectID *object_id = p_job.objects_by_node_id.getptr(p_node_id);
	return object_id ? *object_id : ObjectID();
}

static bool direct_child_named(Node *p_parent, const StringName &p_name, Node *p_except = nullptr) {
	for (int index = 0; index < p_parent->get_child_count(); index++) {
		Node *child = p_parent->get_child(index);
		if (child != p_except && child->get_name() == p_name) {
			return true;
		}
	}
	return false;
}

static uint32_t subtree_node_count(const TransactionSceneResolver::Job &p_job, Node *p_target) {
	uint32_t count = 0;
	for (const KeyValue<ObjectID, String> &entry : p_job.node_ids_by_object) {
		Node *candidate = node_from_id(entry.key);
		if (candidate && (candidate == p_target || p_target->is_ancestor_of(candidate))) {
			count++;
		}
	}
	return count;
}

static bool subtree_owners_remain_valid(const TransactionSceneResolver::Job &p_job, Node *p_root, Node *p_target, Node *p_new_parent) {
	for (const KeyValue<ObjectID, String> &entry : p_job.node_ids_by_object) {
		Node *candidate = node_from_id(entry.key);
		if (!candidate || (candidate != p_target && !p_target->is_ancestor_of(candidate))) {
			continue;
		}
		Node *owner = candidate->get_owner();
		if (!owner || (owner != p_root && !p_root->is_ancestor_of(owner)) || (owner != p_target && !p_target->is_ancestor_of(owner) && owner != p_root && owner != p_new_parent && !owner->is_ancestor_of(p_new_parent))) {
			return false;
		}
	}
	return true;
}

static TransactionSceneResolver::ProcessOutcome resolve_operation(TransactionSceneResolver::Job &r_job) {
	Node *root = node_from_id(r_job.root_id);
	if (!root) {
		return failure("transaction_conflicted", "The prepared scene root is no longer available.");
	}
	const Dictionary operation = r_job.operation;
	const String kind = operation["kind"];
	TransactionSceneResolver::ProcessOutcome outcome;
	outcome.complete = true;
	Array entities;

	if (kind == "create_node") {
		Node *parent = node_from_id(resolve_node(r_job, operation["parent_node_id"]));
		if (!parent) {
			return failure("node_not_editable", "The requested parent node was not found in the open scene.");
		}
		if (!is_editable(root, parent) || parent->is_internal()) {
			return failure("node_not_editable", "The requested parent is outside the editable scene boundary.");
		}
		const StringName type = operation["godot_type"];
		if (!ClassDB::class_exists(type) || !ClassDB::can_instantiate(type) || !ClassDB::is_parent_class(type, "Node")) {
			return failure("scene_operation_unsupported", "The requested Godot node type cannot be instantiated.");
		}
		if (operation.has("insertion_index") && (int64_t)operation["insertion_index"] > parent->get_child_count()) {
			return failure("scene_operation_unsupported", "The requested insertion index is outside the parent bounds.");
		}
		if (direct_child_named(parent, StringName(operation["name"]))) {
			return failure("scene_operation_unsupported", "The requested child name is already in use.");
		}
		outcome.resolution.structural_nodes = 1;
		entities.push_back(affected(operation["parent_node_id"], "parent"));
	} else if (kind == "reparent_node") {
		Node *target = node_from_id(resolve_node(r_job, operation["node_id"]));
		Node *new_parent = node_from_id(resolve_node(r_job, operation["new_parent_node_id"]));
		if (!target || !new_parent) {
			return failure("node_not_editable", "The requested reparent targets are missing.");
		}
		if (target == root || target->is_internal() || new_parent->is_internal() || target == new_parent || target->is_ancestor_of(new_parent)) {
			return failure("scene_operation_unsupported", "The requested reparent operation targets a root/internal node or would create a cycle.");
		}
		if (!is_editable(root, target) || !is_editable(root, new_parent)) {
			return failure("node_not_editable", "The requested reparent targets are outside the editable scene boundary.");
		}
		if (!owner_is_valid(root, target) || !owner_is_valid(root, new_parent)) {
			return failure("node_ownership_invalid", "The requested reparent targets have invalid scene ownership.");
		}
		Node *old_parent = target->get_parent();
		const int insertion_index = (int)(int64_t)operation["insertion_index"];
		const bool same_parent = old_parent == new_parent;
		const int maximum_index = same_parent ? new_parent->get_child_count() - 1 : new_parent->get_child_count();
		if (insertion_index < 0 || insertion_index > maximum_index) {
			return failure("scene_operation_unsupported", "The requested insertion index is outside the new parent bounds.");
		}
		if (same_parent && insertion_index == target->get_index(false)) {
			return failure("scene_operation_unsupported", "The requested reparent operation would not change the scene.");
		}
		if (direct_child_named(new_parent, target->get_name(), target)) {
			return failure("scene_operation_unsupported", "The requested node name is already in use under the new parent.");
		}
		if ((bool)operation["keep_global_transform"] && !Object::cast_to<Node2D>(target) && !Object::cast_to<Node3D>(target)) {
			return failure("scene_operation_unsupported", "Global transform retention is supported only for Node2D and Node3D targets.");
		}
		if (!subtree_owners_remain_valid(r_job, root, target, new_parent)) {
			return failure("node_ownership_invalid", "The requested reparent would invalidate a subtree owner relationship.");
		}
		outcome.resolution.structural_nodes = subtree_node_count(r_job, target);
		entities.push_back(affected(operation["node_id"], "target"));
		entities.push_back(affected(operation["new_parent_node_id"], "new_parent"));
	} else if (kind == "connect_signal" || kind == "disconnect_signal") {
		Node *emitter = node_from_id(resolve_node(r_job, operation["emitter_node_id"]));
		Node *receiver = node_from_id(resolve_node(r_job, operation["receiver_node_id"]));
		String error_code;
		String error_message;
		if (SignalTransactionExecutor::prepare_resolution(root, emitter, receiver, operation, outcome.resolution, error_code, error_message) != OK) {
			return failure(error_code, error_message);
		}
		entities.push_back(affected(operation["emitter_node_id"], "emitter"));
		entities.push_back(affected(operation["receiver_node_id"], "receiver"));
	} else {
		Node *target = node_from_id(resolve_node(r_job, operation["node_id"]));
		if (!target || !is_editable(root, target)) {
			return failure("node_not_editable", "The requested target is missing or outside the editable scene boundary.");
		}
		if (!owner_is_valid(root, target)) {
			return failure("node_ownership_invalid", "The requested target has invalid scene ownership.");
		}
		if (kind == "delete_node") {
			if (target == root || target->is_internal()) {
				return failure("scene_operation_unsupported", "The root or an internal node of an open scene cannot be deleted by this operation.");
			}
			outcome.resolution.structural_nodes = subtree_node_count(r_job, target);
		} else if (kind == "set_property") {
			const StringName property = operation["property"];
			PropertyInfo property_info;
			if (property == SNAME("script") || !ClassDB::get_property_info(target->get_class_name(), property, &property_info) || ClassDB::get_property_setter(target->get_class_name(), property).is_empty() || ClassDB::get_property_getter(target->get_class_name(), property).is_empty() || !(property_info.usage & PROPERTY_USAGE_EDITOR) || (property_info.usage & (PROPERTY_USAGE_INTERNAL | PROPERTY_USAGE_READ_ONLY | PROPERTY_USAGE_SECRET))) {
				return failure("property_not_writable", "The property is not a proven native editor-writable property for the requested value type.");
			}
			String error_code;
			String error_message;
			WritableVariantCodec::Result decoded;
			const Error decode_error = WritableVariantCodec::decode(operation["value"], root, target, decoded, error_code, error_message);
			if (decode_error != OK) {
				return failure(error_code, error_message);
			}
			if (WritableVariantCodec::validate_property_compatibility(property_info, decoded.value, error_code, error_message) != OK) {
				return failure(error_code, error_message);
			}
			const Variant normalized_new_value = WritableVariantCodec::normalize_property_value(property_info, decoded.value);
			bool valid = false;
			const Variant old_value = target->get(property, &valid);
			if (!valid) {
				return failure("property_not_writable", "The current property value could not be read through its native getter.");
			}
			WritableVariantCodec::Result old_value_evidence;
			if (WritableVariantCodec::inspect_native(old_value, root, target, old_value_evidence, error_code, error_message) != OK) {
				return failure(error_code, error_message);
			}
			if (WritableVariantCodec::values_equal(old_value, normalized_new_value)) {
				return failure("property_value_unsupported", "The requested property write would not change the canonical value.");
			}
			outcome.resolution.redacted_change = decoded.redacted_summary;
			outcome.resolution.precondition_digest = old_value_evidence.canonical_digest;
		} else if (kind == "attach_script") {
			String error_code;
			String error_message;
			if (ScriptTransactionExecutor::prepare_resolution(root, target, operation, outcome.resolution, error_code, error_message) != OK) {
				return failure(error_code, error_message);
			}
		} else if (kind == "detach_script") {
			String error_code;
			String error_message;
			if (ScriptTransactionExecutor::prepare_resolution(root, target, operation, outcome.resolution, error_code, error_message) != OK) {
				return failure(error_code, error_message);
			}
		}
		entities.push_back(affected(operation["node_id"], "target"));
	}

	outcome.resolution.affected_entities = entities;
	return outcome;
}

} // namespace

uint64_t TransactionSceneResolver::_default_clock(void *p_userdata) {
	return OS::get_singleton()->get_ticks_usec();
}

Error TransactionSceneResolver::begin(const String &p_editor_session_id, const String &p_scene_id, const String &p_history_id, const Dictionary &p_operation, Job &r_job, String &r_error_code, String &r_error_message) {
	ERR_FAIL_COND_V_MSG(!Thread::is_main_thread(), ERR_BUG, "Transaction scene resolution must begin on the main thread.");
	r_job = Job();
	r_error_code.clear();
	r_error_message.clear();
	EditorData &editor_data = EditorNode::get_editor_data();
	for (int scene_index = 0; scene_index < editor_data.get_edited_scene_count(); scene_index++) {
		Node *root = editor_data.get_edited_scene_root(scene_index);
		if (!root || root->get_scene_file_path().is_empty() || BridgeEditorIdentity::make_scene_id(p_editor_session_id, root) != p_scene_id) {
			continue;
		}
		const int native_history_id = editor_data.get_scene_history_id(scene_index);
		if (BridgeEditorIdentity::make_history_id(p_editor_session_id, native_history_id) != p_history_id) {
			r_error_code = "stale_editor_state";
			r_error_message = "The requested scene and history binding do not match.";
			return ERR_INVALID_DATA;
		}
		r_job.editor_session_id = p_editor_session_id;
		r_job.scene_id = p_scene_id;
		r_job.history_id = p_history_id;
		r_job.operation = p_operation.duplicate(true);
		r_job.root_id = root->get_instance_id();
		r_job.scene_evidence.scene_index = scene_index;
		r_job.scene_evidence.native_history_id = native_history_id;
		EditorUndoRedoManager *manager = EditorUndoRedoManager::get_singleton();
		UndoRedo *history = manager ? manager->get_history_undo_redo(native_history_id) : nullptr;
		if (history) {
			r_job.scene_evidence.history_version = history->get_version();
			r_job.scene_evidence.history_action_position = history->get_current_action();
		}
		r_job.pending_nodes.push_back(r_job.root_id);
		return OK;
	}
	r_error_code = "scene_not_open";
	r_error_message = "The requested saved scene is not open in this editor session.";
	return ERR_DOES_NOT_EXIST;
}

bool TransactionSceneResolver::binding_is_current(const Job &p_job) {
	if (!Thread::is_main_thread()) {
		return false;
	}
	EditorData &editor_data = EditorNode::get_editor_data();
	if (p_job.scene_evidence.scene_index < 0 || p_job.scene_evidence.scene_index >= editor_data.get_edited_scene_count()) {
		return false;
	}
	Node *root = editor_data.get_edited_scene_root(p_job.scene_evidence.scene_index);
	if (!root || root->get_instance_id() != p_job.root_id || BridgeEditorIdentity::make_scene_id(p_job.editor_session_id, root) != p_job.scene_id || editor_data.get_scene_history_id(p_job.scene_evidence.scene_index) != p_job.scene_evidence.native_history_id || BridgeEditorIdentity::make_history_id(p_job.editor_session_id, p_job.scene_evidence.native_history_id) != p_job.history_id) {
		return false;
	}
	EditorUndoRedoManager *manager = EditorUndoRedoManager::get_singleton();
	UndoRedo *history = manager ? manager->get_history_undo_redo(p_job.scene_evidence.native_history_id) : nullptr;
	return (!history && p_job.scene_evidence.history_version == 0 && p_job.scene_evidence.history_action_position == -1) || (history && history->get_version() == p_job.scene_evidence.history_version && history->get_current_action() == p_job.scene_evidence.history_action_position);
}

TransactionSceneResolver::ProcessOutcome TransactionSceneResolver::process(Job &r_job, uint32_t p_max_nodes, uint64_t p_budget_usec, Clock p_clock, void *p_clock_userdata) {
	ERR_FAIL_COND_V_MSG(!Thread::is_main_thread(), failure("transaction_conflicted", "Transaction scene resolution left the main thread."), "Transaction scene resolution must run on the main thread.");
	if (r_job.complete) {
		return failure("transaction_conflicted", "The transaction resolution job was already completed.");
	}
	if (!binding_is_current(r_job)) {
		return failure("transaction_conflicted", "The scene or native history changed during preparation.");
	}
	Clock clock = p_clock ? p_clock : _default_clock;
	const uint64_t started_usec = clock(p_clock_userdata);
	uint32_t processed = 0;
	while (!r_job.pending_nodes.is_empty() && processed < p_max_nodes) {
		if (processed > 0 && clock(p_clock_userdata) - started_usec >= p_budget_usec) {
			break;
		}
		if (r_job.visited_nodes >= MAX_STRUCTURAL_NODES) {
			return failure("transaction_too_large", "The scene exceeds the bounded structural preflight limit.");
		}
		const ObjectID object_id = r_job.pending_nodes.front()->get();
		r_job.pending_nodes.pop_front();
		Node *root = node_from_id(r_job.root_id);
		Node *node = node_from_id(object_id);
		if (!root || !node || (node != root && !root->is_ancestor_of(node))) {
			return failure("transaction_conflicted", "A scene node changed during structural preflight.");
		}
		const String node_path = String(root->get_path_to(node));
		const String node_id = BridgeEditorIdentity::make_node_id(r_job.editor_session_id, r_job.scene_id, node_path);
		if (r_job.objects_by_node_id.has(node_id)) {
			return failure("transaction_conflicted", "The scene contains an ambiguous opaque node identity.");
		}
		r_job.objects_by_node_id.insert(node_id, object_id);
		r_job.node_ids_by_object.insert(object_id, node_id);
		r_job.visited_nodes++;
		processed++;
		for (int child_index = 0; child_index < node->get_child_count(); child_index++) {
			r_job.pending_nodes.push_back(node->get_child(child_index)->get_instance_id());
		}
	}
	if (!r_job.pending_nodes.is_empty()) {
		return ProcessOutcome();
	}
	if (!binding_is_current(r_job)) {
		return failure("transaction_conflicted", "The scene or native history changed after structural preflight.");
	}
	r_job.complete = true;
	return resolve_operation(r_job);
}
