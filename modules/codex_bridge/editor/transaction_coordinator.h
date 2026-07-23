/**************************************************************************/
/*  transaction_coordinator.h                                           */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#pragma once

#include "bridge_revision_clock.h"
#include "prepared_transaction_store.h"
#include "transaction_approval_verifier.h"
#include "transaction_executor.h"
#include "transaction_scene_resolver.h"

#ifdef CODEX_BRIDGE_TESTS_ENABLED
#include "transaction_fault_controller.h"
#endif

#include "core/templates/hash_set.h"
#include "core/templates/list.h"

class TransactionCoordinator {
public:
	struct StartOutcome {
		bool pending = false;
		bool has_result = false;
		Dictionary result;
		String error_code;
		String error_message;
		bool retryable = false;
	};

	struct Completion {
		uint64_t request_id = 0;
		bool has_result = false;
		Dictionary result;
		String error_code;
		String error_message;
		bool retryable = false;
	};

	static constexpr uint64_t WORK_BUDGET_USEC = 600;
	static constexpr uint32_t MAX_WAITERS_PER_TRANSACTION = 64;

private:
	struct PendingJob {
		String transaction_id;
		TransactionSceneResolver::Job resolver_job;
		Vector<uint64_t> waiters;
#ifdef CODEX_BRIDGE_TESTS_ENABLED
		bool fault_preview_ready = false;
		Dictionary fault_preview_result;
#endif
	};

	struct PendingApplyJob {
		String transaction_id;
		String receipt_hash;
		TransactionApprovalVerifier::VerifiedReceipt approval;
		TransactionSceneResolver::Job resolver_job;
		Vector<uint64_t> waiters;
#ifdef CODEX_BRIDGE_TESTS_ENABLED
		bool fault_drop_response = false;
#endif
	};

	String project_id;
	String editor_session_id;
	BridgeRevisionClock *revision_clock = nullptr;
	PreparedTransactionStore prepared_store;
	TransactionApprovalVerifier approval_verifier;
	List<PendingJob> pending_jobs;
	List<PendingApplyJob> pending_apply_jobs;
	Vector<Completion> queued_completions;
	Vector<Dictionary> queued_events;
	HashMap<String, TransactionExecutor *> executors;
	HashMap<String, TransactionExecutor::NativeActionPlan> retained_native_plans;
	HashSet<int> busy_native_histories;
#ifdef CODEX_BRIDGE_TESTS_ENABLED
	TransactionFaultController fault_controller;
#endif

	bool _coordinates_are_current(const Dictionary &p_coordinates, String &r_error_code, String &r_error_message) const;
	bool _binding_is_current(const PreparedTransactionStore::Binding &p_binding) const;
	PendingJob *_find_job(const String &p_transaction_id);
	PendingApplyJob *_find_apply_job(const String &p_transaction_id);
	void _complete_waiters(const PendingJob &p_job, const Dictionary &p_result);
	void _fail_waiters(const PendingJob &p_job, const String &p_code, const String &p_message, bool p_retryable);
	void _complete_apply_waiters(const PendingApplyJob &p_job, const Dictionary &p_result);
	void _fail_apply_waiters(const PendingApplyJob &p_job, const String &p_code, const String &p_message, bool p_retryable);
	void _invalidate_jobs(const String &p_scene_id = String());
	void _expire_prepared(uint64_t p_now_usec, uint64_t p_now_ms);
	bool _transition(const String &p_transaction_id, PreparedTransactionStore::State p_state, const String &p_reason, uint64_t p_now_ms, const String &p_outcome = String(), const String &p_error_code = String(), const String &p_error_message = String(), bool p_error_retryable = false);
	void _queue_event(const PreparedTransactionStore::Record &p_record, const Variant &p_previous_state, const String &p_reason, uint64_t p_now_ms);
	bool _capture_committed_entities(PreparedTransactionStore::Record &r_record, TransactionExecutor *p_executor, const TransactionExecutor::NativeActionPlan &p_plan);
	Dictionary _make_status(const PreparedTransactionStore::Record &p_record) const;
	StartOutcome _latched_apply_outcome(const PreparedTransactionStore::Record &p_record) const;
	void _latch_apply_result(PreparedTransactionStore::Record &r_record, const Dictionary &p_result);
	void _latch_apply_error(PreparedTransactionStore::Record &r_record, const String &p_code, const String &p_message, bool p_retryable);
	void _finish_apply_failure(const PendingApplyJob &p_job, const String &p_code, const String &p_message, bool p_retryable, uint64_t p_now_ms);
	void _process_apply_job(PendingApplyJob &r_job, uint64_t p_now_ms, uint64_t p_budget_usec);
	void _prune_retained_native_plans();
	bool _native_action_is_correlated(const PreparedTransactionStore::Record &p_record, int &r_current_action, uint64_t &r_version) const;
	void _reconcile(PreparedTransactionStore::Record &r_record, uint64_t p_now_ms);

public:
	void initialize(const String &p_project_id, const String &p_editor_session_id, BridgeRevisionClock *p_revision_clock, const PackedByteArray &p_approval_key = PackedByteArray());
	void shutdown();

	StartOutcome prepare(uint64_t p_request_id, const Dictionary &p_params, uint64_t p_now_usec, uint64_t p_now_ms);
	StartOutcome apply(uint64_t p_request_id, const Dictionary &p_params, uint64_t p_now_usec, uint64_t p_now_ms);
	StartOutcome status(const Dictionary &p_params, uint64_t p_now_ms);
	StartOutcome undo(uint64_t p_request_id, const Dictionary &p_params, uint64_t p_now_ms);
	void cancel_waiter(uint64_t p_request_id);
	void process(uint64_t p_now_usec, uint64_t p_now_ms, uint64_t p_budget_usec, Vector<Completion> &r_completions);
	void drain_events(Vector<Dictionary> &r_events);
	bool observe_native_history(int p_native_history_id, int p_action_count, int p_current_action, uint64_t p_version, uint64_t p_now_ms);
	void register_executor(const String &p_operation_kind, TransactionExecutor *p_executor);
	void unregister_executor(const String &p_operation_kind, TransactionExecutor *p_executor);

	void invalidate_scene(const String &p_scene_id);
	void invalidate_all();
	bool has_pending_work(uint64_t p_now_usec) const;
	uint32_t get_active_count() const;
	bool is_approval_available() const;
	bool is_busy() const;
};
