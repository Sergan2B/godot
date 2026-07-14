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

#include "core/io/dir_access.h"
#include "core/io/file_access.h"
#include "core/io/json.h"
#include "core/os/os.h"
#include "core/templates/local_vector.h"

#include "modules/codex_bridge/editor/bridge_revision_clock.h"
#include "modules/codex_bridge/editor/main_thread_dispatcher.h"
#include "modules/codex_bridge/protocol/bridge_crypto.h"
#include "modules/codex_bridge/protocol/bridge_frame_codec.h"
#include "modules/codex_bridge/protocol/bridge_handshake.h"
#include "modules/codex_bridge/protocol/bridge_rpc_session.h"
#include "modules/codex_bridge/transport/bridge_runtime.h"
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

static PackedByteArray bytes_from_range(uint8_t p_start, int p_count) {
	PackedByteArray bytes;
	bytes.resize(p_count);
	for (int index = 0; index < p_count; index++) {
		bytes.ptrw()[index] = p_start + index;
	}
	return bytes;
}

static PackedByteArray bytes_from_utf8(const String &p_text) {
	const CharString utf8 = p_text.utf8();
	PackedByteArray bytes;
	bytes.resize(utf8.length());
	memcpy(bytes.ptrw(), utf8.get_data(), utf8.length());
	return bytes;
}

class TemporaryBridgeProject {
public:
	String root;
	Error error = OK;

	TemporaryBridgeProject() {
		root = OS::get_singleton()->get_temp_path().path_join("gcb_" + itos(OS::get_singleton()->get_process_id()) + "_" + itos(OS::get_singleton()->get_ticks_usec()));
		error = DirAccess::make_dir_absolute(root);
		if (error != OK) {
			return;
		}
		Ref<FileAccess> project_file = FileAccess::open(root.path_join("project.godot"), FileAccess::WRITE, &error);
		if (project_file.is_valid()) {
			project_file->store_string("[application]\nconfig/name=\"Codex Bridge Test\"\n");
			project_file->flush();
		}
	}

	~TemporaryBridgeProject() {
		Error open_error = OK;
		Ref<DirAccess> directory = DirAccess::open(root, &open_error);
		if (directory.is_valid()) {
			directory->erase_contents_recursive();
			directory.unref();
			DirAccess::remove_absolute(root);
		}
	}
};

static PackedByteArray read_file_bytes(const String &p_path) {
	Error error = OK;
	Ref<FileAccess> file = FileAccess::open(p_path, FileAccess::READ, &error);
	if (error != OK || file.is_null()) {
		return PackedByteArray();
	}
	return file->get_buffer(file->get_length());
}

#if defined(UNIX_ENABLED) || defined(WINDOWS_ENABLED)

static Ref<BridgeStreamPeer> connect_test_client(const String &p_endpoint) {
	Ref<BridgeStreamPeer> peer;
	peer.instantiate();
#ifdef WINDOWS_ENABLED
	const String prefix = "127.0.0.1:";
	if (!p_endpoint.begins_with(prefix) || peer->connect_to_host(IPAddress("127.0.0.1"), p_endpoint.trim_prefix(prefix).to_int()) != OK) {
#else
	if (peer->connect_to_host(p_endpoint) != OK) {
#endif
		peer.unref();
		return peer;
	}
	const uint64_t deadline = OS::get_singleton()->get_ticks_usec() + 2000000;
	while (OS::get_singleton()->get_ticks_usec() < deadline) {
		if (peer->poll() != OK) {
			peer.unref();
			return peer;
		}
		if (peer->get_status() == BridgeStreamPeer::STATUS_CONNECTED) {
			return peer;
		}
		OS::get_singleton()->delay_usec(1000);
	}
	peer.unref();
	return peer;
}

static bool send_test_object(const Ref<BridgeStreamPeer> &p_peer, const Dictionary &p_object, int p_fragment_size = 0) {
	PackedByteArray frame;
	if (BridgeFrameCodec::encode_json(p_object, frame) != OK) {
		return false;
	}
	const uint64_t deadline = OS::get_singleton()->get_ticks_usec() + 2000000;
	int offset = 0;
	while (offset < frame.size() && OS::get_singleton()->get_ticks_usec() < deadline) {
		const int requested = p_fragment_size > 0 ? MIN(p_fragment_size, frame.size() - offset) : frame.size() - offset;
		int sent = 0;
		if (p_peer->put_partial_data(frame.ptr() + offset, requested, sent) != OK) {
			return false;
		}
		offset += sent;
		if (sent == 0) {
			OS::get_singleton()->delay_usec(1000);
		}
	}
	return offset == frame.size();
}

static bool receive_test_object(const Ref<BridgeStreamPeer> &p_peer, Dictionary &r_object) {
	BridgeFrameCodec codec;
	const uint64_t deadline = OS::get_singleton()->get_ticks_usec() + 2000000;
	while (OS::get_singleton()->get_ticks_usec() < deadline) {
		if (p_peer->poll() != OK || p_peer->get_status() != BridgeStreamPeer::STATUS_CONNECTED) {
			return false;
		}
		const int available = p_peer->get_available_bytes();
		if (available <= 0) {
			OS::get_singleton()->delay_usec(1000);
			continue;
		}
		PackedByteArray bytes;
		bytes.resize(MIN(available, 65536));
		int received = 0;
		if (p_peer->get_partial_data(bytes.ptrw(), bytes.size(), received) != OK || received <= 0) {
			return false;
		}
		Vector<PackedByteArray> frames;
		if (codec.feed(bytes.ptr(), received, frames) != OK) {
			return false;
		}
		if (!frames.is_empty()) {
			return BridgeJson::parse_strict_object(frames[0], r_object) == OK;
		}
	}
	return false;
}

static Dictionary make_runtime_hello(const Dictionary &p_discovery, const String &p_project_id, const String &p_version, String &r_nonce) {
	PackedByteArray nonce;
	BridgeCrypto::random_bytes(32, nonce);
	BridgeCrypto::base64url_encode_32(nonce, r_nonce);
	Dictionary hello;
	hello["handshake_version"] = "1.0";
	hello["kind"] = "handshake.client_hello";
	Array versions;
	versions.push_back(p_version);
	hello["supported_protocol_versions"] = versions;
	hello["project_id"] = p_project_id;
	hello["editor_session_id"] = p_discovery["editor_session_id"];
	hello["client_nonce"] = r_nonce;
	return hello;
}

#endif // UNIX_ENABLED || WINDOWS_ENABLED

TEST_CASE("[CodexBridge] Crypto matches the canonical handshake vector") {
	const PackedByteArray token = bytes_from_range(0xa0, 32);
	const PackedByteArray client_nonce = bytes_from_range(0x00, 32);
	const PackedByteArray server_nonce = bytes_from_range(0x20, 32);
	PackedStringArray supported_versions;
	supported_versions.push_back("1.0");

	PackedByteArray transcript;
	REQUIRE(BridgeCrypto::build_handshake_transcript(
					"1.0",
					supported_versions,
					"1.0",
					"project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd",
					"editor:0123456789abcdef0123456789abcdef",
					client_nonce,
					server_nonce,
					transcript) == OK);
	CHECK(BridgeCrypto::bytes_to_lower_hex(transcript) == "676f646f742d636f6465782d6272696467652f68616e647368616b652d7472616e7363726970742f76310000000003312e300000000100000003312e3000000003312e300000004f70726f6a6563743a7368613235363a3934636330633834313963666565636164626536326466626139336238393439616366316439626363343865643630326231393464306463346335336264636400000027656469746f723a303132333435363738396162636465663031323334353637383961626364656600000020000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f00000020202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f");

	PackedByteArray server_proof;
	REQUIRE(BridgeCrypto::handshake_proof(true, token, transcript, server_proof) == OK);
	String server_proof_encoded;
	REQUIRE(BridgeCrypto::base64url_encode_32(server_proof, server_proof_encoded) == OK);
	CHECK(server_proof_encoded == "FA-WdGW6swe0_f-qWeYapRoRqHvPjBQIWI-zkFLUFI8");

	PackedByteArray client_proof;
	REQUIRE(BridgeCrypto::handshake_proof(false, token, transcript, client_proof) == OK);
	String client_proof_encoded;
	REQUIRE(BridgeCrypto::base64url_encode_32(client_proof, client_proof_encoded) == OK);
	CHECK(client_proof_encoded == "Po5Rvle7JrEO3__pIRWtSyFMNJSFX3XSU6iVRMbUvw0");

	PackedByteArray decoded;
	REQUIRE(BridgeCrypto::base64url_decode_32(client_proof_encoded, decoded) == OK);
	CHECK(BridgeCrypto::constant_time_equal(client_proof, decoded));
	CHECK(BridgeCrypto::base64url_decode_32(client_proof_encoded + "=", decoded) == ERR_INVALID_DATA);
	CHECK(BridgeCrypto::base64url_decode_32(client_proof_encoded.left(42) + "!", decoded) == ERR_INVALID_DATA);
	CHECK(BridgeCrypto::base64url_decode_32(client_proof_encoded.left(42) + "1", decoded) == ERR_INVALID_DATA);
}

TEST_CASE("[CodexBridge] Project identity matches the protocol fixture") {
	String project_id;
	REQUIRE(BridgeCrypto::project_id_from_canonical_root("/fixtures/codex-smoke", project_id) == OK);
	CHECK(project_id == "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd");
}

TEST_CASE("[CodexBridge] Framing supports fragmentation and multiple frames") {
	Dictionary first_object;
	first_object["kind"] = "first";
	Dictionary second_object;
	second_object["kind"] = "second";
	PackedByteArray first_frame;
	PackedByteArray second_frame;
	REQUIRE(BridgeFrameCodec::encode_json(first_object, first_frame) == OK);
	REQUIRE(BridgeFrameCodec::encode_json(second_object, second_frame) == OK);

	PackedByteArray wire;
	wire.append_array(first_frame);
	wire.append_array(second_frame);
	BridgeFrameCodec codec;
	Vector<PackedByteArray> decoded_frames;
	REQUIRE(codec.feed(wire.ptr(), 2, decoded_frames) == OK);
	CHECK(decoded_frames.is_empty());
	REQUIRE(codec.feed(wire.ptr() + 2, wire.size() - 2, decoded_frames) == OK);
	REQUIRE(decoded_frames.size() == 2);

	Dictionary decoded_first;
	Dictionary decoded_second;
	REQUIRE(BridgeJson::parse_strict_object(decoded_frames[0], decoded_first) == OK);
	REQUIRE(BridgeJson::parse_strict_object(decoded_frames[1], decoded_second) == OK);
	CHECK(decoded_first["kind"] == "first");
	CHECK(decoded_second["kind"] == "second");
}

TEST_CASE("[CodexBridge] Framing and strict JSON reject unsafe inputs") {
	BridgeFrameCodec codec;
	Vector<PackedByteArray> frames;
	const uint8_t zero_length[] = { 0, 0, 0, 0 };
	CHECK(codec.feed(zero_length, 4, frames) == ERR_INVALID_DATA);
	const uint8_t oversized[] = { 0, 0x10, 0, 1 };
	CHECK(codec.feed(oversized, 4, frames) == ERR_INVALID_DATA);

	Dictionary object;
	CHECK(BridgeJson::parse_strict_object(bytes_from_utf8("{\"same\":1,\"same\":2}"), object) == ERR_INVALID_DATA);
	CHECK(BridgeJson::parse_strict_object(bytes_from_utf8("{\"same\":1,\"\\u0073ame\":2}"), object) == ERR_INVALID_DATA);
	CHECK(BridgeJson::parse_strict_object(bytes_from_utf8("[1,2,3]"), object) == ERR_INVALID_DATA);
	String deeply_nested = "{\"value\":";
	for (int index = 0; index <= BridgeJson::MAX_NESTING_DEPTH; index++) {
		deeply_nested += "[";
	}
	deeply_nested += "0";
	for (int index = 0; index <= BridgeJson::MAX_NESTING_DEPTH; index++) {
		deeply_nested += "]";
	}
	deeply_nested += "}";
	CHECK(BridgeJson::parse_strict_object(bytes_from_utf8(deeply_nested), object) == ERR_OUT_OF_MEMORY);
	PackedByteArray invalid_utf8;
	invalid_utf8.push_back('{');
	invalid_utf8.push_back('"');
	invalid_utf8.push_back(0xc0);
	invalid_utf8.push_back(0x80);
	invalid_utf8.push_back('"');
	invalid_utf8.push_back(':');
	invalid_utf8.push_back('1');
	invalid_utf8.push_back('}');
	CHECK(BridgeJson::parse_strict_object(invalid_utf8, object) == ERR_INVALID_DATA);
	PackedByteArray bom = bytes_from_utf8("{\"valid\":true}");
	bom.insert(0, 0xbf);
	bom.insert(0, 0xbb);
	bom.insert(0, 0xef);
	CHECK(BridgeJson::parse_strict_object(bom, object) == ERR_INVALID_DATA);
}

static Dictionary make_client_hello(const String &p_project_id, const String &p_editor_session_id, const String &p_nonce, const String &p_version = "1.0") {
	Dictionary hello;
	hello["handshake_version"] = "1.0";
	hello["kind"] = "handshake.client_hello";
	Array versions;
	versions.push_back(p_version);
	hello["supported_protocol_versions"] = versions;
	hello["project_id"] = p_project_id;
	hello["editor_session_id"] = p_editor_session_id;
	hello["client_nonce"] = p_nonce;
	return hello;
}

static Dictionary make_rpc_context(const String &p_project_id, const String &p_editor_session_id) {
	Dictionary context;
	context["project_id"] = p_project_id;
	context["editor_session_id"] = p_editor_session_id;
	return context;
}

static Dictionary make_rpc_request(const String &p_request_id, const String &p_method, const Dictionary &p_params, const String &p_project_id, const String &p_editor_session_id, int64_t p_deadline_ms = 5000, const String &p_protocol_version = "1.0") {
	Dictionary request;
	request["protocol_version"] = p_protocol_version;
	request["kind"] = "request";
	request["request_id"] = p_request_id;
	request["method"] = p_method;
	request["params"] = p_params;
	request["context"] = make_rpc_context(p_project_id, p_editor_session_id);
	if (p_deadline_ms > 0) {
		request["deadline_ms"] = p_deadline_ms;
	}
	return request;
}

static Dictionary make_initialize_params() {
	Dictionary client;
	client["name"] = "codex-bridge-test";
	client["version"] = "0.1.0";
	Dictionary params;
	params["client"] = client;
	Array requested;
	requested.push_back("bridge.lifecycle");
#ifdef WINDOWS_ENABLED
	requested.push_back("transport.tcp_loopback");
#else
	requested.push_back("transport.uds");
#endif
	params["requested_capabilities"] = requested;
	return params;
}

static String rpc_error_code(const BridgeRpcSession::Outcome &p_outcome) {
	if (!p_outcome.has_response || !p_outcome.response.has("error")) {
		return String();
	}
	return Dictionary(p_outcome.response["error"])["code"];
}

TEST_CASE("[CodexBridge] Mutual handshake authenticates both peers") {
	const String project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	const String editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	const PackedByteArray token = bytes_from_range(0xa0, 32);
	const PackedByteArray client_nonce = bytes_from_range(0x00, 32);
	String client_nonce_encoded;
	REQUIRE(BridgeCrypto::base64url_encode_32(client_nonce, client_nonce_encoded) == OK);

	BridgeHandshakeSession handshake(token, project_id, editor_session_id, 100);
	BridgeHandshakeSession::Outcome challenge;
	REQUIRE(handshake.handle_message(make_client_hello(project_id, editor_session_id, client_nonce_encoded), 200, challenge) == OK);
	REQUIRE(challenge.has_response);
	CHECK_FALSE(challenge.close_after_response);
	CHECK(challenge.response["kind"] == "handshake.server_challenge");

	PackedByteArray server_nonce;
	REQUIRE(BridgeCrypto::base64url_decode_32(challenge.response["server_nonce"], server_nonce) == OK);
	PackedStringArray versions;
	versions.push_back("1.0");
	PackedByteArray transcript;
	REQUIRE(BridgeCrypto::build_handshake_transcript("1.0", versions, "1.0", project_id, editor_session_id, client_nonce, server_nonce, transcript) == OK);
	PackedByteArray expected_server_proof;
	REQUIRE(BridgeCrypto::handshake_proof(true, token, transcript, expected_server_proof) == OK);
	PackedByteArray received_server_proof;
	REQUIRE(BridgeCrypto::base64url_decode_32(challenge.response["server_proof"], received_server_proof) == OK);
	CHECK(BridgeCrypto::constant_time_equal(expected_server_proof, received_server_proof));

	PackedByteArray client_proof;
	REQUIRE(BridgeCrypto::handshake_proof(false, token, transcript, client_proof) == OK);
	String client_proof_encoded;
	REQUIRE(BridgeCrypto::base64url_encode_32(client_proof, client_proof_encoded) == OK);
	Dictionary authenticate;
	authenticate["handshake_version"] = "1.0";
	authenticate["kind"] = "handshake.client_authenticate";
	authenticate["selected_protocol_version"] = "1.0";
	authenticate["project_id"] = project_id;
	authenticate["editor_session_id"] = editor_session_id;
	authenticate["client_proof"] = client_proof_encoded;
	BridgeHandshakeSession::Outcome ready;
	REQUIRE(handshake.handle_message(authenticate, 300, ready) == OK);
	CHECK(ready.response["kind"] == "handshake.server_ready");
	CHECK(handshake.get_state() == BridgeHandshakeSession::STATE_AUTHENTICATED);
	CHECK(handshake.handle_message(authenticate, 400, ready) == ERR_INVALID_DATA);
}

TEST_CASE("[CodexBridge] Handshake negotiates the compatible 1.1 minor") {
	const String project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	const String editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	const PackedByteArray token = bytes_from_range(0xa0, 32);
	const PackedByteArray nonce = bytes_from_range(0x20, 32);
	String nonce_encoded;
	REQUIRE(BridgeCrypto::base64url_encode_32(nonce, nonce_encoded) == OK);

	BridgeHandshakeSession handshake(token, project_id, editor_session_id, 0);
	BridgeHandshakeSession::Outcome challenge;
	REQUIRE(handshake.handle_message(make_client_hello(project_id, editor_session_id, nonce_encoded, "1.1"), 1, challenge) == OK);
	CHECK(challenge.response["selected_protocol_version"] == "1.1");
	CHECK(handshake.get_selected_protocol_version() == "1.1");
}

TEST_CASE("[CodexBridge] Handshake rejects binding, version, proof, replay, and timeout failures") {
	const String project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	const String editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	const PackedByteArray token = bytes_from_range(0xa0, 32);
	const PackedByteArray nonce = bytes_from_range(0x00, 32);
	String nonce_encoded;
	REQUIRE(BridgeCrypto::base64url_encode_32(nonce, nonce_encoded) == OK);

	BridgeHandshakeSession::Outcome outcome;
	BridgeHandshakeSession wrong_project(token, project_id, editor_session_id, 0);
	REQUIRE(wrong_project.handle_message(make_client_hello("project:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", editor_session_id, nonce_encoded), 1, outcome) == OK);
	CHECK(Dictionary(outcome.response["error"])["code"] == "project_not_bound");
	CHECK(outcome.close_after_response);

	BridgeHandshakeSession wrong_version(token, project_id, editor_session_id, 0);
	REQUIRE(wrong_version.handle_message(make_client_hello(project_id, editor_session_id, nonce_encoded, "2.0"), 1, outcome) == OK);
	CHECK(Dictionary(outcome.response["error"])["code"] == "protocol_mismatch");
	CHECK(outcome.response.has("supported_protocol_versions"));

	BridgeHandshakeSession wrong_proof(token, project_id, editor_session_id, 0);
	REQUIRE(wrong_proof.handle_message(make_client_hello(project_id, editor_session_id, nonce_encoded), 1, outcome) == OK);
	Dictionary authenticate;
	authenticate["handshake_version"] = "1.0";
	authenticate["kind"] = "handshake.client_authenticate";
	authenticate["selected_protocol_version"] = "1.0";
	authenticate["project_id"] = project_id;
	authenticate["editor_session_id"] = editor_session_id;
	authenticate["client_proof"] = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
	REQUIRE(wrong_proof.handle_message(authenticate, 2, outcome) == OK);
	CHECK(Dictionary(outcome.response["error"])["code"] == "unauthenticated");
	CHECK(outcome.authentication_failed);

	HashSet<String> seen_nonces;
	BridgeHandshakeSession first(token, project_id, editor_session_id, 0, &seen_nonces);
	REQUIRE(first.handle_message(make_client_hello(project_id, editor_session_id, nonce_encoded), 1, outcome) == OK);
	BridgeHandshakeSession replay(token, project_id, editor_session_id, 0, &seen_nonces);
	CHECK(replay.handle_message(make_client_hello(project_id, editor_session_id, nonce_encoded), 1, outcome) == ERR_INVALID_DATA);

	BridgeHandshakeSession timed_out(token, project_id, editor_session_id, 10);
	CHECK(timed_out.handle_message(make_client_hello(project_id, editor_session_id, nonce_encoded), 10 + BridgeHandshakeSession::HANDSHAKE_TIMEOUT_USEC, outcome) == ERR_TIMEOUT);
}

TEST_CASE("[CodexBridge] RPC lifecycle enforces initialization and returns canonical results") {
	const String project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	const String editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	BridgeRpcSession rpc(project_id, editor_session_id);
	BridgeRpcSession::Outcome outcome;

	Dictionary ping_params;
	ping_params["echo"] = "before-initialize";
	REQUIRE(rpc.handle_message(make_rpc_request("req:before", "bridge.ping", ping_params, project_id, editor_session_id), 100, 1, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "not_initialized");
	CHECK_FALSE(outcome.dispatch);

	REQUIRE(rpc.handle_message(make_rpc_request("req:init", "bridge.initialize", make_initialize_params(), project_id, editor_session_id), 200, 2, outcome) == OK);
	CHECK(outcome.dispatch);
	CHECK(outcome.method == BridgeRpcSession::METHOD_INITIALIZE);
	CHECK(outcome.deadline_usec == 5000200);
	REQUIRE(rpc.complete(2, 300, outcome) == OK);
	REQUIRE(outcome.has_response);
	REQUIRE(outcome.response.has("result"));
	const Dictionary initialize_result = outcome.response["result"];
	CHECK(initialize_result["protocol_version"] == "1.0");
	CHECK(initialize_result["project_id"] == project_id);
	CHECK(initialize_result["editor_session_id"] == editor_session_id);
	CHECK(Array(initialize_result["capabilities"]).size() == 2);
	CHECK((int64_t)Dictionary(initialize_result["limits"])["max_in_flight_requests"] == 64);
	CHECK((int64_t)Dictionary(initialize_result["revisions"])["project_revision"] == 0);
	CHECK(rpc.is_initialized());

	REQUIRE(rpc.handle_message(make_rpc_request("req:init-again", "bridge.initialize", make_initialize_params(), project_id, editor_session_id), 400, 3, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "already_initialized");

	ping_params["echo"] = String::utf8("готово");
	REQUIRE(rpc.handle_message(make_rpc_request("req:ping", "bridge.ping", ping_params, project_id, editor_session_id, -1), 500, 4, outcome) == OK);
	CHECK(outcome.dispatch);
	REQUIRE(rpc.complete(4, 600, outcome) == OK);
	CHECK(Dictionary(outcome.response["result"])["echo"] == ping_params["echo"]);

	REQUIRE(rpc.handle_message(make_rpc_request("req:caps", "bridge.capabilities", Dictionary(), project_id, editor_session_id), 700, 5, outcome) == OK);
	CHECK(outcome.method == BridgeRpcSession::METHOD_CAPABILITIES);
	REQUIRE(rpc.complete(5, 800, outcome) == OK);
	const Dictionary capabilities_result = outcome.response["result"];
	CHECK(Array(capabilities_result["capabilities"]).size() == 2);
	CHECK((int64_t)Dictionary(capabilities_result["limits"])["frame_bytes"] == 1048576);

	REQUIRE(rpc.handle_message(make_rpc_request("req:unknown", "bridge.unknown", Dictionary(), project_id, editor_session_id), 900, 6, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "method_not_found");

	REQUIRE(rpc.handle_message(make_rpc_request("req:shutdown", "bridge.shutdown", Dictionary(), project_id, editor_session_id), 1000, 7, outcome) == OK);
	CHECK(outcome.method == BridgeRpcSession::METHOD_SHUTDOWN);
	REQUIRE(rpc.complete(7, 1100, outcome) == OK);
	CHECK((bool)Dictionary(outcome.response["result"])["closing"]);
	CHECK(outcome.close_after_response);
}

TEST_CASE("[CodexBridge] RPC validates envelopes, parameters, and duplicate request IDs") {
	const String project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	const String editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	BridgeRpcSession rpc(project_id, editor_session_id);
	BridgeRpcSession::Outcome outcome;

	Dictionary invalid_id = make_rpc_request("bad", "bridge.initialize", make_initialize_params(), project_id, editor_session_id);
	CHECK(rpc.handle_message(invalid_id, 0, 1, outcome) == ERR_INVALID_DATA);

	Dictionary wrong_context = make_rpc_request("req:wrong-context", "bridge.initialize", make_initialize_params(), project_id, editor_session_id);
	wrong_context["context"] = make_rpc_context("project:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", editor_session_id);
	REQUIRE(rpc.handle_message(wrong_context, 0, 2, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "invalid_request");

	Dictionary bad_deadline = make_rpc_request("req:bad-deadline", "bridge.initialize", make_initialize_params(), project_id, editor_session_id);
	bad_deadline["deadline_ms"] = 30001;
	REQUIRE(rpc.handle_message(bad_deadline, 0, 3, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "invalid_request");

	REQUIRE(rpc.handle_message(make_rpc_request("req:init", "bridge.initialize", make_initialize_params(), project_id, editor_session_id), 0, 4, outcome) == OK);
	REQUIRE(rpc.complete(4, 1, outcome) == OK);

	Dictionary ping_params;
	ping_params["echo"] = "original";
	const Dictionary ping = make_rpc_request("req:duplicate", "bridge.ping", ping_params, project_id, editor_session_id);
	REQUIRE(rpc.handle_message(ping, 2, 5, outcome) == OK);
	CHECK(outcome.dispatch);
	REQUIRE(rpc.handle_message(ping, 3, 6, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "duplicate_request_id");
	CHECK(rpc.get_in_flight_count() == 1);
	REQUIRE(rpc.complete(5, 4, outcome) == OK);
	CHECK(Dictionary(outcome.response["result"])["echo"] == "original");

	String oversized_echo;
	for (int index = 0; index < 129; index++) {
		oversized_echo += String::utf8("я");
	}
	ping_params["echo"] = oversized_echo;
	REQUIRE(rpc.handle_message(make_rpc_request("req:large-echo", "bridge.ping", ping_params, project_id, editor_session_id), 5, 7, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "invalid_request");

	Dictionary server_response;
	server_response["kind"] = "response";
	CHECK(rpc.handle_message(server_response, 6, 8, outcome) == ERR_INVALID_DATA);
}

TEST_CASE("[CodexBridge] RPC 1.1 exposes editor sync and accepts an atomic snapshot result") {
	const String project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	const String editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	BridgeRpcSession rpc(project_id, editor_session_id);
	rpc.set_protocol_version("1.1");
	BridgeRpcSession::Outcome outcome;

	REQUIRE(rpc.handle_message(make_rpc_request("req:init-sync", "bridge.initialize", make_initialize_params(), project_id, editor_session_id, 5000, "1.1"), 0, 1, outcome) == OK);
	Dictionary live_revisions;
	live_revisions["editor_session_id"] = editor_session_id;
	live_revisions["event_seq"] = 7;
	live_revisions["project_revision"] = 3;
	live_revisions["operation_seq"] = 0;
	live_revisions["scene_revisions"] = Dictionary();
	Dictionary initialize_override;
	initialize_override["revisions"] = live_revisions;
	REQUIRE(rpc.complete(1, 1, initialize_override, outcome) == OK);
	const Dictionary initialize_result = outcome.response["result"];
	CHECK(initialize_result["protocol_version"] == "1.1");
	CHECK(Array(initialize_result["capabilities"]).size() == 6);
	CHECK((int64_t)Dictionary(initialize_result["limits"])["snapshot_chunk_bytes"] == 524288);
	CHECK((int64_t)Dictionary(initialize_result["revisions"])["event_seq"] == 7);

	Dictionary snapshot_params;
	Array domains;
	domains.push_back("editor_context");
	domains.push_back("editor_inspector");
	snapshot_params["domains"] = domains;
	REQUIRE(rpc.handle_message(make_rpc_request("req:snapshot", "editor.snapshot.get", snapshot_params, project_id, editor_session_id, 5000, "1.1"), 2, 2, outcome) == OK);
	CHECK(outcome.method == BridgeRpcSession::METHOD_EDITOR_SNAPSHOT);
	CHECK(outcome.params == snapshot_params);
	Dictionary snapshot_result;
	snapshot_result["snapshot_id"] = "snapshot:0123456789abcdef0123456789abcdef";
	snapshot_result["base_event_seq"] = 7;
	snapshot_result["revisions"] = live_revisions;
	REQUIRE(rpc.complete(2, 3, snapshot_result, outcome) == OK);
	CHECK(Dictionary(outcome.response["result"])["snapshot_id"] == snapshot_result["snapshot_id"]);

	Dictionary ack;
	ack["protocol_version"] = "1.1";
	ack["kind"] = "ack";
	ack["ack_id"] = "ack:snapshot";
	Dictionary ack_params;
	ack_params["snapshot_id"] = snapshot_result["snapshot_id"];
	ack_params["through_chunk"] = 0;
	ack["params"] = ack_params;
	ack["context"] = make_rpc_context(project_id, editor_session_id);
	REQUIRE(rpc.handle_message(ack, 4, 3, outcome) == OK);
	CHECK_FALSE(outcome.has_response);
	ack["params"] = Dictionary();
	CHECK(rpc.handle_message(ack, 5, 4, outcome) == ERR_INVALID_DATA);
}

TEST_CASE("[CodexBridge] Revision clock advances selection and scene domains monotonically") {
	const String editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	const String scene_id = "scene:0123456789abcdef0123456789abcdef";
	BridgeRevisionClock revisions;
	revisions.initialize(editor_session_id);
	CHECK(revisions.record_selection_change() == 1);
	CHECK(revisions.record_scene_change(scene_id) == 2);
	CHECK(revisions.record_scene_change(scene_id) == 3);
	CHECK(revisions.get_scene_revision(scene_id) == 2);
	const Dictionary vector = revisions.get_revision_vector();
	CHECK(vector["editor_session_id"] == editor_session_id);
	CHECK((int64_t)vector["event_seq"] == 3);
	CHECK((int64_t)vector["project_revision"] == 2);
	CHECK((int64_t)Dictionary(vector["scene_revisions"])[scene_id] == 2);
}

TEST_CASE("[CodexBridge] RPC deadlines, cancellation, rejection, and in-flight limits are terminal once") {
	const String project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	const String editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	BridgeRpcSession rpc(project_id, editor_session_id);
	BridgeRpcSession::Outcome outcome;

	REQUIRE(rpc.handle_message(make_rpc_request("req:init-rejected", "bridge.initialize", make_initialize_params(), project_id, editor_session_id), 0, 1, outcome) == OK);
	REQUIRE(rpc.reject_dispatch(1, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "overloaded");
	CHECK_FALSE(rpc.is_initialized());
	REQUIRE(rpc.handle_message(make_rpc_request("req:init", "bridge.initialize", make_initialize_params(), project_id, editor_session_id), 10, 2, outcome) == OK);
	REQUIRE(rpc.complete(2, 20, outcome) == OK);

	Dictionary ping_params;
	ping_params["echo"] = "deadline";
	REQUIRE(rpc.handle_message(make_rpc_request("req:deadline", "bridge.ping", ping_params, project_id, editor_session_id, 1), 100, 3, outcome) == OK);
	Vector<BridgeRpcSession::Outcome> expired;
	rpc.expire_requests(1100, expired);
	REQUIRE(expired.size() == 1);
	CHECK(rpc_error_code(expired[0]) == "deadline_exceeded");
	CHECK(expired[0].cancel_dispatch);
	CHECK(rpc.complete(3, 1200, outcome) == ERR_DOES_NOT_EXIST);

	ping_params["echo"] = "cancel";
	REQUIRE(rpc.handle_message(make_rpc_request("req:cancel", "bridge.ping", ping_params, project_id, editor_session_id), 2000, 4, outcome) == OK);
	Dictionary cancel;
	cancel["protocol_version"] = "1.0";
	cancel["kind"] = "cancel";
	cancel["request_id"] = "req:cancel";
	cancel["context"] = make_rpc_context(project_id, editor_session_id);
	cancel["reason"] = "client_cancelled";
	REQUIRE(rpc.handle_message(cancel, 2100, 5, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "cancelled");
	CHECK(outcome.cancel_dispatch);
	CHECK(rpc.complete(4, 2200, outcome) == ERR_DOES_NOT_EXIST);
	REQUIRE(rpc.handle_message(cancel, 2300, 6, outcome) == OK);
	CHECK_FALSE(outcome.has_response);

	for (uint64_t index = 0; index < BridgeRpcSession::MAX_IN_FLIGHT_REQUESTS; index++) {
		ping_params["echo"] = itos(index);
		REQUIRE(rpc.handle_message(make_rpc_request("req:pending-" + itos(index), "bridge.ping", ping_params, project_id, editor_session_id), 3000, 100 + index, outcome) == OK);
		REQUIRE(outcome.dispatch);
	}
	CHECK(rpc.get_in_flight_count() == BridgeRpcSession::MAX_IN_FLIGHT_REQUESTS);
	REQUIRE(rpc.handle_message(make_rpc_request("req:over-limit", "bridge.ping", ping_params, project_id, editor_session_id), 3000, 1000, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "overloaded");
	Vector<uint64_t> cancelled;
	rpc.cancel_all(cancelled);
	CHECK(cancelled.size() == BridgeRpcSession::MAX_IN_FLIGHT_REQUESTS);
	CHECK(rpc.get_in_flight_count() == 0);
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

TEST_CASE("[CodexBridge] Notification journal enforces its negotiated entry bound") {
	BridgeTransportWorker worker;
	REQUIRE(worker.start() == OK);
	Dictionary notification;
	notification["protocol_version"] = "1.1";
	notification["kind"] = "notification";
	notification["method"] = "sync.event";
	notification["params"] = Dictionary();
	notification["context"] = Dictionary();
	bool accepted = true;
	for (int index = 0; index < 4096; index++) {
		notification["sequence"] = index;
		accepted = accepted && worker.publish_notification(notification);
	}
	CHECK(accepted);
	CHECK_FALSE(worker.publish_notification(notification));
	CHECK(worker.stop() == BridgeTransportWorker::STOPPED);
}

#if defined(UNIX_ENABLED) || defined(WINDOWS_ENABLED)

static String runtime_endpoint(const String &p_project_root, const Dictionary &p_discovery) {
#ifdef WINDOWS_ENABLED
	return p_discovery["endpoint"];
#else
	return p_project_root.path_join(p_discovery["endpoint"]);
#endif
}

static Ref<BridgeStreamPeer> connect_authenticated_test_client(const String &p_project_root, const Dictionary &p_discovery) {
	Ref<BridgeStreamPeer> peer = connect_test_client(runtime_endpoint(p_project_root, p_discovery));
	if (peer.is_null()) {
		return peer;
	}
	const PackedByteArray token = read_file_bytes(p_project_root.path_join(p_discovery["token_file"]));
	if (token.size() != BridgeCrypto::RANDOM_VALUE_BYTES) {
		peer.unref();
		return peer;
	}

	PackedByteArray client_nonce;
	String client_nonce_encoded;
	if (BridgeCrypto::random_bytes(BridgeCrypto::RANDOM_VALUE_BYTES, client_nonce) != OK || BridgeCrypto::base64url_encode_32(client_nonce, client_nonce_encoded) != OK) {
		peer.unref();
		return peer;
	}
	Dictionary hello;
	hello["handshake_version"] = "1.0";
	hello["kind"] = "handshake.client_hello";
	Array versions;
	versions.push_back("1.0");
	hello["supported_protocol_versions"] = versions;
	hello["project_id"] = p_discovery["project_id"];
	hello["editor_session_id"] = p_discovery["editor_session_id"];
	hello["client_nonce"] = client_nonce_encoded;
	Dictionary challenge;
	if (!send_test_object(peer, hello, 3) || !receive_test_object(peer, challenge) || challenge.get("kind", "") != "handshake.server_challenge") {
		peer.unref();
		return peer;
	}

	PackedByteArray server_nonce;
	PackedByteArray received_server_proof;
	PackedStringArray offered;
	offered.push_back("1.0");
	PackedByteArray transcript;
	PackedByteArray expected_server_proof;
	if (BridgeCrypto::base64url_decode_32(challenge["server_nonce"], server_nonce) != OK ||
			BridgeCrypto::base64url_decode_32(challenge["server_proof"], received_server_proof) != OK ||
			BridgeCrypto::build_handshake_transcript("1.0", offered, "1.0", p_discovery["project_id"], p_discovery["editor_session_id"], client_nonce, server_nonce, transcript) != OK ||
			BridgeCrypto::handshake_proof(true, token, transcript, expected_server_proof) != OK ||
			!BridgeCrypto::constant_time_equal(expected_server_proof, received_server_proof)) {
		peer.unref();
		return peer;
	}

	PackedByteArray client_proof;
	String client_proof_encoded;
	if (BridgeCrypto::handshake_proof(false, token, transcript, client_proof) != OK || BridgeCrypto::base64url_encode_32(client_proof, client_proof_encoded) != OK) {
		peer.unref();
		return peer;
	}
	Dictionary authenticate;
	authenticate["handshake_version"] = "1.0";
	authenticate["kind"] = "handshake.client_authenticate";
	authenticate["selected_protocol_version"] = "1.0";
	authenticate["project_id"] = p_discovery["project_id"];
	authenticate["editor_session_id"] = p_discovery["editor_session_id"];
	authenticate["client_proof"] = client_proof_encoded;
	Dictionary ready;
	if (!send_test_object(peer, authenticate) || !receive_test_object(peer, ready) || ready.get("kind", "") != "handshake.server_ready") {
		peer.unref();
	}
	return peer;
}

static void complete_worker_request(const MainThreadDispatcher::Command &p_command, void *p_userdata) {
	static_cast<BridgeTransportWorker *>(p_userdata)->complete_request(p_command.request_id);
}

static bool wait_for_dispatcher_size(MainThreadDispatcher &p_dispatcher, uint32_t p_size) {
	const uint64_t deadline = OS::get_singleton()->get_ticks_usec() + 2000000;
	while (OS::get_singleton()->get_ticks_usec() < deadline) {
		if (p_dispatcher.get_queue_size() == p_size) {
			return true;
		}
		OS::get_singleton()->delay_usec(1000);
	}
	return false;
}

static bool dispatch_worker_request(MainThreadDispatcher &p_dispatcher, BridgeTransportWorker &p_worker) {
	if (!wait_for_dispatcher_size(p_dispatcher, 1)) {
		return false;
	}
	const MainThreadDispatcher::ProcessStats stats = p_dispatcher.process(complete_worker_request, &p_worker);
	return stats.processed == 1;
}

static Dictionary make_rpc_cancel(const String &p_request_id, const Dictionary &p_discovery) {
	Dictionary cancel;
	cancel["protocol_version"] = "1.0";
	cancel["kind"] = "cancel";
	cancel["request_id"] = p_request_id;
	cancel["context"] = make_rpc_context(p_discovery["project_id"], p_discovery["editor_session_id"]);
	cancel["reason"] = "client_cancelled";
	return cancel;
}

static bool wait_for_disconnect(const Ref<BridgeStreamPeer> &p_peer) {
	const uint64_t deadline = OS::get_singleton()->get_ticks_usec() + 2000000;
	while (OS::get_singleton()->get_ticks_usec() < deadline) {
		p_peer->poll();
		if (p_peer->get_status() != BridgeStreamPeer::STATUS_CONNECTED) {
			return true;
		}
		OS::get_singleton()->delay_usec(1000);
	}
	return false;
}

TEST_CASE("[CodexBridge] Private runtime publishes atomically and rotates session secrets") {
	TemporaryBridgeProject project;
	REQUIRE(project.error == OK);
	BridgeRuntime runtime;
	REQUIRE(runtime.initialize(project.root) == OK);
	CHECK(runtime.is_listening());

	const String codex_dir = project.root.path_join(".godot/codex");
	const String run_dir = codex_dir.path_join("run");
	const String discovery_path = codex_dir.path_join("bridge.json");
	const String token_path = codex_dir.path_join("session.token");
	const String lock_path = codex_dir.path_join("bridge.lock");
	CHECK(BridgeRuntime::validate_private_path(codex_dir, 0700, true) == OK);
	CHECK(BridgeRuntime::validate_private_path(run_dir, 0700, true) == OK);
	CHECK(BridgeRuntime::validate_private_path(discovery_path, 0600, false) == OK);
	CHECK(BridgeRuntime::validate_private_path(token_path, 0600, false) == OK);
	CHECK(BridgeRuntime::validate_private_path(lock_path, 0600, false) == OK);
#ifdef UNIX_ENABLED
	CHECK(BridgeRuntime::validate_private_path(runtime.get_endpoint_path(), 0600, false, true) == OK);
#endif

	const PackedByteArray token = read_file_bytes(token_path);
	REQUIRE(token.size() == 32);
	CHECK(BridgeCrypto::constant_time_equal(token, runtime.get_token()));
	const PackedByteArray discovery_bytes = read_file_bytes(discovery_path);
	Dictionary discovery;
	REQUIRE(BridgeJson::parse_strict_object(discovery_bytes, discovery) == OK);
	CHECK((int64_t)discovery["discovery_schema"] == 1);
	CHECK(discovery["project_id"] == runtime.get_project_id());
	CHECK(discovery["editor_session_id"] == runtime.get_editor_session_id());
#ifdef WINDOWS_ENABLED
	CHECK(discovery["transport"] == "tcp_loopback");
	CHECK(String(discovery["endpoint"]).begins_with("127.0.0.1:"));
#else
	CHECK(discovery["transport"] == "uds");
#endif
	CHECK(discovery["token_file"] == ".godot/codex/session.token");
	CHECK_FALSE(String(discovery["endpoint"]).is_absolute_path());
	CHECK_FALSE(String(discovery["endpoint"]).contains(".."));
	CHECK_FALSE(String::utf8(reinterpret_cast<const char *>(discovery_bytes.ptr()), discovery_bytes.size()).contains(project.root));

	BridgeRuntime duplicate;
	CHECK(duplicate.initialize(project.root) == ERR_ALREADY_IN_USE);
	const String first_session = runtime.get_editor_session_id();
	const PackedByteArray first_token = runtime.get_token();
	runtime.cleanup();
	CHECK_FALSE(FileAccess::exists(discovery_path));
	CHECK_FALSE(FileAccess::exists(token_path));
	CHECK_FALSE(FileAccess::exists(lock_path));
#ifdef UNIX_ENABLED
	CHECK_FALSE(FileAccess::exists(runtime.get_endpoint_path()));
#endif

	BridgeRuntime next_session;
	REQUIRE(next_session.initialize(project.root) == OK);
	CHECK(next_session.get_editor_session_id() != first_session);
	CHECK_FALSE(BridgeCrypto::constant_time_equal(next_session.get_token(), first_token));
	next_session.cleanup();
}

#ifdef UNIX_ENABLED
TEST_CASE("[CodexBridge] Private runtime rejects permissive directories") {
	TemporaryBridgeProject project;
	REQUIRE(project.error == OK);
	const String codex_dir = project.root.path_join(".godot/codex");
	REQUIRE(DirAccess::make_dir_recursive_absolute(codex_dir) == OK);
	REQUIRE(FileAccess::set_unix_permissions(codex_dir, 0755) == OK);
	BridgeRuntime runtime;
	CHECK(runtime.initialize(project.root) == ERR_UNAUTHORIZED);
	CHECK_FALSE(FileAccess::exists(codex_dir.path_join("bridge.json")));
}

TEST_CASE("[CodexBridge] Private runtime rejects permissive stale files") {
	TemporaryBridgeProject project;
	REQUIRE(project.error == OK);
	const String codex_dir = project.root.path_join(".godot/codex");
	const String run_dir = codex_dir.path_join("run");
	REQUIRE(DirAccess::make_dir_recursive_absolute(run_dir) == OK);
	REQUIRE(FileAccess::set_unix_permissions(codex_dir, 0700) == OK);
	REQUIRE(FileAccess::set_unix_permissions(run_dir, 0700) == OK);
	Error file_error = OK;
	Ref<FileAccess> token_file = FileAccess::open(codex_dir.path_join("session.token"), FileAccess::WRITE, &file_error);
	REQUIRE(file_error == OK);
	PackedByteArray token;
	token.resize(32);
	token_file->store_buffer(token);
	token_file->close();
	REQUIRE(FileAccess::set_unix_permissions(codex_dir.path_join("session.token"), 0644) == OK);

	BridgeRuntime runtime;
	CHECK(runtime.initialize(project.root) == ERR_UNAUTHORIZED);
	CHECK(FileAccess::exists(codex_dir.path_join("session.token")));
	CHECK_FALSE(FileAccess::exists(codex_dir.path_join("bridge.json")));
}
#endif // UNIX_ENABLED

TEST_CASE("[CodexBridge] Private runtime replaces only inactive unauthenticated stale ownership") {
	TemporaryBridgeProject project;
	REQUIRE(project.error == OK);
	BridgeRuntime first_runtime;
	REQUIRE(first_runtime.initialize(project.root) == OK);
	const String codex_dir = project.root.path_join(".godot/codex");
	Dictionary stale_discovery;
	REQUIRE(BridgeJson::parse_strict_object(read_file_bytes(codex_dir.path_join("bridge.json")), stale_discovery) == OK);
	const PackedByteArray stale_token = first_runtime.get_token();
	const String stale_session = first_runtime.get_editor_session_id();
	first_runtime.cleanup();

	stale_discovery["pid"] = INT32_MAX;
	Error write_error = OK;
	Ref<FileAccess> discovery_file = FileAccess::open(codex_dir.path_join("bridge.json"), FileAccess::WRITE, &write_error);
	REQUIRE(write_error == OK);
	discovery_file->store_string(JSON::stringify(stale_discovery, "", true));
	discovery_file->close();
#ifdef UNIX_ENABLED
	REQUIRE(FileAccess::set_unix_permissions(codex_dir.path_join("bridge.json"), 0600) == OK);
#endif
	Ref<FileAccess> token_file = FileAccess::open(codex_dir.path_join("session.token"), FileAccess::WRITE, &write_error);
	REQUIRE(write_error == OK);
	token_file->store_buffer(stale_token);
	token_file->close();
#ifdef UNIX_ENABLED
	REQUIRE(FileAccess::set_unix_permissions(codex_dir.path_join("session.token"), 0600) == OK);
#endif
	Ref<FileAccess> lock_file = FileAccess::open(codex_dir.path_join("bridge.lock"), FileAccess::WRITE, &write_error);
	REQUIRE(write_error == OK);
	lock_file->store_string("{\"pid\":2147483647}");
	lock_file->close();
#ifdef UNIX_ENABLED
	REQUIRE(FileAccess::set_unix_permissions(codex_dir.path_join("bridge.lock"), 0600) == OK);
#endif

	BridgeRuntime recovered_runtime;
	REQUIRE(recovered_runtime.initialize(project.root) == OK);
	CHECK(recovered_runtime.get_editor_session_id() != stale_session);
	CHECK_FALSE(BridgeCrypto::constant_time_equal(recovered_runtime.get_token(), stale_token));
#ifdef UNIX_ENABLED
	CHECK(BridgeRuntime::validate_private_path(recovered_runtime.get_endpoint_path(), 0600, false, true) == OK);
#endif
	recovered_runtime.cleanup();
}

TEST_CASE("[CodexBridge] Worker serves an authenticated local handshake and preserves active discovery") {
	TemporaryBridgeProject project;
	REQUIRE(project.error == OK);
	BridgeTransportWorker worker;
	REQUIRE(worker.start(project.root) == OK);
	CHECK(BridgeRuntime::probe_authenticated_runtime(project.root));

	const String discovery_path = project.root.path_join(".godot/codex/bridge.json");
	const PackedByteArray discovery_before = read_file_bytes(discovery_path);
	BridgeTransportWorker duplicate;
	CHECK(duplicate.start(project.root) == ERR_ALREADY_IN_USE);
	CHECK(read_file_bytes(discovery_path) == discovery_before);
	CHECK(BridgeRuntime::probe_authenticated_runtime(project.root));

	CHECK(worker.stop() == BridgeTransportWorker::STOPPED);
	CHECK_FALSE(FileAccess::exists(discovery_path));
	CHECK_FALSE(FileAccess::exists(project.root.path_join(".godot/codex/session.token")));
	CHECK_FALSE(FileAccess::exists(project.root.path_join(".godot/codex/bridge.lock")));
}

TEST_CASE("[CodexBridge] Local transport rejects wrong bindings, proof, version, and invalid framing") {
	TemporaryBridgeProject project;
	REQUIRE(project.error == OK);
	BridgeTransportWorker worker;
	REQUIRE(worker.start(project.root) == OK);
	const PackedByteArray discovery_bytes = read_file_bytes(project.root.path_join(".godot/codex/bridge.json"));
	Dictionary discovery;
	REQUIRE(BridgeJson::parse_strict_object(discovery_bytes, discovery) == OK);
	const String endpoint = runtime_endpoint(project.root, discovery);

	String nonce;
	Ref<BridgeStreamPeer> client = connect_test_client(endpoint);
	REQUIRE(client.is_valid());
	REQUIRE(send_test_object(client, make_runtime_hello(discovery, "project:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "1.0", nonce), 1));
	Dictionary response;
	REQUIRE(receive_test_object(client, response));
	CHECK(Dictionary(response["error"])["code"] == "project_not_bound");
	CHECK_FALSE(JSON::stringify(response).contains(project.root));
	client->disconnect_from_host();

	client = connect_test_client(endpoint);
	REQUIRE(client.is_valid());
	REQUIRE(send_test_object(client, make_runtime_hello(discovery, discovery["project_id"], "2.0", nonce)));
	REQUIRE(receive_test_object(client, response));
	CHECK(Dictionary(response["error"])["code"] == "protocol_mismatch");
	CHECK(response.has("supported_protocol_versions"));
	client->disconnect_from_host();

	client = connect_test_client(endpoint);
	REQUIRE(client.is_valid());
	REQUIRE(send_test_object(client, make_runtime_hello(discovery, discovery["project_id"], "1.0", nonce)));
	REQUIRE(receive_test_object(client, response));
	CHECK(response["kind"] == "handshake.server_challenge");
	Dictionary authenticate;
	authenticate["handshake_version"] = "1.0";
	authenticate["kind"] = "handshake.client_authenticate";
	authenticate["selected_protocol_version"] = "1.0";
	authenticate["project_id"] = discovery["project_id"];
	authenticate["editor_session_id"] = discovery["editor_session_id"];
	authenticate["client_proof"] = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
	REQUIRE(send_test_object(client, authenticate));
	REQUIRE(receive_test_object(client, response));
	CHECK(Dictionary(response["error"])["code"] == "unauthenticated");
	CHECK_FALSE(JSON::stringify(response).contains("token"));
	client->disconnect_from_host();

	client = connect_test_client(endpoint);
	REQUIRE(client.is_valid());
	const uint8_t invalid_prefix[] = { 0, 0, 0, 0 };
	int sent = 0;
	REQUIRE(client->put_partial_data(invalid_prefix, 4, sent) == OK);
	REQUIRE(sent == 4);
	const uint64_t disconnect_deadline = OS::get_singleton()->get_ticks_usec() + 1000000;
	while (client->get_status() == BridgeStreamPeer::STATUS_CONNECTED && OS::get_singleton()->get_ticks_usec() < disconnect_deadline) {
		client->poll();
		OS::get_singleton()->delay_usec(1000);
	}
	CHECK(client->get_status() != BridgeStreamPeer::STATUS_CONNECTED);

	CHECK(BridgeRuntime::probe_authenticated_runtime(project.root));
	CHECK(worker.stop() == BridgeTransportWorker::STOPPED);
}

TEST_CASE("[CodexBridge] Local RPC completes initialize, ping, capabilities, duplicate, and shutdown lifecycle") {
	TemporaryBridgeProject project;
	REQUIRE(project.error == OK);
	MainThreadDispatcher dispatcher;
	dispatcher.start_accepting();
	BridgeTransportWorker worker;
	REQUIRE(worker.start(project.root, &dispatcher) == OK);
	Dictionary discovery;
	REQUIRE(BridgeJson::parse_strict_object(read_file_bytes(project.root.path_join(".godot/codex/bridge.json")), discovery) == OK);
	Ref<BridgeStreamPeer> client = connect_authenticated_test_client(project.root, discovery);
	REQUIRE(client.is_valid());

	Dictionary response;
	REQUIRE(send_test_object(client, make_rpc_request("req:live-init", "bridge.initialize", make_initialize_params(), discovery["project_id"], discovery["editor_session_id"]), 5));
	REQUIRE(dispatch_worker_request(dispatcher, worker));
	REQUIRE(receive_test_object(client, response));
	REQUIRE(response.has("result"));
	const Dictionary initialize_result = response["result"];
	CHECK(initialize_result["protocol_version"] == "1.0");
	const Array initialize_capabilities = initialize_result["capabilities"];
	CHECK(initialize_capabilities.size() == 2);
	bool found_transport = false;
	for (int index = 0; index < initialize_capabilities.size(); index++) {
		const Dictionary capability = initialize_capabilities[index];
#ifdef WINDOWS_ENABLED
		found_transport = found_transport || capability.get("name", "") == "transport.tcp_loopback";
#else
		found_transport = found_transport || capability.get("name", "") == "transport.uds";
#endif
	}
	CHECK(found_transport);
	CHECK((int64_t)Dictionary(initialize_result["limits"])["main_thread_commands_per_frame"] == 8);

	Dictionary ping_params;
	ping_params["echo"] = "live-ping";
	const Dictionary duplicate_ping = make_rpc_request("req:live-duplicate", "bridge.ping", ping_params, discovery["project_id"], discovery["editor_session_id"]);
	REQUIRE(send_test_object(client, duplicate_ping));
	REQUIRE(wait_for_dispatcher_size(dispatcher, 1));
	REQUIRE(send_test_object(client, duplicate_ping));
	REQUIRE(receive_test_object(client, response));
	CHECK(Dictionary(response["error"])["code"] == "duplicate_request_id");
	REQUIRE(dispatcher.process(complete_worker_request, &worker).processed == 1);
	REQUIRE(receive_test_object(client, response));
	CHECK(Dictionary(response["result"])["echo"] == "live-ping");

	REQUIRE(send_test_object(client, make_rpc_request("req:live-caps", "bridge.capabilities", Dictionary(), discovery["project_id"], discovery["editor_session_id"])));
	REQUIRE(dispatch_worker_request(dispatcher, worker));
	REQUIRE(receive_test_object(client, response));
	CHECK(Array(Dictionary(response["result"])["capabilities"]).size() == 2);

	REQUIRE(send_test_object(client, make_rpc_request("req:live-unknown", "bridge.unknown", Dictionary(), discovery["project_id"], discovery["editor_session_id"])));
	REQUIRE(receive_test_object(client, response));
	CHECK(Dictionary(response["error"])["code"] == "method_not_found");
	CHECK(dispatcher.get_queue_size() == 0);

	REQUIRE(send_test_object(client, make_rpc_request("req:live-shutdown", "bridge.shutdown", Dictionary(), discovery["project_id"], discovery["editor_session_id"])));
	REQUIRE(dispatch_worker_request(dispatcher, worker));
	REQUIRE(receive_test_object(client, response));
	CHECK((bool)Dictionary(response["result"])["closing"]);
	CHECK(wait_for_disconnect(client));
	CHECK(FileAccess::exists(project.root.path_join(".godot/codex/bridge.json")));
	CHECK(BridgeRuntime::probe_authenticated_runtime(project.root));

	CHECK(worker.stop() == BridgeTransportWorker::STOPPED);
	dispatcher.begin_shutdown();
}

TEST_CASE("[CodexBridge] Local RPC removes expired, cancelled, saturated, and disconnected work before dispatch") {
	TemporaryBridgeProject project;
	REQUIRE(project.error == OK);
	MainThreadDispatcher dispatcher;
	dispatcher.start_accepting();
	BridgeTransportWorker worker;
	REQUIRE(worker.start(project.root, &dispatcher) == OK);
	Dictionary discovery;
	REQUIRE(BridgeJson::parse_strict_object(read_file_bytes(project.root.path_join(".godot/codex/bridge.json")), discovery) == OK);
	Ref<BridgeStreamPeer> client = connect_authenticated_test_client(project.root, discovery);
	REQUIRE(client.is_valid());
	Dictionary response;
	REQUIRE(send_test_object(client, make_rpc_request("req:race-init", "bridge.initialize", make_initialize_params(), discovery["project_id"], discovery["editor_session_id"])));
	REQUIRE(dispatch_worker_request(dispatcher, worker));
	REQUIRE(receive_test_object(client, response));

	Dictionary ping_params;
	ping_params["echo"] = "expire-before-main";
	REQUIRE(send_test_object(client, make_rpc_request("req:live-deadline", "bridge.ping", ping_params, discovery["project_id"], discovery["editor_session_id"], 1)));
	REQUIRE(wait_for_dispatcher_size(dispatcher, 1));
	REQUIRE(receive_test_object(client, response));
	CHECK(Dictionary(response["error"])["code"] == "deadline_exceeded");
	CHECK(wait_for_dispatcher_size(dispatcher, 0));

	ping_params["echo"] = "cancel-before-main";
	REQUIRE(send_test_object(client, make_rpc_request("req:live-cancel", "bridge.ping", ping_params, discovery["project_id"], discovery["editor_session_id"], 30000)));
	REQUIRE(wait_for_dispatcher_size(dispatcher, 1));
	REQUIRE(send_test_object(client, make_rpc_cancel("req:live-cancel", discovery)));
	REQUIRE(receive_test_object(client, response));
	CHECK(Dictionary(response["error"])["code"] == "cancelled");
	CHECK(wait_for_dispatcher_size(dispatcher, 0));

	ping_params["echo"] = "disconnect-before-main";
	REQUIRE(send_test_object(client, make_rpc_request("req:live-disconnect", "bridge.ping", ping_params, discovery["project_id"], discovery["editor_session_id"], 30000)));
	REQUIRE(wait_for_dispatcher_size(dispatcher, 1));
	client->disconnect_from_host();
	CHECK(wait_for_dispatcher_size(dispatcher, 0));

	client = connect_authenticated_test_client(project.root, discovery);
	REQUIRE(client.is_valid());
	REQUIRE(send_test_object(client, make_rpc_request("req:saturation-init", "bridge.initialize", make_initialize_params(), discovery["project_id"], discovery["editor_session_id"])));
	REQUIRE(dispatch_worker_request(dispatcher, worker));
	REQUIRE(receive_test_object(client, response));
	for (uint64_t index = 0; index < BridgeRpcSession::MAX_IN_FLIGHT_REQUESTS + 1; index++) {
		ping_params["echo"] = itos(index);
		REQUIRE(send_test_object(client, make_rpc_request("req:live-pending-" + itos(index), "bridge.ping", ping_params, discovery["project_id"], discovery["editor_session_id"], 30000)));
	}
	REQUIRE(wait_for_dispatcher_size(dispatcher, BridgeRpcSession::MAX_IN_FLIGHT_REQUESTS));
	REQUIRE(receive_test_object(client, response));
	CHECK(Dictionary(response["error"])["code"] == "overloaded");
	client->disconnect_from_host();
	CHECK(wait_for_dispatcher_size(dispatcher, 0));

	CHECK(worker.stop() == BridgeTransportWorker::STOPPED);
	dispatcher.begin_shutdown();
}

#endif // UNIX_ENABLED || WINDOWS_ENABLED

} // namespace TestCodexBridge

#endif // MODULE_CODEX_BRIDGE_ENABLED
