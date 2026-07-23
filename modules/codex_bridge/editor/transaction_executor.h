/**************************************************************************/
/*  transaction_executor.h                                              */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#pragma once

#include "prepared_transaction_store.h"
#include "transaction_scene_resolver.h"

#include "core/object/ref_counted.h"

class EditorUndoRedoManager;

class TransactionExecutor {
public:
	struct NativeActionPlan {
		String action_name;
		int native_history_id = -1;
		Variant payload;
		Ref<RefCounted> context;
	};

	virtual Error final_preflight(const PreparedTransactionStore::Record &p_record, const TransactionSceneResolver::Job &p_resolver_job, const TransactionPreviewBuilder::Resolution &p_resolution, NativeActionPlan &r_plan, String &r_error_code, String &r_error_message) = 0;
	virtual Error register_native_action(const NativeActionPlan &p_plan, EditorUndoRedoManager *p_undo_redo) = 0;
	virtual bool verify_postcondition(const PreparedTransactionStore::Record &p_record, const NativeActionPlan &p_plan) const = 0;
	virtual bool verify_prestate_after_rollback(const PreparedTransactionStore::Record &p_record, const NativeActionPlan &p_plan) const = 0;
	virtual Array collect_committed_entities(const PreparedTransactionStore::Record &p_record, const NativeActionPlan &p_plan) const {
		(void)p_record;
		(void)p_plan;
		return Array();
	}

	virtual ~TransactionExecutor() = default;
};
