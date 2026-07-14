/**************************************************************************/
/*  bridge_transport_worker.cpp                                           */
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

#include "bridge_transport_worker.h"

#include "core/os/os.h"

void BridgeTransportWorker::_release_context(Context *p_context) {
	if (p_context->references.unref()) {
		memdelete(p_context);
	}
}

void BridgeTransportWorker::_thread_main(void *p_userdata) {
	Context *worker_context = static_cast<Context *>(p_userdata);
	Thread::set_name("CodexBridgeTransport");

	while (!worker_context->stop_requested.is_set()) {
		worker_context->wakeup.wait();
	}

	worker_context->exited.set();
	_release_context(worker_context);
}

Error BridgeTransportWorker::start() {
	ERR_FAIL_COND_V_MSG(thread != nullptr, ERR_ALREADY_IN_USE, "Codex bridge transport worker is already started.");

	context = memnew(Context);
	thread = memnew(Thread);
	if (thread->start(_thread_main, context) == Thread::UNASSIGNED_ID) {
		memdelete(thread);
		thread = nullptr;
		_release_context(context);
		_release_context(context);
		context = nullptr;
		return ERR_CANT_CREATE;
	}

	return OK;
}

BridgeTransportWorker::StopResult BridgeTransportWorker::stop(uint64_t p_timeout_usec) {
	if (!thread) {
		return STOP_NOT_RUNNING;
	}

	Context *stopping_context = context;
	stopping_context->stop_requested.set();
	stopping_context->wakeup.post();

	const uint64_t deadline_usec = OS::get_singleton()->get_ticks_usec() + p_timeout_usec;
	while (!stopping_context->exited.is_set() && OS::get_singleton()->get_ticks_usec() < deadline_usec) {
		OS::get_singleton()->delay_usec(1000);
	}

	StopResult result = STOPPED;
	if (stopping_context->exited.is_set()) {
		thread->wait_to_finish();
	} else {
		result = STOP_TIMED_OUT;
	}

	memdelete(thread);
	thread = nullptr;
	context = nullptr;
	_release_context(stopping_context);
	return result;
}

void BridgeTransportWorker::wake() {
	if (context) {
		context->wakeup.post();
	}
}

bool BridgeTransportWorker::is_running() const {
	return thread && context && !context->exited.is_set();
}

BridgeTransportWorker::~BridgeTransportWorker() {
	stop();
}
