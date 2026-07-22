/**************************************************************************/
/*  transaction_coordinator.h                                           */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#pragma once

#include "bridge_revision_clock.h"
#include "prepared_transaction_store.h"
#include "transaction_scene_resolver.h"

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
	};

	String project_id;
	String editor_session_id;
	BridgeRevisionClock *revision_clock = nullptr;
	PreparedTransactionStore prepared_store;
	List<PendingJob> pending_jobs;
	Vector<Completion> queued_completions;

	bool _coordinates_are_current(const Dictionary &p_coordinates, String &r_error_code, String &r_error_message) const;
	bool _binding_is_current(const PreparedTransactionStore::Binding &p_binding) const;
	PendingJob *_find_job(const String &p_transaction_id);
	void _complete_waiters(const PendingJob &p_job, const Dictionary &p_result);
	void _fail_waiters(const PendingJob &p_job, const String &p_code, const String &p_message, bool p_retryable);
	void _invalidate_jobs(const String &p_scene_id = String());

public:
	void initialize(const String &p_project_id, const String &p_editor_session_id, BridgeRevisionClock *p_revision_clock);
	void shutdown();

	StartOutcome prepare(uint64_t p_request_id, const Dictionary &p_params, uint64_t p_now_usec, uint64_t p_now_ms);
	void cancel_waiter(uint64_t p_request_id);
	void process(uint64_t p_now_usec, uint64_t p_now_ms, uint64_t p_budget_usec, Vector<Completion> &r_completions);

	void invalidate_scene(const String &p_scene_id);
	void invalidate_all();
	bool has_pending_work(uint64_t p_now_usec) const;
	uint32_t get_active_count() const;
};
