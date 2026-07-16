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

#include "bridge_runtime.h"

#include "core/os/os.h"
#include "core/templates/hash_set.h"
#include "core/templates/vector.h"

#include "modules/codex_bridge/editor/main_thread_dispatcher.h"
#include "modules/codex_bridge/protocol/bridge_frame_codec.h"
#include "modules/codex_bridge/protocol/bridge_handshake.h"
#include "modules/codex_bridge/protocol/bridge_rpc_session.h"

namespace {

static constexpr int MAX_CLIENTS = 16;
static constexpr int MAX_OUTBOUND_BYTES = 33554432;
static constexpr uint64_t SNAPSHOT_TIMEOUT_USEC = 120000000;
static constexpr int MAX_NOTIFICATION_ENTRIES = 4096;
static constexpr int MAX_NOTIFICATION_BYTES = 16777216;

struct SnapshotStreamState {
	bool active = false;
	uint64_t internal_request_id = 0;
	String request_id;
	String snapshot_id;
	Array messages;
	int next_message = 0;
	int last_acked_chunk = -1;
	Vector<uint64_t> sent_chunk_bytes;
	uint64_t unacked_bytes = 0;
	uint64_t deadline_usec = 0;
};

struct TransportClient {
	Ref<BridgeStreamPeer> peer;
	BridgeFrameCodec codec;
	BridgeHandshakeSession handshake;
	BridgeRpcSession rpc;
	PackedByteArray pending_write;
	int write_offset = 0;
	bool close_after_write = false;
	bool force_close = false;
	SnapshotStreamState snapshot_stream;

	TransportClient(const Ref<BridgeStreamPeer> &p_peer, const PackedByteArray &p_token, const String &p_project_id, const String &p_editor_session_id, uint64_t p_accepted_at_usec, HashSet<String> *p_seen_client_nonces) :
			peer(p_peer), handshake(p_token, p_project_id, p_editor_session_id, p_accepted_at_usec, p_seen_client_nonces), rpc(p_project_id, p_editor_session_id) {}
};

static bool encode_response(const Dictionary &p_response, PackedByteArray &r_response) {
	return BridgeFrameCodec::encode_json(p_response, r_response) == OK;
}

static bool queue_encoded_response(TransportClient &r_client, const PackedByteArray &p_response) {
	if (r_client.pending_write.size() + p_response.size() > MAX_OUTBOUND_BYTES) {
		return false;
	}
	if (r_client.pending_write.is_empty()) {
		r_client.pending_write = p_response;
	} else {
		r_client.pending_write.append_array(p_response);
	}
	return true;
}

static bool queue_response(TransportClient &r_client, const Dictionary &p_response) {
	PackedByteArray response;
	if (!encode_response(p_response, response)) {
		return false;
	}
	return queue_encoded_response(r_client, response);
}

static void sanitize_revision_vector_for_protocol(Dictionary &r_message, const String &p_protocol_version) {
	if (p_protocol_version == "1.2") {
		return;
	}
	if (r_message.has("revisions") && r_message["revisions"].get_type() == Variant::DICTIONARY) {
		Dictionary revisions = r_message["revisions"];
		revisions.erase("resource_revision");
		r_message["revisions"] = revisions;
	}
	if (r_message.has("params") && r_message["params"].get_type() == Variant::DICTIONARY) {
		Dictionary params = r_message["params"];
		if (params.has("revisions") && params["revisions"].get_type() == Variant::DICTIONARY) {
			Dictionary revisions = params["revisions"];
			revisions.erase("resource_revision");
			params["revisions"] = revisions;
			r_message["params"] = params;
		}
	}
}

static bool pump_snapshot_stream(TransportClient &r_client) {
	SnapshotStreamState &stream = r_client.snapshot_stream;
	while (stream.active && stream.next_message < stream.messages.size()) {
		if (stream.messages[stream.next_message].get_type() != Variant::DICTIONARY) {
			return false;
		}
		Dictionary message = stream.messages[stream.next_message];
		message["protocol_version"] = r_client.rpc.get_protocol_version();
		sanitize_revision_vector_for_protocol(message, r_client.rpc.get_protocol_version());
		PackedByteArray encoded;
		if (!encode_response(message, encoded)) {
			return false;
		}
		const bool is_chunk = String(message.get("kind", String())) == "chunk";
		int64_t chunk_index = -1;
		if (is_chunk) {
			if (!message.has("chunk_index") || message["chunk_index"].get_type() != Variant::INT) {
				return false;
			}
			chunk_index = message["chunk_index"];
			if (chunk_index != (int64_t)stream.sent_chunk_bytes.size() || stream.unacked_bytes + encoded.size() > (uint64_t)MAX_OUTBOUND_BYTES) {
				break;
			}
		}
		if (!queue_encoded_response(r_client, encoded)) {
			break;
		}
		if (is_chunk) {
			stream.sent_chunk_bytes.push_back(encoded.size());
			stream.unacked_bytes += encoded.size();
		}
		stream.next_message++;
	}
	return true;
}

static bool start_snapshot_stream(TransportClient &r_client, uint64_t p_internal_request_id, const String &p_request_id, const Array &p_messages, uint64_t p_now_usec) {
	if (p_messages.is_empty()) {
		return true;
	}
	if (r_client.snapshot_stream.active || p_messages[0].get_type() != Variant::DICTIONARY) {
		return false;
	}
	const Dictionary first = p_messages[0];
	if (!first.has("params") || first["params"].get_type() != Variant::DICTIONARY) {
		return false;
	}
	const Dictionary params = first["params"];
	if (!params.has("snapshot_id") || params["snapshot_id"].get_type() != Variant::STRING) {
		return false;
	}
	r_client.snapshot_stream.active = true;
	r_client.snapshot_stream.internal_request_id = p_internal_request_id;
	r_client.snapshot_stream.request_id = p_request_id;
	r_client.snapshot_stream.snapshot_id = params["snapshot_id"];
	r_client.snapshot_stream.messages = p_messages;
	r_client.snapshot_stream.deadline_usec = p_now_usec + SNAPSHOT_TIMEOUT_USEC;
	return pump_snapshot_stream(r_client);
}

static bool acknowledge_snapshot_stream(TransportClient &r_client, const String &p_snapshot_id, int64_t p_through_chunk, uint64_t &r_terminal_request_id) {
	r_terminal_request_id = 0;
	SnapshotStreamState &stream = r_client.snapshot_stream;
	if (!stream.active) {
		return true;
	}
	if (stream.snapshot_id != p_snapshot_id || p_through_chunk < stream.last_acked_chunk || p_through_chunk >= (int64_t)stream.sent_chunk_bytes.size()) {
		return false;
	}
	for (int index = stream.last_acked_chunk + 1; index <= p_through_chunk; index++) {
		stream.unacked_bytes -= stream.sent_chunk_bytes[index];
	}
	stream.last_acked_chunk = p_through_chunk;
	if (!pump_snapshot_stream(r_client)) {
		return false;
	}
	if (stream.next_message == stream.messages.size() && stream.last_acked_chunk + 1 == (int)stream.sent_chunk_bytes.size()) {
		r_terminal_request_id = stream.internal_request_id;
		stream = SnapshotStreamState();
	}
	return true;
}

static MainThreadDispatcher::CommandType command_type_for_method(BridgeRpcSession::Method p_method) {
	switch (p_method) {
		case BridgeRpcSession::METHOD_INITIALIZE:
			return MainThreadDispatcher::COMMAND_INITIALIZE;
		case BridgeRpcSession::METHOD_PING:
			return MainThreadDispatcher::COMMAND_PING;
		case BridgeRpcSession::METHOD_CAPABILITIES:
			return MainThreadDispatcher::COMMAND_CAPABILITIES;
		case BridgeRpcSession::METHOD_EDITOR_SNAPSHOT:
			return MainThreadDispatcher::COMMAND_EDITOR_SNAPSHOT;
		case BridgeRpcSession::METHOD_RESOURCE_SNAPSHOT:
			return MainThreadDispatcher::COMMAND_RESOURCE_SNAPSHOT;
		case BridgeRpcSession::METHOD_RESOURCE_DELTA:
			return MainThreadDispatcher::COMMAND_RESOURCE_DELTA;
		case BridgeRpcSession::METHOD_SHUTDOWN:
			return MainThreadDispatcher::COMMAND_SHUTDOWN;
	}
	return MainThreadDispatcher::COMMAND_NO_OP;
}

static bool cancel_dispatched_request(BridgeTransportWorker::Context *p_context, uint64_t p_request_id) {
	MutexLock lock(p_context->dispatcher_mutex);
	if (!p_context->dispatcher) {
		return false;
	}
	if (p_context->dispatcher->cancel(p_request_id)) {
		return true;
	}
	MainThreadDispatcher::Command cancellation;
	cancellation.type = MainThreadDispatcher::COMMAND_CANCEL;
	cancellation.request_id = p_request_id;
	return p_context->dispatcher->enqueue(cancellation) == MainThreadDispatcher::ENQUEUE_OK;
}

static MainThreadDispatcher::EnqueueResult enqueue_request(BridgeTransportWorker::Context *p_context, const MainThreadDispatcher::Command &p_command, bool &r_expired) {
	MutexLock lock(p_context->dispatcher_mutex);
	r_expired = p_command.deadline_usec > 0 && OS::get_singleton()->get_ticks_usec() >= p_command.deadline_usec;
	if (r_expired) {
		return MainThreadDispatcher::ENQUEUE_STOPPING;
	}
	return p_context->dispatcher ? p_context->dispatcher->enqueue(p_command) : MainThreadDispatcher::ENQUEUE_STOPPING;
}

static bool apply_rpc_outcome(TransportClient &r_client, BridgeRpcSession::Outcome &r_outcome, BridgeTransportWorker::Context *p_context) {
	if (r_outcome.cancel_stream && r_client.snapshot_stream.active && r_client.snapshot_stream.request_id == r_outcome.stream_request_id) {
		const uint64_t internal_request_id = r_client.snapshot_stream.internal_request_id;
		r_client.snapshot_stream = SnapshotStreamState();
		cancel_dispatched_request(p_context, internal_request_id);
	}
	if (r_outcome.ack_received) {
		uint64_t terminal_request_id = 0;
		if (!acknowledge_snapshot_stream(r_client, r_outcome.ack_snapshot_id, r_outcome.ack_through_chunk, terminal_request_id)) {
			return false;
		}
		if (terminal_request_id != 0) {
			cancel_dispatched_request(p_context, terminal_request_id);
		}
	}
	if (r_outcome.cancel_dispatch) {
		cancel_dispatched_request(p_context, r_outcome.internal_request_id);
	}
	if (r_outcome.dispatch) {
		MainThreadDispatcher::Command command;
		command.type = command_type_for_method(r_outcome.method);
		command.request_id = r_outcome.internal_request_id;
		command.deadline_usec = r_outcome.deadline_usec;
		command.params = r_outcome.params;
		bool expired = false;
		const MainThreadDispatcher::EnqueueResult enqueue_result = enqueue_request(p_context, command, expired);
		if (expired) {
			BridgeRpcSession::Outcome deadline;
			if (r_client.rpc.complete(r_outcome.internal_request_id, OS::get_singleton()->get_ticks_usec(), deadline) != OK) {
				return false;
			}
			r_outcome = deadline;
		} else if (enqueue_result != MainThreadDispatcher::ENQUEUE_OK) {
			BridgeRpcSession::Outcome rejected;
			if (r_client.rpc.reject_dispatch(r_outcome.internal_request_id, rejected) != OK) {
				return false;
			}
			r_outcome = rejected;
		}
	}
	if (r_outcome.has_response && !queue_response(r_client, r_outcome.response)) {
		return false;
	}
	r_client.close_after_write = r_client.close_after_write || r_outcome.close_after_response;
	return true;
}

static void register_auth_failure(Vector<uint64_t> &r_failures, uint64_t p_now_usec, uint64_t &r_accept_blocked_until) {
	for (int index = r_failures.size() - 1; index >= 0; index--) {
		if (p_now_usec - r_failures[index] > 1000000) {
			r_failures.remove_at(index);
		}
	}
	r_failures.push_back(p_now_usec);
	if (r_failures.size() >= 8) {
		r_accept_blocked_until = p_now_usec + 250000;
	}
}

static bool process_client(TransportClient &r_client, uint64_t p_now_usec, uint64_t &r_next_internal_request_id, BridgeTransportWorker::Context *p_context, Vector<uint64_t> &r_auth_failures, uint64_t &r_accept_blocked_until) {
	if (r_client.force_close) {
		return false;
	}
	if (r_client.peer->poll() != OK || r_client.peer->get_status() != BridgeStreamPeer::STATUS_CONNECTED) {
		return false;
	}
	if (r_client.handshake.has_timed_out(p_now_usec)) {
		return false;
	}
	if (r_client.snapshot_stream.active && p_now_usec >= r_client.snapshot_stream.deadline_usec) {
		return false;
	}
	if (r_client.handshake.get_state() == BridgeHandshakeSession::STATE_AUTHENTICATED) {
		Vector<BridgeRpcSession::Outcome> expired;
		r_client.rpc.expire_requests(p_now_usec, expired);
		for (BridgeRpcSession::Outcome &outcome : expired) {
			if (!apply_rpc_outcome(r_client, outcome, p_context)) {
				return false;
			}
		}
	}

	const int available = r_client.peer->get_available_bytes();
	if (available > 0) {
		PackedByteArray incoming;
		incoming.resize(MIN(available, 65536));
		int received = 0;
		if (r_client.peer->get_partial_data(incoming.ptrw(), incoming.size(), received) != OK || received <= 0) {
			return false;
		}
		Vector<PackedByteArray> frames;
		if (r_client.codec.feed(incoming.ptr(), received, frames) != OK) {
			return false;
		}
		for (const PackedByteArray &frame : frames) {
			Dictionary message;
			if (BridgeJson::parse_strict_object(frame, message) != OK) {
				return false;
			}
			if (r_client.handshake.get_state() == BridgeHandshakeSession::STATE_AUTHENTICATED) {
				BridgeRpcSession::Outcome rpc_outcome;
				const uint64_t internal_request_id = r_next_internal_request_id++;
				const uint64_t validated_at_usec = OS::get_singleton()->get_ticks_usec();
				if (r_next_internal_request_id == 0) {
					r_next_internal_request_id = 1;
				}
				if (r_client.rpc.handle_message(message, validated_at_usec, internal_request_id, rpc_outcome) != OK || !apply_rpc_outcome(r_client, rpc_outcome, p_context)) {
					return false;
				}
				continue;
			}
			BridgeHandshakeSession::Outcome outcome;
			if (r_client.handshake.handle_message(message, p_now_usec, outcome) != OK) {
				return false;
			}
			if (r_client.handshake.get_state() == BridgeHandshakeSession::STATE_AUTHENTICATED) {
				r_client.rpc.set_protocol_version(r_client.handshake.get_selected_protocol_version());
			}
			if (outcome.authentication_failed) {
				register_auth_failure(r_auth_failures, p_now_usec, r_accept_blocked_until);
			}
			if (outcome.has_response) {
				if (!queue_response(r_client, outcome.response)) {
					return false;
				}
			}
			r_client.close_after_write = r_client.close_after_write || outcome.close_after_response;
		}
	}

	if (!r_client.pending_write.is_empty()) {
		int sent = 0;
		if (r_client.peer->put_partial_data(r_client.pending_write.ptr() + r_client.write_offset, r_client.pending_write.size() - r_client.write_offset, sent) != OK) {
			return false;
		}
		r_client.write_offset += sent;
		if (r_client.write_offset == r_client.pending_write.size()) {
			r_client.pending_write.clear();
			r_client.write_offset = 0;
			if (r_client.close_after_write) {
				return false;
			}
		}
	}
	if (!pump_snapshot_stream(r_client)) {
		return false;
	}
	return true;
}

static void cancel_client_pending(TransportClient &r_client, BridgeTransportWorker::Context *p_context) {
	if (r_client.snapshot_stream.active) {
		cancel_dispatched_request(p_context, r_client.snapshot_stream.internal_request_id);
		r_client.snapshot_stream = SnapshotStreamState();
	}
	Vector<uint64_t> pending;
	r_client.rpc.cancel_all(pending);
	for (uint64_t internal_id : pending) {
		cancel_dispatched_request(p_context, internal_id);
	}
}

static void drain_completions(BridgeTransportWorker::Context *p_context, Vector<BridgeTransportWorker::Completion> &r_completions) {
	MutexLock lock(p_context->completion_mutex);
	while (!p_context->completed_requests.is_empty()) {
		r_completions.push_back(p_context->completed_requests.front()->get());
		p_context->completed_requests.pop_front();
	}
}

static void drain_notifications(BridgeTransportWorker::Context *p_context, Vector<Dictionary> &r_notifications) {
	MutexLock lock(p_context->notification_mutex);
	while (!p_context->notifications.is_empty()) {
		r_notifications.push_back(p_context->notifications.front()->get());
		p_context->notifications.pop_front();
	}
	p_context->notification_bytes = 0;
}

static void run_transport(BridgeTransportWorker::Context *p_context, BridgeRuntime &r_runtime) {
	Vector<TransportClient> clients;
	HashSet<String> seen_client_nonces;
	Vector<uint64_t> authentication_failures;
	uint64_t accept_blocked_until = 0;
	uint64_t next_internal_request_id = 1;

	while (!p_context->stop_requested.is_set()) {
		const uint64_t now_usec = OS::get_singleton()->get_ticks_usec();
		Vector<BridgeTransportWorker::Completion> completions;
		drain_completions(p_context, completions);
		for (const BridgeTransportWorker::Completion &completion : completions) {
			for (TransportClient &client : clients) {
				BridgeRpcSession::Outcome outcome;
				const Error completion_error = completion.is_error ?
						client.rpc.complete_error(completion.request_id, completion.error_code, completion.error_message, completion.error_retryable, completion.error_data, outcome) :
						client.rpc.complete(completion.request_id, now_usec, completion.result, outcome);
				if (completion_error == OK) {
					if (!apply_rpc_outcome(client, outcome, p_context)) {
						cancel_dispatched_request(p_context, completion.request_id);
						client.force_close = true;
					} else if (!completion.server_messages.is_empty() && (client.rpc.get_protocol_version() == "1.1" || client.rpc.get_protocol_version() == "1.2")) {
						const String request_id = outcome.response.get("request_id", String());
						if (request_id.is_empty() || !start_snapshot_stream(client, completion.request_id, request_id, completion.server_messages, now_usec)) {
							cancel_dispatched_request(p_context, completion.request_id);
							client.force_close = true;
						}
					}
					break;
				}
			}
		}
		Vector<Dictionary> notifications;
		drain_notifications(p_context, notifications);
		for (TransportClient &client : clients) {
			if (client.handshake.get_state() != BridgeHandshakeSession::STATE_AUTHENTICATED || !client.rpc.is_initialized() || (client.rpc.get_protocol_version() != "1.1" && client.rpc.get_protocol_version() != "1.2")) {
				continue;
			}
			for (const Dictionary &notification : notifications) {
				const String notification_version = notification.get("protocol_version", "1.1");
				if (notification_version == "1.2" && client.rpc.get_protocol_version() != "1.2") {
					continue;
				}
				Dictionary client_notification = notification;
				client_notification["protocol_version"] = client.rpc.get_protocol_version();
				sanitize_revision_vector_for_protocol(client_notification, client.rpc.get_protocol_version());
				if (!queue_response(client, client_notification)) {
					client.force_close = true;
					break;
				}
			}
		}
		while (r_runtime.is_connection_available()) {
			Ref<BridgeStreamPeer> peer = r_runtime.take_connection();
			if (peer.is_null()) {
				break;
			}
			if (now_usec < accept_blocked_until || clients.size() >= MAX_CLIENTS || seen_client_nonces.size() >= 4096) {
				peer->disconnect_from_host();
				continue;
			}
			clients.push_back(TransportClient(peer, r_runtime.get_token(), r_runtime.get_project_id(), r_runtime.get_editor_session_id(), now_usec, &seen_client_nonces));
		}
		for (int index = clients.size() - 1; index >= 0; index--) {
			if (!process_client(clients.write[index], now_usec, next_internal_request_id, p_context, authentication_failures, accept_blocked_until)) {
				cancel_client_pending(clients.write[index], p_context);
				clients.write[index].peer->disconnect_from_host();
				clients.remove_at(index);
			}
		}
		OS::get_singleton()->delay_usec(1000);
	}
	for (TransportClient &client : clients) {
		cancel_client_pending(client, p_context);
		client.peer->disconnect_from_host();
	}
}

} // namespace

void BridgeTransportWorker::_release_context(Context *p_context) {
	if (p_context->references.unref()) {
		memdelete(p_context);
	}
}

void BridgeTransportWorker::_thread_main(void *p_userdata) {
	Context *worker_context = static_cast<Context *>(p_userdata);
	Thread::set_name("CodexBridgeTransport");

	BridgeRuntime runtime;
	if (!worker_context->project_root.is_empty()) {
		worker_context->startup_error = runtime.initialize(worker_context->project_root);
		if (worker_context->startup_error == OK) {
			worker_context->project_id = runtime.get_project_id();
			worker_context->editor_session_id = runtime.get_editor_session_id();
		}
	}
	worker_context->startup_done.set();
	worker_context->startup.post();

	if (worker_context->startup_error == OK) {
		if (worker_context->project_root.is_empty()) {
			while (!worker_context->stop_requested.is_set()) {
				worker_context->wakeup.wait();
			}
		} else {
			run_transport(worker_context, runtime);
		}
	}
	runtime.cleanup();

	worker_context->exited.set();
	_release_context(worker_context);
}

Error BridgeTransportWorker::start() {
	return start(String());
}

Error BridgeTransportWorker::start(const String &p_project_root, uint64_t p_timeout_usec) {
	return start(p_project_root, nullptr, p_timeout_usec);
}

Error BridgeTransportWorker::start(const String &p_project_root, MainThreadDispatcher *p_dispatcher, uint64_t p_timeout_usec) {
	ERR_FAIL_COND_V_MSG(thread != nullptr, ERR_ALREADY_IN_USE, "Codex bridge transport worker is already started.");

	context = memnew(Context);
	context->project_root = p_project_root;
	context->dispatcher = p_dispatcher;
	thread = memnew(Thread);
	if (thread->start(_thread_main, context) == Thread::UNASSIGNED_ID) {
		memdelete(thread);
		thread = nullptr;
		_release_context(context);
		_release_context(context);
		context = nullptr;
		return ERR_CANT_CREATE;
	}

	const uint64_t deadline_usec = OS::get_singleton()->get_ticks_usec() + p_timeout_usec;
	while (!context->startup.try_wait() && OS::get_singleton()->get_ticks_usec() < deadline_usec) {
		OS::get_singleton()->delay_usec(1000);
	}
	if (!context->startup_done.is_set()) {
		context->stop_requested.set();
		context->wakeup.post();
		thread->wait_to_finish();
		memdelete(thread);
		thread = nullptr;
		Context *timed_out_context = context;
		context = nullptr;
		_release_context(timed_out_context);
		return ERR_TIMEOUT;
	}
	if (context->startup_error != OK) {
		const Error startup_error = context->startup_error;
		thread->wait_to_finish();
		memdelete(thread);
		thread = nullptr;
		Context *failed_context = context;
		context = nullptr;
		_release_context(failed_context);
		return startup_error;
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
	{
		MutexLock lock(stopping_context->dispatcher_mutex);
		stopping_context->dispatcher = nullptr;
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

void BridgeTransportWorker::complete_request(uint64_t p_request_id) {
	complete_request(p_request_id, Dictionary());
}

void BridgeTransportWorker::complete_request(uint64_t p_request_id, const Dictionary &p_result, const Array &p_server_messages) {
	if (!context) {
		return;
	}
	{
		MutexLock lock(context->completion_mutex);
		Completion completion;
		completion.request_id = p_request_id;
		completion.result = p_result;
		completion.server_messages = p_server_messages;
		context->completed_requests.push_back(completion);
	}
}

void BridgeTransportWorker::complete_request_error(uint64_t p_request_id, const String &p_code, const String &p_message, bool p_retryable, const Dictionary &p_data) {
	if (!context) {
		return;
	}
	MutexLock lock(context->completion_mutex);
	Completion completion;
	completion.request_id = p_request_id;
	completion.is_error = true;
	completion.error_code = p_code;
	completion.error_message = p_message;
	completion.error_retryable = p_retryable;
	completion.error_data = p_data;
	context->completed_requests.push_back(completion);
}

bool BridgeTransportWorker::publish_notification(const Dictionary &p_notification) {
	if (!context) {
		return false;
	}
	PackedByteArray encoded;
	if (BridgeFrameCodec::encode_json(p_notification, encoded) != OK) {
		return false;
	}
	MutexLock lock(context->notification_mutex);
	if (context->notifications.size() >= MAX_NOTIFICATION_ENTRIES || context->notification_bytes + encoded.size() > MAX_NOTIFICATION_BYTES) {
		return false;
	}
	context->notifications.push_back(p_notification);
	context->notification_bytes += encoded.size();
	return true;
}

String BridgeTransportWorker::get_project_id() const {
	return context ? context->project_id : String();
}

String BridgeTransportWorker::get_editor_session_id() const {
	return context ? context->editor_session_id : String();
}

bool BridgeTransportWorker::is_running() const {
	return thread && context && !context->exited.is_set();
}

BridgeTransportWorker::~BridgeTransportWorker() {
	stop();
}
