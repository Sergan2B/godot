/**************************************************************************/
/*  codex_bridge_service.cpp                                              */
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

#include "codex_bridge_service.h"

#include "core/string/print_string.h"

CodexBridgeService *CodexBridgeService::singleton = nullptr;

void CodexBridgeService::_dispatch_command(const MainThreadDispatcher::Command &p_command, void *p_userdata) {
	CodexBridgeService *service = static_cast<CodexBridgeService *>(p_userdata);
	ERR_FAIL_NULL(service);

	switch (p_command.type) {
		case MainThreadDispatcher::COMMAND_NO_OP:
			break;
	}
}

void CodexBridgeService::_notification(int p_what) {
	switch (p_what) {
		case NOTIFICATION_ENTER_TREE: {
			start();
		} break;
		case NOTIFICATION_PROCESS: {
			if (state == STATE_RUNNING) {
				dispatcher.process(_dispatch_command, this);
			}
		} break;
		case NOTIFICATION_EXIT_TREE: {
			stop();
		} break;
	}
}

CodexBridgeService *CodexBridgeService::get_singleton() {
	return singleton;
}

Error CodexBridgeService::start() {
	if (state == STATE_RUNNING) {
		return OK;
	}
	ERR_FAIL_COND_V_MSG(state != STATE_STOPPED, ERR_BUSY, "Codex bridge service cannot start while changing state.");

	state = STATE_STARTING;
	dispatcher.start_accepting();
	const Error error = transport_worker.start();
	if (error != OK) {
		dispatcher.begin_shutdown();
		state = STATE_STOPPED;
		ERR_PRINT("[codex_bridge] Failed to start the transport worker.");
		return error;
	}

	state = STATE_RUNNING;
	print_verbose("[codex_bridge] Service started.");
	return OK;
}

void CodexBridgeService::stop() {
	if (state == STATE_STOPPED || state == STATE_STOPPING) {
		return;
	}

	state = STATE_STOPPING;
	dispatcher.begin_shutdown();
	const BridgeTransportWorker::StopResult stop_result = transport_worker.stop();
	if (stop_result == BridgeTransportWorker::STOP_TIMED_OUT) {
		ERR_PRINT("[codex_bridge] Transport worker did not stop within the shutdown timeout.");
	}
	state = STATE_STOPPED;
	print_verbose("[codex_bridge] Service stopped.");
}

CodexBridgeService::State CodexBridgeService::get_service_state() const {
	return state;
}

MainThreadDispatcher &CodexBridgeService::get_dispatcher() {
	return dispatcher;
}

CodexBridgeService::CodexBridgeService() {
	ERR_FAIL_COND_MSG(singleton != nullptr, "Only one Codex bridge service may exist.");
	singleton = this;
	set_process(true);
}

CodexBridgeService::~CodexBridgeService() {
	stop();
	if (singleton == this) {
		singleton = nullptr;
	}
}
