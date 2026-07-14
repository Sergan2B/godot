/**************************************************************************/
/*  test_codex_bridge.cpp                                                 */
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

#include "tests/test_macros.h"

TEST_FORCE_LINK(test_codex_bridge)

#include "modules/modules_enabled.gen.h"

#ifdef MODULE_CODEX_BRIDGE_ENABLED

#include "core/templates/local_vector.h"

#include "modules/codex_bridge/editor/main_thread_dispatcher.h"
#include "modules/codex_bridge/transport/bridge_transport_worker.h"

namespace TestCodexBridge {

struct HandlerContext {
	LocalVector<uint64_t> handled_ids;
	uint64_t now_usec = 0;
	uint64_t advance_usec = 0;
};

static void record_command(const MainThreadDispatcher::Command &p_command, void *p_userdata) {
	HandlerContext *context = static_cast<HandlerContext *>(p_userdata);
	context->handled_ids.push_back(p_command.request_id);
	context->now_usec += context->advance_usec;
}

static uint64_t test_clock(void *p_userdata) {
	return static_cast<HandlerContext *>(p_userdata)->now_usec;
}

static MainThreadDispatcher::Command make_command(uint64_t p_request_id, uint64_t p_deadline_usec = 0) {
	MainThreadDispatcher::Command command;
	command.request_id = p_request_id;
	command.deadline_usec = p_deadline_usec;
	return command;
}

TEST_CASE("[CodexBridge] Dispatcher enforces capacity and shutdown") {
	MainThreadDispatcher dispatcher;
	CHECK(dispatcher.enqueue(make_command(1)) == MainThreadDispatcher::ENQUEUE_STOPPING);

	dispatcher.start_accepting();
	for (uint64_t index = 0; index < MainThreadDispatcher::MAX_QUEUE_SIZE; index++) {
		CHECK(dispatcher.enqueue(make_command(index)) == MainThreadDispatcher::ENQUEUE_OK);
	}
	CHECK(dispatcher.get_queue_size() == MainThreadDispatcher::MAX_QUEUE_SIZE);
	CHECK(dispatcher.enqueue(make_command(100)) == MainThreadDispatcher::ENQUEUE_FULL);

	CHECK(dispatcher.begin_shutdown() == MainThreadDispatcher::MAX_QUEUE_SIZE);
	CHECK_FALSE(dispatcher.is_accepting());
	CHECK(dispatcher.get_queue_size() == 0);
	CHECK(dispatcher.enqueue(make_command(101)) == MainThreadDispatcher::ENQUEUE_STOPPING);
}

TEST_CASE("[CodexBridge] Dispatcher preserves FIFO order and supports cancellation") {
	MainThreadDispatcher dispatcher;
	dispatcher.start_accepting();
	REQUIRE(dispatcher.enqueue(make_command(1)) == MainThreadDispatcher::ENQUEUE_OK);
	REQUIRE(dispatcher.enqueue(make_command(2)) == MainThreadDispatcher::ENQUEUE_OK);
	REQUIRE(dispatcher.enqueue(make_command(3)) == MainThreadDispatcher::ENQUEUE_OK);
	CHECK(dispatcher.cancel(2));
	CHECK_FALSE(dispatcher.cancel(2));

	HandlerContext context;
	const MainThreadDispatcher::ProcessStats stats = dispatcher.process(record_command, &context, 8, 2000, test_clock, &context);
	CHECK(stats.processed == 2);
	CHECK(stats.expired == 0);
	CHECK(stats.remaining == 0);
	REQUIRE(context.handled_ids.size() == 2);
	CHECK(context.handled_ids[0] == 1);
	CHECK(context.handled_ids[1] == 3);
}

TEST_CASE("[CodexBridge] Dispatcher enforces command and time budgets") {
	MainThreadDispatcher dispatcher;
	dispatcher.start_accepting();
	for (uint64_t index = 0; index < 12; index++) {
		REQUIRE(dispatcher.enqueue(make_command(index)) == MainThreadDispatcher::ENQUEUE_OK);
	}

	HandlerContext command_context;
	MainThreadDispatcher::ProcessStats stats = dispatcher.process(record_command, &command_context, MainThreadDispatcher::MAX_COMMANDS_PER_FRAME, MainThreadDispatcher::MAX_PROCESS_USEC_PER_FRAME, test_clock, &command_context);
	CHECK(stats.processed == MainThreadDispatcher::MAX_COMMANDS_PER_FRAME);
	CHECK(stats.remaining == 4);

	dispatcher.begin_shutdown();
	dispatcher.start_accepting();
	for (uint64_t index = 0; index < 5; index++) {
		REQUIRE(dispatcher.enqueue(make_command(index)) == MainThreadDispatcher::ENQUEUE_OK);
	}

	HandlerContext time_context;
	time_context.advance_usec = 1000;
	stats = dispatcher.process(record_command, &time_context, 8, 2000, test_clock, &time_context);
	CHECK(stats.processed == 2);
	CHECK(stats.elapsed_usec == 2000);
	CHECK(stats.remaining == 3);
}

TEST_CASE("[CodexBridge] Dispatcher expires deadlines before execution") {
	MainThreadDispatcher dispatcher;
	dispatcher.start_accepting();
	REQUIRE(dispatcher.enqueue(make_command(1, 99)) == MainThreadDispatcher::ENQUEUE_OK);
	REQUIRE(dispatcher.enqueue(make_command(2, 101)) == MainThreadDispatcher::ENQUEUE_OK);

	HandlerContext context;
	context.now_usec = 100;
	const MainThreadDispatcher::ProcessStats stats = dispatcher.process(record_command, &context, 8, 2000, test_clock, &context);
	CHECK(stats.processed == 1);
	CHECK(stats.expired == 1);
	REQUIRE(context.handled_ids.size() == 1);
	CHECK(context.handled_ids[0] == 2);
}

TEST_CASE("[CodexBridge] Transport worker starts and stops repeatedly") {
	BridgeTransportWorker worker;
	CHECK(worker.start() == OK);
	CHECK(worker.is_running());
	CHECK(worker.stop() == BridgeTransportWorker::STOPPED);
	CHECK_FALSE(worker.is_running());
	CHECK(worker.stop() == BridgeTransportWorker::STOP_NOT_RUNNING);

	CHECK(worker.start() == OK);
	CHECK(worker.stop() == BridgeTransportWorker::STOPPED);
}

} // namespace TestCodexBridge

#endif // MODULE_CODEX_BRIDGE_ENABLED
