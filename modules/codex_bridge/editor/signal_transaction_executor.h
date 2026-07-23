/**************************************************************************/
/*  signal_transaction_executor.h                                       */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#pragma once

#include "transaction_executor.h"

class Node;
class UndoRedo;

class SignalTransactionExecutor : public TransactionExecutor {
public:
	static Error prepare_resolution(Node *p_scene_root, Node *p_emitter, Node *p_receiver, const Dictionary &p_operation, TransactionPreviewBuilder::Resolution &r_resolution, String &r_error_code, String &r_error_message);

	Error final_preflight(const PreparedTransactionStore::Record &p_record, const TransactionSceneResolver::Job &p_resolver_job, const TransactionPreviewBuilder::Resolution &p_resolution, NativeActionPlan &r_plan, String &r_error_code, String &r_error_message) override;
	Error register_native_action(const NativeActionPlan &p_plan, EditorUndoRedoManager *p_undo_redo) override;
	Error register_native_action_on_history(const NativeActionPlan &p_plan, UndoRedo *p_undo_redo);
	bool verify_postcondition(const PreparedTransactionStore::Record &p_record, const NativeActionPlan &p_plan) const override;
	bool verify_prestate_after_rollback(const PreparedTransactionStore::Record &p_record, const NativeActionPlan &p_plan) const override;
};
