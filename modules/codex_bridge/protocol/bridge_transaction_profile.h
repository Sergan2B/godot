/**************************************************************************/
/*  bridge_transaction_profile.h                                          */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#pragma once

#include "core/variant/variant.h"

class BridgeTransactionProfile {
public:
	static constexpr int64_t MAX_OPERATION_BYTES = 65536;
	static constexpr int64_t MAX_STATUS_BYTES = 65536;
	static constexpr int64_t MAX_VARIANT_DEPTH = 8;
	static constexpr int64_t MAX_CONTAINER_ITEMS = 1000;
	static constexpr int64_t MAX_STRING_CHARACTERS = 16384;

	static Dictionary make_unavailable_capability();
	static Dictionary make_ready_capability(bool p_busy = false, bool p_scene_available = true, bool p_approval_available = true, bool p_coordinator_available = true);
	static Dictionary make_limits();
	static void append_global_limits(Dictionary &r_limits);

	static bool validate_prepare_params(const Dictionary &p_params);
	static bool validate_apply_params(const Dictionary &p_params);
	static bool validate_status_params(const Dictionary &p_params);
	static bool validate_undo_params(const Dictionary &p_params);
	static bool validate_operation(const Dictionary &p_operation);
	static bool validate_prepare_result(const Dictionary &p_result);
	static bool validate_status_result(const Dictionary &p_result);
	static bool validate_affected_entities(const Array &p_entities, bool p_allow_empty = false);
	static bool validate_event_params(const Dictionary &p_params);
	static bool is_legal_transition(const String &p_previous_state, const String &p_state);
};
