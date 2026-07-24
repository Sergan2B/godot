/**************************************************************************/
/*  compound_native_executor.cpp                                         */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "compound_native_executor.h"

#include "bridge_editor_identity.h"

#include "core/object/callable_mp.h"
#include "editor/editor_data.h"
#include "editor/editor_node.h"
#include "editor/editor_undo_redo_manager.h"

namespace {

static CompoundNativeExecutor::Outcome failure(const String &p_code, const String &p_message, bool p_in_doubt = false) {
	CompoundNativeExecutor::Outcome outcome;
	outcome.error_code = p_code;
	outcome.error_message = p_message;
	outcome.in_doubt = p_in_doubt;
	return outcome;
}

static bool scene_binding(const String &p_editor_session_id, const String &p_scene_id, String &r_history_id, int &r_native_history_id) {
	EditorData &editor_data = EditorNode::get_editor_data();
	for (int scene_index = 0; scene_index < editor_data.get_edited_scene_count(); scene_index++) {
		Node *root = editor_data.get_edited_scene_root(scene_index);
		if (!root || root->get_scene_file_path().is_empty() || BridgeEditorIdentity::make_scene_id(p_editor_session_id, root) != p_scene_id) {
			continue;
		}
		r_native_history_id = editor_data.get_scene_history_id(scene_index);
		r_history_id = BridgeEditorIdentity::make_history_id(p_editor_session_id, r_native_history_id);
		return true;
	}
	return false;
}

static bool resolve_scene_operation(const String &p_editor_session_id, const String &p_scene_id, const String &p_history_id, const Dictionary &p_operation, TransactionSceneResolver::Job &r_job, TransactionPreviewBuilder::Resolution &r_resolution, String &r_error_code, String &r_error_message) {
	if (TransactionSceneResolver::begin(p_editor_session_id, p_scene_id, p_history_id, p_operation, r_job, r_error_code, r_error_message) != OK) {
		return false;
	}
	for (uint32_t slice = 0; slice <= TransactionSceneResolver::MAX_STRUCTURAL_NODES; slice++) {
		const TransactionSceneResolver::ProcessOutcome outcome = TransactionSceneResolver::process(r_job, TransactionSceneResolver::MAX_NODES_PER_SLICE, TransactionSceneResolver::MAX_SLICE_USEC);
		if (outcome.failed) {
			r_error_code = outcome.error_code;
			r_error_message = outcome.error_message;
			return false;
		}
		if (outcome.complete) {
			r_resolution = outcome.resolution;
			return true;
		}
	}
	r_error_code = "transaction_too_large";
	r_error_message = "The compound scene preflight exceeded its bounded traversal.";
	return false;
}

} // namespace

void CompoundPersistenceAction::initialize(const ScopedPersistenceExecutor::Prepared &p_prepared) {
	prepared = p_prepared;
	last_operation_succeeded = true;
	last_error_code.clear();
	last_error_message.clear();
}

void CompoundPersistenceAction::commit_postimages() {
	last_error_code.clear();
	last_error_message.clear();
	last_operation_succeeded = ScopedPersistenceExecutor::commit(prepared, last_error_code, last_error_message) == OK;
}

void CompoundPersistenceAction::restore_preimages() {
	last_error_code.clear();
	last_error_message.clear();
	last_operation_succeeded = ScopedPersistenceExecutor::restore(prepared, last_error_code, last_error_message) == OK;
}

bool CompoundPersistenceAction::succeeded() const {
	return last_operation_succeeded;
}

String CompoundPersistenceAction::get_error_code() const {
	return last_error_code;
}

String CompoundPersistenceAction::get_error_message() const {
	return last_error_message;
}

bool CompoundPersistenceAction::verify_postimages() const {
	return ScopedPersistenceExecutor::verify_postimages(prepared);
}

bool CompoundPersistenceAction::verify_preimages() const {
	return ScopedPersistenceExecutor::verify_preimages(prepared);
}

void CompoundPersistenceAction::cleanup() {
	ScopedPersistenceExecutor::cleanup(prepared);
}

TransactionExecutor *CompoundNativeExecutor::_executor_for(const String &p_kind) {
	if (p_kind == "create_node" || p_kind == "reparent_node" || p_kind == "delete_node") {
		return &structural_executor;
	}
	if (p_kind == "set_property") {
		return &property_executor;
	}
	if (p_kind == "attach_script" || p_kind == "detach_script") {
		return &script_executor;
	}
	if (p_kind == "connect_signal" || p_kind == "disconnect_signal") {
		return &signal_executor;
	}
	return nullptr;
}

bool CompoundNativeExecutor::_is_scene_operation(const String &p_kind) {
	return p_kind == "create_node" || p_kind == "reparent_node" || p_kind == "delete_node" || p_kind == "set_property" || p_kind == "attach_script" || p_kind == "detach_script" || p_kind == "connect_signal" || p_kind == "disconnect_signal";
}

bool CompoundNativeExecutor::_history_correlated(const Execution &p_execution, bool p_require_newest) {
	EditorUndoRedoManager *manager = EditorUndoRedoManager::get_singleton();
	if (!manager || !manager->has_history(p_execution.native_history_id)) {
		return false;
	}
	UndoRedo *history = manager->get_history_undo_redo(p_execution.native_history_id);
	if (!history || p_execution.history_action_committed < 0 || p_execution.history_action_committed >= history->get_history_count() || history->get_action_name(p_execution.history_action_committed) != p_execution.action_name) {
		return false;
	}
	return !p_require_newest || history->get_current_action() == p_execution.history_action_committed;
}

CompoundNativeExecutor::Outcome CompoundNativeExecutor::apply(const CompoundChangeSetPlanner::Plan &p_plan, const String &p_project_id, const String &p_editor_session_id, const Dictionary &p_coordinates, const Dictionary &p_resource_paths, Execution &r_execution) {
	r_execution = Execution();
	r_execution.change_set_id = p_plan.change_set_id;
	const String scene_id = p_coordinates.get("scene_id", String());
	String history_id;
	int scene_history_id = -1;
	bool has_scene_operation = false;
	for (int index = 0; index < p_plan.ordered_operations.size(); index++) {
		const Dictionary operation = p_plan.ordered_operations[index];
		if (_is_scene_operation(operation.get("kind", String()))) {
			has_scene_operation = true;
			break;
		}
	}
	if (has_scene_operation && !scene_binding(p_editor_session_id, scene_id, history_id, scene_history_id)) {
		return failure("scene_not_open", "The compound scene anchor is not an open saved scene.");
	}
	r_execution.native_history_id = has_scene_operation ? scene_history_id : EditorUndoRedoManager::GLOBAL_HISTORY;

	for (int index = 0; index < p_plan.ordered_operations.size(); index++) {
		const Dictionary operation = p_plan.ordered_operations[index];
		const String kind = operation.get("kind", String());
		if (!_is_scene_operation(kind)) {
			continue;
		}
		TransactionExecutor *executor = _executor_for(kind);
		if (!executor) {
			cleanup(r_execution);
			return failure("scene_operation_unsupported", "A compound scene operation has no registered executor.");
		}
		TransactionSceneResolver::Job resolver_job;
		TransactionPreviewBuilder::Resolution resolution;
		String error_code;
		String error_message;
		if (!resolve_scene_operation(p_editor_session_id, scene_id, history_id, operation, resolver_job, resolution, error_code, error_message)) {
			cleanup(r_execution);
			return failure(error_code, error_message);
		}
		Step step;
		step.operation_kind = kind;
		step.executor = executor;
		step.record.transaction_id = p_plan.change_set_id;
		step.record.binding.project_id = p_project_id;
		step.record.binding.editor_session_id = p_editor_session_id;
		step.record.binding.scene_id = scene_id;
		step.record.binding.history_id = history_id;
		step.record.binding.scene_revision = (uint64_t)(int64_t)p_coordinates.get("scene_revision", 0);
		step.record.binding.operation_seq = (uint64_t)(int64_t)p_coordinates.get("operation_seq", 0);
		step.record.operation_kind = kind;
		step.record.precondition_digest = resolution.precondition_digest;
		if (executor->final_preflight(step.record, resolver_job, resolution, step.native_plan, error_code, error_message) != OK) {
			cleanup(r_execution);
			return failure(error_code.is_empty() ? "transaction_apply_failed" : error_code, error_message.is_empty() ? "A compound step failed final preflight." : error_message);
		}
		if (step.native_plan.native_history_id != r_execution.native_history_id) {
			cleanup(r_execution);
			return failure("change_set_history_mismatch", "A compound step selected a different native history.");
		}
		r_execution.steps.push_back(step);
	}

	ScopedPersistenceExecutor::Prepared staged;
	String persistence_code;
	String persistence_message;
	const bool has_persistence = p_plan.save_scope.size() > 0;
	if (has_persistence) {
		if (ScopedPersistenceExecutor::stage(p_plan, p_resource_paths, staged, persistence_code, persistence_message) != OK) {
			cleanup(r_execution);
			return failure(persistence_code, persistence_message);
		}
		r_execution.persistence.instantiate();
		r_execution.persistence->initialize(staged);
	}
	if (r_execution.steps.is_empty() && r_execution.persistence.is_null()) {
		cleanup(r_execution);
		return failure("change_set_invalid", "The compound plan has no executable steps.");
	}

	EditorUndoRedoManager *manager = EditorUndoRedoManager::get_singleton();
	if (!manager) {
		cleanup(r_execution);
		return failure("transaction_apply_failed", "The editor Undo manager is unavailable.");
	}
	EditorUndoRedoManager::History &history_record = manager->get_or_create_history(r_execution.native_history_id);
	UndoRedo *history = history_record.undo_redo;
	if (!history) {
		cleanup(r_execution);
		return failure("transaction_apply_failed", "The anchor native history is unavailable.");
	}
	r_execution.history_version_before = history->get_version();
	r_execution.history_action_before = history->get_current_action();
	r_execution.history_count_before = history->get_history_count();
	r_execution.action_name = vformat("Codex: change set [%s]", p_plan.change_set_id);
	manager->create_action_for_history(r_execution.action_name, r_execution.native_history_id, UndoRedo::MERGE_DISABLE, false, true);
	for (int index = 0; index < r_execution.steps.size(); index++) {
		Step &step = r_execution.steps.write[index];
		Error registration = ERR_UNAVAILABLE;
		if (step.executor == &structural_executor) {
			registration = structural_executor.register_native_action_on_history(step.native_plan, history);
		} else if (step.executor == &property_executor) {
			registration = property_executor.register_native_action_on_history(step.native_plan, history);
		} else if (step.executor == &script_executor) {
			registration = script_executor.register_native_action_on_history(step.native_plan, history);
		} else if (step.executor == &signal_executor) {
			registration = signal_executor.register_native_action_on_history(step.native_plan, history);
		}
		if (registration != OK) {
			r_execution.commit_point_entered = true;
			manager->commit_action(false);
			return failure("transaction_in_doubt", "A compound native step could not be registered after the action boundary.", true);
		}
	}
	if (r_execution.persistence.is_valid()) {
		history->add_do_method(callable_mp(r_execution.persistence.ptr(), &CompoundPersistenceAction::commit_postimages));
		history->add_undo_method(callable_mp(r_execution.persistence.ptr(), &CompoundPersistenceAction::restore_preimages));
	}
	r_execution.commit_point_entered = true;
	manager->commit_action(true);
	r_execution.history_action_committed = r_execution.history_action_before + 1;
	r_execution.history_count_committed = history->get_history_count();
	r_execution.history_version_committed = history->get_version();
	if (!_history_correlated(r_execution, true)) {
		return failure("transaction_in_doubt", "The compound native action could not be correlated after commit.", true);
	}
	if (!verify_poststate(r_execution)) {
		const bool undone = manager->undo_history(r_execution.native_history_id);
		if (!undone || !verify_prestate(r_execution)) {
			return failure("transaction_in_doubt", "A compound postcondition failed and the exact pre-state could not be proven.", true);
		}
		r_execution.undone = true;
		return failure(r_execution.persistence.is_valid() && !r_execution.persistence->succeeded() ? r_execution.persistence->get_error_code() : "transaction_apply_failed", r_execution.persistence.is_valid() && !r_execution.persistence->succeeded() ? r_execution.persistence->get_error_message() : "A compound postcondition failed; the native action was restored.");
	}
	r_execution.committed = true;
	Outcome outcome;
	outcome.success = true;
	return outcome;
}

bool CompoundNativeExecutor::verify_poststate(const Execution &p_execution) const {
	if (!p_execution.commit_point_entered) {
		return false;
	}
	for (const Step &step : p_execution.steps) {
		if (!step.executor || !step.executor->verify_postcondition(step.record, step.native_plan)) {
			return false;
		}
	}
	return p_execution.persistence.is_null() || (p_execution.persistence->succeeded() && p_execution.persistence->verify_postimages());
}

bool CompoundNativeExecutor::verify_prestate(const Execution &p_execution) const {
	for (const Step &step : p_execution.steps) {
		if (!step.executor || !step.executor->verify_prestate_after_rollback(step.record, step.native_plan)) {
			return false;
		}
	}
	return p_execution.persistence.is_null() || (p_execution.persistence->succeeded() && p_execution.persistence->verify_preimages());
}

CompoundNativeExecutor::Outcome CompoundNativeExecutor::undo(Execution &r_execution) {
	if (!r_execution.committed || r_execution.undone || !_history_correlated(r_execution, true) || !verify_poststate(r_execution)) {
		return failure("transaction_not_undoable", "The compound action is not the newest exact post-state in its native history.");
	}
	EditorUndoRedoManager *manager = EditorUndoRedoManager::get_singleton();
	if (!manager || !manager->undo_history(r_execution.native_history_id)) {
		return failure("transaction_undo_failed", "The native compound Undo could not be executed.");
	}
	if (!verify_prestate(r_execution)) {
		return failure("transaction_in_doubt", "The native compound Undo completed without proof of the exact pre-state.", true);
	}
	r_execution.undone = true;
	Outcome outcome;
	outcome.success = true;
	return outcome;
}

CompoundNativeExecutor::Outcome CompoundNativeExecutor::rollback(Execution &r_execution) {
	const Outcome outcome = undo(r_execution);
	if (!outcome.success && outcome.error_code == "transaction_not_undoable") {
		return failure("rollback_blocked", "Automatic rollback is blocked by an intervening action or changed postimage.");
	}
	return outcome;
}

void CompoundNativeExecutor::cleanup(Execution &r_execution) {
	if (r_execution.persistence.is_valid()) {
		r_execution.persistence->cleanup();
		r_execution.persistence.unref();
	}
	r_execution.steps.clear();
}
