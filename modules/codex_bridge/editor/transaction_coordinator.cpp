/**************************************************************************/
/*  transaction_coordinator.cpp                                         */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "transaction_coordinator.h"

#include "core/io/json.h"
#include "core/os/time.h"
#include "editor/editor_undo_redo_manager.h"

#include "modules/codex_bridge/protocol/bridge_frame_codec.h"
#include "modules/codex_bridge/protocol/bridge_transaction_canonicalizer.h"
#include "modules/codex_bridge/protocol/bridge_transaction_profile.h"

namespace {

static TransactionCoordinator::StartOutcome start_error(const String &p_code, const String &p_message, bool p_retryable = false) {
	TransactionCoordinator::StartOutcome outcome;
	outcome.error_code = p_code;
	outcome.error_message = p_message;
	outcome.retryable = p_retryable;
	return outcome;
}

static void append_completion(Vector<TransactionCoordinator::Completion> &r_completions, uint64_t p_request_id, const Dictionary &p_result) {
	TransactionCoordinator::Completion completion;
	completion.request_id = p_request_id;
	completion.has_result = true;
	completion.result = p_result.duplicate(true);
	r_completions.push_back(completion);
}

static void append_error(Vector<TransactionCoordinator::Completion> &r_completions, uint64_t p_request_id, const String &p_code, const String &p_message, bool p_retryable) {
	TransactionCoordinator::Completion completion;
	completion.request_id = p_request_id;
	completion.error_code = p_code;
	completion.error_message = p_message;
	completion.retryable = p_retryable;
	r_completions.push_back(completion);
}

static uint64_t current_time_ms() {
	return (uint64_t)(Time::get_singleton()->get_unix_time_from_system() * 1000.0);
}

} // namespace

bool TransactionCoordinator::_coordinates_are_current(const Dictionary &p_coordinates, String &r_error_code, String &r_error_message) const {
	if (!revision_clock) {
		r_error_code = "stale_editor_state";
		r_error_message = "The editor revision clock is unavailable.";
		return false;
	}
	const Dictionary revisions = revision_clock->get_revision_vector();
	if (String(revisions.get("editor_session_id", String())) != editor_session_id) {
		r_error_code = "stale_editor_state";
		r_error_message = "The editor session changed before preparation.";
		return false;
	}
	const String scene_id = p_coordinates["scene_id"];
	const Dictionary scene_revisions = revisions["scene_revisions"];
	const uint64_t current_scene_revision = scene_revisions.has(scene_id) ? (uint64_t)(int64_t)scene_revisions[scene_id] : 0;
	if (current_scene_revision != (uint64_t)(int64_t)p_coordinates["scene_revision"]) {
		r_error_code = "stale_scene_revision";
		r_error_message = "The requested scene revision is stale.";
		return false;
	}
	if ((uint64_t)(int64_t)revisions["operation_seq"] != (uint64_t)(int64_t)p_coordinates["operation_seq"]) {
		r_error_code = "stale_editor_state";
		r_error_message = "The requested editor operation sequence is stale.";
		return false;
	}
	return true;
}

bool TransactionCoordinator::_binding_is_current(const PreparedTransactionStore::Binding &p_binding) const {
	if (!revision_clock || p_binding.project_id != project_id || p_binding.editor_session_id != editor_session_id) {
		return false;
	}
	const Dictionary revisions = revision_clock->get_revision_vector();
	const Dictionary scene_revisions = revisions["scene_revisions"];
	const uint64_t current_scene_revision = scene_revisions.has(p_binding.scene_id) ? (uint64_t)(int64_t)scene_revisions[p_binding.scene_id] : 0;
	return String(revisions.get("editor_session_id", String())) == editor_session_id && current_scene_revision == p_binding.scene_revision && (uint64_t)(int64_t)revisions["operation_seq"] == p_binding.operation_seq && (uint64_t)(int64_t)revisions["project_revision"] == p_binding.project_revision && (uint64_t)(int64_t)revisions["resource_revision"] == p_binding.resource_revision && (uint64_t)(int64_t)revisions["scene_graph_revision"] == p_binding.scene_graph_revision && (uint64_t)(int64_t)revisions["script_graph_revision"] == p_binding.script_graph_revision;
}

TransactionCoordinator::PendingJob *TransactionCoordinator::_find_job(const String &p_transaction_id) {
	for (PendingJob &job : pending_jobs) {
		if (job.transaction_id == p_transaction_id) {
			return &job;
		}
	}
	return nullptr;
}

TransactionCoordinator::PendingApplyJob *TransactionCoordinator::_find_apply_job(const String &p_transaction_id) {
	for (PendingApplyJob &job : pending_apply_jobs) {
		if (job.transaction_id == p_transaction_id) {
			return &job;
		}
	}
	return nullptr;
}

void TransactionCoordinator::_complete_waiters(const PendingJob &p_job, const Dictionary &p_result) {
	for (uint64_t request_id : p_job.waiters) {
		append_completion(queued_completions, request_id, p_result);
	}
}

void TransactionCoordinator::_fail_waiters(const PendingJob &p_job, const String &p_code, const String &p_message, bool p_retryable) {
	for (uint64_t request_id : p_job.waiters) {
		append_error(queued_completions, request_id, p_code, p_message, p_retryable);
	}
}

void TransactionCoordinator::_complete_apply_waiters(const PendingApplyJob &p_job, const Dictionary &p_result) {
	for (uint64_t request_id : p_job.waiters) {
		append_completion(queued_completions, request_id, p_result);
	}
}

void TransactionCoordinator::_fail_apply_waiters(const PendingApplyJob &p_job, const String &p_code, const String &p_message, bool p_retryable) {
	for (uint64_t request_id : p_job.waiters) {
		append_error(queued_completions, request_id, p_code, p_message, p_retryable);
	}
}

void TransactionCoordinator::_queue_event(const PreparedTransactionStore::Record &p_record, const Variant &p_previous_state, const String &p_reason, uint64_t p_now_ms) {
	if (!revision_clock) {
		return;
	}
	Dictionary coordinates;
	coordinates["transaction_id"] = p_record.transaction_id;
	coordinates["scene_id"] = p_record.binding.scene_id;
	coordinates["history_id"] = p_record.binding.history_id;
	coordinates["scene_revision"] = (int64_t)p_record.binding.scene_revision;
	coordinates["operation_seq"] = (int64_t)p_record.binding.operation_seq;
	coordinates["transaction_seq"] = (int64_t)p_record.transaction_seq;
	Dictionary event;
	event["coordinates"] = coordinates;
	event["previous_state"] = p_previous_state;
	event["state"] = PreparedTransactionStore::state_name(p_record.state);
	event["reason"] = p_reason;
	event["revisions"] = revision_clock->get_revision_vector();
	event["timestamp_ms"] = (int64_t)p_now_ms;
	if (BridgeTransactionProfile::validate_event_params(event)) {
		queued_events.push_back(event.duplicate(true));
	}
}

bool TransactionCoordinator::_transition(const String &p_transaction_id, PreparedTransactionStore::State p_state, const String &p_reason, uint64_t p_now_ms, const String &p_outcome, const String &p_error_code, const String &p_error_message, bool p_error_retryable) {
	PreparedTransactionStore::Record before;
	if (!prepared_store.get_record(p_transaction_id, before) || before.state == p_state) {
		return false;
	}
	if (prepared_store.transition(p_transaction_id, p_state, p_now_ms, p_outcome, p_error_code, p_error_message, p_error_retryable) != OK) {
		return false;
	}
	PreparedTransactionStore::Record after;
	if (!prepared_store.get_record(p_transaction_id, after)) {
		return false;
	}
	_queue_event(after, PreparedTransactionStore::state_name(before.state), p_reason, p_now_ms);
	return true;
}

void TransactionCoordinator::_expire_prepared(uint64_t p_now_usec, uint64_t p_now_ms) {
	Vector<PreparedTransactionStore::Record> records;
	prepared_store.get_records(records);
	for (const PreparedTransactionStore::Record &record : records) {
		if (p_now_usec < record.expiry_deadline_usec || (record.state != PreparedTransactionStore::STATE_PREPARING && record.state != PreparedTransactionStore::STATE_PREVIEWED && record.state != PreparedTransactionStore::STATE_AWAITING_APPROVAL)) {
			continue;
		}
		const PreparedTransactionStore::State state = record.state == PreparedTransactionStore::STATE_PREPARING ? PreparedTransactionStore::STATE_FAILED : PreparedTransactionStore::STATE_EXPIRED;
		_transition(record.transaction_id, state, "prepared_expired", p_now_ms, String(), "transaction_expired", "The prepared transaction expired.", false);
	}
}

bool TransactionCoordinator::_capture_committed_entities(PreparedTransactionStore::Record &r_record, TransactionExecutor *p_executor, const TransactionExecutor::NativeActionPlan &p_plan) {
	ERR_FAIL_NULL_V(p_executor, false);
	const Array entities = p_executor->collect_committed_entities(r_record, p_plan);
	const bool requires_post_commit_identity = r_record.operation_kind == "create_node" || r_record.operation_kind == "reparent_node";
	if ((requires_post_commit_identity && entities.size() != 1) || !BridgeTransactionProfile::validate_affected_entities(entities, !requires_post_commit_identity)) {
		return false;
	}
	r_record.committed_entities = entities.duplicate(true);
	return true;
}

Dictionary TransactionCoordinator::_make_status(const PreparedTransactionStore::Record &p_record) const {
	Dictionary coordinates;
	coordinates["transaction_id"] = p_record.transaction_id;
	coordinates["scene_id"] = p_record.binding.scene_id;
	coordinates["history_id"] = p_record.binding.history_id;
	coordinates["scene_revision"] = (int64_t)p_record.binding.scene_revision;
	coordinates["operation_seq"] = (int64_t)p_record.binding.operation_seq;
	coordinates["transaction_seq"] = (int64_t)p_record.transaction_seq;

	uint64_t current_scene_revision = 0;
	uint64_t current_operation_seq = 0;
	if (revision_clock) {
		const Dictionary revisions = revision_clock->get_revision_vector();
		const Dictionary scene_revisions = revisions.get("scene_revisions", Dictionary());
		current_scene_revision = scene_revisions.has(p_record.binding.scene_id) ? (uint64_t)(int64_t)scene_revisions[p_record.binding.scene_id] : 0;
		current_operation_seq = (uint64_t)(int64_t)revisions.get("operation_seq", 0);
	}

	Dictionary undo_eligibility;
	undo_eligibility["eligible"] = false;
	undo_eligibility["reason"] = "not_committed";
	if (p_record.state == PreparedTransactionStore::STATE_COMMITTED) {
		int current_action = -1;
		uint64_t version = 0;
		if (!_native_action_is_correlated(p_record, current_action, version)) {
			undo_eligibility["reason"] = "history_unavailable";
		} else if (current_action != p_record.history_action_committed) {
			undo_eligibility["reason"] = "not_newest_action";
		} else {
			undo_eligibility["eligible"] = true;
			undo_eligibility["reason"] = "eligible";
		}
	}

	Dictionary status;
	status["schema_version"] = "transaction/1.0";
	status["coordinates"] = coordinates;
	status["state"] = PreparedTransactionStore::state_name(p_record.state);
	status["operation_kind"] = p_record.operation_kind;
	status["risk"] = p_record.risk;
	status["scope"] = p_record.scope;
	status["preview_digest"] = p_record.preview_digest;
	status["current_scene_revision"] = (int64_t)current_scene_revision;
	status["current_operation_seq"] = (int64_t)current_operation_seq;
	status["outcome"] = p_record.outcome;
	if (!p_record.committed_entities.is_empty()) {
		status["committed_entities"] = p_record.committed_entities.duplicate(true);
	}
	if (!p_record.terminal_error.is_empty()) {
		Dictionary error;
		error["code"] = p_record.terminal_error;
		error["message"] = p_record.error_message.is_empty() ? "The transaction did not complete successfully." : p_record.error_message.left(256);
		error["retryable"] = p_record.error_retryable;
		status["error"] = error;
	}
	status["undo_eligibility"] = undo_eligibility;
	status["updated_at_ms"] = (int64_t)p_record.updated_at_ms;
	status["limits_applied"] = BridgeTransactionProfile::make_limits();
	status["truncated"] = false;
	return status;
}

TransactionCoordinator::StartOutcome TransactionCoordinator::_latched_apply_outcome(const PreparedTransactionStore::Record &p_record) const {
	StartOutcome outcome;
	if (!p_record.apply_terminal_latched) {
		outcome.error_code = "transaction_busy";
		outcome.error_message = "The transaction apply attempt is still running.";
		outcome.retryable = true;
		return outcome;
	}
	if (p_record.apply_terminal_is_error) {
		outcome.error_code = p_record.apply_terminal_error_code;
		outcome.error_message = p_record.apply_terminal_error_message;
		outcome.retryable = p_record.apply_terminal_error_retryable;
	} else {
		outcome.has_result = true;
		outcome.result = p_record.apply_terminal_result.duplicate(true);
	}
	return outcome;
}

void TransactionCoordinator::_latch_apply_result(PreparedTransactionStore::Record &r_record, const Dictionary &p_result) {
	r_record.apply_terminal_latched = true;
	r_record.apply_terminal_is_error = false;
	r_record.apply_terminal_result = p_result.duplicate(true);
	r_record.apply_terminal_error_code.clear();
	r_record.apply_terminal_error_message.clear();
	r_record.apply_terminal_error_retryable = false;
}

void TransactionCoordinator::_latch_apply_error(PreparedTransactionStore::Record &r_record, const String &p_code, const String &p_message, bool p_retryable) {
	r_record.apply_terminal_latched = true;
	r_record.apply_terminal_is_error = true;
	r_record.apply_terminal_result.clear();
	r_record.apply_terminal_error_code = p_code;
	r_record.apply_terminal_error_message = p_message;
	r_record.apply_terminal_error_retryable = p_retryable;
}

void TransactionCoordinator::_invalidate_jobs(const String &p_scene_id) {
	const uint64_t now_ms = current_time_ms();
	for (const PendingJob &job : pending_jobs) {
		PreparedTransactionStore::Record record;
		if (!prepared_store.get_record(job.transaction_id, record) || (!p_scene_id.is_empty() && record.binding.scene_id != p_scene_id)) {
			continue;
		}
		_fail_waiters(job, "transaction_conflicted", "The editor state changed while the transaction was being prepared.", true);
	}
	for (List<PendingJob>::Element *element = pending_jobs.front(); element;) {
		List<PendingJob>::Element *next = element->next();
		PreparedTransactionStore::Record record;
		if (prepared_store.get_record(element->get().transaction_id, record) && (p_scene_id.is_empty() || record.binding.scene_id == p_scene_id)) {
			pending_jobs.erase(element);
		}
		element = next;
	}
	Vector<PreparedTransactionStore::Record> records;
	prepared_store.get_records(records);
	for (const PreparedTransactionStore::Record &record : records) {
		if ((!p_scene_id.is_empty() && record.binding.scene_id != p_scene_id) || (record.state != PreparedTransactionStore::STATE_PREPARING && record.state != PreparedTransactionStore::STATE_PREVIEWED && record.state != PreparedTransactionStore::STATE_AWAITING_APPROVAL)) {
			continue;
		}
		const PreparedTransactionStore::State state = record.state == PreparedTransactionStore::STATE_PREPARING ? PreparedTransactionStore::STATE_FAILED : PreparedTransactionStore::STATE_CONFLICTED;
		_transition(record.transaction_id, state, "revision_conflict", now_ms, String(), "transaction_conflicted", "The prepared transaction was invalidated by an editor change.", true);
	}
}

void TransactionCoordinator::initialize(const String &p_project_id, const String &p_editor_session_id, BridgeRevisionClock *p_revision_clock, const PackedByteArray &p_approval_key) {
	shutdown();
	project_id = p_project_id;
	editor_session_id = p_editor_session_id;
	revision_clock = p_revision_clock;
	approval_verifier.initialize(p_approval_key);
#ifdef CODEX_BRIDGE_TESTS_ENABLED
	fault_controller.initialize();
#endif
}

void TransactionCoordinator::shutdown() {
	project_id.clear();
	editor_session_id.clear();
	revision_clock = nullptr;
	approval_verifier.shutdown();
	prepared_store.clear();
	pending_jobs.clear();
	pending_apply_jobs.clear();
	queued_completions.clear();
	queued_events.clear();
	executors.clear();
	retained_native_plans.clear();
	busy_native_histories.clear();
#ifdef CODEX_BRIDGE_TESTS_ENABLED
	fault_controller.reset();
#endif
}

TransactionCoordinator::StartOutcome TransactionCoordinator::prepare(uint64_t p_request_id, const Dictionary &p_params, uint64_t p_now_usec, uint64_t p_now_ms) {
	if (!revision_clock) {
		return start_error("internal_error", "The transaction coordinator is not initialized.", true);
	}
	_expire_prepared(p_now_usec, p_now_ms);
	Dictionary params = p_params.duplicate(true);
	params.erase("_protocol_version");
	if (!BridgeTransactionProfile::validate_prepare_params(params)) {
		return start_error("invalid_request", "The transaction prepare parameters are invalid.");
	}
	String canonical_request_json;
	String request_digest;
	if (BridgeTransactionCanonicalizer::make_request_digest(project_id, editor_session_id, params, canonical_request_json, request_digest) != OK) {
		return start_error("transaction_too_large", "The canonical transaction request exceeds the bounded limit.");
	}
	const PreparedTransactionStore::Admission existing = prepared_store.inspect_idempotency(params["idempotency_key"], request_digest, p_now_usec);
	if (!existing.transaction_id.is_empty()) {
		if (existing.kind == PreparedTransactionStore::ADMISSION_REPLAY) {
			Dictionary result;
			if (prepared_store.get_prepare_result(existing.transaction_id, result) != OK) {
				return start_error("transaction_conflicted", "The stored transaction preview is unavailable.", true);
			}
			StartOutcome outcome;
			outcome.has_result = true;
			outcome.result = result;
			return outcome;
		}
		if (existing.kind == PreparedTransactionStore::ADMISSION_IN_PROGRESS) {
			PendingJob *job = _find_job(existing.transaction_id);
			if (!job || job->waiters.size() >= MAX_WAITERS_PER_TRANSACTION) {
				return start_error("transaction_busy", "The prepared transaction has too many active waiters.", true);
			}
			job->waiters.push_back(p_request_id);
			StartOutcome outcome;
			outcome.pending = true;
			return outcome;
		}
		if (existing.kind == PreparedTransactionStore::ADMISSION_IDEMPOTENCY_CONFLICT) {
			return start_error("idempotency_conflict", "The idempotency key is already bound to a different request.");
		}
		if (existing.kind == PreparedTransactionStore::ADMISSION_TRANSACTION_EXPIRED) {
			return start_error("transaction_expired", "The prepared transaction has expired.");
		}
		return start_error("transaction_conflicted", "The prepared transaction was invalidated by an editor change.", true);
	}
	const Dictionary coordinates = params["coordinates"];
	String error_code;
	String error_message;
	if (!_coordinates_are_current(coordinates, error_code, error_message)) {
		return start_error(error_code, error_message);
	}
	TransactionSceneResolver::Job resolver_job;
	if (TransactionSceneResolver::begin(editor_session_id, coordinates["scene_id"], coordinates["history_id"], params["operation"], resolver_job, error_code, error_message) != OK) {
		return start_error(error_code, error_message);
	}

	const Dictionary operation = BridgeTransactionCanonicalizer::normalize_operation(params["operation"]);
	const String operation_json = JSON::stringify(operation, "", true, true);
	const Dictionary revisions = revision_clock->get_revision_vector();
	PreparedTransactionStore::Binding binding;
	binding.project_id = project_id;
	binding.editor_session_id = editor_session_id;
	binding.scene_id = coordinates["scene_id"];
	binding.history_id = coordinates["history_id"];
	binding.scene_revision = (uint64_t)(int64_t)coordinates["scene_revision"];
	binding.operation_seq = (uint64_t)(int64_t)coordinates["operation_seq"];
	binding.event_seq = (uint64_t)(int64_t)revisions["event_seq"];
	binding.project_revision = (uint64_t)(int64_t)revisions["project_revision"];
	binding.resource_revision = (uint64_t)(int64_t)revisions["resource_revision"];
	binding.scene_graph_revision = (uint64_t)(int64_t)revisions["scene_graph_revision"];
	binding.script_graph_revision = (uint64_t)(int64_t)revisions["script_graph_revision"];

	const PreparedTransactionStore::Admission admission = prepared_store.admit(params["idempotency_key"], request_digest, canonical_request_json, operation_json, binding, p_now_usec, p_now_ms);
	if (admission.kind == PreparedTransactionStore::ADMISSION_REPLAY) {
		Dictionary result;
		if (prepared_store.get_prepare_result(admission.transaction_id, result) != OK) {
			return start_error("transaction_conflicted", "The stored transaction preview is unavailable.", true);
		}
		StartOutcome outcome;
		outcome.has_result = true;
		outcome.result = result;
		return outcome;
	}
	if (admission.kind == PreparedTransactionStore::ADMISSION_IN_PROGRESS) {
		PendingJob *job = _find_job(admission.transaction_id);
		if (!job || job->waiters.size() >= MAX_WAITERS_PER_TRANSACTION) {
			return start_error("transaction_busy", "The prepared transaction has too many active waiters.", true);
		}
		job->waiters.push_back(p_request_id);
		StartOutcome outcome;
		outcome.pending = true;
		return outcome;
	}
	if (admission.kind != PreparedTransactionStore::ADMISSION_CREATED) {
		switch (admission.kind) {
			case PreparedTransactionStore::ADMISSION_IDEMPOTENCY_CONFLICT:
				return start_error("idempotency_conflict", "The idempotency key is already bound to a different request.");
			case PreparedTransactionStore::ADMISSION_TRANSACTION_CONFLICTED:
				return start_error("transaction_conflicted", "The prepared transaction was invalidated by an editor change.", true);
			case PreparedTransactionStore::ADMISSION_TRANSACTION_EXPIRED:
				return start_error("transaction_expired", "The prepared transaction has expired.");
			case PreparedTransactionStore::ADMISSION_BUSY:
				return start_error("transaction_busy", "The bounded prepared transaction store is full.", true);
			default:
				return start_error("transaction_busy", "A secure transaction identifier could not be allocated.", true);
		}
	}
	PendingJob job;
	job.transaction_id = admission.transaction_id;
	job.resolver_job = resolver_job;
	job.waiters.push_back(p_request_id);
	pending_jobs.push_back(job);
	StartOutcome outcome;
	outcome.pending = true;
	return outcome;
}

TransactionCoordinator::StartOutcome TransactionCoordinator::apply(uint64_t p_request_id, const Dictionary &p_params, uint64_t p_now_usec, uint64_t p_now_ms) {
	if (!revision_clock || !approval_verifier.is_available()) {
		return start_error("approval_required", "The transaction approval workflow is unavailable.");
	}
	Dictionary params = p_params.duplicate(true);
	params.erase("_protocol_version");
	if (!BridgeTransactionProfile::validate_apply_params(params)) {
		return start_error("invalid_request", "The transaction apply parameters are invalid.");
	}
	_expire_prepared(p_now_usec, p_now_ms);
	const String transaction_id = params["transaction_id"];
	PreparedTransactionStore::Record record;
	if (!prepared_store.get_record(transaction_id, record)) {
		return start_error("transaction_not_found", "The transaction was not found in this editor session.");
	}
	if (record.apply_terminal_latched) {
		return _latched_apply_outcome(record);
	}
	if (record.state == PreparedTransactionStore::STATE_EXPIRED || record.terminal_error == "transaction_expired") {
		return start_error("transaction_expired", "The prepared transaction has expired.");
	}
	if (record.state == PreparedTransactionStore::STATE_CONFLICTED || record.terminal_error == "transaction_conflicted") {
		return start_error("transaction_conflicted", "The prepared transaction was invalidated by an editor change.", true);
	}
	if (String(params["preview_digest"]) != record.preview_digest) {
		return start_error("preview_mismatch", "The supplied preview digest does not match the immutable preview.");
	}
	if ((uint64_t)(int64_t)params["expected_scene_revision"] != record.binding.scene_revision) {
		return start_error("stale_scene_revision", "The expected scene revision does not match the prepared transaction.", true);
	}
	if ((uint64_t)(int64_t)params["expected_operation_seq"] != record.binding.operation_seq) {
		return start_error("stale_editor_state", "The expected operation sequence does not match the prepared transaction.", true);
	}

	TransactionApprovalVerifier::Binding approval_binding;
	approval_binding.project_id = record.binding.project_id;
	approval_binding.editor_session_id = record.binding.editor_session_id;
	approval_binding.scene_id = record.binding.scene_id;
	approval_binding.transaction_id = record.transaction_id;
	approval_binding.preview_digest = record.preview_digest;
	approval_binding.scope = record.scope;
	approval_binding.risk = record.risk;
	approval_binding.scene_revision = record.binding.scene_revision;
	approval_binding.operation_seq = record.binding.operation_seq;
	const TransactionApprovalVerifier::Outcome approval = approval_verifier.verify(params["approval"], approval_binding, p_now_ms);
	if (!approval.valid) {
		if (record.state == PreparedTransactionStore::STATE_PREVIEWED) {
			_transition(transaction_id, PreparedTransactionStore::STATE_AWAITING_APPROVAL, "approval_requested", p_now_ms);
		}
		return start_error(approval.error_code, approval.error_message);
	}

	if (record.apply_claimed || record.state == PreparedTransactionStore::STATE_APPLYING) {
		PendingApplyJob *job = _find_apply_job(transaction_id);
		if (!job || job->receipt_hash != approval.receipt.receipt_hash) {
			return start_error("transaction_busy", "A different apply attempt already owns this transaction.", true);
		}
		if (job->waiters.size() >= MAX_WAITERS_PER_TRANSACTION) {
			return start_error("transaction_busy", "The transaction apply attempt has too many waiters.", true);
		}
		job->waiters.push_back(p_request_id);
		StartOutcome outcome;
		outcome.pending = true;
		return outcome;
	}
	if (record.state != PreparedTransactionStore::STATE_PREVIEWED && record.state != PreparedTransactionStore::STATE_AWAITING_APPROVAL) {
		return start_error("transaction_conflicted", "The transaction is not eligible for apply.");
	}
	if (!_binding_is_current(record.binding)) {
		const PreparedTransactionStore::State stale_state = record.state == PreparedTransactionStore::STATE_PREVIEWED || record.state == PreparedTransactionStore::STATE_AWAITING_APPROVAL ? PreparedTransactionStore::STATE_CONFLICTED : PreparedTransactionStore::STATE_FAILED;
		_transition(transaction_id, stale_state, "revision_conflict", p_now_ms, String(), "stale_editor_state", "The editor state changed before apply.", true);
		return start_error("stale_editor_state", "The editor state changed before apply.", true);
	}
	if (record.state == PreparedTransactionStore::STATE_PREVIEWED) {
		_transition(transaction_id, PreparedTransactionStore::STATE_AWAITING_APPROVAL, "approval_requested", p_now_ms);
	}
	PreparedTransactionStore::Record *mutable_record = prepared_store.get_record_mutable(transaction_id);
	ERR_FAIL_NULL_V(mutable_record, start_error("transaction_not_found", "The transaction disappeared before apply."));
	mutable_record->apply_claimed = true;
	mutable_record->approval_receipt_hash = approval.receipt.receipt_hash;
	if (!_transition(transaction_id, PreparedTransactionStore::STATE_APPLYING, "apply_started", p_now_ms)) {
		mutable_record->apply_claimed = false;
		return start_error("transaction_conflicted", "The transaction could not enter the applying state.");
	}

	Dictionary operation;
	if (BridgeJson::parse_strict_object(record.canonical_operation_json.to_utf8_buffer(), operation) != OK) {
		PendingApplyJob failed_job;
		failed_job.transaction_id = transaction_id;
		failed_job.waiters.push_back(p_request_id);
		_finish_apply_failure(failed_job, "transaction_apply_failed", "The immutable operation could not be reopened.", false, p_now_ms);
		StartOutcome outcome;
		outcome.pending = true;
		return outcome;
	}
	PendingApplyJob job;
	job.transaction_id = transaction_id;
	job.receipt_hash = approval.receipt.receipt_hash;
	job.approval = approval.receipt;
	job.waiters.push_back(p_request_id);
	String error_code;
	String error_message;
	if (TransactionSceneResolver::begin(editor_session_id, record.binding.scene_id, record.binding.history_id, operation, job.resolver_job, error_code, error_message) != OK) {
		_finish_apply_failure(job, error_code, error_message, error_code == "transaction_conflicted" || error_code == "stale_editor_state", p_now_ms);
		StartOutcome outcome;
		outcome.pending = true;
		return outcome;
	}
	if (busy_native_histories.has(job.resolver_job.scene_evidence.native_history_id)) {
		_finish_apply_failure(job, "transaction_busy", "Another transaction is applying in this scene history.", true, p_now_ms);
		StartOutcome outcome;
		outcome.pending = true;
		return outcome;
	}
	busy_native_histories.insert(job.resolver_job.scene_evidence.native_history_id);
	pending_apply_jobs.push_back(job);
	StartOutcome outcome;
	outcome.pending = true;
	return outcome;
}

TransactionCoordinator::StartOutcome TransactionCoordinator::status(const Dictionary &p_params, uint64_t p_now_ms) {
	Dictionary params = p_params.duplicate(true);
	params.erase("_protocol_version");
	if (!BridgeTransactionProfile::validate_status_params(params)) {
		return start_error("invalid_request", "The transaction status parameters are invalid.");
	}
	PreparedTransactionStore::Record *record = prepared_store.get_record_mutable(params["transaction_id"]);
	if (!record) {
		return start_error("transaction_not_found", "The transaction was not found in this editor session.");
	}
	if (record->operation_kind.is_empty() || record->preview_digest.is_empty()) {
		return start_error("transaction_busy", "The transaction preview is not available yet.", true);
	}
	_reconcile(*record, p_now_ms);
	record = prepared_store.get_record_mutable(params["transaction_id"]);
	ERR_FAIL_NULL_V(record, start_error("transaction_not_found", "The transaction disappeared during status reconciliation."));
	const Dictionary result = _make_status(*record);
	if (!BridgeTransactionProfile::validate_status_result(result)) {
		return start_error("transaction_apply_failed", "The transaction status could not be serialized safely.", true);
	}
	StartOutcome outcome;
	outcome.has_result = true;
	outcome.result = result;
	return outcome;
}

TransactionCoordinator::StartOutcome TransactionCoordinator::undo(uint64_t p_request_id, const Dictionary &p_params, uint64_t p_now_ms) {
	(void)p_request_id;
	Dictionary params = p_params.duplicate(true);
	params.erase("_protocol_version");
	if (!BridgeTransactionProfile::validate_undo_params(params)) {
		return start_error("invalid_request", "The transaction undo parameters are invalid.");
	}
	const String transaction_id = params["transaction_id"];
	PreparedTransactionStore::Record *record = prepared_store.get_record_mutable(transaction_id);
	if (!record) {
		return start_error("transaction_not_found", "The transaction was not found in this editor session.");
	}
	_reconcile(*record, p_now_ms);
	record = prepared_store.get_record_mutable(transaction_id);
	ERR_FAIL_NULL_V(record, start_error("transaction_not_found", "The transaction disappeared during Undo reconciliation."));
	if (record->state == PreparedTransactionStore::STATE_UNDONE) {
		const Dictionary result = _make_status(*record);
		record->undo_terminal_latched = true;
		record->undo_terminal_result = result.duplicate(true);
		StartOutcome outcome;
		outcome.has_result = true;
		outcome.result = result;
		return outcome;
	}
	if (record->state != PreparedTransactionStore::STATE_COMMITTED) {
		return start_error("transaction_not_undoable", "Only a committed transaction can be undone.");
	}
	if ((uint64_t)(int64_t)params["expected_transaction_seq"] != record->transaction_seq) {
		return start_error("transaction_not_undoable", "The transaction sequence changed before Undo.", true);
	}
	const Dictionary revisions = revision_clock->get_revision_vector();
	const Dictionary scene_revisions = revisions["scene_revisions"];
	const uint64_t current_scene_revision = scene_revisions.has(record->binding.scene_id) ? (uint64_t)(int64_t)scene_revisions[record->binding.scene_id] : 0;
	const uint64_t current_operation_seq = (uint64_t)(int64_t)revisions["operation_seq"];
	if ((uint64_t)(int64_t)params["expected_scene_revision"] != current_scene_revision || (uint64_t)(int64_t)params["expected_operation_seq"] != current_operation_seq) {
		return start_error("transaction_not_undoable", "The editor revisions changed before Undo.", true);
	}
	int current_action = -1;
	uint64_t version = 0;
	if (!_native_action_is_correlated(*record, current_action, version) || current_action != record->history_action_committed) {
		return start_error("transaction_not_undoable", "The transaction action is not the newest action in its history.");
	}
	TransactionExecutor **executor_ptr = executors.getptr(record->operation_kind);
	TransactionExecutor::NativeActionPlan *plan = retained_native_plans.getptr(transaction_id);
	if (!executor_ptr || !*executor_ptr || !plan) {
		return start_error("transaction_not_undoable", "The transaction inverse is no longer retained.");
	}
	if (!(*executor_ptr)->verify_postcondition(*record, *plan)) {
		return start_error("transaction_not_undoable", "The transaction postcondition no longer matches the committed action.");
	}
	if (busy_native_histories.has(record->native_history_id)) {
		return start_error("transaction_busy", "The scene history is already executing another transaction.", true);
	}
	EditorUndoRedoManager *manager = EditorUndoRedoManager::get_singleton();
	if (!manager || !manager->has_history(record->native_history_id)) {
		return start_error("transaction_not_undoable", "The native Undo history is unavailable.");
	}
	busy_native_histories.insert(record->native_history_id);
	record->undo_claimed = true;
	const bool undone = manager->undo_history(record->native_history_id);
	record = prepared_store.get_record_mutable(transaction_id);
	const bool prestate = undone && record && (*executor_ptr)->verify_prestate_after_rollback(*record, *plan);
	if (!undone || !prestate) {
		if (record) {
			record->undo_claimed = false;
		}
		busy_native_histories.erase(plan->native_history_id);
		return start_error("transaction_undo_failed", "The native Undo result could not be proven.", true);
	}
	if (record->state == PreparedTransactionStore::STATE_COMMITTED) {
		_transition(transaction_id, PreparedTransactionStore::STATE_UNDONE, "native_undo_observed", p_now_ms, "undone");
	}
	record = prepared_store.get_record_mutable(transaction_id);
	if (!record) {
		busy_native_histories.erase(plan->native_history_id);
		return start_error("transaction_undo_failed", "The transaction disappeared after Undo.", true);
	}
	record->undo_claimed = false;
	busy_native_histories.erase(record->native_history_id);
	const Dictionary result = _make_status(*record);
	record->undo_terminal_latched = true;
	record->undo_terminal_result = result.duplicate(true);
	StartOutcome outcome;
	outcome.has_result = true;
	outcome.result = result;
	return outcome;
}

void TransactionCoordinator::_finish_apply_failure(const PendingApplyJob &p_job, const String &p_code, const String &p_message, bool p_retryable, uint64_t p_now_ms) {
	PreparedTransactionStore::Record *record = prepared_store.get_record_mutable(p_job.transaction_id);
	if (record) {
		if (record->state == PreparedTransactionStore::STATE_APPLYING) {
			_transition(p_job.transaction_id, PreparedTransactionStore::STATE_FAILED, "precommit_failed", p_now_ms, String(), p_code, p_message, p_retryable);
		}
		record = prepared_store.get_record_mutable(p_job.transaction_id);
		if (record) {
			_latch_apply_error(*record, p_code, p_message, p_retryable);
			if (record->native_history_id >= 0) {
				busy_native_histories.erase(record->native_history_id);
			}
		}
	}
	if (p_job.resolver_job.scene_evidence.native_history_id >= 0) {
		busy_native_histories.erase(p_job.resolver_job.scene_evidence.native_history_id);
	}
	_fail_apply_waiters(p_job, p_code, p_message, p_retryable);
}

bool TransactionCoordinator::_native_action_is_correlated(const PreparedTransactionStore::Record &p_record, int &r_current_action, uint64_t &r_version) const {
	r_current_action = -1;
	r_version = 0;
	if (p_record.native_history_id < 0 || p_record.history_action_committed < 0 || p_record.native_action_tag.is_empty()) {
		return false;
	}
	EditorUndoRedoManager *manager = EditorUndoRedoManager::get_singleton();
	if (!manager || !manager->has_history(p_record.native_history_id)) {
		return false;
	}
	UndoRedo *history = manager->get_history_undo_redo(p_record.native_history_id);
	if (!history || p_record.history_action_committed >= history->get_history_count() || history->get_action_name(p_record.history_action_committed) != p_record.native_action_tag) {
		return false;
	}
	r_current_action = history->get_current_action();
	r_version = history->get_version();
	return true;
}

void TransactionCoordinator::_prune_retained_native_plans() {
	Vector<String> stale_transaction_ids;
	for (const KeyValue<String, TransactionExecutor::NativeActionPlan> &entry : retained_native_plans) {
		PreparedTransactionStore::Record record;
		if (!prepared_store.get_record(entry.key, record)) {
			stale_transaction_ids.push_back(entry.key);
		}
	}
	for (const String &transaction_id : stale_transaction_ids) {
		retained_native_plans.erase(transaction_id);
	}
}

void TransactionCoordinator::_process_apply_job(PendingApplyJob &r_job, uint64_t p_now_ms, uint64_t p_budget_usec) {
#ifdef CODEX_BRIDGE_TESTS_ENABLED
	const TransactionFaultController::Action approval_fault = fault_controller.hit(r_job.transaction_id, "after_approval_before_preflight");
	if (approval_fault == TransactionFaultController::ACTION_WAIT) {
		pending_apply_jobs.push_back(r_job);
		return;
	}
	if (approval_fault == TransactionFaultController::ACTION_TERMINATE) {
		CRASH_NOW_MSG("Codex S9 test fault after approval and before final preflight.");
	}
	if (approval_fault == TransactionFaultController::ACTION_FAIL || approval_fault == TransactionFaultController::ACTION_DROP_RESPONSE) {
		_finish_apply_failure(r_job, "transaction_apply_failed", "The tests-only fault stopped apply before final preflight.", false, p_now_ms);
		return;
	}
#endif
	PreparedTransactionStore::Record *record = prepared_store.get_record_mutable(r_job.transaction_id);
	if (!record || record->state != PreparedTransactionStore::STATE_APPLYING) {
		_finish_apply_failure(r_job, "transaction_conflicted", "The transaction left the applying state before final preflight.", true, p_now_ms);
		return;
	}
	if (r_job.waiters.is_empty() && !record->native_commit_window_entered) {
		_finish_apply_failure(r_job, "approval_cancelled", "The apply request was cancelled before the native commit window.", false, p_now_ms);
		return;
	}
	if (!_binding_is_current(record->binding) || !TransactionSceneResolver::binding_is_current(r_job.resolver_job)) {
		_finish_apply_failure(r_job, "stale_editor_state", "The editor state changed during the final apply preflight.", true, p_now_ms);
		return;
	}
	const TransactionSceneResolver::ProcessOutcome resolver = TransactionSceneResolver::process(r_job.resolver_job, TransactionSceneResolver::MAX_NODES_PER_SLICE, MIN(p_budget_usec, TransactionSceneResolver::MAX_SLICE_USEC));
	if (resolver.failed) {
		_finish_apply_failure(r_job, resolver.error_code, resolver.error_message, resolver.error_code == "transaction_conflicted", p_now_ms);
		return;
	}
	if (!resolver.complete) {
		pending_apply_jobs.push_back(r_job);
		return;
	}
	record = prepared_store.get_record_mutable(r_job.transaction_id);
	if (!record || !_binding_is_current(record->binding) || !TransactionSceneResolver::binding_is_current(r_job.resolver_job)) {
		_finish_apply_failure(r_job, "stale_editor_state", "The editor state changed immediately before native action construction.", true, p_now_ms);
		return;
	}
	TransactionExecutor **executor_ptr = executors.getptr(record->operation_kind);
	if (!executor_ptr || !*executor_ptr) {
		_finish_apply_failure(r_job, "scene_operation_unsupported", "No production executor is registered for this transaction operation.", false, p_now_ms);
		return;
	}
	TransactionExecutor *executor = *executor_ptr;
	TransactionExecutor::NativeActionPlan plan;
	String error_code;
	String error_message;
	if (executor->final_preflight(*record, r_job.resolver_job, resolver.resolution, plan, error_code, error_message) != OK) {
		_finish_apply_failure(r_job, error_code.is_empty() ? "transaction_apply_failed" : error_code, error_message.is_empty() ? "The transaction executor rejected the final preflight." : error_message, false, p_now_ms);
		return;
	}
	if (plan.native_history_id != r_job.resolver_job.scene_evidence.native_history_id) {
		_finish_apply_failure(r_job, "stale_editor_state", "The executor selected a different native history.", false, p_now_ms);
		return;
	}
	EditorUndoRedoManager *manager = EditorUndoRedoManager::get_singleton();
	UndoRedo *history = manager && manager->has_history(plan.native_history_id) ? manager->get_history_undo_redo(plan.native_history_id) : nullptr;
	if (!history || history->get_version() != r_job.resolver_job.scene_evidence.history_version || history->get_current_action() != r_job.resolver_job.scene_evidence.history_action_position) {
		_finish_apply_failure(r_job, "stale_editor_state", "The native history changed in the final preflight frame.", true, p_now_ms);
		return;
	}
#ifdef CODEX_BRIDGE_TESTS_ENABLED
	const TransactionFaultController::Action before_commit_fault = fault_controller.hit(r_job.transaction_id, "before_commit");
	if (before_commit_fault == TransactionFaultController::ACTION_WAIT) {
		pending_apply_jobs.push_back(r_job);
		return;
	}
	if (before_commit_fault == TransactionFaultController::ACTION_TERMINATE) {
		CRASH_NOW_MSG("Codex S9 test fault immediately before native commit.");
	}
	if (before_commit_fault == TransactionFaultController::ACTION_FAIL || before_commit_fault == TransactionFaultController::ACTION_DROP_RESPONSE) {
		_finish_apply_failure(r_job, "transaction_apply_failed", "The tests-only fault stopped apply before native action creation.", false, p_now_ms);
		return;
	}
#endif
	if (approval_verifier.consume(r_job.approval, p_now_ms, error_code, error_message) != OK) {
		_finish_apply_failure(r_job, error_code, error_message, error_code == "transaction_busy", p_now_ms);
		return;
	}
	record = prepared_store.get_record_mutable(r_job.transaction_id);
	if (!record) {
		_finish_apply_failure(r_job, "transaction_conflicted", "The transaction disappeared before the native commit window.", true, p_now_ms);
		return;
	}
	record->native_history_id = plan.native_history_id;
	record->history_version_before = history->get_version();
	record->history_action_before = history->get_current_action();
	record->history_count_before = history->get_history_count();
	record->native_action_tag = vformat("Codex: %s [%s]", record->operation_kind, record->transaction_id);
	plan.action_name = record->native_action_tag;
	retained_native_plans.insert(record->transaction_id, plan);
	record->native_commit_window_entered = true;

	manager->create_action_for_history(plan.action_name, plan.native_history_id, UndoRedo::MERGE_DISABLE, false, true);
#ifdef CODEX_BRIDGE_TESTS_ENABLED
	const TransactionFaultController::Action action_created_fault = fault_controller.hit(r_job.transaction_id, "after_action_created_before_commit");
	if (action_created_fault != TransactionFaultController::ACTION_NO_MATCH && action_created_fault != TransactionFaultController::ACTION_CONTINUE) {
		CRASH_NOW_MSG("Codex S9 test fault after native action creation and before commit.");
	}
#endif
	const Error registration_error = executor->register_native_action(plan, manager);
	if (registration_error != OK) {
		record->commit_point_entered = true;
		manager->commit_action(false);
		_transition(record->transaction_id, PreparedTransactionStore::STATE_IN_DOUBT, "reconciliation_required", p_now_ms, "unknown", "transaction_in_doubt", "Native action registration entered an uncertain state.", false);
		record = prepared_store.get_record_mutable(r_job.transaction_id);
		if (record) {
			const Dictionary result = _make_status(*record);
			_latch_apply_result(*record, result);
			_complete_apply_waiters(r_job, result);
		}
		busy_native_histories.erase(plan.native_history_id);
		return;
	}
	record->commit_point_entered = true;
	manager->commit_action(true);

#ifdef CODEX_BRIDGE_TESTS_ENABLED
	const TransactionFaultController::Action committed_fault = fault_controller.hit(r_job.transaction_id, "after_commit_before_response");
	if (committed_fault == TransactionFaultController::ACTION_TERMINATE) {
		CRASH_NOW_MSG("Codex S9 test fault after native commit and before Bridge response.");
	}
	if (committed_fault == TransactionFaultController::ACTION_DROP_RESPONSE || committed_fault == TransactionFaultController::ACTION_WAIT || committed_fault == TransactionFaultController::ACTION_FAIL) {
		r_job.fault_drop_response = true;
	}
#endif

	record = prepared_store.get_record_mutable(r_job.transaction_id);
	if (!record) {
		busy_native_histories.erase(plan.native_history_id);
		_fail_apply_waiters(r_job, "transaction_in_doubt", "The transaction record disappeared after native commit.", false);
		return;
	}
	history = manager->get_history_undo_redo(plan.native_history_id);
	const int expected_action = record->history_action_before + 1;
	const bool correlated = history && history->get_current_action() == expected_action && history->get_history_count() == expected_action + 1 && history->get_action_name(expected_action) == record->native_action_tag;
	if (!correlated) {
		_transition(record->transaction_id, PreparedTransactionStore::STATE_IN_DOUBT, "reconciliation_required", p_now_ms, "unknown", "transaction_in_doubt", "The native commit result could not be correlated exactly.", false);
		record = prepared_store.get_record_mutable(r_job.transaction_id);
		if (record) {
			const Dictionary result = _make_status(*record);
			_latch_apply_result(*record, result);
			_complete_apply_waiters(r_job, result);
		}
		busy_native_histories.erase(plan.native_history_id);
		return;
	}
	record->native_commit_observed = true;
	record->history_action_committed = expected_action;
	record->history_count_committed = history->get_history_count();
	record->history_version_committed = history->get_version();
	_transition(record->transaction_id, PreparedTransactionStore::STATE_APPLIED, "native_action_committed", p_now_ms, "applied");
	_transition(record->transaction_id, PreparedTransactionStore::STATE_VALIDATING, "validation_started", p_now_ms, "applied");
	record = prepared_store.get_record_mutable(r_job.transaction_id);
	if (!record) {
		busy_native_histories.erase(plan.native_history_id);
		_fail_apply_waiters(r_job, "transaction_in_doubt", "The transaction record disappeared before postcondition validation.", false);
		return;
	}
	if (executor->verify_postcondition(*record, plan) && _capture_committed_entities(*record, executor, plan)) {
		_transition(record->transaction_id, PreparedTransactionStore::STATE_COMMITTED, "validation_succeeded", p_now_ms, "committed");
	} else {
		int current_action = -1;
		uint64_t version = 0;
		const bool exact_top = _native_action_is_correlated(*record, current_action, version) && current_action == record->history_action_committed;
		const bool rolled_back = exact_top && manager->undo_history(record->native_history_id);
		record = prepared_store.get_record_mutable(r_job.transaction_id);
		if (rolled_back && record && executor->verify_prestate_after_rollback(*record, plan)) {
			_transition(record->transaction_id, PreparedTransactionStore::STATE_FAILED_ROLLED_BACK, "postcondition_failed_rolled_back", p_now_ms, "rolled_back", "transaction_apply_failed", "The intrinsic postcondition failed and the exact action was rolled back.", false);
		} else if (record) {
			_transition(record->transaction_id, PreparedTransactionStore::STATE_IN_DOUBT, "reconciliation_required", p_now_ms, "unknown", "transaction_in_doubt", "The intrinsic postcondition and rollback result could not both be proven.", false);
		}
	}
	record = prepared_store.get_record_mutable(r_job.transaction_id);
	if (record) {
		const Dictionary result = _make_status(*record);
		_latch_apply_result(*record, result);
#ifdef CODEX_BRIDGE_TESTS_ENABLED
		if (!r_job.fault_drop_response) {
			_complete_apply_waiters(r_job, result);
		}
#else
		_complete_apply_waiters(r_job, result);
#endif
	}
	busy_native_histories.erase(plan.native_history_id);
}

void TransactionCoordinator::_reconcile(PreparedTransactionStore::Record &r_record, uint64_t p_now_ms) {
	if (r_record.native_history_id < 0 || r_record.history_action_committed < 0) {
		return;
	}
	int current_action = -1;
	uint64_t version = 0;
	if (!_native_action_is_correlated(r_record, current_action, version)) {
		return;
	}
	TransactionExecutor **executor_ptr = executors.getptr(r_record.operation_kind);
	TransactionExecutor::NativeActionPlan *plan = retained_native_plans.getptr(r_record.transaction_id);
	if (r_record.state == PreparedTransactionStore::STATE_COMMITTED && current_action < r_record.history_action_committed) {
		if (executor_ptr && *executor_ptr && plan && (*executor_ptr)->verify_prestate_after_rollback(r_record, *plan)) {
			_transition(r_record.transaction_id, PreparedTransactionStore::STATE_UNDONE, "native_undo_observed", p_now_ms, "undone");
		}
		return;
	}
	if (r_record.state == PreparedTransactionStore::STATE_UNDONE && current_action >= r_record.history_action_committed) {
		if (executor_ptr && *executor_ptr && plan && (*executor_ptr)->verify_postcondition(r_record, *plan) && (!r_record.committed_entities.is_empty() || _capture_committed_entities(r_record, *executor_ptr, *plan))) {
			_transition(r_record.transaction_id, PreparedTransactionStore::STATE_COMMITTED, "native_redo_observed", p_now_ms, "committed");
		}
		return;
	}
	if (r_record.state != PreparedTransactionStore::STATE_IN_DOUBT) {
		return;
	}
	if (!executor_ptr || !*executor_ptr || !plan) {
		return;
	}
	if (current_action >= r_record.history_action_committed && (*executor_ptr)->verify_postcondition(r_record, *plan) && _capture_committed_entities(r_record, *executor_ptr, *plan)) {
		_transition(r_record.transaction_id, PreparedTransactionStore::STATE_COMMITTED, "reconciliation_committed", p_now_ms, "committed");
	} else if (current_action < r_record.history_action_committed && (*executor_ptr)->verify_prestate_after_rollback(r_record, *plan)) {
		_transition(r_record.transaction_id, PreparedTransactionStore::STATE_UNDONE, "reconciliation_undone", p_now_ms, "undone");
	}
}

void TransactionCoordinator::cancel_waiter(uint64_t p_request_id) {
	for (PendingJob &job : pending_jobs) {
		for (int index = job.waiters.size() - 1; index >= 0; index--) {
			if (job.waiters[index] == p_request_id) {
				job.waiters.remove_at(index);
			}
		}
	}
	for (PendingApplyJob &job : pending_apply_jobs) {
		for (int index = job.waiters.size() - 1; index >= 0; index--) {
			if (job.waiters[index] == p_request_id) {
				job.waiters.remove_at(index);
			}
		}
	}
}

void TransactionCoordinator::process(uint64_t p_now_usec, uint64_t p_now_ms, uint64_t p_budget_usec, Vector<Completion> &r_completions) {
	_expire_prepared(p_now_usec, p_now_ms);
	_prune_retained_native_plans();
	for (List<PendingJob>::Element *element = pending_jobs.front(); element;) {
		List<PendingJob>::Element *next = element->next();
		PreparedTransactionStore::Record record;
		const bool has_record = prepared_store.get_record(element->get().transaction_id, record);
		bool valid_pending_state = has_record && record.state == PreparedTransactionStore::STATE_PREPARING;
#ifdef CODEX_BRIDGE_TESTS_ENABLED
		valid_pending_state = valid_pending_state || (has_record && element->get().fault_preview_ready && record.state == PreparedTransactionStore::STATE_PREVIEWED);
#endif
		if (!valid_pending_state) {
			_fail_waiters(element->get(), "transaction_expired", "The prepared transaction expired before preflight completed.", false);
			pending_jobs.erase(element);
		}
		element = next;
	}
	if (!pending_apply_jobs.is_empty()) {
		PendingApplyJob job = pending_apply_jobs.front()->get();
		pending_apply_jobs.pop_front();
		_process_apply_job(job, p_now_ms, p_budget_usec);
	} else if (!pending_jobs.is_empty()) {
		PendingJob job = pending_jobs.front()->get();
		pending_jobs.pop_front();
#ifdef CODEX_BRIDGE_TESTS_ENABLED
		if (job.fault_preview_ready) {
			const TransactionFaultController::Action fault = fault_controller.hit(job.transaction_id, "after_prepare");
			if (fault == TransactionFaultController::ACTION_WAIT) {
				pending_jobs.push_back(job);
			} else if (fault == TransactionFaultController::ACTION_TERMINATE) {
				CRASH_NOW_MSG("Codex S9 test fault after prepare.");
			} else if (fault == TransactionFaultController::ACTION_FAIL) {
				_fail_waiters(job, "transaction_apply_failed", "The tests-only fault stopped the prepare response.", false);
			} else if (fault != TransactionFaultController::ACTION_DROP_RESPONSE) {
				_complete_waiters(job, job.fault_preview_result);
			}
		} else
#endif
		{
		PreparedTransactionStore::Record record;
		if (!prepared_store.get_record(job.transaction_id, record) || !_binding_is_current(record.binding) || !TransactionSceneResolver::binding_is_current(job.resolver_job)) {
			prepared_store.conflict(job.transaction_id);
			_fail_waiters(job, "transaction_conflicted", "The editor state changed while the transaction was being prepared.", true);
		} else {
			const TransactionSceneResolver::ProcessOutcome resolver = TransactionSceneResolver::process(job.resolver_job, TransactionSceneResolver::MAX_NODES_PER_SLICE, MIN(p_budget_usec, TransactionSceneResolver::MAX_SLICE_USEC));
			if (resolver.failed) {
				prepared_store.conflict(job.transaction_id);
				_fail_waiters(job, resolver.error_code, resolver.error_message, resolver.error_code == "transaction_conflicted");
			} else if (!resolver.complete) {
				pending_jobs.push_back(job);
			} else if (!prepared_store.get_record(job.transaction_id, record) || !_binding_is_current(record.binding) || !TransactionSceneResolver::binding_is_current(job.resolver_job)) {
				prepared_store.conflict(job.transaction_id);
				_fail_waiters(job, "transaction_conflicted", "The editor state changed before preview publication.", true);
			} else {
				TransactionPreviewBuilder::Output preview;
				const Error error = TransactionPreviewBuilder::build(job.transaction_id, record.binding, job.resolver_job.operation, resolver.resolution, record.created_at_ms, record.expires_at_ms, preview);
				if (error != OK || prepared_store.publish_preview(job.transaction_id, preview.result_json, preview.preview_payload_json, preview.preview_digest, resolver.resolution.precondition_digest) != OK) {
					prepared_store.conflict(job.transaction_id);
					_fail_waiters(job, "transaction_too_large", "The immutable transaction preview exceeds the bounded limit.", false);
				} else {
					PreparedTransactionStore::Record previewed_record;
					if (prepared_store.get_record(job.transaction_id, previewed_record)) {
						_queue_event(previewed_record, "preparing", "prepared", p_now_ms);
					}
					Dictionary stored_result;
					if (prepared_store.get_prepare_result(job.transaction_id, stored_result) != OK) {
						prepared_store.conflict(job.transaction_id);
						_fail_waiters(job, "transaction_conflicted", "The immutable transaction preview could not be reopened.", true);
					} else {
#ifdef CODEX_BRIDGE_TESTS_ENABLED
						const TransactionFaultController::Action fault = fault_controller.hit(job.transaction_id, "after_prepare");
						if (fault == TransactionFaultController::ACTION_WAIT) {
							job.fault_preview_ready = true;
							job.fault_preview_result = stored_result.duplicate(true);
							pending_jobs.push_back(job);
						} else if (fault == TransactionFaultController::ACTION_TERMINATE) {
							CRASH_NOW_MSG("Codex S9 test fault after prepare.");
						} else if (fault == TransactionFaultController::ACTION_FAIL) {
							_fail_waiters(job, "transaction_apply_failed", "The tests-only fault stopped the prepare response.", false);
						} else if (fault != TransactionFaultController::ACTION_DROP_RESPONSE) {
							_complete_waiters(job, stored_result);
						}
#else
						_complete_waiters(job, stored_result);
#endif
					}
				}
			}
		}
		}
	}
	for (const Completion &completion : queued_completions) {
		r_completions.push_back(completion);
	}
	queued_completions.clear();
	_prune_retained_native_plans();
}

void TransactionCoordinator::drain_events(Vector<Dictionary> &r_events) {
	for (const Dictionary &event : queued_events) {
		r_events.push_back(event.duplicate(true));
	}
	queued_events.clear();
}

bool TransactionCoordinator::observe_native_history(int p_native_history_id, int p_action_count, int p_current_action, uint64_t p_version, uint64_t p_now_ms) {
	(void)p_action_count;
	(void)p_version;
	bool owned_transition = false;
	Vector<PreparedTransactionStore::Record> records;
	prepared_store.get_records(records);
	EditorUndoRedoManager *manager = EditorUndoRedoManager::get_singleton();
	UndoRedo *history = manager && manager->has_history(p_native_history_id) ? manager->get_history_undo_redo(p_native_history_id) : nullptr;
	for (const PreparedTransactionStore::Record &snapshot : records) {
		if (snapshot.native_history_id != p_native_history_id || snapshot.native_action_tag.is_empty()) {
			continue;
		}
		PreparedTransactionStore::Record *record = prepared_store.get_record_mutable(snapshot.transaction_id);
		if (!record) {
			continue;
		}
		if (record->state == PreparedTransactionStore::STATE_APPLYING && record->native_commit_window_entered && history && p_current_action == record->history_action_before + 1 && p_current_action < history->get_history_count() && history->get_action_name(p_current_action) == record->native_action_tag) {
			record->native_commit_observed = true;
			owned_transition = true;
			continue;
		}
		if (record->history_action_committed < 0 || !history || record->history_action_committed >= history->get_history_count() || history->get_action_name(record->history_action_committed) != record->native_action_tag) {
			continue;
		}
		if (record->state == PreparedTransactionStore::STATE_COMMITTED && p_current_action < record->history_action_committed) {
			if (record->undo_claimed) {
				owned_transition = true;
				continue;
			}
			TransactionExecutor **executor_ptr = executors.getptr(record->operation_kind);
			TransactionExecutor::NativeActionPlan *plan = retained_native_plans.getptr(record->transaction_id);
			if (executor_ptr && *executor_ptr && plan && (*executor_ptr)->verify_prestate_after_rollback(*record, *plan)) {
				_transition(record->transaction_id, PreparedTransactionStore::STATE_UNDONE, "native_undo_observed", p_now_ms, "undone");
			}
		} else if (record->state == PreparedTransactionStore::STATE_UNDONE && p_current_action >= record->history_action_committed) {
			TransactionExecutor **executor_ptr = executors.getptr(record->operation_kind);
			TransactionExecutor::NativeActionPlan *plan = retained_native_plans.getptr(record->transaction_id);
			if (executor_ptr && *executor_ptr && plan && (*executor_ptr)->verify_postcondition(*record, *plan) && (!record->committed_entities.is_empty() || _capture_committed_entities(*record, *executor_ptr, *plan))) {
				_transition(record->transaction_id, PreparedTransactionStore::STATE_COMMITTED, "native_redo_observed", p_now_ms, "committed");
			}
		}
	}
	return owned_transition;
}

void TransactionCoordinator::register_executor(const String &p_operation_kind, TransactionExecutor *p_executor) {
	ERR_FAIL_COND(p_operation_kind.is_empty());
	ERR_FAIL_NULL(p_executor);
	executors.insert(p_operation_kind, p_executor);
}

void TransactionCoordinator::unregister_executor(const String &p_operation_kind, TransactionExecutor *p_executor) {
	TransactionExecutor **registered = executors.getptr(p_operation_kind);
	if (registered && *registered == p_executor) {
		executors.erase(p_operation_kind);
	}
}

void TransactionCoordinator::invalidate_scene(const String &p_scene_id) {
	_invalidate_jobs(p_scene_id);
}

void TransactionCoordinator::invalidate_all() {
	_invalidate_jobs();
}

bool TransactionCoordinator::has_pending_work(uint64_t p_now_usec) const {
	return !pending_jobs.is_empty() || !pending_apply_jobs.is_empty() || !queued_completions.is_empty() || !queued_events.is_empty() || p_now_usec >= prepared_store.get_next_expiry_deadline_usec();
}

uint32_t TransactionCoordinator::get_active_count() const {
	return prepared_store.get_active_count();
}

bool TransactionCoordinator::is_approval_available() const {
	return approval_verifier.is_available();
}

bool TransactionCoordinator::is_busy() const {
	return !busy_native_histories.is_empty();
}
