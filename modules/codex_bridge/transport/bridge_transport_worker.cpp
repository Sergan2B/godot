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

#include "core/crypto/crypto_core.h"
#include "core/crypto/hashing_context.h"
#include "core/io/json.h"
#include "core/os/os.h"
#include "core/templates/hash_set.h"
#include "core/templates/vector.h"

#include "modules/codex_bridge/editor/main_thread_dispatcher.h"
#include "modules/codex_bridge/protocol/bridge_crypto.h"
#include "modules/codex_bridge/protocol/bridge_frame_codec.h"
#include "modules/codex_bridge/protocol/bridge_handshake.h"
#include "modules/codex_bridge/protocol/bridge_rpc_session.h"

namespace {

static constexpr int MAX_CLIENTS = 16;
static constexpr int MAX_OUTBOUND_BYTES = 33554432;
static constexpr uint64_t SNAPSHOT_TIMEOUT_USEC = 120000000;
static constexpr int MAX_NOTIFICATION_ENTRIES = 4096;
static constexpr int MAX_NOTIFICATION_BYTES = 16777216;
static constexpr int MAX_SNAPSHOT_CHUNKS = 100001;
static constexpr uint64_t MAX_SNAPSHOT_MESSAGES = MAX_SNAPSHOT_CHUNKS + 2;
static constexpr uint64_t MAX_RESOURCE_SNAPSHOT_SPOOL_BYTES =
		(65536ULL + 2) * (BridgeFrameCodec::MAX_PAYLOAD_BYTES + 8ULL);
static constexpr uint64_t MAX_SCENE_SNAPSHOT_SPOOL_BYTES = 32 * 1024 * 1024;
static constexpr uint64_t MAX_SCRIPT_SNAPSHOT_SPOOL_BYTES = 64 * 1024 * 1024;

struct SnapshotStreamState {
	bool active = false;
	uint64_t internal_request_id = 0;
	String request_id;
	String snapshot_id;
	Array messages;
	Ref<FileAccess> spool;
	int spool_chunk_count = 0;
	uint32_t spool_next_frame_bytes = 0;
	int next_message = 0;
	int last_acked_chunk = -1;
	Vector<uint64_t> sent_chunk_bytes;
	uint64_t unacked_bytes = 0;
	uint64_t deadline_usec = 0;
};

struct SnapshotPreparation {
	BridgeTransportWorker::Completion completion;
	int next_message = 0;
	int next_chunk = 0;
	String snapshot_id;
	String domain;
	Ref<FileAccess> spool;
	Ref<HashingContext> checksum_context;
	uint64_t spool_bytes = 0;
};

enum SnapshotPreparationResult {
	SNAPSHOT_PREPARATION_PENDING,
	SNAPSHOT_PREPARATION_READY,
	SNAPSHOT_PREPARATION_FAILED,
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

static String sha256_hex_utf8(const String &p_value) {
	const CharString bytes = p_value.utf8();
	PackedByteArray digest;
	digest.resize(32);
	if (CryptoCore::sha256(reinterpret_cast<const uint8_t *>(bytes.get_data()), bytes.length(), digest.ptrw()) != OK) {
		return String();
	}
	return BridgeCrypto::bytes_to_lower_hex(digest);
}

static bool is_resource_protocol(const String &p_protocol_version) {
	return p_protocol_version == "1.2" || p_protocol_version == "1.3" || p_protocol_version == "1.4";
}

static bool is_snapshot_protocol(const String &p_protocol_version, const String &p_domain) {
	if (p_domain == "resource_graph") {
		return is_resource_protocol(p_protocol_version);
	}
	if (p_domain == "scene_graph") {
		return p_protocol_version == "1.3" || p_protocol_version == "1.4";
	}
	return p_domain == "script_graph" && p_protocol_version == "1.4";
}

static bool open_snapshot_spool(SnapshotPreparation &r_preparation) {
	Error error = OK;
	r_preparation.spool = FileAccess::create_temp(
			FileAccess::WRITE_READ,
			"codex-graph-snapshot",
			"spool",
			false,
			&error);
	if (error != OK || r_preparation.spool.is_null()) {
		return false;
	}
#ifdef UNIX_ENABLED
	const BitField<FileAccess::UnixPermissionFlags> owner_only =
			FileAccess::UNIX_READ_OWNER | FileAccess::UNIX_WRITE_OWNER;
	if (FileAccess::set_unix_permissions(r_preparation.spool->get_path_absolute(), owner_only) != OK) {
		r_preparation.spool.unref();
		return false;
	}
#endif
	r_preparation.checksum_context.instantiate();
	return r_preparation.checksum_context->start(HashingContext::HASH_SHA256) == OK;
}

static bool append_snapshot_frame(SnapshotPreparation &r_preparation, const PackedByteArray &p_encoded) {
	if (r_preparation.spool.is_null() || p_encoded.is_empty() || p_encoded.size() > (int64_t)BridgeFrameCodec::MAX_PAYLOAD_BYTES + 4 ||
			r_preparation.next_message >= (int)MAX_SNAPSHOT_MESSAGES) {
		return false;
	}
	const uint64_t record_bytes = sizeof(uint32_t) + (uint64_t)p_encoded.size();
	const uint64_t spool_limit = r_preparation.domain == "scene_graph" ? MAX_SCENE_SNAPSHOT_SPOOL_BYTES : (r_preparation.domain == "script_graph" ? MAX_SCRIPT_SNAPSHOT_SPOOL_BYTES : MAX_RESOURCE_SNAPSHOT_SPOOL_BYTES);
	if (r_preparation.spool_bytes > spool_limit || record_bytes > spool_limit - r_preparation.spool_bytes ||
			!r_preparation.spool->store_32((uint32_t)p_encoded.size()) ||
			!r_preparation.spool->store_buffer(p_encoded)) {
		return false;
	}
	r_preparation.spool_bytes += record_bytes;
	return true;
}

static SnapshotPreparationResult prepare_snapshot_dictionary(SnapshotPreparation &r_preparation, Dictionary &r_message, bool p_final_message) {
	Dictionary &message = r_message;
	const String kind = message.get("kind", String());
	if (kind == "chunk") {
		if (p_final_message || r_preparation.next_message == 0 || r_preparation.spool.is_null() || r_preparation.checksum_context.is_null() ||
				!is_snapshot_protocol(String(message.get("protocol_version", String())), r_preparation.domain) || String(message.get("domain", String())) != r_preparation.domain ||
				String(message.get("snapshot_id", String())) != r_preparation.snapshot_id ||
				!message.has("chunk_index") || message["chunk_index"].get_type() != Variant::INT ||
				(int64_t)message["chunk_index"] != r_preparation.next_chunk ||
				!message.has("payload") || message["payload"].get_type() != Variant::DICTIONARY ||
				r_preparation.next_chunk >= MAX_SNAPSHOT_CHUNKS) {
			return SNAPSHOT_PREPARATION_FAILED;
		}
		const String payload_json = JSON::stringify(message["payload"], "", true, true);
		if (payload_json.utf8().length() > 256 * 1024) {
			return SNAPSHOT_PREPARATION_FAILED;
		}
		const String checksum = sha256_hex_utf8(payload_json);
		if (checksum.is_empty()) {
			return SNAPSHOT_PREPARATION_FAILED;
		}
		message["payload_json"] = payload_json;
		message["checksum"] = checksum;
		const PackedByteArray checksum_bytes = checksum.to_utf8_buffer();
		if (r_preparation.checksum_context->update(checksum_bytes) != OK) {
			return SNAPSHOT_PREPARATION_FAILED;
		}
		r_preparation.next_chunk++;
	} else if (String(message.get("method", String())) == "snapshot.end") {
		if (!p_final_message || r_preparation.next_message < 2 || r_preparation.checksum_context.is_null() ||
				!is_snapshot_protocol(String(message.get("protocol_version", String())), r_preparation.domain) || !message.has("params") || message["params"].get_type() != Variant::DICTIONARY) {
			return SNAPSHOT_PREPARATION_FAILED;
		}
		Dictionary params = message["params"];
		if (String(params.get("domain", String())) != r_preparation.domain || String(params.get("snapshot_id", String())) != r_preparation.snapshot_id ||
				!params.has("chunk_count") || params["chunk_count"].get_type() != Variant::INT || (int64_t)params["chunk_count"] != r_preparation.next_chunk) {
			return SNAPSHOT_PREPARATION_FAILED;
		}
		const PackedByteArray checksum_digest = r_preparation.checksum_context->finish();
		r_preparation.checksum_context.unref();
		if (checksum_digest.size() != 32) {
			return SNAPSHOT_PREPARATION_FAILED;
		}
		params["checksum"] = BridgeCrypto::bytes_to_lower_hex(checksum_digest);
		message["params"] = params;
	} else {
		if (p_final_message || r_preparation.next_message != 0 ||
				String(message.get("method", String())) != "snapshot.begin" || !message.has("params") || message["params"].get_type() != Variant::DICTIONARY) {
			return SNAPSHOT_PREPARATION_FAILED;
		}
		const Dictionary params = message["params"];
		r_preparation.snapshot_id = params.get("snapshot_id", String());
		r_preparation.domain = params.get("domain", String());
		if (r_preparation.snapshot_id.is_empty() || !is_snapshot_protocol(String(message.get("protocol_version", String())), r_preparation.domain) || !open_snapshot_spool(r_preparation)) {
			return SNAPSHOT_PREPARATION_FAILED;
		}
	}
	PackedByteArray encoded;
	if (!encode_response(message, encoded) || !append_snapshot_frame(r_preparation, encoded)) {
		return SNAPSHOT_PREPARATION_FAILED;
	}
	r_preparation.next_message++;
	if (p_final_message) {
		r_preparation.spool->flush();
		if (r_preparation.spool->get_error() != OK) {
			return SNAPSHOT_PREPARATION_FAILED;
		}
		r_preparation.spool->seek(0);
		if (r_preparation.spool->get_position() != 0) {
			return SNAPSHOT_PREPARATION_FAILED;
		}
	}
	return p_final_message ? SNAPSHOT_PREPARATION_READY : SNAPSHOT_PREPARATION_PENDING;
}

static void sanitize_revision_vector_for_protocol(Dictionary &r_message, const String &p_protocol_version) {
	if (r_message.has("revisions") && r_message["revisions"].get_type() == Variant::DICTIONARY) {
		Dictionary revisions = r_message["revisions"];
		if (p_protocol_version != "1.2" && p_protocol_version != "1.3" && p_protocol_version != "1.4") {
			revisions.erase("resource_revision");
		}
		if (p_protocol_version != "1.3" && p_protocol_version != "1.4") {
			revisions.erase("scene_graph_revision");
		}
		if (p_protocol_version != "1.4") {
			revisions.erase("script_graph_revision");
		}
		r_message["revisions"] = revisions;
	}
	if (r_message.has("params") && r_message["params"].get_type() == Variant::DICTIONARY) {
		Dictionary params = r_message["params"];
		if (params.has("revisions") && params["revisions"].get_type() == Variant::DICTIONARY) {
			Dictionary revisions = params["revisions"];
			if (p_protocol_version != "1.2" && p_protocol_version != "1.3" && p_protocol_version != "1.4") {
				revisions.erase("resource_revision");
			}
			if (p_protocol_version != "1.3" && p_protocol_version != "1.4") {
				revisions.erase("scene_graph_revision");
			}
			if (p_protocol_version != "1.4") {
				revisions.erase("script_graph_revision");
			}
			params["revisions"] = revisions;
			r_message["params"] = params;
		}
	}
}

static bool pump_snapshot_stream(TransportClient &r_client) {
	SnapshotStreamState &stream = r_client.snapshot_stream;
	if (stream.spool.is_valid()) {
		const int total_messages = stream.spool_chunk_count + 2;
		if (!stream.active || stream.next_message >= total_messages) {
			return true;
		}
		if (stream.spool_next_frame_bytes == 0) {
			if (stream.spool->get_position() + sizeof(uint32_t) > stream.spool->get_length()) {
				return false;
			}
			stream.spool_next_frame_bytes = stream.spool->get_32();
			if (stream.spool_next_frame_bytes < 4 || stream.spool_next_frame_bytes > BridgeFrameCodec::MAX_PAYLOAD_BYTES + 4) {
				return false;
			}
		}
		const bool is_chunk = stream.next_message > 0 && stream.next_message <= stream.spool_chunk_count;
		const uint64_t frame_bytes = stream.spool_next_frame_bytes;
		if ((uint64_t)r_client.pending_write.size() + frame_bytes > (uint64_t)MAX_OUTBOUND_BYTES ||
				(is_chunk && stream.unacked_bytes + frame_bytes > (uint64_t)MAX_OUTBOUND_BYTES)) {
			return true;
		}
		if (stream.spool->get_position() + frame_bytes > stream.spool->get_length()) {
			return false;
		}
		PackedByteArray encoded;
		encoded.resize(frame_bytes);
		if (stream.spool->get_buffer(encoded.ptrw(), frame_bytes) != frame_bytes || !queue_encoded_response(r_client, encoded)) {
			return false;
		}
		if (is_chunk) {
			stream.sent_chunk_bytes.push_back(frame_bytes);
			stream.unacked_bytes += frame_bytes;
		}
		stream.spool_next_frame_bytes = 0;
		stream.next_message++;
		if (stream.next_message == total_messages && stream.spool->get_position() != stream.spool->get_length()) {
			return false;
		}
		return true;
	}

	while (stream.active && stream.next_message < stream.messages.size()) {
		if (stream.messages[stream.next_message].get_type() != Variant::DICTIONARY) {
			return false;
		}
		Dictionary message = stream.messages[stream.next_message];
		message["protocol_version"] = r_client.rpc.get_protocol_version();
		sanitize_revision_vector_for_protocol(message, r_client.rpc.get_protocol_version());
		const bool is_chunk = String(message.get("kind", String())) == "chunk";
		PackedByteArray encoded;
		if (!encode_response(message, encoded)) {
			return false;
		}
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
		// Preparing and framing one message per worker iteration keeps control
		// traffic responsive while a large snapshot is in flight.
		break;
	}
	return true;
}

static bool snapshot_stream_is_fully_queued(const SnapshotStreamState &p_stream) {
	if (p_stream.spool.is_valid()) {
		return p_stream.next_message == p_stream.spool_chunk_count + 2;
	}
	return p_stream.next_message == p_stream.messages.size();
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
	return true;
}

static bool start_spooled_snapshot_stream(TransportClient &r_client, uint64_t p_internal_request_id, const String &p_request_id, const String &p_snapshot_id, const Ref<FileAccess> &p_spool, int p_chunk_count, uint64_t p_now_usec) {
	if (r_client.snapshot_stream.active || !is_resource_protocol(r_client.rpc.get_protocol_version()) || p_request_id.is_empty() || p_snapshot_id.is_empty() ||
			p_spool.is_null() || p_chunk_count <= 0 || p_chunk_count > MAX_SNAPSHOT_CHUNKS || p_spool->get_position() != 0 || p_spool->get_length() == 0) {
		return false;
	}
	r_client.snapshot_stream.active = true;
	r_client.snapshot_stream.internal_request_id = p_internal_request_id;
	r_client.snapshot_stream.request_id = p_request_id;
	r_client.snapshot_stream.snapshot_id = p_snapshot_id;
	r_client.snapshot_stream.spool = p_spool;
	r_client.snapshot_stream.spool_chunk_count = p_chunk_count;
	r_client.snapshot_stream.deadline_usec = p_now_usec + SNAPSHOT_TIMEOUT_USEC;
	return true;
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
	if (snapshot_stream_is_fully_queued(stream) && stream.last_acked_chunk + 1 == (int)stream.sent_chunk_bytes.size()) {
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
		case BridgeRpcSession::METHOD_SCENE_SNAPSHOT:
			return MainThreadDispatcher::COMMAND_SCENE_SNAPSHOT;
		case BridgeRpcSession::METHOD_SCENE_DELTA:
			return MainThreadDispatcher::COMMAND_SCENE_DELTA;
		case BridgeRpcSession::METHOD_SCRIPT_SNAPSHOT:
			return MainThreadDispatcher::COMMAND_SCRIPT_SNAPSHOT;
		case BridgeRpcSession::METHOD_SCRIPT_DELTA:
			return MainThreadDispatcher::COMMAND_SCRIPT_DELTA;
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
	SnapshotStreamState &stream = r_client.snapshot_stream;
	if (stream.active && snapshot_stream_is_fully_queued(stream) && stream.last_acked_chunk + 1 == (int)stream.sent_chunk_bytes.size()) {
		const uint64_t terminal_request_id = stream.internal_request_id;
		stream = SnapshotStreamState();
		cancel_dispatched_request(p_context, terminal_request_id);
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
	Vector<SnapshotPreparation> snapshot_preparations;
	List<BridgeTransportWorker::Completion> snapshot_events;
	HashSet<String> seen_client_nonces;
	Vector<uint64_t> authentication_failures;
	uint64_t accept_blocked_until = 0;
	uint64_t next_internal_request_id = 1;

	while (!p_context->stop_requested.is_set()) {
		const uint64_t now_usec = OS::get_singleton()->get_ticks_usec();
		Vector<BridgeTransportWorker::Completion> drained_completions;
		drain_completions(p_context, drained_completions);
		Vector<BridgeTransportWorker::Completion> completions;
		for (const BridgeTransportWorker::Completion &completion : drained_completions) {
			if (completion.kind != BridgeTransportWorker::Completion::KIND_REQUEST) {
				snapshot_events.push_back(completion);
			} else {
				completions.push_back(completion);
			}
		}
		if (!snapshot_events.is_empty()) {
			const BridgeTransportWorker::Completion event = snapshot_events.front()->get();
			snapshot_events.pop_front();
			bool pending_client = false;
			String pending_protocol_version;
			for (const TransportClient &client : clients) {
				if (client.rpc.has_pending_request(event.request_id)) {
					pending_client = true;
					pending_protocol_version = client.rpc.get_protocol_version();
					break;
				}
			}
			int preparation_index = -1;
			for (int index = 0; index < snapshot_preparations.size(); index++) {
				if (snapshot_preparations[index].completion.request_id == event.request_id) {
					preparation_index = index;
					break;
				}
			}
			if (event.kind == BridgeTransportWorker::Completion::KIND_RESOURCE_SNAPSHOT_ABORT) {
				if (preparation_index >= 0) {
					snapshot_preparations.remove_at(preparation_index);
				}
			} else if (pending_client) {
				if (preparation_index < 0 && event.kind == BridgeTransportWorker::Completion::KIND_RESOURCE_SNAPSHOT_MESSAGE) {
					SnapshotPreparation preparation;
					preparation.completion.request_id = event.request_id;
					snapshot_preparations.push_back(preparation);
					preparation_index = snapshot_preparations.size() - 1;
				}
				SnapshotPreparationResult preparation_result = SNAPSHOT_PREPARATION_FAILED;
				Dictionary message = event.snapshot_message;
				message["protocol_version"] = pending_protocol_version;
				if (preparation_index >= 0) {
					SnapshotPreparation &preparation = snapshot_preparations.write[preparation_index];
					preparation_result = prepare_snapshot_dictionary(
							preparation, message, event.kind == BridgeTransportWorker::Completion::KIND_RESOURCE_SNAPSHOT_END);
				}
				if (preparation_result == SNAPSHOT_PREPARATION_READY && preparation_index >= 0) {
					SnapshotPreparation &preparation = snapshot_preparations.write[preparation_index];
					preparation.completion.result = event.result;
					preparation.completion.snapshot_spool = preparation.spool;
					preparation.completion.snapshot_chunk_count = preparation.next_chunk;
					completions.push_back(preparation.completion);
					snapshot_preparations.remove_at(preparation_index);
				} else if (preparation_result == SNAPSHOT_PREPARATION_FAILED) {
					BridgeTransportWorker::Completion failure;
					failure.request_id = event.request_id;
					failure.is_error = true;
					const String failed_domain = preparation_index >= 0 ? snapshot_preparations[preparation_index].domain : String();
					if (failed_domain == "scene_graph") {
						failure.error_code = "scene_limit_exceeded";
						failure.error_message = "The scene graph snapshot could not be framed within the negotiated limits.";
					} else if (failed_domain == "script_graph") {
						failure.error_code = "script_limit_exceeded";
						failure.error_message = "The script graph snapshot could not be framed within the negotiated limits.";
					} else {
						failure.error_code = "resource_limit_exceeded";
						failure.error_message = "The resource graph snapshot could not be framed within the negotiated limits.";
					}
					failure.error_retryable = false;
					failure.cancel_dispatch = true;
					completions.push_back(failure);
					if (preparation_index >= 0) {
						snapshot_preparations.remove_at(preparation_index);
					}
				}
			}
		}
		for (int index = snapshot_preparations.size() - 1; index >= 0; index--) {
			bool pending_client = false;
			for (const TransportClient &client : clients) {
				if (client.rpc.has_pending_request(snapshot_preparations[index].completion.request_id)) {
					pending_client = true;
					break;
				}
			}
			if (!pending_client) {
				// The RPC path that removed the pending request already queued the
				// terminal cancellation. Pruning only releases detached worker data;
				// canceling again could otherwise race that queued command.
				snapshot_preparations.remove_at(index);
			}
		}
		for (const BridgeTransportWorker::Completion &completion : completions) {
			const uint64_t completion_now_usec = OS::get_singleton()->get_ticks_usec();
			bool handled = false;
			for (TransportClient &client : clients) {
				BridgeRpcSession::Outcome outcome;
				const Error completion_error = completion.is_error ? client.rpc.complete_error(completion.request_id, completion.error_code, completion.error_message, completion.error_retryable, completion.error_data, outcome) : client.rpc.complete(completion.request_id, completion_now_usec, completion.result, outcome);
				if (completion_error == OK) {
					handled = true;
					const bool stream_completion = completion.snapshot_spool.is_valid() || !completion.server_messages.is_empty();
					if (!apply_rpc_outcome(client, outcome, p_context)) {
						cancel_dispatched_request(p_context, completion.request_id);
						client.force_close = true;
					} else if (stream_completion && !outcome.response.has("result")) {
						// complete() also returns OK when it converts a late completion
						// into deadline_exceeded. Never append a stream after that
						// terminal error; release the frozen adapter generation instead.
						cancel_dispatched_request(p_context, completion.request_id);
					} else if (completion.snapshot_spool.is_valid()) {
						const String request_id = outcome.response.get("request_id", String());
						const String snapshot_id = completion.result.get("snapshot_id", String());
						if (!start_spooled_snapshot_stream(client, completion.request_id, request_id, snapshot_id, completion.snapshot_spool, completion.snapshot_chunk_count, completion_now_usec)) {
							cancel_dispatched_request(p_context, completion.request_id);
							client.force_close = true;
						}
					} else if (!completion.server_messages.is_empty() && (client.rpc.get_protocol_version() == "1.1" || client.rpc.get_protocol_version() == "1.2" || client.rpc.get_protocol_version() == "1.3" || client.rpc.get_protocol_version() == "1.4")) {
						const String request_id = outcome.response.get("request_id", String());
						if (request_id.is_empty() || !start_snapshot_stream(client, completion.request_id, request_id, completion.server_messages, completion_now_usec)) {
							cancel_dispatched_request(p_context, completion.request_id);
							client.force_close = true;
						}
					}
					break;
				}
			}
			if (completion.cancel_dispatch || (!handled && (!completion.server_messages.is_empty() || completion.snapshot_spool.is_valid()))) {
				cancel_dispatched_request(p_context, completion.request_id);
			}
		}
		Vector<Dictionary> notifications;
		drain_notifications(p_context, notifications);
		for (TransportClient &client : clients) {
			if (client.handshake.get_state() != BridgeHandshakeSession::STATE_AUTHENTICATED || !client.rpc.is_initialized() || (client.rpc.get_protocol_version() != "1.1" && client.rpc.get_protocol_version() != "1.2" && client.rpc.get_protocol_version() != "1.3" && client.rpc.get_protocol_version() != "1.4")) {
				continue;
			}
			for (const Dictionary &notification : notifications) {
				const String notification_version = notification.get("protocol_version", "1.1");
				if ((notification_version == "1.2" && client.rpc.get_protocol_version() == "1.1") ||
						(notification_version == "1.3" && client.rpc.get_protocol_version() != "1.3" && client.rpc.get_protocol_version() != "1.4") ||
						(notification_version == "1.4" && client.rpc.get_protocol_version() != "1.4")) {
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

void BridgeTransportWorker::stage_resource_snapshot_message(uint64_t p_request_id, const Dictionary &p_message) {
	if (!context) {
		return;
	}
	MutexLock lock(context->completion_mutex);
	Completion completion;
	completion.kind = Completion::KIND_RESOURCE_SNAPSHOT_MESSAGE;
	completion.request_id = p_request_id;
	completion.snapshot_message = p_message;
	context->completed_requests.push_back(completion);
}

void BridgeTransportWorker::complete_resource_snapshot(uint64_t p_request_id, const Dictionary &p_result, const Dictionary &p_end_message) {
	if (!context) {
		return;
	}
	MutexLock lock(context->completion_mutex);
	Completion completion;
	completion.kind = Completion::KIND_RESOURCE_SNAPSHOT_END;
	completion.request_id = p_request_id;
	completion.result = p_result;
	completion.snapshot_message = p_end_message;
	context->completed_requests.push_back(completion);
}

void BridgeTransportWorker::abort_resource_snapshot(uint64_t p_request_id, const Array &p_abandoned_messages) {
	if (!context) {
		return;
	}
	MutexLock lock(context->completion_mutex);
	Completion completion;
	completion.kind = Completion::KIND_RESOURCE_SNAPSHOT_ABORT;
	completion.request_id = p_request_id;
	completion.abandoned_messages = p_abandoned_messages;
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
