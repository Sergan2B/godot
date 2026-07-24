/**************************************************************************/
/*  compound_native_executor.h                                           */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#pragma once

#include "compound_change_set_planner.h"
#include "property_transaction_executor.h"
#include "scoped_persistence_executor.h"
#include "script_transaction_executor.h"
#include "signal_transaction_executor.h"
#include "structural_transaction_executor.h"

#include "core/object/ref_counted.h"

class CompoundPersistenceAction : public RefCounted {
	ScopedPersistenceExecutor::Prepared prepared;
	bool last_operation_succeeded = true;
	String last_error_code;
	String last_error_message;

public:
	void initialize(const ScopedPersistenceExecutor::Prepared &p_prepared);
	void commit_postimages();
	void restore_preimages();
	bool succeeded() const;
	String get_error_code() const;
	String get_error_message() const;
	bool verify_postimages() const;
	bool verify_preimages() const;
	void cleanup();
};

class CompoundNativeExecutor {
public:
	struct Step {
		String operation_kind;
		PreparedTransactionStore::Record record;
		TransactionExecutor::NativeActionPlan native_plan;
		TransactionExecutor *executor = nullptr;
	};

	struct Execution {
		String change_set_id;
		String action_name;
		int native_history_id = -1;
		uint64_t history_version_before = 0;
		int history_action_before = -1;
		int history_count_before = 0;
		uint64_t history_version_committed = 0;
		int history_action_committed = -1;
		int history_count_committed = 0;
		Vector<Step> steps;
		Ref<CompoundPersistenceAction> persistence;
		bool commit_point_entered = false;
		bool committed = false;
		bool undone = false;
	};

	struct Outcome {
		bool success = false;
		bool in_doubt = false;
		String error_code;
		String error_message;
	};

private:
	StructuralTransactionExecutor structural_executor;
	PropertyTransactionExecutor property_executor;
	ScriptTransactionExecutor script_executor;
	SignalTransactionExecutor signal_executor;

	TransactionExecutor *_executor_for(const String &p_kind);
	static bool _is_scene_operation(const String &p_kind);
	static bool _history_correlated(const Execution &p_execution, bool p_require_newest);

public:
	Outcome apply(const CompoundChangeSetPlanner::Plan &p_plan, const String &p_project_id, const String &p_editor_session_id, const Dictionary &p_coordinates, const Dictionary &p_resource_paths, Execution &r_execution);
	Outcome undo(Execution &r_execution);
	Outcome rollback(Execution &r_execution);
	bool verify_poststate(const Execution &p_execution) const;
	bool verify_prestate(const Execution &p_execution) const;
	void cleanup(Execution &r_execution);
};
