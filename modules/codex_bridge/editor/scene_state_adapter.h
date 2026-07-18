/**************************************************************************/
/*  scene_state_adapter.h                                                 */
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

#include "scene_delta_journal.h"

#include "core/object/ref_counted.h"
#include "core/templates/hash_map.h"
#include "core/templates/hash_set.h"
#include "core/templates/rb_map.h"
#include "core/templates/rb_set.h"
#include "core/templates/vector.h"
#include "core/variant/variant.h"

class BridgeRevisionClock;
class EditorFileSystemDirectory;
class PackedScene;
class Resource;
class SceneState;

class SceneStateAdapter {
public:
	static constexpr uint32_t MAX_SCENES = 100000;
	static constexpr uint32_t MAX_NODES = 1000000;
	static constexpr uint32_t MAX_PROPERTIES = 4000000;
	static constexpr uint32_t MAX_RELATIONS = 4000000;
	static constexpr uint32_t MAX_DIAGNOSTICS = 100000;
	static constexpr uint32_t MAX_PATH_BYTES = 1024;
	static constexpr uint32_t SNAPSHOT_CHUNK_BYTES = 256 * 1024;
	static constexpr uint64_t SNAPSHOT_WINDOW_BYTES = 32 * 1024 * 1024;
	static constexpr uint64_t SNAPSHOT_TIMEOUT_USEC = 120000000;
	static constexpr uint64_t SCENE_BUDGET_USEC = 1200;
	// Some editor APIs used by scene project-context capture are indivisible.
	// Reserve the dispatcher lane while scene bulk is pending so any control
	// command and the indivisible scene call run on separate frames.
	static constexpr uint64_t FRAME_SAFETY_MARGIN_USEC = 800;
	// ProjectSettings reads and Variant projection are indivisible within a
	// slice. Keep these batches small enough to preserve the 2 ms frame ceiling
	// on slower Windows hosts while still advancing the capture every frame.
	static constexpr int PROJECT_SETTINGS_PER_SLICE = 32;
	static constexpr int PROJECT_ACTIONS_PER_SLICE = 4;
	static constexpr uint32_t MAX_INSTANCE_DEPTH = 64;

	struct RefreshOutcome {
		bool changed = false;
		bool invalidated = false;
		uint64_t last_contiguous_scene_graph_revision = 0;
		uint64_t current_scene_graph_revision = 0;
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
	friend struct SceneStateAdapterTestAccess;

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

	enum RefreshPhase {
		REFRESH_IDLE,
		REFRESH_COLLECT,
		REFRESH_BEGIN_SCENE,
		REFRESH_NODE_BEGIN,
		REFRESH_NODE_PROPERTY,
		REFRESH_NODE_FINISH,
		REFRESH_CONNECTION,
		REFRESH_SUBRESOURCE,
		REFRESH_SCENE_FINISH,
		REFRESH_PROJECT_CONTEXT,
		REFRESH_RECONCILE,
	};

	BridgeRevisionClock *revision_clock = nullptr;
	SceneDeltaJournal journal;
	RBMap<String, CatalogRecord> catalog;
	RBMap<String, CatalogRecord> observed_catalog;
	Array diagnostics;
	Array observed_diagnostics;
	String diagnostics_checksum;
	String observed_diagnostics_checksum;
	bool catalog_ready = false;
	bool refresh_requested = false;
	bool refresh_limit_exceeded = false;
	RefreshPhase refresh_phase = REFRESH_IDLE;
	Vector<DirectoryCursor> directory_stack;
	Vector<String> scene_paths;
	int scene_path_index = 0;
	uint64_t target_scene_graph_revision = 1;
	uint64_t observed_node_count = 0;
	uint64_t observed_property_count = 0;
	uint64_t observed_relation_count = 0;
	uint64_t observed_diagnostic_count = 0;

	String active_path;
	bool active_load_requested = false;
	Ref<PackedScene> active_scene;
	Ref<SceneState> active_state;
	CatalogRecord active_record;
	Array active_nodes;
	Array active_connections;
	Array active_editable_instances;
	Array active_subresources;
	Array active_animation_tracks;
	HashSet<String> active_node_paths;
	HashMap<ObjectID, RBSet<String>> active_resource_ownership;
	int active_node_index = 0;
	int active_property_index = 0;
	Dictionary active_node;
	Array active_node_properties;
	Vector<String> active_deferred_properties;
	int active_connection_index = 0;
	Vector<Ref<Resource>> active_subresource_values;
	int active_subresource_index = 0;
	Array project_context;
	String project_context_checksum;
	String active_project_context_checksum;
	enum ProjectContextPhase {
		PROJECT_CONTEXT_COLLECT_KEYS,
		PROJECT_CONTEXT_SETTINGS,
		PROJECT_CONTEXT_ACTIONS,
		PROJECT_CONTEXT_FINALIZE,
	};
	ProjectContextPhase project_context_phase = PROJECT_CONTEXT_COLLECT_KEYS;
	bool project_context_keys_valid = false;
	bool project_context_keys_limit_exceeded = false;
	int64_t project_context_keys_task = -1;
	uint64_t project_context_keys_generation = 0;
	uint64_t project_context_keys_task_generation = 0;
	Vector<String> project_context_source_keys;
	int project_context_source_index = 0;
	RBMap<String, Dictionary> project_context_values;
	Vector<String> project_context_actions;
	int project_context_action_index = 0;
	Array reconcile_operations;
	uint64_t reconcile_previous_revision = 0;
	uint64_t reconcile_next_revision = 0;
	bool reconcile_preparing = false;
	int64_t journal_prepare_task = -1;
	Error journal_prepare_error = OK;
	SceneDeltaJournal::PreparedBatch journal_prepared_batch;

	bool snapshot_active = false;
	uint64_t snapshot_request_id = 0;
	uint64_t snapshot_started_usec = 0;
	String snapshot_id;
	uint64_t snapshot_resource_revision = 0;
	uint64_t snapshot_scene_graph_revision = 0;
	Dictionary snapshot_revisions;
	Dictionary snapshot_context;
	RBMap<String, CatalogRecord>::Element *snapshot_record = nullptr;
	Dictionary snapshot_pending_message;
	Array snapshot_scenes;
	Array snapshot_diagnostics;
	uint32_t snapshot_chunk_count = 0;
	int snapshot_phase = 0;

	static bool _is_scene_path(const String &p_path);
	static Dictionary _make_resource_ref(const String &p_path);
	static String _make_snapshot_id();
	static String _sha256_hex(const String &p_value);
	static String _canonical_node_path(const String &p_path);
	static bool _is_safe_node_path(const String &p_path, bool p_allow_empty = false);
	static void _collect_resource_ownership(const Variant &p_value, const String &p_path, int p_depth, HashMap<ObjectID, RBSet<String>> &r_ownership, Vector<Ref<Resource>> &r_subresources);
	bool _append_diagnostic(Array &r_diagnostics, const Dictionary &p_diagnostic);

	void _reset_active_scene();
	bool _collect_one_path();
	bool _begin_active_scene();
	bool _begin_active_node();
	bool _observe_active_property();
	bool _finish_active_node();
	bool _observe_connection();
	bool _observe_subresource();
	bool _finish_active_scene();
	void _reset_project_context_capture();
	bool _reject_project_context_capture();
	static void _collect_project_context_keys_thread(void *p_userdata);
	void _wait_for_project_context_keys();
	bool _capture_project_context();
	void _reset_reconcile();
	static void _prepare_journal_batch_thread(void *p_userdata);
	void _wait_for_journal_preparation();
	bool _reconcile(RefreshOutcome &r_outcome);
	bool _process_refresh_step(RefreshOutcome &r_outcome);

	void _reset_snapshot();
	Array _take_abandoned_snapshot_data();
	bool _emit_pending_snapshot_message(SnapshotCompletion &r_completion);
	bool _flush_snapshot_chunk();
	void _fail_snapshot(const String &p_code, const String &p_message, bool p_retryable, SnapshotCompletion &r_completion);
	void _finish_snapshot(SnapshotCompletion &r_completion);

public:
	void initialize(BridgeRevisionClock *p_revision_clock);
	void shutdown();
	void request_refresh();
	void invalidate_project_context();
	bool process_refresh(uint64_t p_budget_usec, RefreshOutcome &r_outcome);

	Error begin_snapshot(uint64_t p_request_id, uint64_t p_now_usec, const Dictionary &p_context, Dictionary &r_error_data);
	bool process_snapshot(uint64_t p_now_usec, uint64_t p_budget_usec, SnapshotCompletion &r_completion);
	Array cancel_snapshot(uint64_t p_request_id);

	SceneDeltaJournal::QueryResult query_delta(uint64_t p_after_scene_graph_revision) const;
	bool is_catalog_ready() const;
	bool is_snapshot_active() const;
	bool has_pending_work() const;
	uint64_t get_scene_graph_revision() const;
};
