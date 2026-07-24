/**************************************************************************/
/*  signal_transaction_executor.cpp                                     */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "signal_transaction_executor.h"

#include "writable_variant_codec.h"

#include "core/io/json.h"
#include "core/object/object.h"
#include "core/object/undo_redo.h"
#include "core/os/os.h"
#include "editor/editor_undo_redo_manager.h"
#include "scene/main/node.h"

#include "modules/codex_bridge/protocol/bridge_transaction_canonicalizer.h"

namespace {

constexpr uint32_t ALLOWED_CONNECTION_FLAGS = Object::CONNECT_DEFERRED | Object::CONNECT_PERSIST | Object::CONNECT_ONE_SHOT;

class SignalPlanData : public RefCounted {
public:
	ObjectID emitter_id;
	ObjectID receiver_id;
	StringName signal;
	StringName method;
	Callable callable;
	uint32_t flags = 0;
	bool connect = false;
};

static SignalPlanData *plan_data(const TransactionExecutor::NativeActionPlan &p_plan) {
	return p_plan.context.is_valid() ? static_cast<SignalPlanData *>(p_plan.context.ptr()) : nullptr;
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

static bool find_method_info(Object *p_object, const StringName &p_name, MethodInfo &r_info) {
	List<MethodInfo> methods;
	p_object->get_method_list(&methods);
	for (const MethodInfo &method : methods) {
		if (method.name == p_name) {
			r_info = method;
			return true;
		}
	}
	return false;
}

static bool find_signal_info(Object *p_object, const StringName &p_name, MethodInfo &r_info) {
	List<MethodInfo> signals;
	p_object->get_signal_list(&signals);
	for (const MethodInfo &signal : signals) {
		if (signal.name == p_name) {
			r_info = signal;
			return true;
		}
	}
	return false;
}

static Callable make_callable(Node *p_receiver, const StringName &p_method, const Array &p_binds, int p_unbinds) {
	Callable callable(p_receiver, p_method);
	if (!p_binds.is_empty()) {
		callable = callable.bindv(p_binds);
	}
	if (p_unbinds > 0) {
		callable = callable.unbind(p_unbinds);
	}
	return callable;
}

static bool callable_matches(const Callable &p_actual, Node *p_receiver, const StringName &p_method, const Array &p_binds, int p_unbinds) {
	if (p_actual.get_object_id() != p_receiver->get_instance_id() || p_actual.get_method() != p_method || p_actual.get_unbound_arguments_count() != p_unbinds || p_actual.get_bound_arguments_count() != p_binds.size()) {
		return false;
	}
	const Array actual_binds = p_actual.get_bound_arguments();
	if (actual_binds.size() != p_binds.size()) {
		return false;
	}
	for (int index = 0; index < p_binds.size(); index++) {
		if (!WritableVariantCodec::values_equal(actual_binds[index], p_binds[index])) {
			return false;
		}
	}
	return true;
}

static bool exact_connection_exists(Node *p_emitter, Node *p_receiver, const StringName &p_signal, const StringName &p_method, const Array &p_binds, int p_unbinds, uint32_t p_flags) {
	if (!p_emitter || !p_receiver) {
		return false;
	}
	List<Object::Connection> connections;
	p_emitter->get_signal_connection_list(p_signal, &connections);
	for (const Object::Connection &connection : connections) {
		if (connection.flags == p_flags && callable_matches(connection.callable, p_receiver, p_method, p_binds, p_unbinds)) {
			return true;
		}
	}
	return false;
}

static Error connection_digest(const Dictionary &p_operation, const String &p_binds_digest, bool p_exists, String &r_digest) {
	Dictionary commitment;
	commitment["emitter_node_id"] = p_operation["emitter_node_id"];
	commitment["receiver_node_id"] = p_operation["receiver_node_id"];
	commitment["signal"] = p_operation["signal"];
	commitment["method"] = p_operation["method"];
	commitment["flags"] = p_operation["flags"];
	commitment["unbinds"] = p_operation["unbinds"];
	commitment["binds_digest"] = p_binds_digest;
	commitment["exists"] = p_exists;
	return BridgeTransactionCanonicalizer::sha256_utf8(JSON::stringify(commitment, "", true, true), r_digest, "godot-codex-signal-precondition/v1\n");
}

static Dictionary signal_summary(const Dictionary &p_binds_summary, const String &p_digest, bool p_connect, uint32_t p_flags, int p_unbinds) {
	Dictionary summary;
	summary["type"] = "signal_connection";
	summary["redacted"] = true;
	summary["digest"] = p_digest;
	summary["action"] = p_connect ? "connect" : "disconnect";
	summary["flags"] = (int64_t)p_flags;
	summary["unbinds"] = (int64_t)p_unbinds;
	summary["binds"] = p_binds_summary;
	return summary;
}

static Error inspect_operation(Node *p_root, Node *p_emitter, Node *p_receiver, const Dictionary &p_operation, Array &r_binds, Callable &r_callable, String &r_precondition_digest, TransactionPreviewBuilder::Resolution *r_resolution, String &r_error_code, String &r_error_message) {
	const String kind = p_operation.get("kind", String());
	const bool connect = kind == "connect_signal";
	if ((!connect && kind != "disconnect_signal") || !is_editable(p_root, p_emitter) || !is_editable(p_root, p_receiver)) {
		return fail("node_not_editable", "A signal endpoint is missing or outside the editable scene boundary.", r_error_code, r_error_message, ERR_DOES_NOT_EXIST);
	}
	const StringName signal = p_operation["signal"];
	const StringName method = p_operation["method"];
	MethodInfo signal_info;
	MethodInfo method_info;
	if (!find_signal_info(p_emitter, signal, signal_info) || !find_method_info(p_receiver, method, method_info)) {
		return fail("signal_connection_invalid", "The requested declared signal or receiver method is not available.", r_error_code, r_error_message, ERR_DOES_NOT_EXIST);
	}
	const int64_t flags_integer = p_operation["flags"];
	if (flags_integer < 0 || flags_integer > UINT32_MAX) {
		return fail("signal_connection_invalid", "The signal connection flags are outside the supported range.", r_error_code, r_error_message);
	}
	const uint32_t flags = (uint32_t)flags_integer;
	if (!(flags & Object::CONNECT_PERSIST) || (flags & ~ALLOWED_CONNECTION_FLAGS) != 0) {
		return fail("signal_connection_invalid", "The connection must be persistent and may use only deferred or one-shot modifiers.", r_error_code, r_error_message);
	}
	const int unbinds = (int)(int64_t)p_operation["unbinds"];
	if (unbinds < 0 || unbinds > signal_info.arguments.size()) {
		return fail("signal_connection_invalid", "The signal unbind count exceeds the declared signal arity.", r_error_code, r_error_message);
	}
	String binds_digest;
	Dictionary binds_summary;
	if (WritableVariantCodec::decode_binds(p_operation["binds"], p_root, p_receiver, r_binds, binds_digest, binds_summary, r_error_code, r_error_message) != OK) {
		return ERR_INVALID_DATA;
	}
	const int delivered_arguments = signal_info.arguments.size() - unbinds + r_binds.size();
	const int required_arguments = method_info.arguments.size() - method_info.default_arguments.size();
	if (delivered_arguments < required_arguments || (!(method_info.flags & METHOD_FLAG_VARARG) && delivered_arguments > method_info.arguments.size())) {
		return fail("signal_connection_invalid", "The signal, unbinds, binds, and receiver method arities are incompatible.", r_error_code, r_error_message);
	}
	r_callable = make_callable(p_receiver, method, r_binds, unbinds);
	if (!r_callable.is_valid()) {
		return fail("signal_connection_invalid", "The canonical signal callable is invalid.", r_error_code, r_error_message);
	}
	const bool exists = exact_connection_exists(p_emitter, p_receiver, signal, method, r_binds, unbinds, flags);
	if ((connect && exists) || (!connect && !exists)) {
		return fail("signal_connection_invalid", "The exact signal connection is already in the requested state.", r_error_code, r_error_message);
	}
	if (connection_digest(p_operation, binds_digest, exists, r_precondition_digest) != OK) {
		return fail("transaction_too_large", "The signal connection commitment could not be created.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
	}
	if (r_resolution) {
		r_resolution->precondition_digest = r_precondition_digest;
		r_resolution->redacted_change = signal_summary(binds_summary, r_precondition_digest, connect, flags, unbinds);
	}
	return OK;
}

class NativeActionRegistrar {
	EditorUndoRedoManager *manager = nullptr;
	UndoRedo *history = nullptr;

public:
	explicit NativeActionRegistrar(EditorUndoRedoManager *p_manager) :
			manager(p_manager) {}
	explicit NativeActionRegistrar(UndoRedo *p_history) :
			history(p_history) {}

	void add_do_connect(Node *p_emitter, const StringName &p_signal, const Callable &p_callable, uint32_t p_flags) {
		if (manager) {
			manager->add_do_method(p_emitter, SNAME("connect"), p_signal, p_callable, p_flags);
		} else {
			history->add_do_method(Callable(p_emitter, SNAME("connect")).bind(p_signal, p_callable, p_flags));
		}
	}

	void add_undo_connect(Node *p_emitter, const StringName &p_signal, const Callable &p_callable, uint32_t p_flags) {
		if (manager) {
			manager->add_undo_method(p_emitter, SNAME("connect"), p_signal, p_callable, p_flags);
		} else {
			history->add_undo_method(Callable(p_emitter, SNAME("connect")).bind(p_signal, p_callable, p_flags));
		}
	}

	void add_do_disconnect(Node *p_emitter, const StringName &p_signal, const Callable &p_callable) {
		if (manager) {
			manager->add_do_method(p_emitter, SNAME("disconnect"), p_signal, p_callable);
		} else {
			history->add_do_method(Callable(p_emitter, SNAME("disconnect")).bind(p_signal, p_callable));
		}
	}

	void add_undo_disconnect(Node *p_emitter, const StringName &p_signal, const Callable &p_callable) {
		if (manager) {
			manager->add_undo_method(p_emitter, SNAME("disconnect"), p_signal, p_callable);
		} else {
			history->add_undo_method(Callable(p_emitter, SNAME("disconnect")).bind(p_signal, p_callable));
		}
	}
};

static Error register_action(const TransactionExecutor::NativeActionPlan &p_plan, NativeActionRegistrar &p_registrar) {
	SignalPlanData *data = plan_data(p_plan);
	ERR_FAIL_NULL_V(data, ERR_INVALID_DATA);
	const String operation = data->connect ? "connect_signal" : "disconnect_signal";
	if (inject_fault(operation, "registration")) {
		return ERR_BUG;
	}
	Node *emitter = node_from_id(data->emitter_id);
	ERR_FAIL_NULL_V(emitter, ERR_DOES_NOT_EXIST);
	if (data->connect) {
		p_registrar.add_do_connect(emitter, data->signal, data->callable, data->flags);
		p_registrar.add_undo_disconnect(emitter, data->signal, data->callable);
	} else {
		p_registrar.add_do_disconnect(emitter, data->signal, data->callable);
		p_registrar.add_undo_connect(emitter, data->signal, data->callable, data->flags);
	}
	return OK;
}

static bool plan_connection_exists(const SignalPlanData *p_data) {
	if (!p_data) {
		return false;
	}
	Node *emitter = node_from_id(p_data->emitter_id);
	Node *receiver = node_from_id(p_data->receiver_id);
	return exact_connection_exists(emitter, receiver, p_data->signal, p_data->method, p_data->callable.get_bound_arguments(), p_data->callable.get_unbound_arguments_count(), p_data->flags);
}

} // namespace

Error SignalTransactionExecutor::prepare_resolution(Node *p_scene_root, Node *p_emitter, Node *p_receiver, const Dictionary &p_operation, TransactionPreviewBuilder::Resolution &r_resolution, String &r_error_code, String &r_error_message) {
	Array binds;
	Callable callable;
	String precondition_digest;
	return inspect_operation(p_scene_root, p_emitter, p_receiver, p_operation, binds, callable, precondition_digest, &r_resolution, r_error_code, r_error_message);
}

Error SignalTransactionExecutor::final_preflight(const PreparedTransactionStore::Record &p_record, const TransactionSceneResolver::Job &p_resolver_job, const TransactionPreviewBuilder::Resolution &p_resolution, NativeActionPlan &r_plan, String &r_error_code, String &r_error_message) {
	r_plan = NativeActionPlan();
	r_error_code.clear();
	r_error_message.clear();
	const Dictionary operation = p_resolver_job.operation;
	const String kind = operation.get("kind", String());
	if ((kind != "connect_signal" && kind != "disconnect_signal") || p_record.operation_kind != kind || p_record.binding.editor_session_id != p_resolver_job.editor_session_id || p_record.binding.scene_id != p_resolver_job.scene_id || p_record.binding.history_id != p_resolver_job.history_id) {
		return fail("stale_editor_state", "The signal transaction binding changed before final preflight.", r_error_code, r_error_message);
	}
	if (inject_fault(kind, "preflight")) {
		return fail("transaction_apply_failed", "The signal executor injected a final-preflight failure.", r_error_code, r_error_message, ERR_BUG);
	}
	Node *root = node_from_id(p_resolver_job.root_id);
	Node *emitter = resolve_node(p_resolver_job, operation["emitter_node_id"]);
	Node *receiver = resolve_node(p_resolver_job, operation["receiver_node_id"]);
	Array binds;
	Callable callable;
	String precondition_digest;
	if (inspect_operation(root, emitter, receiver, operation, binds, callable, precondition_digest, nullptr, r_error_code, r_error_message) != OK) {
		return ERR_INVALID_DATA;
	}
	if (p_record.precondition_digest.is_empty() || p_record.precondition_digest != precondition_digest || p_resolution.precondition_digest != precondition_digest) {
		return fail("stale_editor_state", "The exact signal connection changed after the immutable preview.", r_error_code, r_error_message);
	}
	Ref<SignalPlanData> data;
	data.instantiate();
	data->emitter_id = emitter->get_instance_id();
	data->receiver_id = receiver->get_instance_id();
	data->signal = StringName(operation["signal"]);
	data->method = StringName(operation["method"]);
	data->callable = callable;
	data->flags = (uint32_t)(int64_t)operation["flags"];
	data->connect = kind == "connect_signal";
	r_plan.native_history_id = p_resolver_job.scene_evidence.native_history_id;
	r_plan.context = data;
	return OK;
}

Error SignalTransactionExecutor::register_native_action(const NativeActionPlan &p_plan, EditorUndoRedoManager *p_undo_redo) {
	ERR_FAIL_NULL_V(p_undo_redo, ERR_UNAVAILABLE);
	NativeActionRegistrar registrar(p_undo_redo);
	return register_action(p_plan, registrar);
}

Error SignalTransactionExecutor::register_native_action_on_history(const NativeActionPlan &p_plan, UndoRedo *p_undo_redo) {
	ERR_FAIL_NULL_V(p_undo_redo, ERR_UNAVAILABLE);
	NativeActionRegistrar registrar(p_undo_redo);
	return register_action(p_plan, registrar);
}

bool SignalTransactionExecutor::verify_postcondition(const PreparedTransactionStore::Record &p_record, const NativeActionPlan &p_plan) const {
	if (inject_fault(p_record.operation_kind, "postcondition") || inject_fault(p_record.operation_kind, "rollback_proof")) {
		return false;
	}
	SignalPlanData *data = plan_data(p_plan);
	return data && plan_connection_exists(data) == data->connect;
}

bool SignalTransactionExecutor::verify_prestate_after_rollback(const PreparedTransactionStore::Record &p_record, const NativeActionPlan &p_plan) const {
	if (inject_fault(p_record.operation_kind, "rollback_proof")) {
		return false;
	}
	SignalPlanData *data = plan_data(p_plan);
	return data && plan_connection_exists(data) != data->connect;
}
