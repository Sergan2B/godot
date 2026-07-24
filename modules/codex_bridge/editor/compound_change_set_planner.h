/**************************************************************************/
/*  compound_change_set_planner.h                                         */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#pragma once

#include "core/variant/variant.h"

class CompoundChangeSetPlanner {
public:
	struct Plan {
		String change_set_id;
		String idempotency_key;
		String request_digest;
		String preview_digest;
		String canonical_request_json;
		String canonical_preview_json;
		Array ordered_operations;
		Array original_order;
		Array preconditions;
		Array save_scope;
		Dictionary validation_policy;
		String risk;
		uint64_t created_at_ms = 0;
		uint64_t expires_at_ms = 0;
	};

	static constexpr int64_t MAX_OPERATIONS = 16;
	static constexpr int64_t MAX_SAVE_PATHS = 18;
	static constexpr int64_t MAX_PREVIEW_BYTES = 65536;
	static constexpr uint64_t PREPARED_TTL_MS = 300000;

	static Error build(const String &p_project_id, const String &p_editor_session_id, const Dictionary &p_params, uint64_t p_now_ms, Plan &r_plan, String &r_error_code, String &r_error_message);
	static bool validate_params(const Dictionary &p_params);
};
