/**************************************************************************/
/*  transaction_scene_resolver.cpp                                      */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "transaction_scene_resolver.h"

#include "bridge_editor_identity.h"

#include "core/io/file_access.h"
#include "core/io/resource_loader.h"
#include "core/io/resource_uid.h"
#include "core/object/class_db.h"
#include "core/object/object.h"
#include "core/os/os.h"
#include "core/os/thread.h"
#include "editor/editor_data.h"
#include "editor/editor_node.h"
#include "editor/editor_undo_redo_manager.h"
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
	if (!p_root || !p_node || (p_node != p_root && !p_root->is_ancestor_of(p_node))) {
		return false;
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

static bool direct_child_named(Node *p_parent, const StringName &p_name) {
	for (int index = 0; index < p_parent->get_child_count(); index++) {
		if (p_parent->get_child(index)->get_name() == p_name) {
			return true;
		}
	}
	return false;
}

static String resource_ref_path(const Dictionary &p_reference) {
	if (p_reference.has("path")) {
		return p_reference["path"];
	}
	if (!p_reference.has("uid") || !ResourceUID::get_singleton()) {
		return String();
	}
	const ResourceUID::ID uid = ResourceUID::get_singleton()->text_to_id(p_reference["uid"]);
	if (uid == ResourceUID::INVALID_ID || !ResourceUID::get_singleton()->has_id(uid)) {
		return String();
	}
	return ResourceUID::get_singleton()->get_id_path(uid);
}

static bool wire_type_matches_property(const Dictionary &p_value, Variant::Type p_property_type) {
	const String type = p_value.get("type", String());
	switch (p_property_type) {
		case Variant::NIL:
			return type == "nil";
		case Variant::BOOL:
			return type == "bool";
		case Variant::INT:
			return type == "int";
		case Variant::FLOAT:
			return type == "float" || type == "int";
		case Variant::STRING:
			return type == "string";
		case Variant::STRING_NAME:
			return type == "string_name";
		case Variant::NODE_PATH:
			return type == "node_path";
		case Variant::VECTOR2:
			return type == "vector2";
		case Variant::VECTOR2I:
			return type == "vector2i";
		case Variant::VECTOR3:
			return type == "vector3";
		case Variant::VECTOR3I:
			return type == "vector3i";
		case Variant::VECTOR4:
			return type == "vector4";
		case Variant::VECTOR4I:
			return type == "vector4i";
		case Variant::RECT2:
			return type == "rect2";
		case Variant::RECT2I:
			return type == "rect2i";
		case Variant::TRANSFORM2D:
			return type == "transform2d";
		case Variant::PLANE:
			return type == "plane";
		case Variant::QUATERNION:
			return type == "quaternion";
		case Variant::AABB:
			return type == "aabb";
		case Variant::BASIS:
			return type == "basis";
		case Variant::TRANSFORM3D:
			return type == "transform3d";
		case Variant::PROJECTION:
			return type == "projection";
		case Variant::COLOR:
			return type == "color";
		case Variant::OBJECT:
			return type == "resource";
		case Variant::ARRAY:
			return type == "array";
		case Variant::DICTIONARY:
			return type == "dictionary";
		default:
			return false;
	}
}

static bool connection_matches(Node *p_emitter, Node *p_receiver, const Dictionary &p_operation) {
	List<Object::Connection> connections;
	p_emitter->get_signal_connection_list(StringName(p_operation["signal"]), &connections);
	for (const Object::Connection &connection : connections) {
		if (connection.callable.get_object_id() != p_receiver->get_instance_id() || connection.callable.get_method() != StringName(p_operation["method"]) || connection.flags != (uint32_t)(int64_t)p_operation["flags"] || connection.callable.get_unbound_arguments_count() != (int)(int64_t)p_operation["unbinds"]) {
			continue;
		}
		const Array requested_binds = p_operation["binds"];
		if (connection.callable.get_bound_arguments_count() != requested_binds.size()) {
			continue;
		}
		// S9-03 never evaluates user code to decode or compare arbitrary bound
		// values. An empty bind list is exact; non-empty lists fail closed until
		// the S9-06 value decoder is available.
		if (requested_binds.is_empty()) {
			return true;
		}
	}
	return false;
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
		if (!is_editable(root, parent)) {
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
		const String parent_path = String(root->get_path_to(parent));
		const String created_path = parent_path == "." ? String(operation["name"]) : parent_path.path_join(operation["name"]);
		entities.push_back(affected(operation["parent_node_id"], "parent"));
		entities.push_back(affected(BridgeEditorIdentity::make_node_id(r_job.editor_session_id, r_job.scene_id, created_path), "created"));
	} else if (kind == "reparent_node") {
		Node *target = node_from_id(resolve_node(r_job, operation["node_id"]));
		Node *new_parent = node_from_id(resolve_node(r_job, operation["new_parent_node_id"]));
		if (!target || !new_parent || target == root || target == new_parent || target->is_ancestor_of(new_parent)) {
			return failure("node_not_editable", "The requested reparent targets are missing or would create a cycle.");
		}
		if (!is_editable(root, target) || !is_editable(root, new_parent)) {
			return failure("node_not_editable", "The requested reparent targets are outside the editable scene boundary.");
		}
		if (!owner_is_valid(root, target) || !owner_is_valid(root, new_parent)) {
			return failure("node_ownership_invalid", "The requested reparent targets have invalid scene ownership.");
		}
		if ((int64_t)operation["insertion_index"] > new_parent->get_child_count()) {
			return failure("scene_operation_unsupported", "The requested insertion index is outside the new parent bounds.");
		}
		entities.push_back(affected(operation["node_id"], "target"));
		entities.push_back(affected(operation["new_parent_node_id"], "new_parent"));
	} else if (kind == "connect_signal" || kind == "disconnect_signal") {
		Node *emitter = node_from_id(resolve_node(r_job, operation["emitter_node_id"]));
		Node *receiver = node_from_id(resolve_node(r_job, operation["receiver_node_id"]));
		if (!emitter || !receiver || !is_editable(root, emitter) || !is_editable(root, receiver)) {
			return failure("node_not_editable", "A signal endpoint is missing or outside the editable scene boundary.");
		}
		if (!emitter->has_signal(StringName(operation["signal"])) || !receiver->has_method(StringName(operation["method"]))) {
			return failure("signal_connection_invalid", "The requested signal or receiver method is not available.");
		}
		const bool exists = connection_matches(emitter, receiver, operation);
		if ((kind == "connect_signal" && exists) || (kind == "disconnect_signal" && !exists)) {
			return failure("signal_connection_invalid", "The exact signal connection is already in the requested state or cannot be proven.");
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
			if (target == root) {
				return failure("scene_operation_unsupported", "The root of an open scene cannot be deleted by this operation.");
			}
			uint32_t subtree_nodes = 0;
			for (const KeyValue<ObjectID, String> &entry : r_job.node_ids_by_object) {
				Node *candidate = node_from_id(entry.key);
				if (candidate && (candidate == target || target->is_ancestor_of(candidate))) {
					subtree_nodes++;
				}
			}
			outcome.resolution.structural_nodes = subtree_nodes;
		} else if (kind == "set_property") {
			const StringName property = operation["property"];
			PropertyInfo property_info;
			if (property == SNAME("script") || !ClassDB::get_property_info(target->get_class_name(), property, &property_info) || ClassDB::get_property_setter(target->get_class_name(), property).is_empty() || !(property_info.usage & PROPERTY_USAGE_EDITOR) || (property_info.usage & (PROPERTY_USAGE_INTERNAL | PROPERTY_USAGE_READ_ONLY)) || !wire_type_matches_property(operation["value"], property_info.type)) {
				return failure("property_not_writable", "The property is not a proven native editor-writable property for the requested value type.");
			}
		} else if (kind == "attach_script") {
			const String path = resource_ref_path(operation["script_ref"]);
			const String type = path.is_empty() || !FileAccess::exists(path) ? String() : ResourceLoader::get_resource_type(path);
			if (type.is_empty() || !ClassDB::is_parent_class(type, "Script")) {
				return failure("script_incompatible", "The requested script resource is missing or incompatible.");
			}
			outcome.resolution.script_already_attached = target->get_script().get_type() != Variant::NIL;
		} else if (kind == "detach_script") {
			if (target->get_script().get_type() == Variant::NIL) {
				return failure("script_incompatible", "The requested target has no attached script.");
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
