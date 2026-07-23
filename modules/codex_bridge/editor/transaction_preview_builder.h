/**************************************************************************/
/*  transaction_preview_builder.h                                        */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#pragma once

#include "prepared_transaction_store.h"

class TransactionPreviewBuilder {
public:
	struct Resolution {
		Array affected_entities;
		uint32_t structural_nodes = 0;
		bool script_already_attached = false;
		Dictionary redacted_change;
		String precondition_digest;
	};

	struct Output {
		Dictionary result;
		String result_json;
		String preview_payload_json;
		String preview_digest;
	};

	static Error build(const String &p_transaction_id, const PreparedTransactionStore::Binding &p_binding, const Dictionary &p_operation, const Resolution &p_resolution, uint64_t p_created_at_ms, uint64_t p_expires_at_ms, Output &r_output);
};
