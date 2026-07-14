/**************************************************************************/
/*  bridge_runtime.cpp                                                    */
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

#include "bridge_runtime.h"

#include "core/io/json.h"
#include "core/os/os.h"
#include "core/os/time.h"

#include "modules/codex_bridge/protocol/bridge_crypto.h"
#include "modules/codex_bridge/protocol/bridge_frame_codec.h"

#ifdef UNIX_ENABLED
#include <fcntl.h>
#include <signal.h>
#include <sys/file.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

#include <cerrno>
#include <climits>
#include <cstdio>
#endif

namespace {

#ifdef UNIX_ENABLED

static constexpr uint64_t PROBE_TIMEOUT_USEC = 500000;

static CharString path_utf8(const String &p_path) {
	return p_path.utf8();
}

static bool path_exists_no_follow(const String &p_path) {
	struct stat status = {};
	const CharString path = path_utf8(p_path);
	return lstat(path.get_data(), &status) == 0;
}

static Error remove_path_no_follow(const String &p_path) {
	const CharString path = path_utf8(p_path);
	if (unlink(path.get_data()) == 0 || errno == ENOENT) {
		return OK;
	}
	return FAILED;
}

static bool is_process_alive(int64_t p_pid) {
	if (p_pid <= 0 || p_pid > INT32_MAX) {
		return false;
	}
	if (kill((pid_t)p_pid, 0) == 0) {
		return true;
	}
	return errno == EPERM;
}

static Error sync_directory(const String &p_directory) {
	const CharString path = path_utf8(p_directory);
	const int descriptor = open(path.get_data(), O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
	if (descriptor < 0) {
		return ERR_CANT_OPEN;
	}
	const int result = fsync(descriptor);
	close(descriptor);
	return result == 0 || errno == EINVAL ? OK : FAILED;
}

static Error ensure_private_directory(const String &p_directory) {
	struct stat status = {};
	const CharString path = path_utf8(p_directory);
	if (lstat(path.get_data(), &status) != 0) {
		if (errno != ENOENT || mkdir(path.get_data(), BridgeRuntime::DIRECTORY_MODE) != 0) {
			return ERR_CANT_CREATE;
		}
		if (chmod(path.get_data(), BridgeRuntime::DIRECTORY_MODE) != 0) {
			return ERR_CANT_CREATE;
		}
	}
	return BridgeRuntime::validate_private_path(p_directory, BridgeRuntime::DIRECTORY_MODE, true);
}

static Error ensure_project_data_directory(const String &p_directory) {
	struct stat status = {};
	const CharString path = path_utf8(p_directory);
	if (lstat(path.get_data(), &status) == 0) {
		return S_ISDIR(status.st_mode) && !S_ISLNK(status.st_mode) ? OK : ERR_INVALID_DATA;
	}
	if (errno != ENOENT || mkdir(path.get_data(), BridgeRuntime::DIRECTORY_MODE) != 0) {
		return ERR_CANT_CREATE;
	}
	return OK;
}

static Error write_all(int p_descriptor, const uint8_t *p_bytes, size_t p_size) {
	size_t written = 0;
	while (written < p_size) {
		const ssize_t result = write(p_descriptor, p_bytes + written, p_size - written);
		if (result < 0 && errno == EINTR) {
			continue;
		}
		if (result <= 0) {
			return ERR_FILE_CANT_WRITE;
		}
		written += result;
	}
	return OK;
}

static Error atomic_write_private_file(const String &p_target, const String &p_temporary, const uint8_t *p_bytes, size_t p_size, const String &p_parent_directory) {
	remove_path_no_follow(p_temporary);
	const CharString temporary = path_utf8(p_temporary);
	int descriptor = open(temporary.get_data(), O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, BridgeRuntime::PRIVATE_FILE_MODE);
	if (descriptor < 0) {
		return ERR_CANT_CREATE;
	}
	Error error = OK;
	if (fchmod(descriptor, BridgeRuntime::PRIVATE_FILE_MODE) != 0) {
		error = ERR_CANT_CREATE;
	}
	if (error == OK) {
		error = write_all(descriptor, p_bytes, p_size);
	}
	if (error == OK && fsync(descriptor) != 0) {
		error = ERR_FILE_CANT_WRITE;
	}
	struct stat status = {};
	if (error == OK && (fstat(descriptor, &status) != 0 || !S_ISREG(status.st_mode) || status.st_uid != geteuid() || (status.st_mode & 0777) != BridgeRuntime::PRIVATE_FILE_MODE)) {
		error = ERR_UNAUTHORIZED;
	}
	close(descriptor);
	if (error != OK) {
		remove_path_no_follow(p_temporary);
		return error;
	}

	const CharString target = path_utf8(p_target);
	if (rename(temporary.get_data(), target.get_data()) != 0) {
		remove_path_no_follow(p_temporary);
		return ERR_CANT_CREATE;
	}
	error = BridgeRuntime::validate_private_path(p_target, BridgeRuntime::PRIVATE_FILE_MODE, false);
	if (error != OK) {
		remove_path_no_follow(p_target);
		return error;
	}
	return sync_directory(p_parent_directory);
}

static Error read_private_file(const String &p_path, int p_expected_size, PackedByteArray &r_bytes) {
	Error error = BridgeRuntime::validate_private_path(p_path, BridgeRuntime::PRIVATE_FILE_MODE, false);
	if (error != OK) {
		return error;
	}
	const CharString path = path_utf8(p_path);
	const int descriptor = open(path.get_data(), O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
	if (descriptor < 0) {
		return ERR_CANT_OPEN;
	}
	struct stat status = {};
	if (fstat(descriptor, &status) != 0 || !S_ISREG(status.st_mode) || status.st_uid != geteuid() || (status.st_mode & 0777) != BridgeRuntime::PRIVATE_FILE_MODE || status.st_size < 0 || status.st_size > BridgeFrameCodec::MAX_PAYLOAD_BYTES || (p_expected_size >= 0 && status.st_size != p_expected_size)) {
		close(descriptor);
		return ERR_INVALID_DATA;
	}
	r_bytes.resize(status.st_size);
	size_t read_size = 0;
	while (read_size < (size_t)status.st_size) {
		const ssize_t result = read(descriptor, r_bytes.ptrw() + read_size, status.st_size - read_size);
		if (result < 0 && errno == EINTR) {
			continue;
		}
		if (result <= 0) {
			close(descriptor);
			r_bytes.clear();
			return ERR_FILE_CANT_READ;
		}
		read_size += result;
	}
	close(descriptor);
	return OK;
}

static bool is_lower_hex_32(const String &p_value) {
	if (p_value.length() != 32) {
		return false;
	}
	for (int index = 0; index < p_value.length(); index++) {
		const char32_t character = p_value[index];
		if (!((character >= '0' && character <= '9') || (character >= 'a' && character <= 'f'))) {
			return false;
		}
	}
	return true;
}

static bool is_lower_hex(const String &p_value, int p_length) {
	if (p_value.length() != p_length) {
		return false;
	}
	for (int index = 0; index < p_value.length(); index++) {
		const char32_t character = p_value[index];
		if (!((character >= '0' && character <= '9') || (character >= 'a' && character <= 'f'))) {
			return false;
		}
	}
	return true;
}

static bool validate_relative_endpoint(const String &p_endpoint) {
	static const String prefix = ".godot/codex/run/bridge-";
	static const String suffix = ".sock";
	return !p_endpoint.is_absolute_path() && !p_endpoint.contains("..") && p_endpoint.begins_with(prefix) && p_endpoint.ends_with(suffix) && is_lower_hex_32(p_endpoint.substr(prefix.length(), 32)) && p_endpoint.length() == prefix.length() + 32 + suffix.length();
}

static bool wait_for_connected(const Ref<StreamPeerUDS> &p_peer, uint64_t p_deadline) {
	while (OS::get_singleton()->get_ticks_usec() < p_deadline) {
		if (p_peer->poll() != OK) {
			return false;
		}
		if (p_peer->get_status() == StreamPeerUDS::STATUS_CONNECTED) {
			return true;
		}
		if (p_peer->get_status() == StreamPeerUDS::STATUS_ERROR || p_peer->get_status() == StreamPeerUDS::STATUS_NONE) {
			return false;
		}
		OS::get_singleton()->delay_usec(1000);
	}
	return false;
}

static bool send_frame(const Ref<StreamPeerUDS> &p_peer, const PackedByteArray &p_frame, uint64_t p_deadline) {
	int offset = 0;
	while (offset < p_frame.size() && OS::get_singleton()->get_ticks_usec() < p_deadline) {
		int sent = 0;
		const Error error = p_peer->put_partial_data(p_frame.ptr() + offset, p_frame.size() - offset, sent);
		if (error != OK) {
			return false;
		}
		offset += sent;
		if (sent == 0) {
			OS::get_singleton()->delay_usec(1000);
		}
	}
	return offset == p_frame.size();
}

static bool receive_object(const Ref<StreamPeerUDS> &p_peer, uint64_t p_deadline, Dictionary &r_object) {
	BridgeFrameCodec codec;
	while (OS::get_singleton()->get_ticks_usec() < p_deadline) {
		if (p_peer->poll() != OK || p_peer->get_status() != StreamPeerUDS::STATUS_CONNECTED) {
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

static bool get_string_field(const Dictionary &p_object, const StringName &p_key, String &r_value) {
	if (!p_object.has(p_key) || p_object[p_key].get_type() != Variant::STRING) {
		return false;
	}
	r_value = p_object[p_key];
	return true;
}

static bool get_bounded_integer_field(const Dictionary &p_object, const StringName &p_key, int64_t p_minimum, int64_t p_maximum, int64_t &r_value) {
	if (!p_object.has(p_key)) {
		return false;
	}
	const Variant value = p_object[p_key];
	if (value.get_type() == Variant::INT) {
		r_value = value;
		return r_value >= p_minimum && r_value <= p_maximum;
	}
	if (value.get_type() != Variant::FLOAT) {
		return false;
	}
	const double number = value;
	if (number < (double)p_minimum || number > (double)p_maximum) {
		return false;
	}
	r_value = (int64_t)number;
	return (double)r_value == number;
}

static bool validate_discovery_record(const Dictionary &p_discovery, const String &p_expected_project_id) {
	String transport;
	String endpoint;
	String token_file;
	String project_id;
	String editor_session_id;
	String created_at;
	int64_t discovery_schema = 0;
	int64_t pid = 0;
	if (!get_bounded_integer_field(p_discovery, "discovery_schema", 1, 1, discovery_schema) ||
			!get_bounded_integer_field(p_discovery, "pid", 1, INT32_MAX, pid) ||
			!get_string_field(p_discovery, "transport", transport) || transport != "uds" ||
			!get_string_field(p_discovery, "endpoint", endpoint) || !validate_relative_endpoint(endpoint) ||
			!get_string_field(p_discovery, "token_file", token_file) || token_file != ".godot/codex/session.token" ||
			!get_string_field(p_discovery, "project_id", project_id) || project_id != p_expected_project_id || !project_id.begins_with("project:sha256:") || !is_lower_hex(project_id.trim_prefix("project:sha256:"), 64) ||
			!get_string_field(p_discovery, "editor_session_id", editor_session_id) || !editor_session_id.begins_with("editor:") || !is_lower_hex(editor_session_id.trim_prefix("editor:"), 32) ||
			!get_string_field(p_discovery, "created_at", created_at) || created_at.is_empty() || created_at.length() > 64 ||
			!p_discovery.has("protocol_versions") || p_discovery["protocol_versions"].get_type() != Variant::ARRAY) {
		return false;
	}
	const Array versions = p_discovery["protocol_versions"];
	return versions.size() == 1 && versions[0].get_type() == Variant::STRING && String(versions[0]) == "1.0";
}

static bool probe_authenticated_endpoint(const String &p_project_root, const Dictionary &p_discovery) {
	String endpoint_relative;
	String token_relative;
	String project_id;
	String editor_session_id;
	if (!get_string_field(p_discovery, "endpoint", endpoint_relative) || !validate_relative_endpoint(endpoint_relative) ||
			!get_string_field(p_discovery, "token_file", token_relative) || token_relative != ".godot/codex/session.token" ||
			!get_string_field(p_discovery, "project_id", project_id) || !get_string_field(p_discovery, "editor_session_id", editor_session_id)) {
		return false;
	}
	const String endpoint = p_project_root.path_join(endpoint_relative);
	const String token_path = p_project_root.path_join(token_relative);
	if (BridgeRuntime::validate_private_path(endpoint, BridgeRuntime::PRIVATE_FILE_MODE, false, true) != OK) {
		return false;
	}
	PackedByteArray token;
	if (read_private_file(token_path, BridgeCrypto::RANDOM_VALUE_BYTES, token) != OK) {
		return false;
	}

	Ref<StreamPeerUDS> peer;
	peer.instantiate();
	if (peer->connect_to_host(endpoint) != OK) {
		return false;
	}
	const uint64_t deadline = OS::get_singleton()->get_ticks_usec() + PROBE_TIMEOUT_USEC;
	if (!wait_for_connected(peer, deadline)) {
		return false;
	}

	PackedByteArray client_nonce;
	if (BridgeCrypto::random_bytes(BridgeCrypto::RANDOM_VALUE_BYTES, client_nonce) != OK) {
		return false;
	}
	String client_nonce_encoded;
	BridgeCrypto::base64url_encode_32(client_nonce, client_nonce_encoded);
	Dictionary hello;
	hello["handshake_version"] = "1.0";
	hello["kind"] = "handshake.client_hello";
	Array versions;
	versions.push_back("1.0");
	hello["supported_protocol_versions"] = versions;
	hello["project_id"] = project_id;
	hello["editor_session_id"] = editor_session_id;
	hello["client_nonce"] = client_nonce_encoded;
	PackedByteArray frame;
	if (BridgeFrameCodec::encode_json(hello, frame) != OK || !send_frame(peer, frame, deadline)) {
		return false;
	}
	Dictionary challenge;
	String kind;
	String server_nonce_encoded;
	String server_proof_encoded;
	String challenge_handshake_version;
	String challenge_selected_version;
	String challenge_project_id;
	String challenge_editor_session_id;
	if (!receive_object(peer, deadline, challenge) || !get_string_field(challenge, "kind", kind) || kind != "handshake.server_challenge" ||
			!get_string_field(challenge, "handshake_version", challenge_handshake_version) || challenge_handshake_version != "1.0" ||
			!get_string_field(challenge, "selected_protocol_version", challenge_selected_version) || challenge_selected_version != "1.0" ||
			!get_string_field(challenge, "project_id", challenge_project_id) || challenge_project_id != project_id ||
			!get_string_field(challenge, "editor_session_id", challenge_editor_session_id) || challenge_editor_session_id != editor_session_id ||
			!get_string_field(challenge, "server_nonce", server_nonce_encoded) || !get_string_field(challenge, "server_proof", server_proof_encoded)) {
		return false;
	}
	PackedByteArray server_nonce;
	PackedByteArray received_server_proof;
	if (BridgeCrypto::base64url_decode_32(server_nonce_encoded, server_nonce) != OK || BridgeCrypto::base64url_decode_32(server_proof_encoded, received_server_proof) != OK) {
		return false;
	}
	PackedStringArray offered;
	offered.push_back("1.0");
	PackedByteArray transcript;
	PackedByteArray expected_server_proof;
	if (BridgeCrypto::build_handshake_transcript("1.0", offered, "1.0", project_id, editor_session_id, client_nonce, server_nonce, transcript) != OK ||
			BridgeCrypto::handshake_proof(true, token, transcript, expected_server_proof) != OK || !BridgeCrypto::constant_time_equal(expected_server_proof, received_server_proof)) {
		return false;
	}
	PackedByteArray client_proof;
	String client_proof_encoded;
	if (BridgeCrypto::handshake_proof(false, token, transcript, client_proof) != OK || BridgeCrypto::base64url_encode_32(client_proof, client_proof_encoded) != OK) {
		return false;
	}
	Dictionary authenticate;
	authenticate["handshake_version"] = "1.0";
	authenticate["kind"] = "handshake.client_authenticate";
	authenticate["selected_protocol_version"] = "1.0";
	authenticate["project_id"] = project_id;
	authenticate["editor_session_id"] = editor_session_id;
	authenticate["client_proof"] = client_proof_encoded;
	if (BridgeFrameCodec::encode_json(authenticate, frame) != OK || !send_frame(peer, frame, deadline)) {
		return false;
	}
	Dictionary ready;
	String ready_handshake_version;
	String ready_selected_version;
	String ready_project_id;
	String ready_editor_session_id;
	return receive_object(peer, deadline, ready) && get_string_field(ready, "kind", kind) && kind == "handshake.server_ready" &&
			get_string_field(ready, "handshake_version", ready_handshake_version) && ready_handshake_version == "1.0" &&
			get_string_field(ready, "selected_protocol_version", ready_selected_version) && ready_selected_version == "1.0" &&
			get_string_field(ready, "project_id", ready_project_id) && ready_project_id == project_id &&
			get_string_field(ready, "editor_session_id", ready_editor_session_id) && ready_editor_session_id == editor_session_id;
}

static Error read_discovery(const String &p_path, Dictionary &r_discovery) {
	PackedByteArray bytes;
	const Error error = read_private_file(p_path, -1, bytes);
	if (error != OK) {
		return error;
	}
	return BridgeJson::parse_strict_object(bytes, r_discovery);
}

#endif // UNIX_ENABLED

} // namespace

Error BridgeRuntime::canonicalize_project_root(const String &p_project_root, String &r_canonical_root) {
#ifdef UNIX_ENABLED
	if (p_project_root.is_empty()) {
		return ERR_INVALID_PARAMETER;
	}
	const CharString source = p_project_root.utf8();
	char resolved[PATH_MAX];
	if (realpath(source.get_data(), resolved) == nullptr) {
		return ERR_FILE_NOT_FOUND;
	}
	struct stat root_status = {};
	if (stat(resolved, &root_status) != 0 || !S_ISDIR(root_status.st_mode)) {
		return ERR_FILE_NOT_FOUND;
	}
	const String canonical = String::utf8(resolved).trim_suffix("/");
	const String project_file = (canonical.is_empty() ? String("/") : canonical).path_join("project.godot");
	struct stat project_status = {};
	const CharString project_file_utf8 = project_file.utf8();
	if (stat(project_file_utf8.get_data(), &project_status) != 0 || !S_ISREG(project_status.st_mode)) {
		return ERR_FILE_NOT_FOUND;
	}
	r_canonical_root = canonical.is_empty() ? String("/") : canonical;
	return OK;
#else
	return ERR_UNAVAILABLE;
#endif
}

Error BridgeRuntime::validate_private_path(const String &p_path, uint32_t p_mode, bool p_directory, bool p_socket) {
#ifdef UNIX_ENABLED
	struct stat status = {};
	const CharString path = p_path.utf8();
	if (lstat(path.get_data(), &status) != 0 || S_ISLNK(status.st_mode) || status.st_uid != geteuid() || (status.st_mode & 0777) != p_mode) {
		return ERR_UNAUTHORIZED;
	}
	if ((p_directory && !S_ISDIR(status.st_mode)) || (p_socket && !S_ISSOCK(status.st_mode)) || (!p_directory && !p_socket && !S_ISREG(status.st_mode))) {
		return ERR_INVALID_DATA;
	}
	return OK;
#else
	return ERR_UNAVAILABLE;
#endif
}

bool BridgeRuntime::probe_authenticated_runtime(const String &p_project_root) {
#ifdef UNIX_ENABLED
	String canonical_root;
	if (canonicalize_project_root(p_project_root, canonical_root) != OK) {
		return false;
	}
	const String codex_dir = canonical_root.path_join(".godot/codex");
	if (validate_private_path(codex_dir, DIRECTORY_MODE, true) != OK || validate_private_path(codex_dir.path_join("run"), DIRECTORY_MODE, true) != OK) {
		return false;
	}
	Dictionary before;
	if (read_discovery(codex_dir.path_join("bridge.json"), before) != OK) {
		return false;
	}
	String expected_project_id;
	String published_project_id;
	String before_session;
	if (BridgeCrypto::project_id_from_canonical_root(canonical_root, expected_project_id) != OK || !validate_discovery_record(before, expected_project_id) || !get_string_field(before, "project_id", published_project_id) || published_project_id != expected_project_id || !get_string_field(before, "editor_session_id", before_session)) {
		return false;
	}
	if (!probe_authenticated_endpoint(canonical_root, before)) {
		return false;
	}
	Dictionary after;
	String after_session;
	return read_discovery(codex_dir.path_join("bridge.json"), after) == OK && get_string_field(after, "editor_session_id", after_session) && after_session == before_session && JSON::stringify(after, "", true) == JSON::stringify(before, "", true);
#else
	return false;
#endif
}

Error BridgeRuntime::_acquire_lock() {
#ifdef UNIX_ENABLED
	bool existed = path_exists_no_follow(lock_path);
	if (existed && validate_private_path(lock_path, PRIVATE_FILE_MODE, false) != OK) {
		return ERR_UNAUTHORIZED;
	}
	const CharString path = lock_path.utf8();
	lock_fd = open(path.get_data(), O_RDWR | O_CREAT | O_CLOEXEC | O_NOFOLLOW, PRIVATE_FILE_MODE);
	if (lock_fd < 0) {
		return ERR_CANT_OPEN;
	}
	if (!existed && fchmod(lock_fd, PRIVATE_FILE_MODE) != 0) {
		_release_lock();
		return ERR_UNAUTHORIZED;
	}
	struct stat status = {};
	if (fstat(lock_fd, &status) != 0 || !S_ISREG(status.st_mode) || status.st_uid != geteuid() || (status.st_mode & 0777) != PRIVATE_FILE_MODE) {
		_release_lock();
		return ERR_UNAUTHORIZED;
	}
	if (flock(lock_fd, LOCK_EX | LOCK_NB) != 0) {
		_release_lock();
		return ERR_ALREADY_IN_USE;
	}
	return OK;
#else
	return ERR_UNAVAILABLE;
#endif
}

Error BridgeRuntime::_remove_or_reject_stale_runtime() {
#ifdef UNIX_ENABLED
	if ((path_exists_no_follow(discovery_path) && validate_private_path(discovery_path, PRIVATE_FILE_MODE, false) != OK) ||
			(path_exists_no_follow(token_path) && validate_private_path(token_path, PRIVATE_FILE_MODE, false) != OK)) {
		return ERR_UNAUTHORIZED;
	}
	Dictionary discovery;
	if (read_discovery(discovery_path, discovery) == OK) {
		bool process_alive = false;
		int64_t pid = 0;
		if (get_bounded_integer_field(discovery, "pid", 1, INT32_MAX, pid)) {
			process_alive = is_process_alive(pid);
		}
		const bool authenticated = probe_authenticated_runtime(canonical_project_root);
		if (process_alive || authenticated) {
			return ERR_ALREADY_IN_USE;
		}
		String stale_endpoint;
		if (get_string_field(discovery, "endpoint", stale_endpoint) && validate_relative_endpoint(stale_endpoint)) {
			const String stale_endpoint_path = canonical_project_root.path_join(stale_endpoint);
			if (path_exists_no_follow(stale_endpoint_path) && validate_private_path(stale_endpoint_path, PRIVATE_FILE_MODE, false, true) != OK) {
				return ERR_UNAUTHORIZED;
			}
			remove_path_no_follow(stale_endpoint_path);
		}
	}
	remove_path_no_follow(discovery_path);
	remove_path_no_follow(token_path);
	return OK;
#else
	return ERR_UNAVAILABLE;
#endif
}

Error BridgeRuntime::_write_lock_metadata() {
#ifdef UNIX_ENABLED
	Dictionary metadata;
	metadata["editor_session_id"] = editor_session_id;
	metadata["pid"] = OS::get_singleton()->get_process_id();
	const CharString json = JSON::stringify(metadata, "", true).utf8();
	if (ftruncate(lock_fd, 0) != 0 || lseek(lock_fd, 0, SEEK_SET) < 0 || write_all(lock_fd, reinterpret_cast<const uint8_t *>(json.get_data()), json.length()) != OK || fsync(lock_fd) != 0) {
		return ERR_FILE_CANT_WRITE;
	}
	return sync_directory(codex_directory);
#else
	return ERR_UNAVAILABLE;
#endif
}

Error BridgeRuntime::_bind_server() {
#ifdef UNIX_ENABLED
	const CharString endpoint = endpoint_path.utf8();
	if (endpoint.length() >= (int)sizeof(sockaddr_un::sun_path)) {
		return ERR_INVALID_PARAMETER;
	}
	remove_path_no_follow(endpoint_path);
	server.instantiate();
	const Error error = server->listen(endpoint_path);
	if (error != OK) {
		server.unref();
		return error;
	}
	if (chmod(endpoint.get_data(), PRIVATE_FILE_MODE) != 0 || validate_private_path(endpoint_path, PRIVATE_FILE_MODE, false, true) != OK) {
		server->stop();
		server.unref();
		remove_path_no_follow(endpoint_path);
		return ERR_UNAUTHORIZED;
	}
	return OK;
#else
	return ERR_UNAVAILABLE;
#endif
}

Error BridgeRuntime::_publish_token() {
#ifdef UNIX_ENABLED
	const String temporary = token_path + ".tmp-" + editor_session_id.trim_prefix("editor:");
	const Error error = atomic_write_private_file(token_path, temporary, token.ptr(), token.size(), codex_directory);
	if (error == OK) {
		token_published = true;
	}
	return error;
#else
	return ERR_UNAVAILABLE;
#endif
}

Error BridgeRuntime::_publish_discovery() {
#ifdef UNIX_ENABLED
	Dictionary discovery;
	discovery["created_at"] = Time::get_singleton()->get_datetime_string_from_system(true, false) + "Z";
	discovery["discovery_schema"] = 1;
	discovery["editor_session_id"] = editor_session_id;
	discovery["endpoint"] = endpoint_relative_path;
	discovery["pid"] = OS::get_singleton()->get_process_id();
	discovery["project_id"] = project_id;
	Array versions;
	versions.push_back("1.0");
	discovery["protocol_versions"] = versions;
	discovery["token_file"] = ".godot/codex/session.token";
	discovery["transport"] = "uds";
	const CharString json = JSON::stringify(discovery, "", true).utf8();
	const String temporary = discovery_path + ".tmp-" + editor_session_id.trim_prefix("editor:");
	const Error error = atomic_write_private_file(discovery_path, temporary, reinterpret_cast<const uint8_t *>(json.get_data()), json.length(), codex_directory);
	if (error == OK) {
		discovery_published = true;
	}
	return error;
#else
	return ERR_UNAVAILABLE;
#endif
}

void BridgeRuntime::_release_lock() {
#ifdef UNIX_ENABLED
	if (lock_fd >= 0) {
		flock(lock_fd, LOCK_UN);
		close(lock_fd);
		lock_fd = -1;
	}
#endif
}

Error BridgeRuntime::initialize(const String &p_project_root) {
	ERR_FAIL_COND_V(server.is_valid() || lock_fd >= 0, ERR_ALREADY_IN_USE);
#ifdef UNIX_ENABLED
	Error error = canonicalize_project_root(p_project_root, canonical_project_root);
	if (error != OK) {
		return error;
	}
	error = BridgeCrypto::project_id_from_canonical_root(canonical_project_root, project_id);
	if (error != OK) {
		return error;
	}
	PackedByteArray session_bytes;
	if (BridgeCrypto::random_bytes(16, session_bytes) != OK || BridgeCrypto::random_bytes(BridgeCrypto::RANDOM_VALUE_BYTES, token) != OK) {
		return FAILED;
	}
	const String session_hex = BridgeCrypto::bytes_to_lower_hex(session_bytes);
	editor_session_id = "editor:" + session_hex;
	codex_directory = canonical_project_root.path_join(".godot/codex");
	run_directory = codex_directory.path_join("run");
	discovery_path = codex_directory.path_join("bridge.json");
	token_path = codex_directory.path_join("session.token");
	lock_path = codex_directory.path_join("bridge.lock");
	endpoint_relative_path = ".godot/codex/run/bridge-" + session_hex + ".sock";
	endpoint_path = canonical_project_root.path_join(endpoint_relative_path);

	error = ensure_project_data_directory(canonical_project_root.path_join(".godot"));
	if (error == OK) {
		error = ensure_private_directory(codex_directory);
	}
	if (error == OK) {
		error = ensure_private_directory(run_directory);
	}
	if (error == OK) {
		error = _acquire_lock();
	}
	if (error == OK) {
		error = _remove_or_reject_stale_runtime();
	}
	if (error == OK) {
		error = _write_lock_metadata();
	}
	if (error == OK) {
		error = _bind_server();
	}
	if (error == OK) {
		error = _publish_token();
	}
	if (error == OK) {
		error = _publish_discovery();
	}
	if (error != OK) {
		if (error == ERR_ALREADY_IN_USE && server.is_null() && !discovery_published && !token_published) {
			_release_lock();
		} else {
			cleanup();
		}
	}
	return error;
#else
	return ERR_UNAVAILABLE;
#endif
}

void BridgeRuntime::cleanup() {
#ifdef UNIX_ENABLED
	if (server.is_valid()) {
		server->stop();
		server.unref();
	}
	if (!endpoint_path.is_empty()) {
		remove_path_no_follow(endpoint_path);
	}
	if (discovery_published) {
		Dictionary discovery;
		String published_session;
		if (read_discovery(discovery_path, discovery) == OK && get_string_field(discovery, "editor_session_id", published_session) && published_session == editor_session_id) {
			remove_path_no_follow(discovery_path);
		}
	}
	if (token_published && lock_fd >= 0) {
		remove_path_no_follow(token_path);
	}
	discovery_published = false;
	token_published = false;
	if (lock_fd >= 0) {
		remove_path_no_follow(lock_path);
		sync_directory(codex_directory);
	}
	_release_lock();
#endif
}

bool BridgeRuntime::is_listening() const {
	return server.is_valid() && server->is_listening();
}

bool BridgeRuntime::is_connection_available() const {
	return server.is_valid() && server->is_connection_available();
}

Ref<StreamPeerUDS> BridgeRuntime::take_connection() {
	return server.is_valid() ? server->take_connection() : Ref<StreamPeerUDS>();
}

const String &BridgeRuntime::get_canonical_project_root() const {
	return canonical_project_root;
}

const String &BridgeRuntime::get_project_id() const {
	return project_id;
}

const String &BridgeRuntime::get_editor_session_id() const {
	return editor_session_id;
}

const String &BridgeRuntime::get_endpoint_path() const {
	return endpoint_path;
}

const PackedByteArray &BridgeRuntime::get_token() const {
	return token;
}

BridgeRuntime::~BridgeRuntime() {
	cleanup();
}
