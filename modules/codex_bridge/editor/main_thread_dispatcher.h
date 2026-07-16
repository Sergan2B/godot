/**************************************************************************/
/*  main_thread_dispatcher.h                                              */
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

#include "core/os/mutex.h"
#include "core/templates/list.h"
#include "core/variant/variant.h"

class MainThreadDispatcher {
public:
	enum CommandType {
		COMMAND_NO_OP,
		COMMAND_INITIALIZE,
		COMMAND_PING,
		COMMAND_CAPABILITIES,
		COMMAND_EDITOR_SNAPSHOT,
		COMMAND_RESOURCE_SNAPSHOT,
		COMMAND_RESOURCE_DELTA,
		COMMAND_CANCEL,
		COMMAND_SHUTDOWN,
	};

	struct Command {
		CommandType type = COMMAND_NO_OP;
		uint64_t request_id = 0;
		uint64_t deadline_usec = 0;
		Dictionary params;
	};

	enum EnqueueResult {
		ENQUEUE_OK,
		ENQUEUE_FULL,
		ENQUEUE_STOPPING,
	};

	struct ProcessStats {
		uint32_t processed = 0;
		uint32_t expired = 0;
		uint32_t consumed = 0;
		uint32_t remaining = 0;
		uint64_t elapsed_usec = 0;
	};

	typedef void (*CommandHandler)(const Command &p_command, void *p_userdata);
	typedef uint64_t (*Clock)(void *p_userdata);

	static constexpr uint32_t MAX_QUEUE_SIZE = 64;
	static constexpr uint32_t MAX_COMMANDS_PER_FRAME = 8;
	static constexpr uint64_t MAX_PROCESS_USEC_PER_FRAME = 2000;

private:
	mutable Mutex queue_mutex;
	List<Command> queue;
	bool accepting = false;

	static uint64_t _default_clock(void *p_userdata);

public:
	void start_accepting();
	uint32_t begin_shutdown();

	EnqueueResult enqueue(const Command &p_command);
	bool cancel(uint64_t p_request_id);

	ProcessStats process(CommandHandler p_handler, void *p_handler_userdata = nullptr, uint32_t p_max_commands = MAX_COMMANDS_PER_FRAME, uint64_t p_budget_usec = MAX_PROCESS_USEC_PER_FRAME, Clock p_clock = nullptr, void *p_clock_userdata = nullptr);

	uint32_t get_queue_size() const;
	bool is_accepting() const;
};
