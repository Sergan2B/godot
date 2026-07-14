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

#include "main_thread_dispatcher.h"

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

	static void _dispatch_command(const MainThreadDispatcher::Command &p_command, void *p_userdata);

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
