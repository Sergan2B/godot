/**************************************************************************/
/*  runtime_debugger_adapter.h                                           */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#pragma once

#include "core/templates/hash_map.h"
#include "core/variant/variant.h"
#include "editor/debugger/editor_debugger_plugin.h"

class BridgeRevisionClock;
class BridgeTransportWorker;
class ScriptEditorDebugger;

class RuntimeLifecyclePolicy {
public:
	static bool is_terminal(const String &p_state);
	static bool is_reconnect(const String &p_state);
	static String classify_debugger_stop(const String &p_state, bool p_editor_playing, bool p_normal_quit_requested);
	static String classify_process_exit(const String &p_state, bool p_normal_quit_requested);
};

class RuntimeDebuggerAdapter : public EditorDebuggerPlugin {
	GDCLASS(RuntimeDebuggerAdapter, EditorDebuggerPlugin);

public:
	static constexpr int MAX_TREE_NODES = 10000;
	static constexpr int MAX_TREE_DEPTH = 256;
	static constexpr int MAX_PROPERTIES = 512;
	static constexpr int MAX_OBJECT_BYTES = 262144;
	static constexpr int MAX_DIAGNOSTICS = 200;
	static constexpr int MAX_DIAGNOSTIC_BYTES = 262144;
	static constexpr int MAX_STACKS = 64;
	static constexpr int MAX_STACK_FRAMES = 128;
	static constexpr int MAX_STACK_BYTES = 262144;
	static constexpr int MAX_SCREENSHOT_WIDTH = 1280;
	static constexpr int MAX_SCREENSHOT_HEIGHT = 720;
	static constexpr int MAX_SCREENSHOT_BYTES = 524288;
	static constexpr int MAX_SCREENSHOT_SOURCE_BYTES = 33554432;

private:
	BridgeTransportWorker *transport = nullptr;
	BridgeRevisionClock *revisions = nullptr;
	HashMap<int, ObjectID> debugger_sessions;
	int active_debugger_session = -1;

	String runtime_session_id;
	String state;
	String origin;
	String target;
	String scene_path;
	String terminal_reason;
	bool normal_quit_requested = false;
	bool internal_forced_stop = false;
	uint64_t state_since_usec = 0;
	uint64_t disconnected_since_usec = 0;

	Array runtime_tree;
	String runtime_tree_checksum;
	HashMap<String, uint64_t> object_ids;
	HashMap<uint64_t, String> opaque_by_object_id;
	Array diagnostics;
	int diagnostic_bytes = 0;
	Dictionary stacks;
	Array stack_order;
	int stack_bytes = 0;

	uint64_t pending_run = 0;
	uint64_t pending_stop = 0;
	uint64_t pending_pause = 0;
	uint64_t pending_continue = 0;
	uint64_t pending_snapshot = 0;
	uint64_t pending_object = 0;
	uint64_t pending_capture = 0;
	Dictionary pending_snapshot_params;
	Dictionary pending_object_params;
	Dictionary pending_capture_params;
	String pending_tree_correlation;
	String pending_object_correlation;
	uint64_t last_capture_usec = 0;

	static void _bind_methods() {}
	ScriptEditorDebugger *_get_debugger(int p_session_id) const;
	ScriptEditorDebugger *_get_active_debugger() const;
	String _make_runtime_session_id() const;
	String _make_opaque_id(const String &p_prefix, const String &p_domain, const String &p_value) const;
	Dictionary _make_context() const;
	Dictionary _make_limits(bool p_truncated) const;
	Dictionary _make_state_result() const;
	Dictionary _safe_coordinates() const;
	bool _is_terminal() const;
	bool _guard(uint64_t p_request_id, const Dictionary &p_params, bool p_require_running, bool p_allow_paused = true);
	void _begin_session(const String &p_origin, const String &p_target, const String &p_scene_path);
	bool _transition(const String &p_state, const String &p_event_type, const Array &p_changed_domains, const String &p_reason = String(), bool p_advance = true);
	void _publish_event(const String &p_event_type, const Array &p_changed_domains);
	void _publish_invalidated(const String &p_runtime_session_id, uint64_t p_last_contiguous_event_seq, const String &p_reason);
	void _retire_live_data();
	void _fail_pending_live_requests(const String &p_code, const String &p_message, bool p_retryable);
	void _complete_pending_controls(const String &p_confirmed_state);

	String _redact_message(const String &p_message, bool &r_redacted, bool &r_truncated) const;
	void _append_diagnostic(const String &p_severity, const String &p_source, const String &p_message, const String &p_script_path = String(), int p_line = 0, const String &p_function = String(), const String &p_stack_id = String());
	String _append_stack(const String &p_kind, const Array &p_frames, const String &p_identity);
	Array _project_tree(const Array &p_serialized, bool &r_truncated);
	Dictionary _project_object(const Array &p_serialized, bool p_game_truncated, bool &r_truncated);
	void _complete_snapshot(const Array &p_tree, bool p_tree_truncated);

	void _on_started(int p_session_id);
	void _on_stopped(int p_session_id);
	void _on_editor_stop_requested();
	void _on_stop_requested(int p_session_id);
	void _on_breaked(bool p_really_did, bool p_can_debug, const String &p_message, bool p_has_stackdump, int p_session_id);
	void _on_output(const String &p_message, int p_level, int p_session_id);
	void _on_runtime_error(const Dictionary &p_error, int p_session_id);
	void _on_runtime_stack(int64_t p_thread_id, const Array &p_frames, int p_session_id);
	void _on_screenshot(int p_width, int p_height, const String &p_path, const Rect2i &p_rect);

public:
	void initialize(BridgeTransportWorker *p_transport, BridgeRevisionClock *p_revisions);
	void shutdown();
	void process();
	void cancel(uint64_t p_request_id);

	void run(uint64_t p_request_id, const Dictionary &p_params);
	void stop(uint64_t p_request_id, const Dictionary &p_params);
	void pause(uint64_t p_request_id, const Dictionary &p_params);
	void continue_run(uint64_t p_request_id, const Dictionary &p_params);
	void snapshot(uint64_t p_request_id, const Dictionary &p_params);
	void inspect_object(uint64_t p_request_id, const Dictionary &p_params);
	void get_stack(uint64_t p_request_id, const Dictionary &p_params);
	void capture_viewport(uint64_t p_request_id, const Dictionary &p_params);

	virtual bool has_capture(const String &p_capture) const override;
	virtual bool capture(const String &p_message, const Array &p_data, int p_session_id) override;
	virtual void setup_session(int p_session_id) override;
};
