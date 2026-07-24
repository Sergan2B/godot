/**************************************************************************/
/*  prepared_change_set_store.cpp                                         */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#include "prepared_change_set_store.h"

bool PreparedChangeSetStore::_terminal(State p_state) {
	return p_state == STATE_COMMITTED || p_state == STATE_ROLLED_BACK || p_state == STATE_ROLLBACK_BLOCKED || p_state == STATE_UNDONE || p_state == STATE_CONFLICTED || p_state == STATE_EXPIRED || p_state == STATE_FAILED;
}

void PreparedChangeSetStore::_trim() {
	while (terminal_order.size() > MAX_TERMINAL) {
		const String id = terminal_order.front()->get();
		terminal_order.pop_front();
		const Record *record = records.getptr(id);
		if (record) {
			by_idempotency.erase(record->plan.idempotency_key);
			records.erase(id);
		}
	}
}

PreparedChangeSetStore::Admission PreparedChangeSetStore::admit(const CompoundChangeSetPlanner::Plan &p_plan, Record &r_record) {
	if (const String *existing_id = by_idempotency.getptr(p_plan.idempotency_key)) {
		const Record *existing = records.getptr(*existing_id);
		if (!existing) {
			by_idempotency.erase(p_plan.idempotency_key);
		} else if (existing->plan.request_digest == p_plan.request_digest) {
			r_record = *existing;
			return ADMISSION_REPLAY;
		} else {
			r_record = *existing;
			return ADMISSION_CONFLICT;
		}
	}
	if (active >= MAX_ACTIVE) {
		return ADMISSION_BUSY;
	}
	Record record;
	record.plan = p_plan;
	record.updated_at_ms = p_plan.created_at_ms;
	records.insert(p_plan.change_set_id, record);
	by_idempotency.insert(p_plan.idempotency_key, p_plan.change_set_id);
	active++;
	r_record = record;
	return ADMISSION_CREATED;
}

Error PreparedChangeSetStore::transition(const String &p_change_set_id, State p_state, uint64_t p_now_ms, const String &p_outcome, const String &p_error_code, const String &p_error_message) {
	Record *record = records.getptr(p_change_set_id);
	ERR_FAIL_NULL_V(record, ERR_DOES_NOT_EXIST);
	ERR_FAIL_COND_V(!legal_transition(record->state, p_state), ERR_INVALID_PARAMETER);
	const bool was_terminal = _terminal(record->state);
	const bool now_terminal = _terminal(p_state);
	record->state = p_state;
	record->transaction_seq++;
	record->updated_at_ms = p_now_ms;
	if (!p_outcome.is_empty()) {
		record->outcome = p_outcome;
	}
	record->error_code = p_error_code;
	record->error_message = p_error_message;
	if (!was_terminal && now_terminal) {
		active--;
		terminal_order.push_back(p_change_set_id);
		_trim();
	}
	return OK;
}

Vector<String> PreparedChangeSetStore::expire(uint64_t p_now_ms) {
	Vector<String> expired;
	for (KeyValue<String, Record> &entry : records) {
		if (!_terminal(entry.value.state) && entry.value.state != STATE_APPLYING && entry.value.state != STATE_PERSISTING && entry.value.state != STATE_VALIDATING && entry.value.state != STATE_ROLLING_BACK && p_now_ms >= entry.value.plan.expires_at_ms) {
			expired.push_back(entry.key);
		}
	}
	for (const String &id : expired) {
		transition(id, STATE_EXPIRED, p_now_ms, "not_applied", "change_set_expired", "The prepared change set expired.");
	}
	return expired;
}

bool PreparedChangeSetStore::get(const String &p_change_set_id, Record &r_record) const {
	const Record *record = records.getptr(p_change_set_id);
	if (!record) {
		return false;
	}
	r_record = *record;
	return true;
}

PreparedChangeSetStore::Record *PreparedChangeSetStore::get_mutable(const String &p_change_set_id) {
	return records.getptr(p_change_set_id);
}

void PreparedChangeSetStore::clear() {
	records.clear();
	by_idempotency.clear();
	terminal_order.clear();
	active = 0;
}

uint32_t PreparedChangeSetStore::get_active_count() const {
	return active;
}

uint32_t PreparedChangeSetStore::get_total_count() const {
	return records.size();
}

String PreparedChangeSetStore::state_name(State p_state) {
	switch (p_state) {
		case STATE_PREVIEWED: return "previewed";
		case STATE_AWAITING_APPROVAL: return "awaiting_approval";
		case STATE_APPLYING: return "applying";
		case STATE_PERSISTING: return "persisting";
		case STATE_VALIDATING: return "validating";
		case STATE_COMMITTED: return "committed";
		case STATE_ROLLING_BACK: return "rolling_back";
		case STATE_ROLLED_BACK: return "rolled_back";
		case STATE_ROLLBACK_BLOCKED: return "rollback_blocked";
		case STATE_UNDONE: return "undone";
		case STATE_CONFLICTED: return "conflicted";
		case STATE_EXPIRED: return "expired";
		case STATE_FAILED: return "failed";
		case STATE_IN_DOUBT: return "in_doubt";
	}
	return "failed";
}

bool PreparedChangeSetStore::legal_transition(State p_from, State p_to) {
	if (p_from == p_to) {
		return false;
	}
	switch (p_from) {
		case STATE_PREVIEWED:
			return p_to == STATE_AWAITING_APPROVAL || p_to == STATE_APPLYING || p_to == STATE_CONFLICTED || p_to == STATE_EXPIRED || p_to == STATE_FAILED;
		case STATE_AWAITING_APPROVAL:
			return p_to == STATE_APPLYING || p_to == STATE_CONFLICTED || p_to == STATE_EXPIRED || p_to == STATE_FAILED;
		case STATE_APPLYING:
			return p_to == STATE_PERSISTING || p_to == STATE_VALIDATING || p_to == STATE_FAILED || p_to == STATE_IN_DOUBT;
		case STATE_PERSISTING:
			return p_to == STATE_VALIDATING || p_to == STATE_FAILED || p_to == STATE_IN_DOUBT;
		case STATE_VALIDATING:
			return p_to == STATE_COMMITTED || p_to == STATE_ROLLING_BACK || p_to == STATE_FAILED || p_to == STATE_IN_DOUBT;
		case STATE_COMMITTED:
			return p_to == STATE_UNDONE || p_to == STATE_ROLLING_BACK;
		case STATE_ROLLING_BACK:
			return p_to == STATE_ROLLED_BACK || p_to == STATE_ROLLBACK_BLOCKED || p_to == STATE_IN_DOUBT;
		case STATE_IN_DOUBT:
			return p_to == STATE_COMMITTED || p_to == STATE_ROLLED_BACK || p_to == STATE_ROLLBACK_BLOCKED;
		default:
			return false;
	}
}
