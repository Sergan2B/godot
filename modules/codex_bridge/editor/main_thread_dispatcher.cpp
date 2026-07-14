/**************************************************************************/
/*  main_thread_dispatcher.cpp                                            */
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

#include "main_thread_dispatcher.h"

#include "core/os/os.h"
#include "core/os/thread.h"

uint64_t MainThreadDispatcher::_default_clock(void *p_userdata) {
	return OS::get_singleton()->get_ticks_usec();
}

void MainThreadDispatcher::start_accepting() {
	MutexLock lock(queue_mutex);
	accepting = true;
}

uint32_t MainThreadDispatcher::begin_shutdown() {
	MutexLock lock(queue_mutex);
	accepting = false;
	const uint32_t canceled = queue.size();
	queue.clear();
	return canceled;
}

MainThreadDispatcher::EnqueueResult MainThreadDispatcher::enqueue(const Command &p_command) {
	MutexLock lock(queue_mutex);
	if (!accepting) {
		return ENQUEUE_STOPPING;
	}
	if (uint32_t(queue.size()) >= MAX_QUEUE_SIZE) {
		return ENQUEUE_FULL;
	}
	queue.push_back(p_command);
	return ENQUEUE_OK;
}

bool MainThreadDispatcher::cancel(uint64_t p_request_id) {
	MutexLock lock(queue_mutex);
	for (List<Command>::Element *element = queue.front(); element; element = element->next()) {
		if (element->get().request_id == p_request_id) {
			queue.erase(element);
			return true;
		}
	}
	return false;
}

MainThreadDispatcher::ProcessStats MainThreadDispatcher::process(CommandHandler p_handler, void *p_handler_userdata, uint32_t p_max_commands, uint64_t p_budget_usec, Clock p_clock, void *p_clock_userdata) {
	ProcessStats stats;
	ERR_FAIL_COND_V_MSG(!Thread::is_main_thread(), stats, "Codex bridge commands must be dispatched on the main thread.");
	ERR_FAIL_NULL_V(p_handler, stats);

	Clock clock = p_clock ? p_clock : _default_clock;
	const uint64_t started_usec = clock(p_clock_userdata);
	uint64_t now_usec = started_usec;

	while (stats.consumed < p_max_commands) {
		now_usec = clock(p_clock_userdata);
		if (stats.consumed > 0 && now_usec - started_usec >= p_budget_usec) {
			break;
		}

		Command command;
		{
			MutexLock lock(queue_mutex);
			List<Command>::Element *front = queue.front();
			if (!front) {
				break;
			}
			command = front->get();
			queue.pop_front();
		}

		stats.consumed++;
		if (command.deadline_usec > 0 && now_usec >= command.deadline_usec) {
			stats.expired++;
			continue;
		}

		p_handler(command, p_handler_userdata);
		stats.processed++;
	}

	stats.elapsed_usec = clock(p_clock_userdata) - started_usec;
	stats.remaining = get_queue_size();
	return stats;
}

uint32_t MainThreadDispatcher::get_queue_size() const {
	MutexLock lock(queue_mutex);
	return queue.size();
}

bool MainThreadDispatcher::is_accepting() const {
	MutexLock lock(queue_mutex);
	return accepting;
}
