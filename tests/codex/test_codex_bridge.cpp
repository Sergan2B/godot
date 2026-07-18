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

#include "core/crypto/crypto_core.h"
#include "core/io/dir_access.h"
#include "core/io/file_access.h"
#include "core/io/json.h"
#include "core/io/resource_uid.h"
#include "core/object/ref_counted.h"
#include "core/os/os.h"
#include "core/os/thread.h"
#include "core/templates/local_vector.h"
#include "core/templates/safe_refcount.h"
#include "editor/file_system/editor_file_system.h"

#include "modules/codex_bridge/editor/bounded_variant_projector.h"
#include "modules/codex_bridge/editor/bridge_frame_telemetry.h"
#include "modules/codex_bridge/editor/bridge_revision_clock.h"
#include "modules/codex_bridge/editor/main_thread_dispatcher.h"
#include "modules/codex_bridge/editor/resource_delta_journal.h"
#include "modules/codex_bridge/editor/resource_graph_adapter.h"
#include "modules/codex_bridge/editor/scene_delta_journal.h"
#include "modules/codex_bridge/editor/scene_state_adapter.h"
#include "modules/codex_bridge/editor/script_delta_journal.h"
#include "modules/codex_bridge/protocol/bridge_crypto.h"
#include "modules/codex_bridge/protocol/bridge_frame_codec.h"
#include "modules/codex_bridge/protocol/bridge_handshake.h"
#include "modules/codex_bridge/protocol/bridge_rpc_session.h"
#include "modules/codex_bridge/transport/bridge_runtime.h"
#include "modules/codex_bridge/transport/bridge_transport_worker.h"

struct ResourceGraphAdapterTestAccess {
	static bool can_append_diagnostics(uint64_t p_current_count, uint64_t p_additional_count) {
		return ResourceGraphAdapter::_can_append_diagnostics(p_current_count, p_additional_count);
	}

	static bool valid_resource_path(const String &p_value) {
		return ResourceGraphAdapter::_is_valid_resource_path(p_value);
	}

	static bool valid_resource_uid(const String &p_value) {
		return ResourceGraphAdapter::_is_valid_resource_uid(p_value);
	}

	static bool valid_raw_dependency_spec(const String &p_value) {
		return ResourceGraphAdapter::_is_valid_raw_dependency_spec(p_value);
	}

	static bool paths_match_lexically(const String &p_left, const String &p_right, bool p_case_sensitive) {
		return ResourceGraphAdapter::_paths_match_lexically(p_left, p_right, p_case_sensitive);
	}

	static bool validate_resource(const Dictionary &p_value) {
		return ResourceGraphAdapter::_validate_resource_observation(p_value);
	}

	static bool validate_dependency(const Dictionary &p_value) {
		return ResourceGraphAdapter::_validate_dependency_observation(p_value);
	}

	static bool validate_diagnostic(const Dictionary &p_value) {
		return ResourceGraphAdapter::_validate_diagnostic(p_value);
	}
};

struct ResourceDeltaJournalTestAccess {
	static void append_retained_batch(ResourceDeltaJournal &r_journal, const Dictionary &p_value, uint64_t p_revision) {
		ResourceDeltaJournal::StoredBatch stored;
		stored.value = p_value;
		stored.previous_resource_revision = p_revision - 1;
		stored.resource_revision = p_revision;
		stored.encoded_bytes = 1;
		r_journal.batches.push_back(stored);
		r_journal.total_bytes++;
	}

	static void retire_prepared(ResourceDeltaJournal &r_journal, ResourceDeltaJournal::PreparedBatch &r_prepared) {
		r_journal._retire_prepared_batch(r_prepared);
	}

	static void wait_for_cleanup(ResourceDeltaJournal &r_journal) {
		r_journal._wait_for_cleanup();
	}
};

struct SceneStateAdapterTestAccess {
	static bool safe_node_path(const String &p_value, bool p_allow_empty = false) {
		return SceneStateAdapter::_is_safe_node_path(p_value, p_allow_empty);
	}
};

struct ScriptDeltaJournalTestAccess {
	static void append_retained_batch(ScriptDeltaJournal &r_journal, const Dictionary &p_value, uint64_t p_revision, uint64_t p_encoded_bytes = 1) {
		ScriptDeltaJournal::StoredBatch stored;
		stored.value = p_value;
		stored.previous_script_graph_revision = p_revision - 1;
		stored.script_graph_revision = p_revision;
		stored.encoded_bytes = p_encoded_bytes;
		r_journal.batches.push_back(stored);
		r_journal.total_bytes += p_encoded_bytes;
	}

	static void retire_prepared(ScriptDeltaJournal &r_journal, ScriptDeltaJournal::PreparedBatch &r_prepared) {
		r_journal._retire_prepared_batch(r_prepared);
	}

	static void wait_for_cleanup(ScriptDeltaJournal &r_journal) {
		r_journal._wait_for_cleanup();
	}
};

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

TEST_CASE("[CodexBridge] Editor dependency cache parses every documented typed form") {
	EditorFileSystemDependency dependency = EditorFileSystemDependency::from_cache("res://script.gd::Script");
	CHECK(dependency.uid.is_empty());
	CHECK(dependency.declared_type == "Script");
	CHECK(dependency.fallback_path == "res://script.gd");

	dependency = EditorFileSystemDependency::from_cache("res://script.gd");
	CHECK(dependency.uid.is_empty());
	CHECK(dependency.declared_type.is_empty());
	CHECK(dependency.fallback_path == "res://script.gd");

	ResourceUID *resource_uid = ResourceUID::get_singleton();
	REQUIRE(resource_uid != nullptr);
	const ResourceUID::ID id = resource_uid->create_id();
	const String uid = resource_uid->id_to_text(id);
	resource_uid->add_id(id, "res://current/script.gd");

	dependency = EditorFileSystemDependency::from_cache(uid + "::Script::res://old/script.gd");
	CHECK(dependency.uid == uid);
	CHECK(dependency.declared_type == "Script");
	CHECK(dependency.fallback_path == "res://old/script.gd");
	CHECK(dependency.get_current_path() == "res://current/script.gd");
	CHECK(dependency.get_uid_path() == "res://current/script.gd");

	dependency = EditorFileSystemDependency::from_cache(uid + "::::res://script.gd");
	CHECK(dependency.declared_type.is_empty());
	CHECK(dependency.fallback_path == "res://script.gd");

	dependency = EditorFileSystemDependency::from_cache(uid + "::res://script.gd");
	CHECK(dependency.declared_type.is_empty());
	CHECK(dependency.fallback_path == "res://script.gd");

	dependency = EditorFileSystemDependency::from_cache(uid + "::Script");
	CHECK(dependency.declared_type == "Script");
	CHECK(dependency.fallback_path.is_empty());
	CHECK(dependency.get_current_path() == "res://current/script.gd");

	Error current_error = OK;
	Ref<FileAccess> current_file = FileAccess::create_temp(FileAccess::WRITE_READ, "codex-current-dependency", "tres", false, &current_error);
	Error fallback_error = OK;
	Ref<FileAccess> fallback_file = FileAccess::create_temp(FileAccess::WRITE_READ, "codex-fallback-dependency", "tres", false, &fallback_error);
	REQUIRE(current_error == OK);
	REQUIRE(fallback_error == OK);
	REQUIRE(current_file.is_valid());
	REQUIRE(fallback_file.is_valid());
	resource_uid->remove_id(id);
	resource_uid->add_id(id, current_file->get_path_absolute());
	dependency = EditorFileSystemDependency::from_cache(uid + "::Resource::" + fallback_file->get_path_absolute());
	CHECK(dependency.has_existing_path_mismatch());
	dependency.fallback_path = current_file->get_path_absolute();
	CHECK_FALSE(dependency.has_existing_path_mismatch());

	resource_uid->remove_id(id);
}

TEST_CASE("[CodexBridge] Evidence telemetry is opt-in, bounded, and records budget overruns") {
	BridgeFrameTelemetry telemetry;
	telemetry.reset(false);
	telemetry.record(100, true);
	CHECK((int64_t)telemetry.to_dictionary()["busy_frame_count"] == 0);

	telemetry.reset(true);
	telemetry.record(42, false);
	telemetry.record(BridgeFrameTelemetry::BUDGET_USEC, true);
	telemetry.record(BridgeFrameTelemetry::BUDGET_USEC + 1, true);
	Dictionary result = telemetry.to_dictionary();
	CHECK((int64_t)result["busy_frame_count"] == 2);
	CHECK((int64_t)result["over_budget_count"] == 1);
	CHECK((int64_t)result["max_elapsed_usec"] == (int64_t)BridgeFrameTelemetry::BUDGET_USEC + 1);
	CHECK_FALSE((bool)result["overflow"]);
	CHECK(Array(result["samples_usec"]).size() == 2);

	for (uint64_t index = 2; index < BridgeFrameTelemetry::MAX_SAMPLES; index++) {
		telemetry.record(1, true);
	}
	telemetry.record(1, true);
	result = telemetry.to_dictionary();
	CHECK((int64_t)result["busy_frame_count"] == (int64_t)BridgeFrameTelemetry::MAX_SAMPLES + 1);
	CHECK(Array(result["samples_usec"]).size() == (int)BridgeFrameTelemetry::MAX_SAMPLES);
	CHECK((bool)result["overflow"]);
}

TEST_CASE("[CodexBridge] Bounded variant projection preserves type and fails closed at limits") {
	Dictionary integer = BoundedVariantProjector::project_typed(42);
	CHECK(integer["type"] == "int");
	CHECK((int64_t)integer["value"] == 42);
	CHECK_FALSE((bool)integer["truncated"]);

	Dictionary node_path = BoundedVariantProjector::project_typed(NodePath("Root/Child:position"));
	CHECK(node_path["type"] == "node_path");
	CHECK(node_path["value"] == "Root/Child:position");
	CHECK_FALSE((bool)node_path["truncated"]);

	Dictionary vector = BoundedVariantProjector::project_typed(Vector3(1.0, 2.0, 3.0));
	CHECK(vector["type"] == "vector3");
	Dictionary vector_value = vector["value"];
	CHECK((double)vector_value["x"] == doctest::Approx(1.0));
	CHECK((double)vector_value["y"] == doctest::Approx(2.0));
	CHECK((double)vector_value["z"] == doctest::Approx(3.0));

	Dictionary long_string = BoundedVariantProjector::project_typed(String("x").repeat(BoundedVariantProjector::MAX_STRING_CHARACTERS + 1));
	CHECK(long_string["type"] == "string");
	CHECK((bool)long_string["truncated"]);
	CHECK(String(long_string["value"]).length() == BoundedVariantProjector::MAX_STRING_CHARACTERS);

	Array nested;
	for (int depth = 0; depth <= BoundedVariantProjector::MAX_DEPTH + 1; depth++) {
		Array parent;
		parent.push_back(nested);
		nested = parent;
	}
	Dictionary deep_array = BoundedVariantProjector::project_typed(nested);
	CHECK(deep_array["type"] == "array");
	CHECK((bool)deep_array["truncated"]);

	Ref<Resource> resource;
	resource.instantiate();
	resource->set_path("user://private-resource.tres");
	Dictionary projected_resource = BoundedVariantProjector::project_typed(resource);
	CHECK(projected_resource["type"] == "resource");
	CHECK_FALSE(Dictionary(projected_resource["value"]).has("path"));
}

TEST_CASE("[CodexBridge] Scene paths and delta journal fail closed") {
	CHECK(SceneStateAdapterTestAccess::safe_node_path("."));
	CHECK(SceneStateAdapterTestAccess::safe_node_path("Root/Child"));
	CHECK(SceneStateAdapterTestAccess::safe_node_path(String(), true));
	CHECK_FALSE(SceneStateAdapterTestAccess::safe_node_path(String()));
	CHECK_FALSE(SceneStateAdapterTestAccess::safe_node_path("/root/Child"));
	CHECK_FALSE(SceneStateAdapterTestAccess::safe_node_path("Root\\Child"));
	CHECK_FALSE(SceneStateAdapterTestAccess::safe_node_path("Root/../Child"));
	CHECK_FALSE(SceneStateAdapterTestAccess::safe_node_path("Root:property"));

	SceneDeltaJournal journal;
	journal.initialize(1);
	Array operations;
	Dictionary operation;
	operation["kind"] = "project_context";
	operation["values"] = Array();
	operations.push_back(operation);
	Dictionary batch;
	bool invalidated = false;
	REQUIRE(journal.commit(2, 4, 7, operations, batch, invalidated) == OK);
	CHECK_FALSE(invalidated);
	CHECK((int64_t)batch["previous_scene_graph_revision"] == 1);
	CHECK((int64_t)batch["scene_graph_revision"] == 2);
	CHECK((int64_t)batch["resource_revision"] == 4);
	CHECK((int64_t)batch["project_revision"] == 7);
	CHECK(String(batch["batch_id"]).begins_with("scene-batch:"));
	CHECK(String(batch["checksum"]).length() == 64);
	SceneDeltaJournal prepared_journal;
	prepared_journal.initialize(1);
	SceneDeltaJournal::PreparedBatch prepared;
	REQUIRE(SceneDeltaJournal::prepare_batch(2, operations, prepared) == OK);
	Dictionary prepared_batch;
	bool prepared_invalidated = false;
	REQUIRE(prepared_journal.commit_prepared(2, 4, 7, prepared, prepared_batch, prepared_invalidated) == OK);
	CHECK_FALSE(prepared_invalidated);
	CHECK(prepared_batch["batch_id"] == batch["batch_id"]);
	CHECK(prepared_batch["checksum"] == batch["checksum"]);
	CHECK(prepared_batch["operations"] == batch["operations"]);
	CHECK(journal.query_after(2).status == SceneDeltaJournal::QUERY_CURRENT);
	CHECK(journal.query_after(1).status == SceneDeltaJournal::QUERY_BATCH);
	CHECK(journal.query_after(0).status == SceneDeltaJournal::QUERY_GAP);
	CHECK(journal.query_after(3).status == SceneDeltaJournal::QUERY_FUTURE);

	journal.invalidate_to(3);
	CHECK_FALSE(journal.is_invalidating());
	CHECK(journal.query_after(2).status == SceneDeltaJournal::QUERY_GAP);
	CHECK(journal.query_after(3).status == SceneDeltaJournal::QUERY_CURRENT);

	Array oversized_operations;
	Dictionary oversized_operation;
	oversized_operation["kind"] = "project_context";
	Array oversized_values;
	oversized_values.push_back(String("x").repeat(SceneDeltaJournal::MAX_BATCH_BYTES));
	oversized_operation["values"] = oversized_values;
	oversized_operations.push_back(oversized_operation);
	CHECK(journal.commit(4, 4, 4, oversized_operations, batch, invalidated) == ERR_OUT_OF_MEMORY);
	CHECK(invalidated);
	CHECK(journal.get_current_revision() == 4);
	CHECK(journal.query_after(3).status == SceneDeltaJournal::QUERY_GAP);
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
		String temporary_root = OS::get_singleton()->get_temp_path();
#ifdef MACOS_ENABLED
		// NSTemporaryDirectory is too long for a project-local sockaddr_un path.
		temporary_root = "/tmp";
#endif
		root = temporary_root.path_join("gcb_" + itos(OS::get_singleton()->get_process_id()) + "_" + itos(OS::get_singleton()->get_ticks_usec()));
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
	if (p_peer.is_null()) {
		return false;
	}
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
	if (p_peer.is_null()) {
		return false;
	}
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

TEST_CASE("[CodexS5BridgeProfile] Handshake negotiates Bridge RPC 1.4 and caps future major-one minors") {
	const String project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	const String editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	const PackedByteArray token = bytes_from_range(0xa0, 32);
	String nonce_encoded;
	REQUIRE(BridgeCrypto::base64url_encode_32(bytes_from_range(0x40, 32), nonce_encoded) == OK);

	BridgeHandshakeSession handshake(token, project_id, editor_session_id, 0);
	BridgeHandshakeSession::Outcome challenge;
	REQUIRE(handshake.handle_message(make_client_hello(project_id, editor_session_id, nonce_encoded, "1.9"), 1, challenge) == OK);
	CHECK(challenge.response["selected_protocol_version"] == "1.4");
	CHECK(handshake.get_selected_protocol_version() == "1.4");

	BridgeHandshakeSession exact(token, project_id, editor_session_id, 0);
	REQUIRE(exact.handle_message(make_client_hello(project_id, editor_session_id, nonce_encoded, "1.4"), 1, challenge) == OK);
	CHECK(challenge.response["selected_protocol_version"] == "1.4");
	CHECK(exact.get_selected_protocol_version() == "1.4");
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

TEST_CASE("[CodexBridge] RPC 1.2 exposes strict resource graph snapshot and delta methods") {
	const String project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	const String editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	BridgeRpcSession rpc(project_id, editor_session_id);
	rpc.set_protocol_version("1.2");
	BridgeRpcSession::Outcome outcome;

	REQUIRE(rpc.handle_message(make_rpc_request("req:init-resource", "bridge.initialize", make_initialize_params(), project_id, editor_session_id, 5000, "1.2"), 0, 1, outcome) == OK);
	REQUIRE(rpc.complete(1, 1, outcome) == OK);
	const Dictionary initialize_result = outcome.response["result"];
	CHECK(initialize_result["protocol_version"] == "1.2");
	CHECK(Array(initialize_result["capabilities"]).size() == 8);
	CHECK((int64_t)Dictionary(initialize_result["limits"])["resource_records"] == 250000);
	CHECK((int64_t)Dictionary(initialize_result["revisions"])["resource_revision"] == 1);

	REQUIRE(rpc.handle_message(make_rpc_request("req:resource-snapshot", "resource.snapshot.get", Dictionary(), project_id, editor_session_id, 5000, "1.2"), 2, 2, outcome) == OK);
	CHECK(outcome.method == BridgeRpcSession::METHOD_RESOURCE_SNAPSHOT);
	CHECK(outcome.deadline_usec == 120000002);
	Dictionary snapshot_result;
	snapshot_result["snapshot_id"] = "snapshot:0123456789abcdef0123456789abcdef";
	snapshot_result["domain"] = "resource_graph";
	REQUIRE(rpc.complete(2, 3, snapshot_result, outcome) == OK);
	CHECK(Dictionary(outcome.response["result"])["domain"] == "resource_graph");

	Dictionary delta_params;
	delta_params["after_resource_revision"] = (int64_t)1;
	REQUIRE(rpc.handle_message(make_rpc_request("req:resource-delta", "resource.delta.get", delta_params, project_id, editor_session_id, 5000, "1.2"), 4, 3, outcome) == OK);
	CHECK(outcome.method == BridgeRpcSession::METHOD_RESOURCE_DELTA);
	Dictionary delta_result;
	delta_result["status"] = "current";
	delta_result["current_resource_revision"] = (int64_t)1;
	REQUIRE(rpc.complete(3, 5, delta_result, outcome) == OK);
	CHECK(Dictionary(outcome.response["result"])["status"] == "current");

	Dictionary invalid_delta = delta_params;
	invalid_delta["unknown"] = true;
	REQUIRE(rpc.handle_message(make_rpc_request("req:invalid-resource-delta", "resource.delta.get", invalid_delta, project_id, editor_session_id, 5000, "1.2"), 6, 4, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "invalid_request");

	Dictionary ack;
	ack["protocol_version"] = "1.2";
	ack["kind"] = "ack";
	ack["ack_id"] = "ack:resource-snapshot";
	Dictionary ack_params;
	ack_params["snapshot_id"] = snapshot_result["snapshot_id"];
	ack_params["domain"] = "resource_graph";
	ack_params["through_chunk"] = 0;
	ack["params"] = ack_params;
	ack["context"] = make_rpc_context(project_id, editor_session_id);
	CHECK(rpc.handle_message(ack, 7, 5, outcome) == OK);
}

TEST_CASE("[CodexBridge] Resource methods fail explicitly after a 1.1 downgrade") {
	const String project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	const String editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	BridgeRpcSession rpc(project_id, editor_session_id);
	rpc.set_protocol_version("1.1");
	BridgeRpcSession::Outcome outcome;
	REQUIRE(rpc.handle_message(make_rpc_request("req:init-downgrade", "bridge.initialize", make_initialize_params(), project_id, editor_session_id, 5000, "1.1"), 0, 1, outcome) == OK);
	REQUIRE(rpc.complete(1, 1, outcome) == OK);
	REQUIRE(rpc.handle_message(make_rpc_request("req:no-resource", "resource.snapshot.get", Dictionary(), project_id, editor_session_id, 5000, "1.1"), 2, 2, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "capability_unavailable");
}

TEST_CASE("[CodexBridge] RPC 1.3 exposes strict scene graph snapshot and delta methods") {
	const String project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	const String editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	BridgeRpcSession rpc(project_id, editor_session_id);
	rpc.set_protocol_version("1.3");
	BridgeRpcSession::Outcome outcome;

	REQUIRE(rpc.handle_message(make_rpc_request("req:init-scene", "bridge.initialize", make_initialize_params(), project_id, editor_session_id, 5000, "1.3"), 0, 1, outcome) == OK);
	REQUIRE(rpc.complete(1, 1, outcome) == OK);
	const Dictionary initialize_result = outcome.response["result"];
	CHECK(initialize_result["protocol_version"] == "1.3");
	CHECK(Array(initialize_result["capabilities"]).size() == 11);
	CHECK((int64_t)Dictionary(initialize_result["limits"])["scene_nodes"] == 1000000);
	CHECK((int64_t)Dictionary(initialize_result["revisions"])["scene_graph_revision"] == 1);

	REQUIRE(rpc.handle_message(make_rpc_request("req:scene-snapshot", "scene.snapshot.get", Dictionary(), project_id, editor_session_id, 5000, "1.3"), 2, 2, outcome) == OK);
	CHECK(outcome.method == BridgeRpcSession::METHOD_SCENE_SNAPSHOT);
	CHECK(outcome.deadline_usec == 120000002);
	Dictionary snapshot_result;
	snapshot_result["snapshot_id"] = "snapshot:2123456789abcdef0123456789abcdef";
	snapshot_result["domain"] = "scene_graph";
	REQUIRE(rpc.complete(2, 3, snapshot_result, outcome) == OK);
	CHECK(Dictionary(outcome.response["result"])["domain"] == "scene_graph");

	Dictionary delta_params;
	delta_params["after_scene_graph_revision"] = (int64_t)1;
	REQUIRE(rpc.handle_message(make_rpc_request("req:scene-delta", "scene.delta.get", delta_params, project_id, editor_session_id, 5000, "1.3"), 4, 3, outcome) == OK);
	CHECK(outcome.method == BridgeRpcSession::METHOD_SCENE_DELTA);
	Dictionary delta_result;
	delta_result["status"] = "current";
	delta_result["current_scene_graph_revision"] = (int64_t)1;
	REQUIRE(rpc.complete(3, 5, delta_result, outcome) == OK);
	CHECK(Dictionary(outcome.response["result"])["status"] == "current");

	Dictionary invalid_delta = delta_params;
	invalid_delta["unknown"] = true;
	REQUIRE(rpc.handle_message(make_rpc_request("req:invalid-scene-delta", "scene.delta.get", invalid_delta, project_id, editor_session_id, 5000, "1.3"), 6, 4, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "invalid_request");

	Dictionary ack;
	ack["protocol_version"] = "1.3";
	ack["kind"] = "ack";
	ack["ack_id"] = "ack:scene-snapshot";
	Dictionary ack_params;
	ack_params["snapshot_id"] = snapshot_result["snapshot_id"];
	ack_params["domain"] = "scene_graph";
	ack_params["through_chunk"] = 0;
	ack["params"] = ack_params;
	ack["context"] = make_rpc_context(project_id, editor_session_id);
	CHECK(rpc.handle_message(ack, 7, 5, outcome) == OK);
}

TEST_CASE("[CodexBridge] Scene methods fail explicitly after a 1.2 downgrade") {
	const String project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	const String editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	BridgeRpcSession rpc(project_id, editor_session_id);
	rpc.set_protocol_version("1.2");
	BridgeRpcSession::Outcome outcome;
	REQUIRE(rpc.handle_message(make_rpc_request("req:init-scene-downgrade", "bridge.initialize", make_initialize_params(), project_id, editor_session_id, 5000, "1.2"), 0, 1, outcome) == OK);
	REQUIRE(rpc.complete(1, 1, outcome) == OK);
	REQUIRE(rpc.handle_message(make_rpc_request("req:no-scene", "scene.snapshot.get", Dictionary(), project_id, editor_session_id, 5000, "1.2"), 2, 2, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "capability_unavailable");
}

TEST_CASE("[CodexS5BridgeProfile] RPC 1.4 negotiates a strict unavailable script profile") {
	const String project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	const String editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	BridgeRpcSession rpc(project_id, editor_session_id);
	rpc.set_protocol_version("1.4");
	BridgeRpcSession::Outcome outcome;

	REQUIRE(rpc.handle_message(make_rpc_request("req:init-script", "bridge.initialize", make_initialize_params(), project_id, editor_session_id, 5000, "1.4"), 0, 1, outcome) == OK);
	REQUIRE(rpc.complete(1, 1, outcome) == OK);
	const Dictionary initialize_result = outcome.response["result"];
	CHECK(initialize_result["protocol_version"] == "1.4");
	const Array capabilities = initialize_result["capabilities"];
	CHECK(capabilities.size() == 15);
	int script_capabilities = 0;
	for (int index = 0; index < capabilities.size(); index++) {
		const Dictionary capability = capabilities[index];
		if (String(capability["name"]).begins_with("script.")) {
			script_capabilities++;
			CHECK(capability["readiness"] == "unavailable");
		}
	}
	CHECK(script_capabilities == 4);
	const Dictionary limits = initialize_result["limits"];
	CHECK((int64_t)limits["script_documents"] == 250000);
	CHECK((int64_t)limits["script_symbols"] == 2000000);
	CHECK((int64_t)limits["script_snapshot_chunk_bytes"] == 524288);
	const Dictionary revisions = initialize_result["revisions"];
	CHECK((int64_t)revisions["resource_revision"] == 1);
	CHECK((int64_t)revisions["scene_graph_revision"] == 1);
	CHECK((int64_t)revisions["script_graph_revision"] == 1);

	REQUIRE(rpc.handle_message(make_rpc_request("req:resource-on-1.4", "resource.snapshot.get", Dictionary(), project_id, editor_session_id, 5000, "1.4"), 2, 2, outcome) == OK);
	CHECK(outcome.method == BridgeRpcSession::METHOD_RESOURCE_SNAPSHOT);
	Dictionary resource_result;
	resource_result["snapshot_id"] = "snapshot:6123456789abcdef0123456789abcdef";
	resource_result["domain"] = "resource_graph";
	REQUIRE(rpc.complete(2, 3, resource_result, outcome) == OK);

	REQUIRE(rpc.handle_message(make_rpc_request("req:scene-on-1.4", "scene.snapshot.get", Dictionary(), project_id, editor_session_id, 5000, "1.4"), 4, 3, outcome) == OK);
	CHECK(outcome.method == BridgeRpcSession::METHOD_SCENE_SNAPSHOT);
	Dictionary scene_result;
	scene_result["snapshot_id"] = "snapshot:7123456789abcdef0123456789abcdef";
	scene_result["domain"] = "scene_graph";
	REQUIRE(rpc.complete(3, 5, scene_result, outcome) == OK);

	REQUIRE(rpc.handle_message(make_rpc_request("req:script-snapshot", "script.snapshot.get", Dictionary(), project_id, editor_session_id, 5000, "1.4"), 6, 4, outcome) == OK);
	CHECK(outcome.method == BridgeRpcSession::METHOD_SCRIPT_SNAPSHOT);
	CHECK(outcome.deadline_usec == 120000006);
	Dictionary snapshot_result;
	snapshot_result["snapshot_id"] = "snapshot:8123456789abcdef0123456789abcdef";
	snapshot_result["domain"] = "script_graph";
	REQUIRE(rpc.complete(4, 7, snapshot_result, outcome) == OK);
	CHECK(Dictionary(outcome.response["result"])["domain"] == "script_graph");

	Dictionary delta_params;
	delta_params["after_script_graph_revision"] = (int64_t)1;
	REQUIRE(rpc.handle_message(make_rpc_request("req:script-delta", "script.delta.get", delta_params, project_id, editor_session_id, 5000, "1.4"), 8, 5, outcome) == OK);
	CHECK(outcome.method == BridgeRpcSession::METHOD_SCRIPT_DELTA);
	Dictionary delta_result;
	delta_result["status"] = "current";
	delta_result["current_script_graph_revision"] = (int64_t)1;
	REQUIRE(rpc.complete(5, 9, delta_result, outcome) == OK);
	CHECK(Dictionary(outcome.response["result"])["status"] == "current");

	Dictionary invalid_delta = delta_params;
	invalid_delta["unknown"] = true;
	REQUIRE(rpc.handle_message(make_rpc_request("req:invalid-script-delta", "script.delta.get", invalid_delta, project_id, editor_session_id, 5000, "1.4"), 10, 6, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "invalid_request");

	Dictionary ack;
	ack["protocol_version"] = "1.4";
	ack["kind"] = "ack";
	ack["ack_id"] = "ack:script-snapshot";
	Dictionary ack_params;
	ack_params["snapshot_id"] = snapshot_result["snapshot_id"];
	ack_params["domain"] = "script_graph";
	ack_params["through_chunk"] = 0;
	ack["params"] = ack_params;
	ack["context"] = make_rpc_context(project_id, editor_session_id);
	CHECK(rpc.handle_message(ack, 11, 7, outcome) == OK);

	Dictionary metrics;
	metrics["schema_version"] = 1;
	metrics["protocol_version"] = "1.4";
	metrics["script_capabilities"] = script_capabilities;
	metrics["script_methods"] = 2;
	metrics["resource_scene_retained"] = true;
	metrics["script_readiness"] = "unavailable";
	print_line("[codex_s5_bridge] " + JSON::stringify(metrics));
}

TEST_CASE("[CodexS5BridgeProfile] RPC 1.3 downgrade omits every script field") {
	const String project_id = "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd";
	const String editor_session_id = "editor:0123456789abcdef0123456789abcdef";
	BridgeRpcSession rpc(project_id, editor_session_id);
	rpc.set_protocol_version("1.3");
	BridgeRpcSession::Outcome outcome;
	REQUIRE(rpc.handle_message(make_rpc_request("req:init-script-downgrade", "bridge.initialize", make_initialize_params(), project_id, editor_session_id, 5000, "1.3"), 0, 1, outcome) == OK);
	Dictionary revisions_override;
	revisions_override["editor_session_id"] = editor_session_id;
	revisions_override["event_seq"] = (int64_t)0;
	revisions_override["project_revision"] = (int64_t)0;
	revisions_override["operation_seq"] = (int64_t)0;
	revisions_override["resource_revision"] = (int64_t)1;
	revisions_override["scene_graph_revision"] = (int64_t)1;
	revisions_override["script_graph_revision"] = (int64_t)99;
	revisions_override["scene_revisions"] = Dictionary();
	Dictionary initialize_override;
	initialize_override["revisions"] = revisions_override;
	REQUIRE(rpc.complete(1, 1, initialize_override, outcome) == OK);
	const Dictionary initialize_result = outcome.response["result"];
	const Dictionary revisions = initialize_result["revisions"];
	CHECK_FALSE(revisions.has("script_graph_revision"));
	CHECK_FALSE(Dictionary(initialize_result["limits"]).has("script_documents"));
	const Array capabilities = initialize_result["capabilities"];
	CHECK(capabilities.size() == 11);
	for (int index = 0; index < capabilities.size(); index++) {
		CHECK_FALSE(String(Dictionary(capabilities[index])["name"]).begins_with("script."));
	}

	REQUIRE(rpc.handle_message(make_rpc_request("req:no-script", "script.snapshot.get", Dictionary(), project_id, editor_session_id, 5000, "1.3"), 2, 2, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "capability_unavailable");
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
	CHECK((int64_t)vector["resource_revision"] == 1);
	CHECK((int64_t)vector["scene_graph_revision"] == 1);
	CHECK((int64_t)Dictionary(vector["scene_revisions"])[scene_id] == 2);
	CHECK(revisions.record_resource_change() == 2);
	CHECK(revisions.record_scene_graph_change() == 2);
	CHECK(revisions.get_scene_graph_revision() == 2);
	CHECK(revisions.record_script_graph_change() == 2);
	CHECK(revisions.get_script_graph_revision() == 2);
	CHECK((int64_t)revisions.get_revision_vector()["script_graph_revision"] == 2);
}

static Dictionary make_resource_ref(const String &p_uid, const String &p_path = String()) {
	Dictionary resource_ref;
	if (!p_uid.is_empty()) {
		resource_ref["uid"] = p_uid;
	} else {
		resource_ref["uid_missing"] = true;
		resource_ref["path"] = p_path;
	}
	return resource_ref;
}

static Dictionary make_resource_value(const Dictionary &p_resource_ref, const String &p_path, int64_t p_marker) {
	Dictionary resource;
	resource["resource_ref"] = p_resource_ref;
	resource["path"] = p_path;
	resource["marker"] = p_marker;
	Dictionary value;
	value["resource"] = resource;
	value["dependencies"] = Array();
	return value;
}

static Dictionary make_upsert(const Dictionary &p_resource_ref, const String &p_path, int64_t p_marker) {
	Dictionary operation;
	operation["kind"] = "upsert";
	operation["value"] = make_resource_value(p_resource_ref, p_path, p_marker);
	return operation;
}

class ResourceDtoCleanupProbe : public RefCounted {
	SafeNumeric<uint32_t> *destroyed = nullptr;
	SafeFlag *destroyed_off_main = nullptr;

public:
	ResourceDtoCleanupProbe(SafeNumeric<uint32_t> *p_destroyed, SafeFlag *p_destroyed_off_main) :
			destroyed(p_destroyed),
			destroyed_off_main(p_destroyed_off_main) {
	}

	~ResourceDtoCleanupProbe() {
		if (!Thread::is_main_thread()) {
			destroyed_off_main->set();
		}
		destroyed->increment();
	}
};

TEST_CASE("[CodexBridge] Resource graph diagnostic limit rejects overflow before allocation") {
	CHECK(ResourceGraphAdapterTestAccess::can_append_diagnostics(0, ResourceGraphAdapter::MAX_DIAGNOSTICS));
	CHECK(ResourceGraphAdapterTestAccess::can_append_diagnostics(ResourceGraphAdapter::MAX_DIAGNOSTICS - 1, 1));
	CHECK_FALSE(ResourceGraphAdapterTestAccess::can_append_diagnostics(ResourceGraphAdapter::MAX_DIAGNOSTICS, 1));
	CHECK_FALSE(ResourceGraphAdapterTestAccess::can_append_diagnostics(ResourceGraphAdapter::MAX_DIAGNOSTICS - 1, 2));
	CHECK_FALSE(ResourceGraphAdapterTestAccess::can_append_diagnostics(UINT64_MAX, 1));
	CHECK_FALSE(ResourceGraphAdapterTestAccess::can_append_diagnostics(0, UINT64_MAX));
}

TEST_CASE("[CodexBridge] Resource graph DTO validation matches the Rust path and identity contract") {
	CHECK(ResourceGraphAdapterTestAccess::valid_resource_path("res://folder/item.tres"));
	CHECK(ResourceGraphAdapterTestAccess::valid_resource_path("res://" + String("a").repeat(ResourceGraphAdapter::MAX_PATH_BYTES - 6)));
	CHECK_FALSE(ResourceGraphAdapterTestAccess::valid_resource_path("res://" + String("a").repeat(ResourceGraphAdapter::MAX_PATH_BYTES - 5)));
	CHECK(ResourceGraphAdapterTestAccess::valid_resource_path("res://" + String::chr(0x00e9).repeat((ResourceGraphAdapter::MAX_PATH_BYTES - 6) / 2)));
	CHECK_FALSE(ResourceGraphAdapterTestAccess::valid_resource_path("res://" + String::chr(0x00e9).repeat((ResourceGraphAdapter::MAX_PATH_BYTES - 6) / 2 + 1)));
	for (const char *invalid : { "res://", "res:///item.tres", "res://folder//item.tres", "res://folder/./item.tres", "res://folder/../item.tres", "res://folder\\item.tres", "res://item.tres?query", "res://item.tres#fragment", "user://item.tres" }) {
		CHECK_FALSE(ResourceGraphAdapterTestAccess::valid_resource_path(invalid));
	}
	CHECK_FALSE(ResourceGraphAdapterTestAccess::valid_resource_path("res://item" + String::chr(0x1f) + ".tres"));

	CHECK(ResourceGraphAdapterTestAccess::valid_resource_uid("uid://abc_DEF-123"));
	CHECK(ResourceGraphAdapterTestAccess::valid_resource_uid("uid://" + String("a").repeat(ResourceGraphAdapter::MAX_UID_BYTES - 6)));
	CHECK_FALSE(ResourceGraphAdapterTestAccess::valid_resource_uid("uid://" + String("a").repeat(ResourceGraphAdapter::MAX_UID_BYTES - 5)));
	CHECK_FALSE(ResourceGraphAdapterTestAccess::valid_resource_uid("uid://"));
	CHECK_FALSE(ResourceGraphAdapterTestAccess::valid_resource_uid("uid://bad/value"));
	CHECK_FALSE(ResourceGraphAdapterTestAccess::valid_resource_uid("uid://caf" + String::chr(0x00e9)));

	CHECK(ResourceGraphAdapterTestAccess::valid_raw_dependency_spec(String("x").repeat(ResourceGraphAdapter::MAX_RAW_DEPENDENCY_BYTES)));
	CHECK_FALSE(ResourceGraphAdapterTestAccess::valid_raw_dependency_spec(String("x").repeat(ResourceGraphAdapter::MAX_RAW_DEPENDENCY_BYTES + 1)));
	CHECK(ResourceGraphAdapterTestAccess::paths_match_lexically("res://Folder/Item.tres", "res://folder/item.tres", false));
	CHECK_FALSE(ResourceGraphAdapterTestAccess::paths_match_lexically("res://Folder/Item.tres", "res://folder/item.tres", true));
}

TEST_CASE("[CodexBridge] Resource graph observations reject unsafe fields before publication") {
	const Dictionary resource_ref = make_resource_ref(String(), "res://folder/item.tres");
	Dictionary resource;
	resource["resource_ref"] = resource_ref;
	resource["path"] = "res://folder/item.tres";
	resource["godot_type"] = "Resource";
	resource["source_kind"] = "source";
	resource["import_state"] = "not_imported";
	resource["modified_time_unix_seconds"] = (int64_t)ResourceGraphAdapter::MAX_SAFE_INTEGER;
	resource["byte_size"] = (int64_t)ResourceGraphAdapter::MAX_SAFE_INTEGER;
	resource["validity"] = "valid";
	resource["authority"] = "editor_file_system";
	resource["resource_revision"] = 1;
	CHECK(ResourceGraphAdapterTestAccess::validate_resource(resource));

	Dictionary invalid_resource = resource.duplicate(true);
	invalid_resource["unknown"] = true;
	CHECK_FALSE(ResourceGraphAdapterTestAccess::validate_resource(invalid_resource));
	invalid_resource = resource.duplicate(true);
	invalid_resource["godot_type"] = "";
	CHECK_FALSE(ResourceGraphAdapterTestAccess::validate_resource(invalid_resource));
	invalid_resource = resource.duplicate(true);
	invalid_resource["godot_type"] = String("T").repeat(ResourceGraphAdapter::MAX_TYPE_BYTES + 1);
	CHECK_FALSE(ResourceGraphAdapterTestAccess::validate_resource(invalid_resource));
	invalid_resource = resource.duplicate(true);
	invalid_resource["modified_time_unix_seconds"] = -1;
	CHECK_FALSE(ResourceGraphAdapterTestAccess::validate_resource(invalid_resource));
	invalid_resource = resource.duplicate(true);
	invalid_resource["byte_size"] = (int64_t)ResourceGraphAdapter::MAX_SAFE_INTEGER + 1;
	CHECK_FALSE(ResourceGraphAdapterTestAccess::validate_resource(invalid_resource));

	Dictionary dependency;
	dependency["source_ref"] = resource_ref;
	dependency["target_uid"] = "uid://target-1";
	dependency["fallback_path"] = "res://folder/target.tres";
	dependency["resolved_path"] = Variant();
	dependency["declared_type"] = String("T").repeat(ResourceGraphAdapter::MAX_TYPE_BYTES);
	dependency["resolution"] = "stale_uid";
	dependency["authority"] = "resource_loader_dependencies";
	dependency["resource_revision"] = 1;
	CHECK(ResourceGraphAdapterTestAccess::validate_dependency(dependency));
	Dictionary invalid_dependency = dependency.duplicate(true);
	invalid_dependency["target_uid"] = "uid://bad/value";
	CHECK_FALSE(ResourceGraphAdapterTestAccess::validate_dependency(invalid_dependency));
	invalid_dependency = dependency.duplicate(true);
	invalid_dependency["fallback_path"] = "res://folder/../target.tres";
	CHECK_FALSE(ResourceGraphAdapterTestAccess::validate_dependency(invalid_dependency));
	invalid_dependency = dependency.duplicate(true);
	invalid_dependency["declared_type"] = String("T").repeat(ResourceGraphAdapter::MAX_TYPE_BYTES + 1);
	CHECK_FALSE(ResourceGraphAdapterTestAccess::validate_dependency(invalid_dependency));

	Dictionary diagnostic;
	diagnostic["code"] = "stale_resource_uid";
	diagnostic["subject"] = resource_ref;
	diagnostic["target_reference"] = String::chr(0x00e9).repeat(ResourceGraphAdapter::MAX_PATH_BYTES / 2);
	diagnostic["resource_revision"] = 1;
	CHECK(ResourceGraphAdapterTestAccess::validate_diagnostic(diagnostic));
	diagnostic["target_reference"] = String::chr(0x00e9).repeat(ResourceGraphAdapter::MAX_PATH_BYTES / 2 + 1);
	CHECK_FALSE(ResourceGraphAdapterTestAccess::validate_diagnostic(diagnostic));
}

TEST_CASE("[CodexBridge] Resource delta journal coalesces and distinguishes current gap and future") {
	ResourceDeltaJournal journal;
	journal.initialize(1);
	const Dictionary stable_ref = make_resource_ref("uid://a");
	const Dictionary temporary_ref = make_resource_ref(String(), "res://temporary.tres");
	Array operations;
	operations.push_back(make_upsert(stable_ref, "res://old.tres", 1));
	operations.push_back(make_upsert(stable_ref, "res://old.tres", 2));
	operations.push_back(make_upsert(temporary_ref, "res://temporary.tres", 1));
	Dictionary remove_temporary;
	remove_temporary["kind"] = "remove";
	remove_temporary["resource_ref"] = temporary_ref;
	remove_temporary["path"] = "res://temporary.tres";
	operations.push_back(remove_temporary);

	Dictionary batch;
	bool invalidated = false;
	ResourceDeltaJournal::PreparedBatch prepared;
	REQUIRE(ResourceDeltaJournal::prepare_batch(2, operations, HashSet<String>(), prepared) == OK);
	CHECK(prepared.encoded_bytes > 0);
	REQUIRE(journal.commit_prepared(2, 2, prepared, batch, invalidated) == OK);
	CHECK_FALSE(invalidated);
	CHECK((int64_t)batch["project_revision"] == 2);
	const Array coalesced = batch["operations"];
	REQUIRE(coalesced.size() == 1);
	const Dictionary latest_resource = Dictionary(Dictionary(coalesced[0])["value"])["resource"];
	CHECK((int64_t)latest_resource["marker"] == 2);
	CHECK(journal.query_after(2).status == ResourceDeltaJournal::QUERY_CURRENT);
	CHECK(journal.query_after(1).status == ResourceDeltaJournal::QUERY_BATCH);
	CHECK(journal.query_after(0).status == ResourceDeltaJournal::QUERY_GAP);
	CHECK(journal.query_after(3).status == ResourceDeltaJournal::QUERY_FUTURE);
}

TEST_CASE("[CodexBridge] Resource delta journal collapses UID move chains and evicts at its entry bound") {
	ResourceDeltaJournal journal;
	journal.initialize(1);
	const Dictionary resource_ref = make_resource_ref("uid://b");
	HashSet<String> preexisting;
	preexisting.insert(ResourceDeltaJournal::resource_ref_key(resource_ref));
	Array moves;
	Dictionary first;
	first["kind"] = "move";
	first["uid"] = "uid://b";
	first["from_path"] = "res://a.tres";
	first["to_path"] = "res://b.tres";
	first["value"] = make_resource_value(resource_ref, "res://b.tres", 1);
	moves.push_back(first);
	Dictionary second = first.duplicate(true);
	second["from_path"] = "res://b.tres";
	second["to_path"] = "res://c.tres";
	second["value"] = make_resource_value(resource_ref, "res://c.tres", 2);
	moves.push_back(second);
	Dictionary batch;
	bool invalidated = false;
	REQUIRE(journal.commit(2, 2, moves, preexisting, batch, invalidated) == OK);
	const Dictionary move = Array(batch["operations"])[0];
	CHECK(move["from_path"] == "res://a.tres");
	CHECK(move["to_path"] == "res://c.tres");

	for (uint64_t revision = 3; revision <= ResourceDeltaJournal::MAX_ENTRIES + 2; revision++) {
		Array update;
		update.push_back(make_upsert(resource_ref, "res://c.tres", revision));
		REQUIRE(journal.commit(revision, revision, update, preexisting, batch, invalidated) == OK);
	}
	CHECK(journal.get_entry_count() == ResourceDeltaJournal::MAX_ENTRIES);
	CHECK(journal.query_after(1).status == ResourceDeltaJournal::QUERY_GAP);
	CHECK(journal.query_after(2).status == ResourceDeltaJournal::QUERY_BATCH);
}

TEST_CASE("[CodexBridge] Resource delta journal keeps one slot across add remove add and invalidates oversized batches") {
	ResourceDeltaJournal journal;
	journal.initialize(1);
	const Dictionary resource_ref = make_resource_ref(String(), "res://temporary.tres");
	Array operations;
	operations.push_back(make_upsert(resource_ref, "res://temporary.tres", 1));
	Dictionary remove;
	remove["kind"] = "remove";
	remove["resource_ref"] = resource_ref;
	remove["path"] = "res://temporary.tres";
	operations.push_back(remove);
	operations.push_back(make_upsert(resource_ref, "res://temporary.tres", 2));
	Dictionary batch;
	bool invalidated = false;
	REQUIRE(journal.commit(2, 2, operations, HashSet<String>(), batch, invalidated) == OK);
	const Array coalesced = batch["operations"];
	REQUIRE(coalesced.size() == 1);
	const Dictionary latest_resource = Dictionary(Dictionary(coalesced[0])["value"])["resource"];
	CHECK((int64_t)latest_resource["marker"] == 2);

	Array oversized;
	Dictionary oversized_upsert = make_upsert(resource_ref, "res://temporary.tres", 3);
	Dictionary oversized_value = oversized_upsert["value"];
	Dictionary oversized_resource = oversized_value["resource"];
	oversized_resource["test_payload"] = String("x").repeat(ResourceDeltaJournal::MAX_BATCH_BYTES);
	oversized_value["resource"] = oversized_resource;
	oversized_upsert["value"] = oversized_value;
	oversized.push_back(oversized_upsert);
	REQUIRE(journal.commit(3, 3, oversized, HashSet<String>(), batch, invalidated) == OK);
	CHECK(invalidated);
	CHECK(journal.get_entry_count() == 0);
	CHECK(journal.query_after(2).status == ResourceDeltaJournal::QUERY_GAP);
	CHECK(journal.is_invalidating());
	int drained = 0;
	while (journal.drain_invalidation_step()) {
		drained++;
	}
	// The oversized candidate is rejected before insertion, so only the prior
	// retained batch needs incremental destruction.
	CHECK(drained == 1);
	CHECK_FALSE(journal.is_invalidating());
}

TEST_CASE("[CodexBridge] Resource journal releases worst-case retained and prepared DTOs off the main thread") {
	ResourceDeltaJournal journal;
	journal.initialize(1);

	SafeNumeric<uint32_t> retained_destroyed;
	SafeFlag retained_off_main;
	Ref<ResourceDtoCleanupProbe> retained_probe;
	retained_probe.instantiate(&retained_destroyed, &retained_off_main);
	Dictionary retained_leaf;
	retained_leaf["probe"] = retained_probe;
	Array retained_operations;
	retained_operations.resize(ResourceGraphAdapter::MAX_INCREMENTAL_DEPENDENCIES);
	for (int index = 0; index < retained_operations.size(); index++) {
		retained_operations[index] = retained_leaf;
	}
	Dictionary retained_batch;
	retained_batch["operations"] = retained_operations;
	ResourceDeltaJournalTestAccess::append_retained_batch(journal, retained_batch, 2);
	retained_batch = Dictionary();
	retained_operations = Array();
	retained_leaf = Dictionary();
	retained_probe.unref();

	journal.invalidate_to(2);
	CHECK(journal.drain_invalidation_step());
	CHECK_FALSE(journal.drain_invalidation_step());
	ResourceDeltaJournalTestAccess::wait_for_cleanup(journal);
	CHECK(retained_destroyed.get() == 1);
	CHECK(retained_off_main.is_set());

	SafeNumeric<uint32_t> prepared_destroyed;
	SafeFlag prepared_off_main;
	Ref<ResourceDtoCleanupProbe> prepared_probe;
	prepared_probe.instantiate(&prepared_destroyed, &prepared_off_main);
	Dictionary prepared_leaf;
	prepared_leaf["probe"] = prepared_probe;
	Array prepared_operations;
	prepared_operations.resize(ResourceGraphAdapter::MAX_INCREMENTAL_DEPENDENCIES);
	for (int index = 0; index < prepared_operations.size(); index++) {
		prepared_operations[index] = prepared_leaf;
	}
	ResourceDeltaJournal::PreparedBatch prepared;
	prepared.value["operations"] = prepared_operations;
	prepared.encoded_bytes = ResourceDeltaJournal::MAX_BATCH_BYTES + 1;
	prepared_operations = Array();
	prepared_leaf = Dictionary();
	prepared_probe.unref();

	ResourceDeltaJournalTestAccess::retire_prepared(journal, prepared);
	CHECK(prepared.value.is_empty());
	CHECK(prepared.encoded_bytes == 0);
	ResourceDeltaJournalTestAccess::wait_for_cleanup(journal);
	CHECK(prepared_destroyed.get() == 1);
	CHECK(prepared_off_main.is_set());
}

static Dictionary make_script_upsert(const Dictionary &p_script_ref, const String &p_path, uint64_t p_script_graph_revision) {
	Dictionary document;
	document["script_ref"] = p_script_ref;
	document["path"] = p_path;
	document["language"] = "gdscript";
	document["content_sha256"] = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
	document["adapter_profile"] = "gdscript_parser_analyzer_v1";
	document["completeness"] = "complete";
	document["resource_revision"] = 1;
	document["script_graph_revision"] = (int64_t)p_script_graph_revision;
	Dictionary bundle;
	bundle["document"] = document;
	bundle["symbols"] = Array();
	bundle["relations"] = Array();
	bundle["diagnostics"] = Array();
	Dictionary operation;
	operation["kind"] = "upsert_document";
	operation["value"] = bundle;
	return operation;
}

static String script_test_sha256_hex(const String &p_value) {
	const CharString bytes = p_value.utf8();
	PackedByteArray digest;
	digest.resize(32);
	REQUIRE(CryptoCore::sha256(reinterpret_cast<const uint8_t *>(bytes.get_data()), bytes.length(), digest.ptrw()) == OK);
	return BridgeCrypto::bytes_to_lower_hex(digest);
}

TEST_CASE("[CodexS5ScriptJournal] Script delta journal binds exact operations and distinguishes current gap and future") {
	ScriptDeltaJournal journal;
	journal.initialize(1);
	const Dictionary script_ref = make_resource_ref("uid://script-a");
	Array operations;
	operations.push_back(make_script_upsert(script_ref, "res://scripts/a.gd", 2));
	Dictionary status;
	status["language"] = "csharp";
	status["availability"] = "discovery_only";
	status["profile"] = "csharp_discovery_only_v1";
	status["version"] = "1.0";
	status["diagnostic"] = Variant();
	Dictionary status_operation;
	status_operation["kind"] = "adapter_status";
	status_operation["value"] = status;
	operations.push_back(status_operation);

	ScriptDeltaJournal::PreparedBatch prepared;
	REQUIRE(ScriptDeltaJournal::prepare_batch(2, operations, prepared) == OK);
	const String operations_json = JSON::stringify(operations, "", true, true);
	CHECK(prepared.checksum == script_test_sha256_hex(operations_json));
	CHECK(prepared.batch_id.begins_with("script-batch:"));
	CHECK(prepared.batch_id.length() == 45);
	CHECK(prepared.operations_bytes == (uint64_t)operations_json.utf8().length());
	CHECK(prepared.script_graph_revision == 2);

	Dictionary batch;
	bool invalidated = false;
	REQUIRE(journal.commit_prepared(2, 7, 5, 10, prepared, batch, invalidated) == OK);
	CHECK_FALSE(invalidated);
	CHECK((int64_t)batch["previous_script_graph_revision"] == 1);
	CHECK((int64_t)batch["script_graph_revision"] == 2);
	CHECK((int64_t)batch["resource_revision"] == 7);
	CHECK((int64_t)batch["scene_graph_revision"] == 5);
	CHECK((int64_t)batch["project_revision"] == 10);
	CHECK(bool(batch["source_complete"]));
	CHECK(batch["checksum"] == prepared.checksum);
	CHECK(Array(batch["operations"]) == operations);
	CHECK(journal.query_after(2).status == ScriptDeltaJournal::QUERY_CURRENT);
	CHECK(journal.query_after(1).status == ScriptDeltaJournal::QUERY_BATCH);
	CHECK(journal.query_after(0).status == ScriptDeltaJournal::QUERY_GAP);
	CHECK(journal.query_after(3).status == ScriptDeltaJournal::QUERY_FUTURE);

	Dictionary remove;
	remove["kind"] = "remove_document";
	remove["script_ref"] = script_ref;
	remove["path"] = "res://scripts/a.gd";
	CHECK(ScriptDeltaJournal::operation_key(Dictionary(operations[0])) == ScriptDeltaJournal::operation_key(remove));
	Array duplicate_operations = operations.duplicate(true);
	duplicate_operations.push_back(remove);
	ERR_PRINT_OFF;
	CHECK(ScriptDeltaJournal::prepare_batch(3, duplicate_operations, prepared) == ERR_INVALID_PARAMETER);
	ERR_PRINT_ON;
	CHECK(journal.get_current_script_graph_revision() == 2);
}

TEST_CASE("[CodexS5ScriptJournal] Script delta journal evicts at its entry bound without breaking newer replay") {
	ScriptDeltaJournal journal;
	journal.initialize(1);
	const Dictionary script_ref = make_resource_ref("uid://script-b");
	Dictionary batch;
	bool invalidated = false;
	for (uint64_t revision = 2; revision <= ScriptDeltaJournal::MAX_ENTRIES + 2; revision++) {
		Array update;
		update.push_back(make_script_upsert(script_ref, "res://scripts/b.gd", revision));
		REQUIRE(journal.commit(revision, revision, revision, revision, update, batch, invalidated) == OK);
	}
	CHECK(invalidated);
	CHECK(journal.get_entry_count() == ScriptDeltaJournal::MAX_ENTRIES);
	CHECK(journal.get_total_bytes() <= ScriptDeltaJournal::MAX_BYTES);
	CHECK(journal.query_after(1).status == ScriptDeltaJournal::QUERY_GAP);
	CHECK(journal.query_after(2).status == ScriptDeltaJournal::QUERY_BATCH);
}

TEST_CASE("[CodexS5ScriptJournal] Oversized script delta creates a recoverable journal gap") {
	ScriptDeltaJournal journal;
	journal.initialize(1);
	const Dictionary script_ref = make_resource_ref("uid://script-c");
	Dictionary batch;
	bool invalidated = false;
	Array initial;
	initial.push_back(make_script_upsert(script_ref, "res://scripts/c.gd", 2));
	REQUIRE(journal.commit(2, 2, 2, 2, initial, batch, invalidated) == OK);

	Dictionary oversized_operation = make_script_upsert(script_ref, "res://scripts/c.gd", 3);
	Dictionary oversized_bundle = oversized_operation["value"];
	oversized_bundle["test_payload"] = String("x").repeat(ScriptDeltaJournal::MAX_BATCH_BYTES);
	oversized_operation["value"] = oversized_bundle;
	Array oversized;
	oversized.push_back(oversized_operation);
	CHECK(journal.commit(3, 3, 3, 3, oversized, batch, invalidated) == ERR_OUT_OF_MEMORY);
	CHECK(invalidated);
	CHECK(journal.get_current_script_graph_revision() == 3);
	CHECK(journal.query_after(2).status == ScriptDeltaJournal::QUERY_GAP);
	CHECK(journal.is_invalidating());
	CHECK(journal.drain_invalidation_step());
	CHECK_FALSE(journal.drain_invalidation_step());
	CHECK_FALSE(journal.is_invalidating());

	Array recovered;
	recovered.push_back(make_script_upsert(script_ref, "res://scripts/c.gd", 4));
	REQUIRE(journal.commit(4, 4, 4, 4, recovered, batch, invalidated) == OK);
	CHECK_FALSE(invalidated);
	CHECK(journal.query_after(3).status == ScriptDeltaJournal::QUERY_BATCH);
}

TEST_CASE("[CodexS5ScriptJournal] Retired script DTOs are destroyed off the main thread") {
	ScriptDeltaJournal journal;
	journal.initialize(1);

	SafeNumeric<uint32_t> retained_destroyed;
	SafeFlag retained_off_main;
	Ref<ResourceDtoCleanupProbe> retained_probe;
	retained_probe.instantiate(&retained_destroyed, &retained_off_main);
	Dictionary retained_leaf;
	retained_leaf["probe"] = retained_probe;
	Array retained_operations;
	retained_operations.resize(1024);
	for (int index = 0; index < retained_operations.size(); index++) {
		retained_operations[index] = retained_leaf;
	}
	Dictionary retained_batch;
	retained_batch["operations"] = retained_operations;
	ScriptDeltaJournalTestAccess::append_retained_batch(journal, retained_batch, 2);
	retained_batch = Dictionary();
	retained_operations = Array();
	retained_leaf = Dictionary();
	retained_probe.unref();

	journal.invalidate_to(2);
	CHECK(journal.drain_invalidation_step());
	CHECK_FALSE(journal.drain_invalidation_step());
	ScriptDeltaJournalTestAccess::wait_for_cleanup(journal);
	CHECK(retained_destroyed.get() == 1);
	CHECK(retained_off_main.is_set());

	SafeNumeric<uint32_t> prepared_destroyed;
	SafeFlag prepared_off_main;
	Ref<ResourceDtoCleanupProbe> prepared_probe;
	prepared_probe.instantiate(&prepared_destroyed, &prepared_off_main);
	Dictionary prepared_leaf;
	prepared_leaf["probe"] = prepared_probe;
	ScriptDeltaJournal::PreparedBatch prepared;
	prepared.operations.resize(1024);
	for (int index = 0; index < prepared.operations.size(); index++) {
		prepared.operations[index] = prepared_leaf;
	}
	prepared.operations_bytes = ScriptDeltaJournal::MAX_BATCH_BYTES;
	prepared_leaf = Dictionary();
	prepared_probe.unref();

	ScriptDeltaJournalTestAccess::retire_prepared(journal, prepared);
	CHECK(prepared.operations.is_empty());
	CHECK(prepared.operations_bytes == 0);
	CHECK(prepared.script_graph_revision == 0);
	ScriptDeltaJournalTestAccess::wait_for_cleanup(journal);
	CHECK(prepared_destroyed.get() == 1);
	CHECK(prepared_off_main.is_set());
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

	BridgeRpcSession late_snapshot(project_id, editor_session_id);
	late_snapshot.set_protocol_version("1.2");
	REQUIRE(late_snapshot.handle_message(make_rpc_request("req:late-init", "bridge.initialize", make_initialize_params(), project_id, editor_session_id, 5000, "1.2"), 0, 1, outcome) == OK);
	REQUIRE(late_snapshot.complete(1, 1, outcome) == OK);
	REQUIRE(late_snapshot.handle_message(make_rpc_request("req:late-snapshot", "resource.snapshot.get", Dictionary(), project_id, editor_session_id, 30000, "1.2"), 1000, 2, outcome) == OK);
	Dictionary snapshot_result;
	snapshot_result["snapshot_id"] = "snapshot:0123456789abcdef0123456789abcdef";
	REQUIRE(late_snapshot.complete(2, 120001000, snapshot_result, outcome) == OK);
	CHECK(rpc_error_code(outcome) == "deadline_exceeded");
	CHECK_FALSE(outcome.response.has("result"));

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

TEST_CASE("[CodexBridge] Dispatcher preserves an already queued terminal cancellation") {
	MainThreadDispatcher dispatcher;
	dispatcher.start_accepting();
	MainThreadDispatcher::Command cancellation = make_command(42);
	cancellation.type = MainThreadDispatcher::COMMAND_CANCEL;
	REQUIRE(dispatcher.enqueue(cancellation) == MainThreadDispatcher::ENQUEUE_OK);

	CHECK(dispatcher.cancel(42));
	CHECK(dispatcher.get_queue_size() == 1);

	HandlerContext context;
	const MainThreadDispatcher::ProcessStats stats = dispatcher.process(record_command, &context, 8, 2000, test_clock, &context);
	CHECK(stats.processed == 1);
	CHECK(stats.remaining == 0);
	REQUIRE(context.handled_ids.size() == 1);
	CHECK(context.handled_ids[0] == 42);
}

TEST_CASE("[CodexBridge] Dispatcher reserves priority capacity for terminal cancellation") {
	MainThreadDispatcher dispatcher;
	dispatcher.start_accepting();
	for (uint64_t index = 0; index < MainThreadDispatcher::MAX_QUEUE_SIZE; index++) {
		REQUIRE(dispatcher.enqueue(make_command(index)) == MainThreadDispatcher::ENQUEUE_OK);
	}
	MainThreadDispatcher::Command cancellation = make_command(999);
	cancellation.type = MainThreadDispatcher::COMMAND_CANCEL;
	REQUIRE(dispatcher.enqueue(cancellation) == MainThreadDispatcher::ENQUEUE_OK);

	HandlerContext context;
	const MainThreadDispatcher::ProcessStats stats = dispatcher.process(record_command, &context, 1, 2000, test_clock, &context);
	CHECK(stats.processed == 1);
	CHECK(stats.remaining == MainThreadDispatcher::MAX_QUEUE_SIZE);
	REQUIRE(context.handled_ids.size() == 1);
	CHECK(context.handled_ids[0] == 999);
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

static Ref<BridgeStreamPeer> connect_authenticated_test_client(const String &p_project_root, const Dictionary &p_discovery, const String &p_protocol_version = "1.0") {
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
	versions.push_back(p_protocol_version);
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
	offered.push_back(p_protocol_version);
	PackedByteArray transcript;
	PackedByteArray expected_server_proof;
	if (BridgeCrypto::base64url_decode_32(challenge["server_nonce"], server_nonce) != OK ||
			BridgeCrypto::base64url_decode_32(challenge["server_proof"], received_server_proof) != OK ||
			BridgeCrypto::build_handshake_transcript("1.0", offered, p_protocol_version, p_discovery["project_id"], p_discovery["editor_session_id"], client_nonce, server_nonce, transcript) != OK ||
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
	authenticate["selected_protocol_version"] = p_protocol_version;
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

static Dictionary make_rpc_cancel(const String &p_request_id, const Dictionary &p_discovery, const String &p_protocol_version = "1.0") {
	Dictionary cancel;
	cancel["protocol_version"] = p_protocol_version;
	cancel["kind"] = "cancel";
	cancel["request_id"] = p_request_id;
	cancel["context"] = make_rpc_context(p_discovery["project_id"], p_discovery["editor_session_id"]);
	cancel["reason"] = "client_cancelled";
	return cancel;
}

struct ResourceSnapshotDraftContext {
	BridgeTransportWorker *worker = nullptr;
	Dictionary result;
	Array messages;
	uint64_t request_id = 0;
};

static void stage_resource_snapshot_draft(const MainThreadDispatcher::Command &p_command, void *p_userdata) {
	ResourceSnapshotDraftContext *context = static_cast<ResourceSnapshotDraftContext *>(p_userdata);
	context->request_id = p_command.request_id;
	for (int index = 0; index < context->messages.size() - 1; index++) {
		context->worker->stage_resource_snapshot_message(p_command.request_id, context->messages[index]);
	}
	context->worker->complete_resource_snapshot(p_command.request_id, context->result, context->messages[context->messages.size() - 1]);
}

struct CapturedCommands {
	LocalVector<uint64_t> request_ids;
	LocalVector<MainThreadDispatcher::CommandType> types;
};

static void capture_command(const MainThreadDispatcher::Command &p_command, void *p_userdata) {
	CapturedCommands *captured = static_cast<CapturedCommands *>(p_userdata);
	captured->request_ids.push_back(p_command.request_id);
	captured->types.push_back(p_command.type);
}

static ResourceSnapshotDraftContext make_resource_snapshot_draft(BridgeTransportWorker &p_worker, const Dictionary &p_discovery, int p_chunk_count) {
	ResourceSnapshotDraftContext context;
	context.worker = &p_worker;
	const String snapshot_id = "snapshot:0123456789abcdef0123456789abcdef";
	const Dictionary rpc_context = make_rpc_context(p_discovery["project_id"], p_discovery["editor_session_id"]);
	context.result["snapshot_id"] = snapshot_id;
	context.result["domain"] = "resource_graph";

	Dictionary begin_params;
	begin_params["snapshot_id"] = snapshot_id;
	begin_params["domain"] = "resource_graph";
	Dictionary begin;
	begin["protocol_version"] = "1.2";
	begin["kind"] = "notification";
	begin["method"] = "snapshot.begin";
	begin["params"] = begin_params;
	begin["context"] = rpc_context;
	context.messages.push_back(begin);

	for (int chunk_index = 0; chunk_index < p_chunk_count; chunk_index++) {
		Dictionary payload;
		payload["resources"] = Array();
		payload["dependencies"] = Array();
		payload["diagnostics"] = Array();
		Dictionary chunk;
		chunk["protocol_version"] = "1.2";
		chunk["kind"] = "chunk";
		chunk["snapshot_id"] = snapshot_id;
		chunk["domain"] = "resource_graph";
		chunk["chunk_index"] = chunk_index;
		chunk["payload"] = payload;
		chunk["context"] = rpc_context;
		context.messages.push_back(chunk);
	}

	Dictionary end_params;
	end_params["snapshot_id"] = snapshot_id;
	end_params["domain"] = "resource_graph";
	end_params["chunk_count"] = p_chunk_count;
	end_params["checksum"] = "";
	Dictionary end;
	end["protocol_version"] = "1.2";
	end["kind"] = "notification";
	end["method"] = "snapshot.end";
	end["params"] = end_params;
	end["context"] = rpc_context;
	context.messages.push_back(end);
	return context;
}

static bool wait_for_disconnect(const Ref<BridgeStreamPeer> &p_peer) {
	if (p_peer.is_null()) {
		return false;
	}
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

TEST_CASE("[CodexBridge] Incremental resource snapshot preparation delivers one terminal cancellation") {
	TemporaryBridgeProject project;
	REQUIRE(project.error == OK);
	MainThreadDispatcher dispatcher;
	dispatcher.start_accepting();
	BridgeTransportWorker worker;
	REQUIRE(worker.start(project.root, &dispatcher) == OK);
	Dictionary discovery;
	REQUIRE(BridgeJson::parse_strict_object(read_file_bytes(project.root.path_join(".godot/codex/bridge.json")), discovery) == OK);
	Ref<BridgeStreamPeer> client = connect_authenticated_test_client(project.root, discovery, "1.2");
	REQUIRE(client.is_valid());

	Dictionary response;
	REQUIRE(send_test_object(client, make_rpc_request("req:prepared-init", "bridge.initialize", make_initialize_params(), discovery["project_id"], discovery["editor_session_id"], 5000, "1.2")));
	REQUIRE(dispatch_worker_request(dispatcher, worker));
	REQUIRE(receive_test_object(client, response));
	REQUIRE(response.has("result"));
	CHECK(Dictionary(response["result"])["protocol_version"] == "1.2");

	REQUIRE(send_test_object(client, make_rpc_request("req:prepared-cancel", "resource.snapshot.get", Dictionary(), discovery["project_id"], discovery["editor_session_id"], 30000, "1.2")));
	REQUIRE(wait_for_dispatcher_size(dispatcher, 1));
	ResourceSnapshotDraftContext draft = make_resource_snapshot_draft(worker, discovery, 512);
	const MainThreadDispatcher::ProcessStats completion_stats = dispatcher.process(stage_resource_snapshot_draft, &draft, 1);
	REQUIRE(completion_stats.processed == 1);
	REQUIRE(draft.request_id != 0);

	REQUIRE(send_test_object(client, make_rpc_cancel("req:prepared-cancel", discovery, "1.2")));
	REQUIRE(receive_test_object(client, response));
	REQUIRE(response.has("error"));
	CHECK(Dictionary(response["error"])["code"] == "cancelled");
	REQUIRE(wait_for_dispatcher_size(dispatcher, 1));
	// Allow the preparation-pruning pass to observe the same cancelled request.
	// It must preserve, rather than remove or duplicate, the terminal command.
	OS::get_singleton()->delay_usec(20000);
	CHECK(dispatcher.get_queue_size() == 1);

	CapturedCommands captured;
	const MainThreadDispatcher::ProcessStats cancellation_stats = dispatcher.process(capture_command, &captured, 8);
	CHECK(cancellation_stats.processed == 1);
	CHECK(cancellation_stats.remaining == 0);
	REQUIRE(captured.request_ids.size() == 1);
	REQUIRE(captured.types.size() == 1);
	CHECK(captured.request_ids[0] == draft.request_id);
	CHECK(captured.types[0] == MainThreadDispatcher::COMMAND_CANCEL);
	OS::get_singleton()->delay_usec(10000);
	CHECK(dispatcher.get_queue_size() == 0);

	client->disconnect_from_host();
	CHECK(worker.stop() == BridgeTransportWorker::STOPPED);
	dispatcher.begin_shutdown();
}

TEST_CASE("[CodexBridge] Resource snapshot spool preserves response order and releases on ACK") {
	TemporaryBridgeProject project;
	REQUIRE(project.error == OK);
	MainThreadDispatcher dispatcher;
	dispatcher.start_accepting();
	BridgeTransportWorker worker;
	REQUIRE(worker.start(project.root, &dispatcher) == OK);
	Dictionary discovery;
	REQUIRE(BridgeJson::parse_strict_object(read_file_bytes(project.root.path_join(".godot/codex/bridge.json")), discovery) == OK);
	Ref<BridgeStreamPeer> client = connect_authenticated_test_client(project.root, discovery, "1.2");
	REQUIRE(client.is_valid());

	Dictionary message;
	REQUIRE(send_test_object(client, make_rpc_request("req:spool-init", "bridge.initialize", make_initialize_params(), discovery["project_id"], discovery["editor_session_id"], 5000, "1.2")));
	REQUIRE(dispatch_worker_request(dispatcher, worker));
	REQUIRE(receive_test_object(client, message));
	REQUIRE(message.has("result"));

	REQUIRE(send_test_object(client, make_rpc_request("req:spooled", "resource.snapshot.get", Dictionary(), discovery["project_id"], discovery["editor_session_id"], 30000, "1.2")));
	REQUIRE(wait_for_dispatcher_size(dispatcher, 1));
	ResourceSnapshotDraftContext draft = make_resource_snapshot_draft(worker, discovery, 2);
	const MainThreadDispatcher::ProcessStats completion_stats = dispatcher.process(stage_resource_snapshot_draft, &draft, 1);
	REQUIRE(completion_stats.processed == 1);

	REQUIRE(receive_test_object(client, message));
	REQUIRE(message.has("result"));
	const String snapshot_id = Dictionary(message["result"])["snapshot_id"];
	REQUIRE(receive_test_object(client, message));
	CHECK(message["method"] == "snapshot.begin");
	String chunk_checksums;
	for (int chunk_index = 0; chunk_index < 2; chunk_index++) {
		REQUIRE(receive_test_object(client, message));
		CHECK(message["kind"] == "chunk");
		CHECK(message["snapshot_id"] == snapshot_id);
		CHECK((int64_t)message["chunk_index"] == chunk_index);
		CHECK_FALSE(String(message["payload_json"]).is_empty());
		const String chunk_checksum = message["checksum"];
		CHECK(chunk_checksum.length() == 64);
		chunk_checksums += chunk_checksum;
	}
	REQUIRE(receive_test_object(client, message));
	CHECK(message["method"] == "snapshot.end");
	const CharString checksum_bytes = chunk_checksums.utf8();
	PackedByteArray checksum_digest;
	checksum_digest.resize(32);
	REQUIRE(CryptoCore::sha256(reinterpret_cast<const uint8_t *>(checksum_bytes.get_data()), checksum_bytes.length(), checksum_digest.ptrw()) == OK);
	CHECK(Dictionary(message["params"])["checksum"] == BridgeCrypto::bytes_to_lower_hex(checksum_digest));

	Dictionary ack_params;
	ack_params["snapshot_id"] = snapshot_id;
	ack_params["domain"] = "resource_graph";
	ack_params["through_chunk"] = 1;
	Dictionary ack;
	ack["protocol_version"] = "1.2";
	ack["kind"] = "ack";
	ack["ack_id"] = "ack:spooled";
	ack["params"] = ack_params;
	ack["context"] = make_rpc_context(discovery["project_id"], discovery["editor_session_id"]);
	REQUIRE(send_test_object(client, ack));
	REQUIRE(wait_for_dispatcher_size(dispatcher, 1));
	CapturedCommands captured;
	const MainThreadDispatcher::ProcessStats release_stats = dispatcher.process(capture_command, &captured, 1);
	CHECK(release_stats.processed == 1);
	REQUIRE(captured.types.size() == 1);
	CHECK(captured.types[0] == MainThreadDispatcher::COMMAND_CANCEL);

	client->disconnect_from_host();
	CHECK(worker.stop() == BridgeTransportWorker::STOPPED);
	dispatcher.begin_shutdown();
}

#endif // UNIX_ENABLED || WINDOWS_ENABLED

} // namespace TestCodexBridge

#endif // MODULE_CODEX_BRIDGE_ENABLED
