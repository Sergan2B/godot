/**************************************************************************/
/*  codex_bridge_service.h                                                */
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

#include "bridge_frame_telemetry.h"
#include "bridge_revision_clock.h"
#include "main_thread_dispatcher.h"
#include "resource_graph_adapter.h"
#include "scene_state_adapter.h"
#include "script_graph_adapter.h"

#include "editor/plugins/editor_plugin.h"

#include "modules/codex_bridge/transport/bridge_transport_worker.h"

class CodexBridgeService : public EditorPlugin {
	GDCLASS(CodexBridgeService, EditorPlugin);

public:
	enum State {
		STATE_STOPPED,
		STATE_STARTING,
		STATE_RUNNING,
		STATE_STOPPING,
	};

private:
	static CodexBridgeService *singleton;

	State state = STATE_STOPPED;
	MainThreadDispatcher dispatcher;
	BridgeTransportWorker transport_worker;
	BridgeRevisionClock revision_clock;
	ResourceGraphAdapter resource_graph_adapter;
	SceneStateAdapter scene_state_adapter;
	ScriptGraphAdapter script_graph_adapter;
	BridgeFrameTelemetry frame_telemetry;
	bool editor_signals_connected = false;
	int work_lane_turn = 0;
	bool scene_change_pending = false;
	String pending_property;

	static void _dispatch_command(const MainThreadDispatcher::Command &p_command, void *p_userdata);
	Dictionary _make_context() const;
	String _get_current_scene_id() const;
	void _connect_editor_signals();
	void _disconnect_editor_signals();
	void _publish_event(const String &p_event_type, const String &p_property = String(), bool p_scene_mutation = false);
	void _on_selection_changed();
	void _on_scene_changed();
	void _on_property_edited(const String &p_property);
	void _on_undo_redo_version_changed();
	void _on_filesystem_changed();
	void _on_resources_reimported(const Vector<String> &p_paths);
	void _on_resources_reload(const PackedStringArray &p_paths);
	void _on_project_settings_changed();
	void _flush_scene_change();
	void _complete_snapshot(uint64_t p_request_id);
	void _complete_resource_delta(uint64_t p_request_id, uint64_t p_after_resource_revision);
	void _complete_scene_delta(uint64_t p_request_id, uint64_t p_after_scene_graph_revision);
	void _complete_script_delta(uint64_t p_request_id, uint64_t p_after_script_graph_revision);
	void _process_resource_graph(uint64_t p_budget_usec);
	void _process_scene_graph(uint64_t p_budget_usec);
	void _process_script_graph(uint64_t p_budget_usec);

protected:
	void _notification(int p_what);

public:
	static CodexBridgeService *get_singleton();

	Error start();
	void stop();

	State get_service_state() const;
	MainThreadDispatcher &get_dispatcher();

	CodexBridgeService();
	~CodexBridgeService();
};
