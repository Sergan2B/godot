/**************************************************************************/
/*  bridge_transport_worker.h                                             */
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

#include "core/io/file_access.h"
#include "core/os/mutex.h"
#include "core/os/semaphore.h"
#include "core/os/thread.h"
#include "core/string/ustring.h"
#include "core/templates/list.h"
#include "core/templates/safe_refcount.h"
#include "core/variant/variant.h"

class MainThreadDispatcher;

class BridgeTransportWorker {
public:
	enum StopResult {
		STOP_NOT_RUNNING,
		STOPPED,
		STOP_TIMED_OUT,
	};

	static constexpr uint64_t DEFAULT_STOP_TIMEOUT_USEC = 1000000;
	static constexpr uint64_t DEFAULT_START_TIMEOUT_USEC = 5000000;

public:
	struct Completion {
		enum Kind {
			KIND_REQUEST,
			KIND_RESOURCE_SNAPSHOT_MESSAGE,
			KIND_RESOURCE_SNAPSHOT_END,
			KIND_RESOURCE_SNAPSHOT_ABORT,
		};

		Kind kind = KIND_REQUEST;
		uint64_t request_id = 0;
		Dictionary result;
		Array server_messages;
		Ref<FileAccess> snapshot_spool;
		int snapshot_chunk_count = 0;
		Dictionary snapshot_message;
		Array abandoned_messages;
		bool is_error = false;
		String error_code;
		String error_message;
		bool error_retryable = false;
		Dictionary error_data;
		bool cancel_dispatch = false;
	};

	struct Context {
		SafeRefCount references;
		SafeFlag stop_requested;
		SafeFlag exited;
		SafeFlag startup_done;
		Semaphore wakeup;
		Semaphore startup;
		String project_root;
		String project_id;
		String editor_session_id;
		Mutex dispatcher_mutex;
		MainThreadDispatcher *dispatcher = nullptr;
		Mutex completion_mutex;
		List<Completion> completed_requests;
		Mutex notification_mutex;
		List<Dictionary> notifications;
		uint64_t notification_bytes = 0;
		Error startup_error = OK;

		Context() {
			references.init(2);
		}
	};

private:
	Thread *thread = nullptr;
	Context *context = nullptr;

	static void _thread_main(void *p_userdata);
	static void _release_context(Context *p_context);

public:
	Error start();
	Error start(const String &p_project_root, uint64_t p_timeout_usec = DEFAULT_START_TIMEOUT_USEC);
	Error start(const String &p_project_root, MainThreadDispatcher *p_dispatcher, uint64_t p_timeout_usec = DEFAULT_START_TIMEOUT_USEC);
	StopResult stop(uint64_t p_timeout_usec = DEFAULT_STOP_TIMEOUT_USEC);
	void wake();
	void complete_request(uint64_t p_request_id);
	void complete_request(uint64_t p_request_id, const Dictionary &p_result, const Array &p_server_messages = Array());
	void complete_request_error(uint64_t p_request_id, const String &p_code, const String &p_message, bool p_retryable, const Dictionary &p_data = Dictionary());
	void stage_resource_snapshot_message(uint64_t p_request_id, const Dictionary &p_message);
	void complete_resource_snapshot(uint64_t p_request_id, const Dictionary &p_result, const Dictionary &p_end_message);
	void abort_resource_snapshot(uint64_t p_request_id, const Array &p_abandoned_messages = Array());
	bool publish_notification(const Dictionary &p_notification);
	String get_project_id() const;
	String get_editor_session_id() const;

	bool is_running() const;

	~BridgeTransportWorker();
};
