/**************************************************************************/
/*  compound_change_set_coordinator.h                                     */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#pragma once

#include "bridge_revision_clock.h"
#include "compound_native_executor.h"
#include "prepared_change_set_store.h"
#include "transaction_approval_verifier.h"

class CompoundChangeSetCoordinator {
public:
	struct Outcome {
		bool has_result = false;
		Dictionary result;
		String error_code;
		String error_message;
		bool retryable = false;
	};

private:
	String project_id;
	String editor_session_id;
	BridgeRevisionClock *revision_clock = nullptr;
	PreparedChangeSetStore store;
	TransactionApprovalVerifier approval_verifier;
	CompoundNativeExecutor native_executor;
	HashMap<String, CompoundNativeExecutor::Execution> executions;
	Dictionary resource_paths;
	Vector<Dictionary> events;

	bool _coordinates_current(const Dictionary &p_coordinates) const;
	static Dictionary _preview_result(const PreparedChangeSetStore::Record &p_record);
	static Outcome _error(const String &p_code, const String &p_message, bool p_retryable = false);
	void _emit(const PreparedChangeSetStore::Record &p_record);
	Error _transition(const String &p_change_set_id, PreparedChangeSetStore::State p_state, uint64_t p_now_ms, const String &p_outcome = String(), const String &p_error_code = String(), const String &p_error_message = String());
	void _expire(uint64_t p_now_ms);
	void _reconcile(PreparedChangeSetStore::Record &r_record, uint64_t p_now_ms);

public:
	void initialize(const String &p_project_id, const String &p_editor_session_id, BridgeRevisionClock *p_revision_clock, const PackedByteArray &p_approval_key = PackedByteArray());
	void shutdown();
	Outcome prepare(const Dictionary &p_params, uint64_t p_now_ms);
	Outcome apply(const Dictionary &p_params, uint64_t p_now_ms);
	Outcome status(const String &p_change_set_id, uint64_t p_now_ms);
	Outcome validation_complete(const Dictionary &p_params, uint64_t p_now_ms);
	Outcome rollback(const Dictionary &p_params, uint64_t p_now_ms);
	Outcome undo(const Dictionary &p_params, uint64_t p_now_ms);
	void drain_events(Vector<Dictionary> &r_events);
	void invalidate_all(uint64_t p_now_ms);
	uint32_t get_active_count() const;
	bool is_ready() const;
	bool is_approval_available() const;
};
