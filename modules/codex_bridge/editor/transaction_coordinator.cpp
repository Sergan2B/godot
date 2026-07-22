/**************************************************************************/
/*  transaction_coordinator.cpp                                         */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "transaction_coordinator.h"

#include "core/io/json.h"

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

void TransactionCoordinator::_invalidate_jobs(const String &p_scene_id) {
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
	if (p_scene_id.is_empty()) {
		prepared_store.conflict_all();
	} else {
		prepared_store.conflict_scene(p_scene_id);
	}
}

void TransactionCoordinator::initialize(const String &p_project_id, const String &p_editor_session_id, BridgeRevisionClock *p_revision_clock) {
	shutdown();
	project_id = p_project_id;
	editor_session_id = p_editor_session_id;
	revision_clock = p_revision_clock;
}

void TransactionCoordinator::shutdown() {
	project_id.clear();
	editor_session_id.clear();
	revision_clock = nullptr;
	prepared_store.clear();
	pending_jobs.clear();
	queued_completions.clear();
}

TransactionCoordinator::StartOutcome TransactionCoordinator::prepare(uint64_t p_request_id, const Dictionary &p_params, uint64_t p_now_usec, uint64_t p_now_ms) {
	if (!revision_clock) {
		return start_error("internal_error", "The transaction coordinator is not initialized.", true);
	}
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

void TransactionCoordinator::cancel_waiter(uint64_t p_request_id) {
	for (PendingJob &job : pending_jobs) {
		for (int index = job.waiters.size() - 1; index >= 0; index--) {
			if (job.waiters[index] == p_request_id) {
				job.waiters.remove_at(index);
			}
		}
	}
}

void TransactionCoordinator::process(uint64_t p_now_usec, uint64_t p_now_ms, uint64_t p_budget_usec, Vector<Completion> &r_completions) {
	prepared_store.expire(p_now_usec);
	for (List<PendingJob>::Element *element = pending_jobs.front(); element;) {
		List<PendingJob>::Element *next = element->next();
		PreparedTransactionStore::Record record;
		if (!prepared_store.get_record(element->get().transaction_id, record) || record.state != PreparedTransactionStore::STATE_PREPARING) {
			_fail_waiters(element->get(), "transaction_expired", "The prepared transaction expired before preflight completed.", false);
			pending_jobs.erase(element);
		}
		element = next;
	}
	if (!pending_jobs.is_empty()) {
		PendingJob job = pending_jobs.front()->get();
		pending_jobs.pop_front();
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
				if (error != OK || prepared_store.publish_preview(job.transaction_id, preview.result_json, preview.preview_payload_json, preview.preview_digest) != OK) {
					prepared_store.conflict(job.transaction_id);
					_fail_waiters(job, "transaction_too_large", "The immutable transaction preview exceeds the bounded limit.", false);
				} else {
					Dictionary stored_result;
					if (prepared_store.get_prepare_result(job.transaction_id, stored_result) != OK) {
						prepared_store.conflict(job.transaction_id);
						_fail_waiters(job, "transaction_conflicted", "The immutable transaction preview could not be reopened.", true);
					} else {
						_complete_waiters(job, stored_result);
					}
				}
			}
		}
	}
	for (const Completion &completion : queued_completions) {
		r_completions.push_back(completion);
	}
	queued_completions.clear();
	(void)p_now_ms;
}

void TransactionCoordinator::invalidate_scene(const String &p_scene_id) {
	_invalidate_jobs(p_scene_id);
}

void TransactionCoordinator::invalidate_all() {
	_invalidate_jobs();
}

bool TransactionCoordinator::has_pending_work(uint64_t p_now_usec) const {
	return !pending_jobs.is_empty() || !queued_completions.is_empty() || p_now_usec >= prepared_store.get_next_expiry_deadline_usec();
}

uint32_t TransactionCoordinator::get_active_count() const {
	return prepared_store.get_active_count();
}
