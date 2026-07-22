/**************************************************************************/
/*  prepared_transaction_store.h                                         */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#pragma once

#include "core/templates/hash_map.h"
#include "core/templates/list.h"
#include "core/variant/variant.h"

class PreparedTransactionStore {
public:
	enum State {
		STATE_PREPARING,
		STATE_PREVIEWED,
		STATE_CONFLICTED,
		STATE_EXPIRED,
	};

	enum AdmissionKind {
		ADMISSION_CREATED,
		ADMISSION_IN_PROGRESS,
		ADMISSION_REPLAY,
		ADMISSION_IDEMPOTENCY_CONFLICT,
		ADMISSION_TRANSACTION_CONFLICTED,
		ADMISSION_TRANSACTION_EXPIRED,
		ADMISSION_BUSY,
		ADMISSION_ID_FAILURE,
	};

	struct Binding {
		String project_id;
		String editor_session_id;
		String scene_id;
		String history_id;
		uint64_t scene_revision = 0;
		uint64_t operation_seq = 0;
		uint64_t event_seq = 0;
		uint64_t project_revision = 0;
		uint64_t resource_revision = 0;
		uint64_t scene_graph_revision = 0;
		uint64_t script_graph_revision = 0;
	};

	struct Record {
		String transaction_id;
		String idempotency_key;
		String request_digest;
		String canonical_request_json;
		String canonical_operation_json;
		Binding binding;
		State state = STATE_PREPARING;
		uint64_t transaction_seq = 1;
		uint64_t created_at_ms = 0;
		uint64_t expires_at_ms = 0;
		uint64_t expiry_deadline_usec = 0;
		String prepare_result_json;
		String preview_payload_json;
		String preview_digest;
		String terminal_error;
	};

	struct Admission {
		AdmissionKind kind = ADMISSION_ID_FAILURE;
		String transaction_id;
		Record record;
	};

	typedef Error (*IdGenerator)(String &r_transaction_id, void *p_userdata);

	static constexpr uint32_t MAX_ACTIVE_RECORDS = 64;
	static constexpr uint32_t MAX_TERMINAL_TOMBSTONES = 64;
	static constexpr uint64_t PREPARED_TTL_MSEC = 300000;

private:
	HashMap<String, Record> records;
	HashMap<String, String> transaction_by_idempotency;
	List<String> terminal_order;
	uint32_t active_records = 0;
	IdGenerator id_generator = nullptr;
	void *id_generator_userdata = nullptr;

	static Error _default_generate_id(String &r_transaction_id, void *p_userdata);
	static bool _is_transaction_id(const String &p_value);
	void _terminalize(const String &p_transaction_id, State p_state, const String &p_error);
	void _trim_terminal_tombstones();

public:
	PreparedTransactionStore(IdGenerator p_id_generator = nullptr, void *p_id_generator_userdata = nullptr);

	Admission inspect_idempotency(const String &p_idempotency_key, const String &p_request_digest, uint64_t p_now_usec);
	Admission admit(const String &p_idempotency_key, const String &p_request_digest, const String &p_canonical_request_json, const String &p_canonical_operation_json, const Binding &p_binding, uint64_t p_now_usec, uint64_t p_now_ms);
	Error publish_preview(const String &p_transaction_id, const String &p_prepare_result_json, const String &p_preview_payload_json, const String &p_preview_digest);
	bool conflict(const String &p_transaction_id);
	uint32_t conflict_scene(const String &p_scene_id);
	uint32_t conflict_operation_sequence(uint64_t p_current_operation_seq);
	uint32_t conflict_all();
	void expire(uint64_t p_now_usec);
	void clear();

	bool get_record(const String &p_transaction_id, Record &r_record) const;
	Error get_prepare_result(const String &p_transaction_id, Dictionary &r_result) const;
	uint32_t get_active_count() const;
	uint32_t get_terminal_count() const;
	uint32_t get_total_count() const;
	uint64_t get_next_expiry_deadline_usec() const;
};
