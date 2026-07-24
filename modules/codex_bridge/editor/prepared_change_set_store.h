/**************************************************************************/
/*  prepared_change_set_store.h                                           */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#pragma once

#include "compound_change_set_planner.h"

#include "core/templates/hash_map.h"
#include "core/templates/list.h"

class PreparedChangeSetStore {
public:
	enum State {
		STATE_PREVIEWED,
		STATE_AWAITING_APPROVAL,
		STATE_APPLYING,
		STATE_PERSISTING,
		STATE_VALIDATING,
		STATE_COMMITTED,
		STATE_ROLLING_BACK,
		STATE_ROLLED_BACK,
		STATE_ROLLBACK_BLOCKED,
		STATE_UNDONE,
		STATE_CONFLICTED,
		STATE_EXPIRED,
		STATE_FAILED,
		STATE_IN_DOUBT,
	};

	enum Admission {
		ADMISSION_CREATED,
		ADMISSION_REPLAY,
		ADMISSION_CONFLICT,
		ADMISSION_BUSY,
	};

	struct Record {
		CompoundChangeSetPlanner::Plan plan;
		State state = STATE_PREVIEWED;
		uint64_t transaction_seq = 1;
		uint64_t updated_at_ms = 0;
		String outcome = "none";
		String error_code;
		String error_message;
		bool mutation_started = false;
		bool commit_point_entered = false;
		bool response_lost = false;
		int native_history_id = -1;
		String native_action_tag;
		String postimage_digest;
		String validation_report_id;
	};

	static constexpr uint32_t MAX_ACTIVE = 64;
	static constexpr uint32_t MAX_TERMINAL = 64;

private:
	HashMap<String, Record> records;
	HashMap<String, String> by_idempotency;
	List<String> terminal_order;
	uint32_t active = 0;

	static bool _terminal(State p_state);
	void _trim();

public:
	Admission admit(const CompoundChangeSetPlanner::Plan &p_plan, Record &r_record);
	Error transition(const String &p_change_set_id, State p_state, uint64_t p_now_ms, const String &p_outcome = String(), const String &p_error_code = String(), const String &p_error_message = String());
	Vector<String> expire(uint64_t p_now_ms);
	bool get(const String &p_change_set_id, Record &r_record) const;
	Record *get_mutable(const String &p_change_set_id);
	void clear();
	uint32_t get_active_count() const;
	uint32_t get_total_count() const;
	static String state_name(State p_state);
	static bool legal_transition(State p_from, State p_to);
};
