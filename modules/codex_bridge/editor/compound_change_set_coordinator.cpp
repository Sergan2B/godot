/**************************************************************************/
/*  compound_change_set_coordinator.cpp                                   */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "compound_change_set_coordinator.h"

#include "core/io/json.h"

#include "modules/codex_bridge/protocol/bridge_transaction_canonicalizer.h"

namespace {

static bool parse_request_coordinates(const CompoundChangeSetPlanner::Plan &p_plan, Dictionary &r_coordinates) {
	const Variant parsed = JSON::parse_string(p_plan.canonical_request_json);
	if (parsed.get_type() != Variant::DICTIONARY) {
		return false;
	}
	const Dictionary request = parsed;
	const Variant coordinates = request.get("coordinates", Variant());
	if (coordinates.get_type() != Variant::DICTIONARY) {
		return false;
	}
	r_coordinates = Dictionary(coordinates).duplicate(true);
	for (const char *key : { "scene_revision", "operation_seq", "resource_revision", "script_graph_revision" }) {
		if (r_coordinates.has(key) && r_coordinates[key].get_type() == Variant::FLOAT) {
			r_coordinates[key] = (int64_t)(double)r_coordinates[key];
		}
	}
	return true;
}

} // namespace

bool CompoundChangeSetCoordinator::_coordinates_current(const Dictionary &p_coordinates) const {
	if (!revision_clock || String(p_coordinates.get("editor_session_id", String())) != editor_session_id) {
		return false;
	}
	const Dictionary revisions = revision_clock->get_revision_vector();
	const String scene_id = p_coordinates.get("scene_id", String());
	return (uint64_t)(int64_t)p_coordinates.get("scene_revision", -1) == revision_clock->get_scene_revision(scene_id) &&
			(uint64_t)(int64_t)p_coordinates.get("operation_seq", -1) == (uint64_t)(int64_t)revisions.get("operation_seq", -2) &&
			(uint64_t)(int64_t)p_coordinates.get("resource_revision", -1) == revision_clock->get_resource_revision() &&
			(uint64_t)(int64_t)p_coordinates.get("script_graph_revision", -1) == revision_clock->get_script_graph_revision();
}

Dictionary CompoundChangeSetCoordinator::_preview_result(const PreparedChangeSetStore::Record &p_record) {
	Dictionary result;
	result["schema_version"] = "change-set/1.0";
	result["change_set_id"] = p_record.plan.change_set_id;
	result["state"] = PreparedChangeSetStore::state_name(p_record.state);
	result["transaction_seq"] = (int64_t)p_record.transaction_seq;
	result["preview_digest"] = p_record.plan.preview_digest;
	result["request_digest"] = p_record.plan.request_digest;
	result["preview"] = JSON::parse_string(p_record.plan.canonical_preview_json);
	Dictionary coordinates;
	if (parse_request_coordinates(p_record.plan, coordinates)) {
		result["coordinates"] = coordinates;
	}
	result["risk"] = p_record.plan.risk;
	result["scope"] = "change_set.atomic";
	result["operation_count"] = p_record.plan.ordered_operations.size();
	result["created_at_ms"] = (int64_t)p_record.plan.created_at_ms;
	result["expires_at_ms"] = (int64_t)p_record.plan.expires_at_ms;
	Dictionary limits;
	limits["operations"] = CompoundChangeSetPlanner::MAX_OPERATIONS;
	limits["preview_bytes"] = CompoundChangeSetPlanner::MAX_PREVIEW_BYTES;
	result["limits_applied"] = limits;
	return result;
}

CompoundChangeSetCoordinator::Outcome CompoundChangeSetCoordinator::_error(const String &p_code, const String &p_message, bool p_retryable) {
	Outcome outcome;
	outcome.error_code = p_code;
	outcome.error_message = p_message;
	outcome.retryable = p_retryable;
	return outcome;
}

void CompoundChangeSetCoordinator::_emit(const PreparedChangeSetStore::Record &p_record) {
	Dictionary event;
	event["change_set_id"] = p_record.plan.change_set_id;
	event["transaction_seq"] = (int64_t)p_record.transaction_seq;
	event["state"] = PreparedChangeSetStore::state_name(p_record.state);
	if (!p_record.validation_report_id.is_empty()) {
		event["validation_report_id"] = p_record.validation_report_id;
	}
	events.push_back(event);
}

Error CompoundChangeSetCoordinator::_transition(const String &p_change_set_id, PreparedChangeSetStore::State p_state, uint64_t p_now_ms, const String &p_outcome, const String &p_error_code, const String &p_error_message) {
	const Error error = store.transition(p_change_set_id, p_state, p_now_ms, p_outcome, p_error_code, p_error_message);
	if (error == OK) {
		PreparedChangeSetStore::Record record;
		if (store.get(p_change_set_id, record)) {
			_emit(record);
		}
	}
	return error;
}

void CompoundChangeSetCoordinator::_expire(uint64_t p_now_ms) {
	const Vector<String> expired = store.expire(p_now_ms);
	for (const String &change_set_id : expired) {
		PreparedChangeSetStore::Record record;
		if (store.get(change_set_id, record)) {
			_emit(record);
		}
	}
}

void CompoundChangeSetCoordinator::initialize(const String &p_project_id, const String &p_editor_session_id, BridgeRevisionClock *p_revision_clock, const PackedByteArray &p_approval_key) {
	project_id = p_project_id;
	editor_session_id = p_editor_session_id;
	revision_clock = p_revision_clock;
	approval_verifier.initialize(p_approval_key);
	store.clear();
	executions.clear();
	resource_paths.clear();
	events.clear();
}

void CompoundChangeSetCoordinator::shutdown() {
	for (KeyValue<String, CompoundNativeExecutor::Execution> &entry : executions) {
		native_executor.cleanup(entry.value);
	}
	executions.clear();
	approval_verifier.shutdown();
	store.clear();
	resource_paths.clear();
	events.clear();
	project_id.clear();
	editor_session_id.clear();
	revision_clock = nullptr;
}

CompoundChangeSetCoordinator::Outcome CompoundChangeSetCoordinator::prepare(const Dictionary &p_params, uint64_t p_now_ms) {
	Outcome outcome;
	if (!is_ready()) {
		return _error("compound_executor_unavailable", "The compound coordinator is not initialized.");
	}
	_expire(p_now_ms);
	if (!CompoundChangeSetPlanner::validate_params(p_params)) {
		return _error("invalid_request", "The compound change-set parameters are invalid.");
	}
	const Dictionary coordinates = p_params["coordinates"];
	if (!_coordinates_current(coordinates)) {
		return _error("stale_editor_state", "The requested revision coordinates are stale.", true);
	}
	CompoundChangeSetPlanner::Plan plan;
	if (CompoundChangeSetPlanner::build(project_id, editor_session_id, p_params, p_now_ms, plan, outcome.error_code, outcome.error_message) != OK) {
		return outcome;
	}
	PreparedChangeSetStore::Record record;
	switch (store.admit(plan, record)) {
		case PreparedChangeSetStore::ADMISSION_CREATED:
			_emit(record);
			outcome.has_result = true;
			outcome.result = _preview_result(record);
			return outcome;
		case PreparedChangeSetStore::ADMISSION_REPLAY:
			outcome.has_result = true;
			outcome.result = _preview_result(record);
			return outcome;
		case PreparedChangeSetStore::ADMISSION_CONFLICT:
			return _error("idempotency_conflict", "The idempotency key is bound to a different immutable request.");
		case PreparedChangeSetStore::ADMISSION_BUSY:
			return _error("transaction_busy", "The bounded prepared change-set store is full.", true);
	}
	return _error("internal_error", "The change-set admission result is invalid.");
}

CompoundChangeSetCoordinator::Outcome CompoundChangeSetCoordinator::apply(const Dictionary &p_params, uint64_t p_now_ms) {
	_expire(p_now_ms);
	const String change_set_id = p_params.get("change_set_id", String());
	PreparedChangeSetStore::Record *record = store.get_mutable(change_set_id);
	if (!record) {
		return _error("change_set_not_found", "The bounded change-set record was not found.");
	}
	if (record->state == PreparedChangeSetStore::STATE_VALIDATING || record->state == PreparedChangeSetStore::STATE_COMMITTED || record->state == PreparedChangeSetStore::STATE_ROLLED_BACK || record->state == PreparedChangeSetStore::STATE_UNDONE || record->state == PreparedChangeSetStore::STATE_IN_DOUBT) {
		Outcome outcome;
		outcome.has_result = true;
		outcome.result = _preview_result(*record);
		outcome.result["outcome"] = record->outcome;
		if (!record->postimage_digest.is_empty()) {
			outcome.result["postimage_digest"] = record->postimage_digest;
		}
		return outcome;
	}
	if (record->state == PreparedChangeSetStore::STATE_EXPIRED) {
		return _error("change_set_expired", "The prepared change set expired.");
	}
	if (String(p_params.get("preview_digest", String())) != record->plan.preview_digest) {
		return _error("preview_mismatch", "The supplied preview digest does not match the immutable change set.");
	}
	Dictionary coordinates;
	if (!parse_request_coordinates(record->plan, coordinates) || !_coordinates_current(coordinates)) {
		_transition(change_set_id, PreparedChangeSetStore::STATE_CONFLICTED, p_now_ms, "not_applied", "stale_editor_state", "The editor state changed before compound apply.");
		return _error("stale_editor_state", "The editor state changed before compound apply.", true);
	}
	if ((uint64_t)(int64_t)p_params.get("expected_scene_revision", -1) != (uint64_t)(int64_t)coordinates.get("scene_revision", -2) || (uint64_t)(int64_t)p_params.get("expected_operation_seq", -1) != (uint64_t)(int64_t)coordinates.get("operation_seq", -2)) {
		return _error("stale_editor_state", "The supplied apply coordinates do not match the immutable preview.", true);
	}
	TransactionApprovalVerifier::Binding binding;
	binding.project_id = project_id;
	binding.editor_session_id = editor_session_id;
	binding.scene_id = coordinates.get("scene_id", String());
	binding.transaction_id = change_set_id;
	binding.preview_digest = record->plan.preview_digest;
	binding.scope = "change_set.atomic";
	binding.risk = record->plan.risk;
	binding.scene_revision = (uint64_t)(int64_t)coordinates.get("scene_revision", 0);
	binding.operation_seq = (uint64_t)(int64_t)coordinates.get("operation_seq", 0);
	const Dictionary receipt = p_params.get("approval", Dictionary());
	const TransactionApprovalVerifier::Outcome approval = approval_verifier.verify(receipt, binding, p_now_ms);
	if (!approval.valid) {
		if (record->state == PreparedChangeSetStore::STATE_PREVIEWED) {
			_transition(change_set_id, PreparedChangeSetStore::STATE_AWAITING_APPROVAL, p_now_ms);
		}
		return _error(approval.error_code, approval.error_message);
	}
	String approval_error;
	String approval_message;
	if (approval_verifier.consume(approval.receipt, p_now_ms, approval_error, approval_message) != OK) {
		return _error(approval_error, approval_message, approval_error == "transaction_busy");
	}
	if (record->state != PreparedChangeSetStore::STATE_PREVIEWED && record->state != PreparedChangeSetStore::STATE_AWAITING_APPROVAL) {
		return _error("transaction_busy", "The change set is not eligible for apply.", true);
	}
	_transition(change_set_id, PreparedChangeSetStore::STATE_APPLYING, p_now_ms);
	record = store.get_mutable(change_set_id);
	ERR_FAIL_NULL_V(record, _error("change_set_not_found", "The change set disappeared before apply."));
	record->mutation_started = true;
	CompoundNativeExecutor::Execution execution;
	const CompoundNativeExecutor::Outcome applied = native_executor.apply(record->plan, project_id, editor_session_id, coordinates, resource_paths, execution);
	record = store.get_mutable(change_set_id);
	ERR_FAIL_NULL_V(record, _error("change_set_not_found", "The change set disappeared during apply."));
	record->commit_point_entered = execution.commit_point_entered;
	record->native_history_id = execution.native_history_id;
	record->native_action_tag = execution.action_name;
	executions.insert(change_set_id, execution);
	if (!applied.success) {
		if (applied.in_doubt) {
			_transition(change_set_id, PreparedChangeSetStore::STATE_IN_DOUBT, p_now_ms, "unknown", applied.error_code, applied.error_message);
		} else {
			_transition(change_set_id, PreparedChangeSetStore::STATE_FAILED, p_now_ms, execution.undone ? "rolled_back" : "not_applied", applied.error_code, applied.error_message);
		}
		return _error(applied.error_code, applied.error_message);
	}
	String postimage_digest;
	const String postimage_material = record->plan.preview_digest + "\n" + itos(execution.history_version_committed) + "\n" + itos(execution.history_action_committed);
	if (BridgeTransactionCanonicalizer::sha256_utf8(postimage_material, postimage_digest, "godot-codex-change-set-postimage/v1\n") != OK) {
		_transition(change_set_id, PreparedChangeSetStore::STATE_IN_DOUBT, p_now_ms, "unknown", "transaction_in_doubt", "The committed postimage identity could not be created.");
		return _error("transaction_in_doubt", "The committed postimage identity could not be created.");
	}
	record->postimage_digest = postimage_digest;
	_transition(change_set_id, PreparedChangeSetStore::STATE_VALIDATING, p_now_ms, "applied");
	record = store.get_mutable(change_set_id);
	Outcome outcome;
	outcome.has_result = true;
	outcome.result = _preview_result(*record);
	outcome.result["postimage_digest"] = record->postimage_digest;
	outcome.result["outcome"] = record->outcome;
	return outcome;
}

void CompoundChangeSetCoordinator::_reconcile(PreparedChangeSetStore::Record &r_record, uint64_t p_now_ms) {
	CompoundNativeExecutor::Execution *execution = executions.getptr(r_record.plan.change_set_id);
	if (!execution) {
		return;
	}
	if (r_record.state == PreparedChangeSetStore::STATE_COMMITTED && native_executor.verify_prestate(*execution)) {
		execution->undone = true;
		_transition(r_record.plan.change_set_id, PreparedChangeSetStore::STATE_UNDONE, p_now_ms, "undone");
	} else if (r_record.state == PreparedChangeSetStore::STATE_UNDONE && native_executor.verify_poststate(*execution)) {
		execution->undone = false;
		_transition(r_record.plan.change_set_id, PreparedChangeSetStore::STATE_IN_DOUBT, p_now_ms, "unknown", "validation_required", "Native Redo requires a new automatic validation result.");
	}
}

CompoundChangeSetCoordinator::Outcome CompoundChangeSetCoordinator::status(const String &p_change_set_id, uint64_t p_now_ms) {
	_expire(p_now_ms);
	PreparedChangeSetStore::Record *mutable_record = store.get_mutable(p_change_set_id);
	if (!mutable_record) {
		return _error("change_set_not_found", "The bounded change-set record was not found.");
	}
	_reconcile(*mutable_record, p_now_ms);
	PreparedChangeSetStore::Record record;
	if (!store.get(p_change_set_id, record)) {
		return _error("change_set_not_found", "The bounded change-set record disappeared.");
	}
	Outcome outcome;
	outcome.has_result = true;
	outcome.result = _preview_result(record);
	outcome.result["outcome"] = record.outcome;
	if (!record.postimage_digest.is_empty()) {
		outcome.result["postimage_digest"] = record.postimage_digest;
	}
	if (!record.validation_report_id.is_empty()) {
		outcome.result["validation_report_id"] = record.validation_report_id;
	}
	if (!record.error_code.is_empty()) {
		Dictionary error;
		error["code"] = record.error_code;
		error["message"] = record.error_message;
		outcome.result["terminal_error"] = error;
	}
	return outcome;
}

CompoundChangeSetCoordinator::Outcome CompoundChangeSetCoordinator::validation_complete(const Dictionary &p_params, uint64_t p_now_ms) {
	const String change_set_id = p_params.get("change_set_id", String());
	PreparedChangeSetStore::Record *record = store.get_mutable(change_set_id);
	if (!record) {
		return _error("change_set_not_found", "The validation target was not found.");
	}
	if (record->state != PreparedChangeSetStore::STATE_VALIDATING) {
		return _error("validation_state_invalid", "The change set is not awaiting validation.");
	}
	if (record->transaction_seq != (uint64_t)(int64_t)p_params.get("expected_transaction_seq", -1) ||
			record->postimage_digest != String(p_params.get("expected_postimage_digest", String()))) {
		return _error("validation_report_mismatch", "The validation report does not bind the exact retained post-state.");
	}
	const String report_id = p_params.get("validation_report_id", String());
	const String report_digest = p_params.get("report_digest", String());
	const String outcome_name = p_params.get("outcome", String());
	if (!report_id.begins_with("validation-report:") || report_id.length() != 50 || !report_digest.begins_with("sha256:") || report_digest.length() != 71 || (outcome_name != "passed" && outcome_name != "failed" && outcome_name != "inconclusive" && outcome_name != "timed_out")) {
		return _error("invalid_request", "The validation completion binding is invalid.");
	}
	record->validation_report_id = report_id;
	if (outcome_name == "passed") {
		_transition(change_set_id, PreparedChangeSetStore::STATE_COMMITTED, p_now_ms, "committed");
		return status(change_set_id, p_now_ms);
	}
	const String rollback_policy = record->plan.validation_policy.get("rollback", "on_required_failure");
	if (rollback_policy == "never") {
		_transition(change_set_id, PreparedChangeSetStore::STATE_FAILED, p_now_ms, "validation_failed", "validation_failed", "Automatic validation did not pass and rollback policy is never.");
		return status(change_set_id, p_now_ms);
	}
	_transition(change_set_id, PreparedChangeSetStore::STATE_ROLLING_BACK, p_now_ms, "validation_failed");
	CompoundNativeExecutor::Execution *execution = executions.getptr(change_set_id);
	if (!execution) {
		_transition(change_set_id, PreparedChangeSetStore::STATE_IN_DOUBT, p_now_ms, "unknown", "rollback_in_doubt", "The retained compound inverse is unavailable.");
		return status(change_set_id, p_now_ms);
	}
	const CompoundNativeExecutor::Outcome rolled_back = native_executor.rollback(*execution);
	if (rolled_back.success) {
		_transition(change_set_id, PreparedChangeSetStore::STATE_ROLLED_BACK, p_now_ms, "failed_rolled_back", "validation_failed", "Automatic validation did not pass; the exact pre-state was restored.");
	} else if (rolled_back.in_doubt) {
		_transition(change_set_id, PreparedChangeSetStore::STATE_IN_DOUBT, p_now_ms, "unknown", rolled_back.error_code, rolled_back.error_message);
	} else {
		_transition(change_set_id, PreparedChangeSetStore::STATE_ROLLBACK_BLOCKED, p_now_ms, "validation_failed", rolled_back.error_code, rolled_back.error_message);
	}
	return status(change_set_id, p_now_ms);
}

CompoundChangeSetCoordinator::Outcome CompoundChangeSetCoordinator::rollback(const Dictionary &p_params, uint64_t p_now_ms) {
	const String change_set_id = p_params.get("change_set_id", String());
	PreparedChangeSetStore::Record *record = store.get_mutable(change_set_id);
	if (!record) {
		return _error("change_set_not_found", "The rollback target was not found.");
	}
	if (record->postimage_digest != String(p_params.get("expected_postimage_digest", String())) || record->transaction_seq != (uint64_t)(int64_t)p_params.get("expected_transaction_seq", -1)) {
		return _error("rollback_blocked", "Rollback coordinates do not match the exact retained post-state.");
	}
	if (record->state != PreparedChangeSetStore::STATE_VALIDATING && record->state != PreparedChangeSetStore::STATE_COMMITTED) {
		return _error("rollback_blocked", "The change set is not eligible for rollback.");
	}
	_transition(change_set_id, PreparedChangeSetStore::STATE_ROLLING_BACK, p_now_ms);
	CompoundNativeExecutor::Execution *execution = executions.getptr(change_set_id);
	if (!execution) {
		_transition(change_set_id, PreparedChangeSetStore::STATE_IN_DOUBT, p_now_ms, "unknown", "rollback_in_doubt", "The retained compound inverse is unavailable.");
		return status(change_set_id, p_now_ms);
	}
	const CompoundNativeExecutor::Outcome rolled_back = native_executor.rollback(*execution);
	if (rolled_back.success) {
		_transition(change_set_id, PreparedChangeSetStore::STATE_ROLLED_BACK, p_now_ms, "rolled_back");
	} else if (rolled_back.in_doubt) {
		_transition(change_set_id, PreparedChangeSetStore::STATE_IN_DOUBT, p_now_ms, "unknown", rolled_back.error_code, rolled_back.error_message);
	} else {
		_transition(change_set_id, PreparedChangeSetStore::STATE_ROLLBACK_BLOCKED, p_now_ms, "rollback_blocked", rolled_back.error_code, rolled_back.error_message);
	}
	return status(change_set_id, p_now_ms);
}

CompoundChangeSetCoordinator::Outcome CompoundChangeSetCoordinator::undo(const Dictionary &p_params, uint64_t p_now_ms) {
	const String change_set_id = p_params.get("change_set_id", String());
	PreparedChangeSetStore::Record *record = store.get_mutable(change_set_id);
	if (!record || record->state != PreparedChangeSetStore::STATE_COMMITTED) {
		return _error("transaction_not_undoable", "Only a committed retained change set can be undone.");
	}
	if (record->transaction_seq != (uint64_t)(int64_t)p_params.get("expected_transaction_seq", -1)) {
		return _error("transaction_not_undoable", "The change-set sequence changed before Undo.", true);
	}
	CompoundNativeExecutor::Execution *execution = executions.getptr(change_set_id);
	if (!execution) {
		return _error("transaction_not_undoable", "The retained compound inverse is unavailable.");
	}
	const CompoundNativeExecutor::Outcome undone = native_executor.undo(*execution);
	if (!undone.success) {
		return _error(undone.error_code, undone.error_message, undone.in_doubt);
	}
	_transition(change_set_id, PreparedChangeSetStore::STATE_UNDONE, p_now_ms, "undone");
	return status(change_set_id, p_now_ms);
}

void CompoundChangeSetCoordinator::drain_events(Vector<Dictionary> &r_events) {
	r_events.append_array(events);
	events.clear();
}

void CompoundChangeSetCoordinator::invalidate_all(uint64_t p_now_ms) {
	(void)p_now_ms;
	for (KeyValue<String, CompoundNativeExecutor::Execution> &entry : executions) {
		native_executor.cleanup(entry.value);
	}
	executions.clear();
	store.clear();
	events.clear();
}

uint32_t CompoundChangeSetCoordinator::get_active_count() const {
	return store.get_active_count();
}

bool CompoundChangeSetCoordinator::is_ready() const {
	return revision_clock && !project_id.is_empty() && !editor_session_id.is_empty();
}

bool CompoundChangeSetCoordinator::is_approval_available() const {
	return approval_verifier.is_available();
}
