/**************************************************************************/
/*  prepared_transaction_store.cpp                                       */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "prepared_transaction_store.h"

#include "modules/codex_bridge/protocol/bridge_crypto.h"
#include "modules/codex_bridge/protocol/bridge_frame_codec.h"
#include "modules/codex_bridge/protocol/bridge_transaction_profile.h"

bool PreparedTransactionStore::_is_active_state(State p_state) {
	return p_state == STATE_PREPARING || p_state == STATE_PREVIEWED || p_state == STATE_AWAITING_APPROVAL || p_state == STATE_APPLYING || p_state == STATE_APPLIED || p_state == STATE_VALIDATING;
}

String PreparedTransactionStore::state_name(State p_state) {
	switch (p_state) {
		case STATE_PREPARING:
			return "preparing";
		case STATE_PREVIEWED:
			return "previewed";
		case STATE_AWAITING_APPROVAL:
			return "awaiting_approval";
		case STATE_APPLYING:
			return "applying";
		case STATE_APPLIED:
			return "applied";
		case STATE_VALIDATING:
			return "validating";
		case STATE_COMMITTED:
			return "committed";
		case STATE_UNDONE:
			return "undone";
		case STATE_CONFLICTED:
			return "conflicted";
		case STATE_EXPIRED:
			return "expired";
		case STATE_REJECTED:
			return "rejected";
		case STATE_FAILED:
			return "failed";
		case STATE_FAILED_ROLLED_BACK:
			return "failed_rolled_back";
		case STATE_IN_DOUBT:
			return "in_doubt";
	}
	return "failed";
}

Error PreparedTransactionStore::_default_generate_id(String &r_transaction_id, void *p_userdata) {
	PackedByteArray random;
	const Error error = BridgeCrypto::random_bytes(16, random);
	if (error != OK) {
		return error;
	}
	r_transaction_id = "transaction:" + BridgeCrypto::bytes_to_lower_hex(random);
	return OK;
}

bool PreparedTransactionStore::_is_transaction_id(const String &p_value) {
	if (!p_value.begins_with("transaction:") || p_value.length() != 44) {
		return false;
	}
	for (int index = 12; index < p_value.length(); index++) {
		const char32_t character = p_value[index];
		if (!((character >= '0' && character <= '9') || (character >= 'a' && character <= 'f'))) {
			return false;
		}
	}
	return true;
}

void PreparedTransactionStore::_trim_terminal_tombstones() {
	while ((uint32_t)terminal_order.size() > MAX_TERMINAL_TOMBSTONES) {
		const String transaction_id = terminal_order.front()->get();
		terminal_order.pop_front();
		const Record *record = records.getptr(transaction_id);
		if (!record || _is_active_state(record->state)) {
			continue;
		}
		const String idempotency_key = record->idempotency_key;
		const String *mapped_transaction = transaction_by_idempotency.getptr(idempotency_key);
		if (mapped_transaction && *mapped_transaction == transaction_id) {
			transaction_by_idempotency.erase(idempotency_key);
		}
		records.erase(transaction_id);
	}
}

void PreparedTransactionStore::_terminalize(const String &p_transaction_id, State p_state, const String &p_error) {
	Record *record = records.getptr(p_transaction_id);
	if (!record || (record->state != STATE_PREPARING && record->state != STATE_PREVIEWED && record->state != STATE_AWAITING_APPROVAL)) {
		return;
	}
	const State terminal_state = record->state == STATE_PREPARING ? STATE_FAILED : p_state;
	transition(p_transaction_id, terminal_state, record->updated_at_ms, String(), p_error, p_error, p_state == STATE_CONFLICTED);
}

PreparedTransactionStore::PreparedTransactionStore(IdGenerator p_id_generator, void *p_id_generator_userdata) :
		id_generator(p_id_generator ? p_id_generator : _default_generate_id), id_generator_userdata(p_id_generator_userdata) {
}

PreparedTransactionStore::Admission PreparedTransactionStore::inspect_idempotency(const String &p_idempotency_key, const String &p_request_digest, uint64_t p_now_usec) {
	expire(p_now_usec);
	Admission admission;
	const String *existing_transaction = transaction_by_idempotency.getptr(p_idempotency_key);
	if (existing_transaction) {
		const Record *record = records.getptr(*existing_transaction);
		if (!record) {
			transaction_by_idempotency.erase(p_idempotency_key);
		} else {
			admission.transaction_id = record->transaction_id;
			admission.record = *record;
			if (record->request_digest != p_request_digest) {
				admission.kind = ADMISSION_IDEMPOTENCY_CONFLICT;
			} else if (record->state == STATE_PREPARING) {
				admission.kind = ADMISSION_IN_PROGRESS;
			} else if (record->state == STATE_PREVIEWED || record->state == STATE_AWAITING_APPROVAL) {
				admission.kind = ADMISSION_REPLAY;
			} else if (record->state == STATE_CONFLICTED || record->terminal_error == "transaction_conflicted") {
				admission.kind = ADMISSION_TRANSACTION_CONFLICTED;
			} else if (record->state == STATE_EXPIRED || record->terminal_error == "transaction_expired") {
				admission.kind = ADMISSION_TRANSACTION_EXPIRED;
			} else {
				admission.kind = ADMISSION_TRANSACTION_CONFLICTED;
			}
			return admission;
		}
	}
	admission.kind = ADMISSION_CREATED;
	return admission;
}

PreparedTransactionStore::Admission PreparedTransactionStore::admit(const String &p_idempotency_key, const String &p_request_digest, const String &p_canonical_request_json, const String &p_canonical_operation_json, const Binding &p_binding, uint64_t p_now_usec, uint64_t p_now_ms) {
	Admission admission = inspect_idempotency(p_idempotency_key, p_request_digest, p_now_usec);
	if (admission.kind != ADMISSION_CREATED || !admission.transaction_id.is_empty()) {
		return admission;
	}
	if (active_records >= MAX_ACTIVE_RECORDS) {
		admission.kind = ADMISSION_BUSY;
		return admission;
	}
	String transaction_id;
	bool unique_id = false;
	for (int attempt = 0; attempt < 4; attempt++) {
		if (id_generator(transaction_id, id_generator_userdata) != OK || !_is_transaction_id(transaction_id)) {
			admission.kind = ADMISSION_ID_FAILURE;
			return admission;
		}
		if (!records.has(transaction_id)) {
			unique_id = true;
			break;
		}
	}
	if (!unique_id) {
		admission.kind = ADMISSION_ID_FAILURE;
		return admission;
	}
	Record record;
	record.transaction_id = transaction_id;
	record.idempotency_key = p_idempotency_key;
	record.request_digest = p_request_digest;
	record.canonical_request_json = p_canonical_request_json;
	record.canonical_operation_json = p_canonical_operation_json;
	record.binding = p_binding;
	record.created_at_ms = p_now_ms;
	record.updated_at_ms = p_now_ms;
	record.expires_at_ms = p_now_ms <= UINT64_MAX - PREPARED_TTL_MSEC ? p_now_ms + PREPARED_TTL_MSEC : UINT64_MAX;
	const uint64_t ttl_usec = PREPARED_TTL_MSEC * 1000;
	record.expiry_deadline_usec = p_now_usec <= UINT64_MAX - ttl_usec ? p_now_usec + ttl_usec : UINT64_MAX;
	records.insert(transaction_id, record);
	transaction_by_idempotency.insert(p_idempotency_key, transaction_id);
	active_records++;
	admission.kind = ADMISSION_CREATED;
	admission.transaction_id = transaction_id;
	admission.record = record;
	return admission;
}

Error PreparedTransactionStore::publish_preview(const String &p_transaction_id, const String &p_prepare_result_json, const String &p_preview_payload_json, const String &p_preview_digest, const String &p_precondition_digest) {
	Record *record = records.getptr(p_transaction_id);
	if (!record) {
		return ERR_DOES_NOT_EXIST;
	}
	if (record->state != STATE_PREPARING || p_prepare_result_json.is_empty() || p_preview_payload_json.is_empty() || p_preview_digest.is_empty()) {
		return ERR_INVALID_DATA;
	}
	Dictionary parsed_result;
	if (BridgeJson::parse_strict_object(p_prepare_result_json.to_utf8_buffer(), parsed_result) != OK) {
		return ERR_INVALID_DATA;
	}
	record->prepare_result_json = p_prepare_result_json;
	record->preview_payload_json = p_preview_payload_json;
	record->preview_digest = p_preview_digest;
	record->precondition_digest = p_precondition_digest;
	record->operation_kind = parsed_result["operation_kind"];
	record->risk = parsed_result["risk"];
	record->scope = parsed_result["scope"];
	record->updated_at_ms = (uint64_t)(int64_t)parsed_result["created_at_ms"];
	record->state = STATE_PREVIEWED;
	record->transaction_seq = 2;
	return OK;
}

Error PreparedTransactionStore::transition(const String &p_transaction_id, State p_state, uint64_t p_updated_at_ms, const String &p_outcome, const String &p_error_code, const String &p_error_message, bool p_error_retryable) {
	Record *record = records.getptr(p_transaction_id);
	if (!record) {
		return ERR_DOES_NOT_EXIST;
	}
	if (record->state == p_state) {
		return OK;
	}
	const String previous = state_name(record->state);
	const String next = state_name(p_state);
	if (!BridgeTransactionProfile::is_legal_transition(previous, next)) {
		return ERR_INVALID_DATA;
	}
	const bool was_active = _is_active_state(record->state);
	const bool is_active = _is_active_state(p_state);
	record->state = p_state;
	record->transaction_seq++;
	record->updated_at_ms = p_updated_at_ms;
	if (!p_outcome.is_empty()) {
		record->outcome = p_outcome;
	}
	record->terminal_error = p_error_code;
	record->error_message = p_error_message;
	record->error_retryable = p_error_retryable;
	if (was_active && !is_active) {
		active_records--;
	} else if (!was_active && is_active) {
		active_records++;
	}
	if (!is_active && !record->retained_ordered) {
		record->retained_ordered = true;
		terminal_order.push_back(p_transaction_id);
	}
	if (!is_active) {
		record->canonical_request_json.clear();
		record->prepare_result_json.clear();
		record->preview_payload_json.clear();
		if (p_state == STATE_CONFLICTED || p_state == STATE_EXPIRED || p_state == STATE_REJECTED || p_state == STATE_FAILED || p_state == STATE_FAILED_ROLLED_BACK) {
			record->canonical_operation_json.clear();
		}
		_trim_terminal_tombstones();
	}
	return OK;
}

bool PreparedTransactionStore::conflict(const String &p_transaction_id) {
	const Record *record = records.getptr(p_transaction_id);
	if (!record || (record->state != STATE_PREPARING && record->state != STATE_PREVIEWED && record->state != STATE_AWAITING_APPROVAL)) {
		return false;
	}
	_terminalize(p_transaction_id, STATE_CONFLICTED, "transaction_conflicted");
	return true;
}

uint32_t PreparedTransactionStore::conflict_scene(const String &p_scene_id, Vector<String> *r_transaction_ids) {
	Vector<String> conflicts;
	for (const KeyValue<String, Record> &entry : records) {
		if ((entry.value.state == STATE_PREPARING || entry.value.state == STATE_PREVIEWED || entry.value.state == STATE_AWAITING_APPROVAL) && entry.value.binding.scene_id == p_scene_id) {
			conflicts.push_back(entry.key);
		}
	}
	for (const String &transaction_id : conflicts) {
		_terminalize(transaction_id, STATE_CONFLICTED, "transaction_conflicted");
		if (r_transaction_ids) {
			r_transaction_ids->push_back(transaction_id);
		}
	}
	return conflicts.size();
}

uint32_t PreparedTransactionStore::conflict_operation_sequence(uint64_t p_current_operation_seq) {
	Vector<String> conflicts;
	for (const KeyValue<String, Record> &entry : records) {
		if ((entry.value.state == STATE_PREPARING || entry.value.state == STATE_PREVIEWED || entry.value.state == STATE_AWAITING_APPROVAL) && entry.value.binding.operation_seq != p_current_operation_seq) {
			conflicts.push_back(entry.key);
		}
	}
	for (const String &transaction_id : conflicts) {
		_terminalize(transaction_id, STATE_CONFLICTED, "transaction_conflicted");
	}
	return conflicts.size();
}

uint32_t PreparedTransactionStore::conflict_all(Vector<String> *r_transaction_ids) {
	Vector<String> conflicts;
	for (const KeyValue<String, Record> &entry : records) {
		if (entry.value.state == STATE_PREPARING || entry.value.state == STATE_PREVIEWED || entry.value.state == STATE_AWAITING_APPROVAL) {
			conflicts.push_back(entry.key);
		}
	}
	for (const String &transaction_id : conflicts) {
		_terminalize(transaction_id, STATE_CONFLICTED, "transaction_conflicted");
		if (r_transaction_ids) {
			r_transaction_ids->push_back(transaction_id);
		}
	}
	return conflicts.size();
}

Vector<String> PreparedTransactionStore::expire(uint64_t p_now_usec) {
	Vector<String> expired;
	for (const KeyValue<String, Record> &entry : records) {
		if ((entry.value.state == STATE_PREPARING || entry.value.state == STATE_PREVIEWED || entry.value.state == STATE_AWAITING_APPROVAL) && p_now_usec >= entry.value.expiry_deadline_usec) {
			expired.push_back(entry.key);
		}
	}
	for (const String &transaction_id : expired) {
		_terminalize(transaction_id, STATE_EXPIRED, "transaction_expired");
	}
	return expired;
}

void PreparedTransactionStore::clear() {
	records.clear();
	transaction_by_idempotency.clear();
	terminal_order.clear();
	active_records = 0;
}

bool PreparedTransactionStore::get_record(const String &p_transaction_id, Record &r_record) const {
	const Record *record = records.getptr(p_transaction_id);
	if (!record) {
		return false;
	}
	r_record = *record;
	return true;
}

PreparedTransactionStore::Record *PreparedTransactionStore::get_record_mutable(const String &p_transaction_id) {
	return records.getptr(p_transaction_id);
}

void PreparedTransactionStore::get_records(Vector<Record> &r_records) const {
	for (const KeyValue<String, Record> &entry : records) {
		r_records.push_back(entry.value);
	}
}

Error PreparedTransactionStore::get_prepare_result(const String &p_transaction_id, Dictionary &r_result) const {
	const Record *record = records.getptr(p_transaction_id);
	if (!record) {
		return ERR_DOES_NOT_EXIST;
	}
	if ((record->state != STATE_PREVIEWED && record->state != STATE_AWAITING_APPROVAL) || record->prepare_result_json.is_empty()) {
		return ERR_UNAVAILABLE;
	}
	return BridgeJson::parse_strict_object(record->prepare_result_json.to_utf8_buffer(), r_result);
}

uint32_t PreparedTransactionStore::get_active_count() const {
	return active_records;
}

uint32_t PreparedTransactionStore::get_terminal_count() const {
	return terminal_order.size();
}

uint32_t PreparedTransactionStore::get_total_count() const {
	return records.size();
}

uint64_t PreparedTransactionStore::get_next_expiry_deadline_usec() const {
	uint64_t deadline_usec = UINT64_MAX;
	for (const KeyValue<String, Record> &entry : records) {
		if (entry.value.state == STATE_PREPARING || entry.value.state == STATE_PREVIEWED || entry.value.state == STATE_AWAITING_APPROVAL) {
			deadline_usec = MIN(deadline_usec, entry.value.expiry_deadline_usec);
		}
	}
	return deadline_usec;
}
