/**************************************************************************/
/*  resource_graph_adapter.h                                              */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#pragma once

#include "resource_delta_journal.h"

#include "core/templates/hash_map.h"
#include "core/templates/hash_set.h"
#include "core/templates/vector.h"
#include "core/variant/variant.h"

class BridgeRevisionClock;
class EditorFileSystemDirectory;

class ResourceGraphAdapter {
public:
	static constexpr uint32_t MAX_RESOURCES = 250000;
	static constexpr uint32_t MAX_DEPENDENCIES = 2000000;
	static constexpr uint32_t MAX_DEPENDENCIES_PER_RESOURCE = 4096;
	static constexpr uint32_t MAX_PATH_BYTES = 1024;
	static constexpr uint32_t SNAPSHOT_CHUNK_BYTES = 256 * 1024;
	static constexpr uint64_t SNAPSHOT_WINDOW_BYTES = 32 * 1024 * 1024;
	static constexpr uint64_t SNAPSHOT_TIMEOUT_USEC = 120000000;
	static constexpr uint64_t RESOURCE_BUDGET_USEC = 500;
	static constexpr uint32_t SNAPSHOT_BUILD_CHUNK_BYTES = 2 * 1024;
	static constexpr uint32_t BULK_INVALIDATION_RECORD_DELTA = 512;

	struct RefreshOutcome {
		bool changed = false;
		bool invalidated = false;
		uint64_t last_contiguous_resource_revision = 0;
		uint64_t current_resource_revision = 0;
		Dictionary revisions;
	};

	struct SnapshotCompletion {
		bool ready = false;
		uint64_t request_id = 0;
		bool is_error = false;
		String error_code;
		String error_message;
		bool error_retryable = false;
		Dictionary error_data;
		Dictionary result;
		Array server_messages;
	};

private:
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

		_FORCE_INLINE_ bool operator<(const RefreshFile &p_other) const {
			return path < p_other.path;
		}
	};

	enum RefreshPhase {
		REFRESH_IDLE,
		REFRESH_COLLECT_PATHS,
		REFRESH_OBSERVE,
	};

	BridgeRevisionClock *revision_clock = nullptr;
	ResourceDeltaJournal journal;
	HashMap<String, CatalogRecord> catalog;
	bool catalog_ready = false;
	bool catalog_limit_exceeded = false;

	bool refresh_requested = false;
	RefreshPhase refresh_phase = REFRESH_IDLE;
	Vector<DirectoryCursor> directory_stack;
	Vector<RefreshFile> refresh_files;
	int refresh_path_index = 0;
	HashMap<String, CatalogRecord> observed_catalog;
	HashSet<String> pending_reimport_paths;
	HashSet<String> active_reimport_paths;
	uint64_t observed_dependency_count = 0;
	bool refresh_limit_exceeded = false;

	bool snapshot_active = false;
	bool snapshot_waiting_for_terminal = false;
	uint64_t snapshot_request_id = 0;
	uint64_t snapshot_started_usec = 0;
	String snapshot_id;
	uint64_t snapshot_resource_revision = 0;
	Dictionary snapshot_revisions;
	Dictionary snapshot_context;
	Vector<String> snapshot_keys;
	int snapshot_record_index = 0;
	bool snapshot_resource_added = false;
	int snapshot_dependency_index = 0;
	int snapshot_diagnostic_index = 0;
	Array snapshot_chunks;
	Array snapshot_resources;
	Array snapshot_dependencies;
	Array snapshot_diagnostics;
	uint64_t snapshot_payload_bytes = 0;
	String snapshot_checksum_input;
	uint64_t snapshot_resource_count = 0;
	uint64_t snapshot_dependency_count = 0;
	uint64_t snapshot_diagnostic_count = 0;

	static String _sha256_hex_utf8(const String &p_value);
	static String _make_snapshot_id();
	static Dictionary _make_resource_ref(const String &p_path, int64_t p_uid);
	static void _stamp_record(CatalogRecord &r_record, uint64_t p_resource_revision);
	static void _collect_sorted_keys(const HashMap<String, CatalogRecord> &p_catalog, Vector<String> &r_keys);

	bool _begin_refresh();
	bool _collect_one_path();
	bool _observe_resource(const RefreshFile &p_file, CatalogRecord &r_record);
	void _finish_refresh(RefreshOutcome &r_outcome);
	void _reset_snapshot_payload();
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
	void cancel_snapshot(uint64_t p_request_id);

	ResourceDeltaJournal::QueryResult query_delta(uint64_t p_after_resource_revision) const;
	bool is_catalog_ready() const;
	bool is_snapshot_active() const;
	bool has_pending_work() const;
	uint64_t get_resource_revision() const;
};
