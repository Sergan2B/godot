/**************************************************************************/
/*  resource_graph_adapter.h                                              */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/
/* Copyright (c) 2014-present Godot Engine contributors (see AUTHORS.md). */
/* Copyright (c) 2007-2014 Juan Linietsky, Ariel Manzur.                  */
/*                                                                        */
/* Permission is hereby granted, free of charge, to any person obtaining  */
/* a copy of this software and associated documentation files (the        */
/* "Software"), to deal in the Software without restriction, including    */
/* without limitation the rights to use, copy, modify, merge, publish,    */
/* distribute, sublicense, and/or sell copies of the Software, and to     */
/* permit persons to whom the Software is furnished to do so, subject to  */
/* the following conditions:                                              */
/*                                                                        */
/* The above copyright notice and this permission notice shall be         */
/* included in all copies or substantial portions of the Software.        */
/*                                                                        */
/* THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,        */
/* EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF     */
/* MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. */
/* IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY   */
/* CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,   */
/* TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE      */
/* SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.                 */
/**************************************************************************/

#pragma once

#include "resource_delta_journal.h"

#include "core/crypto/crypto_core.h"
#include "core/templates/hash_map.h"
#include "core/templates/hash_set.h"
#include "core/templates/rb_map.h"
#include "core/templates/rb_set.h"
#include "core/templates/vector.h"
#include "core/variant/variant.h"

class BridgeRevisionClock;
class EditorFileSystemDirectory;

class ResourceGraphAdapter {
public:
	static constexpr uint32_t MAX_RESOURCES = 250000;
	static constexpr uint32_t MAX_DEPENDENCIES = 2000000;
	static constexpr uint32_t MAX_DIAGNOSTICS = 2000000;
	static constexpr uint32_t MAX_DEPENDENCIES_PER_RESOURCE = 4096;
	static constexpr uint32_t MAX_PATH_BYTES = 1024;
	static constexpr uint32_t MAX_UID_BYTES = 128;
	static constexpr uint32_t MAX_TYPE_BYTES = 256;
	static constexpr uint32_t MAX_RAW_DEPENDENCY_BYTES = MAX_UID_BYTES + MAX_TYPE_BYTES + MAX_PATH_BYTES + 4;
	static constexpr uint64_t MAX_SAFE_INTEGER = 9007199254740991ULL;
	static constexpr uint32_t SNAPSHOT_CHUNK_BYTES = 256 * 1024;
	static constexpr uint64_t SNAPSHOT_WINDOW_BYTES = 32 * 1024 * 1024;
	static constexpr uint64_t SNAPSHOT_TIMEOUT_USEC = 120000000;
	static constexpr uint64_t RESOURCE_BUDGET_USEC = 800;
	static constexpr uint64_t FRAME_SAFETY_MARGIN_USEC = 200;
	static constexpr uint32_t SNAPSHOT_BUILD_CHUNK_BYTES = SNAPSHOT_CHUNK_BYTES;
	static constexpr uint32_t MAX_SNAPSHOT_CHUNKS = 65536;
	static constexpr uint32_t BULK_INVALIDATION_RECORD_DELTA = 512;
	// Every valid operation/dependency occupies substantially more than 64
	// canonical JSON bytes. These guards therefore bound the temporary main-
	// thread collection without invalidating a batch that could fit the exact
	// 512 KiB journal limit checked on the worker.
	static constexpr uint32_t MAX_INCREMENTAL_OPERATIONS = ResourceDeltaJournal::MAX_BATCH_BYTES / 64;
	static constexpr uint32_t MAX_INCREMENTAL_DEPENDENCIES = ResourceDeltaJournal::MAX_BATCH_BYTES / 64;

	struct RefreshOutcome {
		bool changed = false;
		bool invalidated = false;
		uint64_t last_contiguous_resource_revision = 0;
		uint64_t current_resource_revision = 0;
		Dictionary revisions;
	};

	struct SnapshotCompletion {
		bool ready = false;
		bool terminal = false;
		uint64_t request_id = 0;
		bool is_error = false;
		String error_code;
		String error_message;
		bool error_retryable = false;
		Dictionary error_data;
		Dictionary result;
		Dictionary server_message;
		Array abandoned_messages;
	};

private:
	friend struct ResourceGraphAdapterTestAccess;

	struct CatalogRecord {
		Dictionary value;
		Array diagnostics;
		String facts_checksum;
	};

	struct DirectoryCursor {
		EditorFileSystemDirectory *directory = nullptr;
		int file_index = 0;
		int subdirectory_index = 0;
	};

	struct RefreshFile {
		String path;
		EditorFileSystemDirectory *directory = nullptr;
		int file_index = 0;
	};

	enum RefreshPhase {
		REFRESH_IDLE,
		REFRESH_COLLECT_PATHS,
		REFRESH_OBSERVE,
		REFRESH_OBSERVE_DEPENDENCIES,
		REFRESH_OBSERVE_FINISH,
		REFRESH_RECONCILE_OBSERVED,
		REFRESH_RELEASE_OBSERVED,
		REFRESH_RECONCILE_REMOVED,
		REFRESH_RELEASE_CATALOG,
		REFRESH_RELEASE_REIMPORTS,
		REFRESH_COMMIT,
		REFRESH_DRAIN_JOURNAL,
		REFRESH_RELEASE_RECONCILE_OPERATIONS,
		REFRESH_INITIAL_RELEASE_REIMPORTS,
		REFRESH_DRAIN_OBSERVATION,
		REFRESH_DRAIN_DIRECTORIES,
		REFRESH_DRAIN_FILES,
		REFRESH_DRAIN_OBSERVED,
		REFRESH_DRAIN_NEXT,
		REFRESH_DRAIN_OPERATIONS,
		REFRESH_DRAIN_REIMPORTS,
		REFRESH_DRAIN_FINALIZE,
	};

	enum RefreshDrainDisposition {
		REFRESH_DRAIN_NONE,
		REFRESH_DRAIN_RESTART,
		REFRESH_DRAIN_LIMIT_FAILURE,
	};

	BridgeRevisionClock *revision_clock = nullptr;
	ResourceDeltaJournal journal;
	RBMap<String, CatalogRecord> catalog;
	bool catalog_ready = false;
	bool catalog_limit_exceeded = false;

	bool refresh_requested = false;
	RefreshPhase refresh_phase = REFRESH_IDLE;
	EditorFileSystemDirectory *refresh_root = nullptr;
	Vector<DirectoryCursor> directory_stack;
	RBMap<String, RefreshFile> refresh_files;
	RBMap<String, CatalogRecord> observed_catalog;
	RBSet<String> pending_reimport_paths;
	RBSet<String> active_reimport_paths;
	uint64_t observed_dependency_count = 0;
	uint64_t observed_diagnostic_count = 0;
	bool refresh_limit_exceeded = false;
	RefreshFile active_refresh_file;
	CatalogRecord active_observed_record;
	Dictionary active_resource_ref;
	int active_dependency_index = 0;
	int active_dependency_count = 0;
	uint64_t active_resource_revision = 0;
	CryptoCore::SHA256Context active_facts_hash;
	bool active_facts_hash_started = false;
	RBMap<String, CatalogRecord> next_catalog;
	RBMap<String, CatalogRecord>::Element *reconcile_record = nullptr;
	Array reconcile_operations;
	RefreshPhase reconcile_resume_phase = REFRESH_IDLE;
	uint64_t reconcile_next_revision = 0;
	uint32_t reconcile_operation_count = 0;
	uint32_t reconcile_dependency_count = 0;
	bool reconcile_changed = false;
	bool reconcile_invalidated = false;
	bool reconcile_commit_started = false;
	uint64_t reconcile_previous_revision = 0;
	int64_t journal_prepare_task = -1;
	Error journal_prepare_error = OK;
	ResourceDeltaJournal::PreparedBatch journal_prepared_batch;
	RefreshDrainDisposition refresh_drain_disposition = REFRESH_DRAIN_NONE;

	bool snapshot_active = false;
	bool snapshot_waiting_for_terminal = false;
	uint64_t snapshot_request_id = 0;
	uint64_t snapshot_started_usec = 0;
	String snapshot_id;
	uint64_t snapshot_resource_revision = 0;
	Dictionary snapshot_revisions;
	Dictionary snapshot_context;
	RBMap<String, CatalogRecord>::Element *snapshot_record = nullptr;
	bool snapshot_resource_added = false;
	int snapshot_dependency_index = 0;
	int snapshot_diagnostic_index = 0;
	Dictionary snapshot_pending_message;
	uint32_t snapshot_chunk_count = 0;
	bool snapshot_finishing = false;
	Array snapshot_resources;
	Array snapshot_dependencies;
	Array snapshot_diagnostics;
	uint64_t snapshot_payload_bytes = 0;
	uint64_t snapshot_resource_count = 0;
	uint64_t snapshot_dependency_count = 0;
	uint64_t snapshot_diagnostic_count = 0;

	static String _make_snapshot_id();
	static Dictionary _make_resource_ref(const String &p_path, int64_t p_uid);
	static bool _utf8_within_limit(const String &p_value, uint32_t p_limit, uint32_t *r_length = nullptr);
	static bool _is_valid_resource_path(const String &p_path);
	static bool _is_valid_resource_uid(const String &p_uid);
	static bool _is_valid_raw_dependency_spec(const String &p_spec);
	static bool _paths_match_lexically(const String &p_left, const String &p_right, bool p_case_sensitive);
	static bool _paths_match_on_volume(const String &p_left, const String &p_right);
	static bool _validate_resource_ref(const Dictionary &p_resource_ref);
	static bool _validate_resource_observation(const Dictionary &p_resource);
	static bool _validate_dependency_observation(const Dictionary &p_dependency);
	static bool _validate_diagnostic(const Dictionary &p_diagnostic);
	static bool _can_append_diagnostics(uint64_t p_current_count, uint64_t p_additional_count);
	bool _append_active_diagnostic(const Dictionary &p_diagnostic);
	bool _update_active_facts_hash(const String &p_name, const String &p_value);
	void _reset_active_observation();
	bool _release_active_observation_step();
	bool _release_catalog_front_step(RBMap<String, CatalogRecord> &r_catalog, const RBMap<String, CatalogRecord> *p_preserved_catalog = nullptr);
	bool _begin_refresh();
	void _abort_refresh();
	void _start_refresh_drain(RefreshDrainDisposition p_disposition);
	bool _process_refresh_drain_step(RefreshOutcome &r_outcome);
	bool _collect_one_path();
	bool _begin_observe_resource(const RefreshFile &p_file);
	bool _observe_one_dependency();
	bool _finish_observe_resource(CatalogRecord &r_record);
	void _reset_reconcile();
	static void _prepare_journal_batch_thread(void *p_userdata);
	void _wait_for_journal_preparation();
	void _release_reconcile_operations_then(RefreshPhase p_resume_phase);
	bool _process_reconcile_step(RefreshOutcome &r_outcome);
	void _commit_reconcile(RefreshOutcome &r_outcome);
	void _finish_refresh(RefreshOutcome &r_outcome);
	void _reset_snapshot_payload();
	Array _take_abandoned_snapshot_data();
	bool _emit_pending_snapshot_message(SnapshotCompletion &r_completion);
	bool _append_snapshot_value(Array &r_values, const Variant &p_value);
	bool _flush_snapshot_payload();
	void _finish_snapshot(SnapshotCompletion &r_completion);
	void _fail_snapshot(const String &p_code, const String &p_message, bool p_retryable, SnapshotCompletion &r_completion);

public:
	void initialize(BridgeRevisionClock *p_revision_clock);
	void shutdown();

	void request_refresh();
	void mark_reimported(const Vector<String> &p_paths);
	bool process_refresh(uint64_t p_budget_usec, RefreshOutcome &r_outcome);

	Error begin_snapshot(uint64_t p_request_id, uint64_t p_now_usec, const Dictionary &p_context, Dictionary &r_error_data);
	bool process_snapshot(uint64_t p_now_usec, uint64_t p_budget_usec, SnapshotCompletion &r_completion);
	Array cancel_snapshot(uint64_t p_request_id);

	ResourceDeltaJournal::QueryResult query_delta(uint64_t p_after_resource_revision) const;
	bool is_catalog_ready() const;
	bool is_snapshot_active() const;
	bool has_pending_work() const;
	uint64_t get_resource_revision() const;
};
