/**************************************************************************/
/*  script_graph_adapter.h                                                */
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

#include "script_delta_journal.h"
#include "script_semantic_adapter.h"

#include "core/io/dir_access.h"
#include "core/templates/rb_map.h"
#include "core/templates/rb_set.h"
#include "core/templates/vector.h"
#include "core/variant/variant.h"

class BridgeRevisionClock;
class EditorFileSystemDirectory;

class ScriptGraphAdapter {
public:
	static constexpr uint32_t SNAPSHOT_CHUNK_BYTES = 256 * 1024;
	static constexpr uint64_t SNAPSHOT_WINDOW_BYTES = 32 * 1024 * 1024;
	static constexpr uint64_t SNAPSHOT_TIMEOUT_USEC = 120000000;
	static constexpr uint32_t MAX_SNAPSHOT_CHUNKS = 65536;
	static constexpr uint32_t MAX_ADAPTER_STATUSES = 16;
	static constexpr uint32_t MAX_RETIRE_RECORDS_PER_BATCH = 256;
	static constexpr uint64_t MAX_RETIRE_SOURCE_BYTES_PER_BATCH = 8 * 1024 * 1024;
	static constexpr uint64_t SCRIPT_BUDGET_USEC = 600;
	static constexpr uint64_t FRAME_SAFETY_MARGIN_USEC = 1000;

	struct RefreshOutcome {
		bool changed = false;
		bool invalidated = false;
		uint64_t last_contiguous_script_graph_revision = 0;
		uint64_t current_script_graph_revision = 0;
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
	friend struct ScriptGraphAdapterTestAccess;

	struct CatalogRecord {
		Dictionary bundle;
		String facts_checksum;
		uint64_t source_modified_time = 0;
		uint64_t source_bytes = 0;
	};

	struct DirectoryCursor {
		EditorFileSystemDirectory *directory = nullptr;
		int file_index = 0;
		int subdirectory_index = 0;
	};

	struct RawDirectoryCursor {
		Ref<DirAccess> directory;
		String path;
	};

	struct RefreshFile {
		String key;
		String path;
		Dictionary script_ref;
	};

	enum RefreshPhase {
		REFRESH_IDLE,
		REFRESH_COLLECT,
		REFRESH_INVALIDATE_CACHE,
		REFRESH_PROJECT,
		REFRESH_RECONCILE_BEGIN,
		REFRESH_RECONCILE_OBSERVED,
		REFRESH_RECONCILE_REMOVED,
		REFRESH_RECONCILE_PREPARE,
		REFRESH_RECONCILE_WAIT,
		REFRESH_DRAIN_FILES,
		REFRESH_DRAIN_CATALOG,
		REFRESH_DRAIN_JOURNAL,
	};

	BridgeRevisionClock *revision_clock = nullptr;
	ScriptDeltaJournal journal;
	RBMap<String, CatalogRecord> catalog;
	RBMap<String, CatalogRecord> observed_catalog;
	RBMap<String, CatalogRecord> retiring_catalog;
	RBMap<String, CatalogRecord> pending_retiring_catalog;
	Array retiring_bundles;
	uint64_t retiring_source_bytes = 0;
	uint64_t catalog_resource_revision = 0;
	Array adapter_statuses;
	bool catalog_ready = false;
	bool catalog_limit_exceeded = false;
	bool refresh_requested = false;
	bool refresh_limit_exceeded = false;
	bool refresh_projection_failed = false;
	RefreshPhase refresh_phase = REFRESH_IDLE;
	Vector<DirectoryCursor> directory_stack;
	Vector<RawDirectoryCursor> raw_directory_stack;
	bool raw_scan_started = false;
	RBMap<String, RefreshFile> refresh_files;
	RBSet<String> refresh_paths;
	RBSet<String> pending_cache_invalidations;
	RBMap<String, RefreshFile>::Element *refresh_file = nullptr;
	RBMap<String, RefreshFile>::Element *cache_invalidation_file = nullptr;
	bool projection_preparing = false;
	int64_t projection_task = -1;
	RefreshFile projection_file;
	uint64_t projection_resource_revision = 1;
	Error projection_error = OK;
	ScriptSemanticAdapter::DocumentProjection projection_result;
	uint64_t target_resource_revision = 1;
	uint64_t target_script_graph_revision = 1;
	uint64_t observed_symbol_count = 0;
	uint64_t observed_relation_count = 0;
	uint64_t observed_diagnostic_count = 0;
	RefreshPhase drain_resume_phase = REFRESH_IDLE;

	Array reconcile_operations;
	RBMap<String, CatalogRecord>::Element *reconcile_observed = nullptr;
	RBMap<String, CatalogRecord>::Element *reconcile_existing = nullptr;
	uint64_t reconcile_previous_revision = 0;
	uint64_t reconcile_next_revision = 0;
	bool reconcile_preparing = false;
	int64_t journal_prepare_task = -1;
	Error journal_prepare_error = OK;
	ScriptDeltaJournal::PreparedBatch journal_prepared_batch;

	bool snapshot_active = false;
	bool snapshot_waiting_for_terminal = false;
	uint64_t snapshot_request_id = 0;
	uint64_t snapshot_started_usec = 0;
	String snapshot_id;
	uint64_t snapshot_resource_revision = 0;
	uint64_t snapshot_scene_graph_revision = 0;
	uint64_t snapshot_script_graph_revision = 0;
	Dictionary snapshot_revisions;
	Dictionary snapshot_context;
	RBMap<String, CatalogRecord>::Element *snapshot_record = nullptr;
	bool snapshot_document_added = false;
	int snapshot_symbol_index = 0;
	int snapshot_relation_index = 0;
	int snapshot_diagnostic_index = 0;
	int snapshot_adapter_status_index = 0;
	Dictionary snapshot_pending_message;
	Array snapshot_documents;
	Array snapshot_symbols;
	Array snapshot_relations;
	Array snapshot_diagnostics;
	Array snapshot_adapter_statuses;
	uint64_t snapshot_payload_bytes = 0;
	uint32_t snapshot_chunk_count = 0;
	uint64_t snapshot_document_count = 0;
	uint64_t snapshot_symbol_count = 0;
	uint64_t snapshot_relation_count = 0;
	uint64_t snapshot_diagnostic_count = 0;
	uint64_t snapshot_adapter_status_count = 0;
	bool snapshot_finishing = false;

	static String _make_snapshot_id();
	static Dictionary _make_script_ref(const String &p_path, int64_t p_uid);
	static Dictionary _make_raw_script_ref(const String &p_path);
	static String _script_ref_key(const Dictionary &p_script_ref);
	static bool _is_script_path(const String &p_path);
	static uint64_t _bundle_count(const Dictionary &p_bundle, const String &p_key);
	static bool _stamp_bundle_script_graph_revision(Dictionary &r_bundle, uint64_t p_script_graph_revision);
	bool _account_bundle(const Dictionary &p_bundle);

	void _reset_refresh();
	void _enqueue_catalog_retirement(RBMap<String, CatalogRecord> &r_source);
	void _flush_retirement_batch();
	bool _drain_retired_catalog_step();
	void _begin_refresh_drain(RefreshPhase p_resume_phase);
	void _finish_refresh_drain();
	bool _begin_refresh();
	bool _begin_raw_scan();
	bool _collect_one_raw_path();
	bool _collect_one_path();
	bool _invalidate_one_cached_script();
	static void _project_document_thread(void *p_userdata);
	void _wait_for_projection();
	bool _project_one_document();
	void _reset_reconcile();
	void _restart_refresh();
	static void _prepare_journal_batch_thread(void *p_userdata);
	void _wait_for_journal_preparation();
	bool _reconcile(RefreshOutcome &r_outcome);
	bool _process_refresh_step(RefreshOutcome &r_outcome);

	void _reset_snapshot_payload();
	void _reset_snapshot();
	Array _take_abandoned_snapshot_data();
	bool _emit_pending_snapshot_message(SnapshotCompletion &r_completion);
	bool _append_snapshot_value(Array &r_values, const Variant &p_value);
	bool _flush_snapshot_payload();
	void _fail_snapshot(const String &p_code, const String &p_message, bool p_retryable, SnapshotCompletion &r_completion);
	void _finish_snapshot(SnapshotCompletion &r_completion);

public:
	void initialize(BridgeRevisionClock *p_revision_clock);
	void shutdown();
	void request_refresh();
	void invalidate_saved_paths(const Vector<String> &p_paths);
	bool process_refresh(uint64_t p_budget_usec, RefreshOutcome &r_outcome);

	Error begin_snapshot(uint64_t p_request_id, uint64_t p_now_usec, const Dictionary &p_context, Dictionary &r_error_data);
	bool process_snapshot(uint64_t p_now_usec, uint64_t p_budget_usec, SnapshotCompletion &r_completion);
	Array cancel_snapshot(uint64_t p_request_id);

	ScriptDeltaJournal::QueryResult query_delta(uint64_t p_after_script_graph_revision) const;
	bool is_catalog_ready() const;
	bool has_catalog_limit_failure() const;
	bool is_snapshot_active() const;
	bool has_pending_work() const;
	uint64_t get_script_graph_revision() const;
};
