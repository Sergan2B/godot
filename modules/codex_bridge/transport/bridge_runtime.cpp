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

#ifdef WINDOWS_ENABLED
#include <aclapi.h>
#include <windows.h>

#include <climits>
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
	bool has_supported_version = false;
	for (int index = 0; index < versions.size(); index++) {
		if (versions[index].get_type() == Variant::STRING && (String(versions[index]) == "1.0" || String(versions[index]) == "1.1")) {
			has_supported_version = true;
		}
	}
	return has_supported_version;
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

#ifdef WINDOWS_ENABLED

static constexpr uint64_t PROBE_TIMEOUT_USEC = 500000;

static Char16String path_utf16(const String &p_path) {
	return p_path.replace_char('/', '\\').utf16();
}

static bool path_exists_no_follow(const String &p_path) {
	const Char16String path = path_utf16(p_path);
	return GetFileAttributesW(reinterpret_cast<LPCWSTR>(path.get_data())) != INVALID_FILE_ATTRIBUTES;
}

static Error remove_path_no_follow(const String &p_path) {
	const Char16String path = path_utf16(p_path);
	const DWORD attributes = GetFileAttributesW(reinterpret_cast<LPCWSTR>(path.get_data()));
	if (attributes == INVALID_FILE_ATTRIBUTES) {
		return GetLastError() == ERROR_FILE_NOT_FOUND || GetLastError() == ERROR_PATH_NOT_FOUND ? OK : FAILED;
	}
	if ((attributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) != 0) {
		return ERR_UNAUTHORIZED;
	}
	return DeleteFileW(reinterpret_cast<LPCWSTR>(path.get_data())) ? OK : FAILED;
}

static bool current_user_sid(Vector<uint8_t> &r_sid) {
	HANDLE token = nullptr;
	if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token)) {
		return false;
	}
	DWORD size = 0;
	GetTokenInformation(token, TokenUser, nullptr, 0, &size);
	Vector<uint8_t> buffer;
	buffer.resize(size);
	const bool read = size > 0 && GetTokenInformation(token, TokenUser, buffer.ptrw(), size, &size);
	CloseHandle(token);
	if (!read) {
		return false;
	}
	const PSID sid = reinterpret_cast<TOKEN_USER *>(buffer.ptrw())->User.Sid;
	const DWORD sid_size = GetLengthSid(sid);
	r_sid.resize(sid_size);
	return CopySid(sid_size, r_sid.ptrw(), sid);
}

static bool well_known_sid(WELL_KNOWN_SID_TYPE p_type, Vector<uint8_t> &r_sid) {
	DWORD size = SECURITY_MAX_SID_SIZE;
	r_sid.resize(size);
	if (!CreateWellKnownSid(p_type, nullptr, r_sid.ptrw(), &size)) {
		return false;
	}
	r_sid.resize(size);
	return true;
}

static Error set_private_acl(const String &p_path, bool p_directory) {
	Vector<uint8_t> user_sid;
	Vector<uint8_t> system_sid;
	Vector<uint8_t> administrators_sid;
	if (!current_user_sid(user_sid) || !well_known_sid(WinLocalSystemSid, system_sid) || !well_known_sid(WinBuiltinAdministratorsSid, administrators_sid)) {
		return ERR_UNAUTHORIZED;
	}
	EXPLICIT_ACCESSW entries[3] = {};
	PSID sids[3] = { user_sid.ptrw(), system_sid.ptrw(), administrators_sid.ptrw() };
	for (int index = 0; index < 3; index++) {
		entries[index].grfAccessPermissions = GENERIC_ALL;
		entries[index].grfAccessMode = SET_ACCESS;
		entries[index].grfInheritance = p_directory ? SUB_CONTAINERS_AND_OBJECTS_INHERIT : NO_INHERITANCE;
		BuildTrusteeWithSidW(&entries[index].Trustee, sids[index]);
	}
	PACL acl = nullptr;
	if (SetEntriesInAclW(3, entries, nullptr, &acl) != ERROR_SUCCESS) {
		return ERR_UNAUTHORIZED;
	}
	Char16String path = path_utf16(p_path);
	const DWORD result = SetNamedSecurityInfoW(reinterpret_cast<LPWSTR>(path.ptrw()), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION, nullptr, nullptr, acl, nullptr);
	LocalFree(acl);
	return result == ERROR_SUCCESS ? OK : ERR_UNAUTHORIZED;
}

static bool validate_private_acl(const String &p_path, bool p_require_protected) {
	Vector<uint8_t> user_sid;
	Vector<uint8_t> system_sid;
	Vector<uint8_t> administrators_sid;
	if (!current_user_sid(user_sid) || !well_known_sid(WinLocalSystemSid, system_sid) || !well_known_sid(WinBuiltinAdministratorsSid, administrators_sid)) {
		return false;
	}
	PSID owner = nullptr;
	PACL acl = nullptr;
	PSECURITY_DESCRIPTOR descriptor = nullptr;
	Char16String path = path_utf16(p_path);
	const DWORD result = GetNamedSecurityInfoW(reinterpret_cast<LPWSTR>(path.ptrw()), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION, &owner, nullptr, &acl, nullptr, &descriptor);
	if (result != ERROR_SUCCESS || !owner || !acl || !EqualSid(owner, user_sid.ptrw())) {
		if (descriptor) {
			LocalFree(descriptor);
		}
		return false;
	}
	SECURITY_DESCRIPTOR_CONTROL control = 0;
	DWORD revision = 0;
	if (!GetSecurityDescriptorControl(descriptor, &control, &revision) || (p_require_protected && (control & SE_DACL_PROTECTED) == 0)) {
		LocalFree(descriptor);
		return false;
	}
	bool user_allowed = false;
	bool valid = true;
	for (DWORD index = 0; index < acl->AceCount; index++) {
		void *raw_ace = nullptr;
		if (!GetAce(acl, index, &raw_ace)) {
			valid = false;
			break;
		}
		const ACE_HEADER *header = static_cast<const ACE_HEADER *>(raw_ace);
		if (header->AceType != ACCESS_ALLOWED_ACE_TYPE) {
			valid = false;
			break;
		}
		const ACCESS_ALLOWED_ACE *ace = static_cast<const ACCESS_ALLOWED_ACE *>(raw_ace);
		PSID sid = const_cast<DWORD *>(&ace->SidStart);
		const bool is_user = EqualSid(sid, user_sid.ptrw());
		if (!is_user && !EqualSid(sid, system_sid.ptrw()) && !EqualSid(sid, administrators_sid.ptrw())) {
			valid = false;
			break;
		}
		user_allowed = user_allowed || is_user;
	}
	LocalFree(descriptor);
	return valid && user_allowed;
}

static Error ensure_private_directory(const String &p_directory) {
	const Char16String path = path_utf16(p_directory);
	DWORD attributes = GetFileAttributesW(reinterpret_cast<LPCWSTR>(path.get_data()));
	if (attributes == INVALID_FILE_ATTRIBUTES) {
		if (!CreateDirectoryW(reinterpret_cast<LPCWSTR>(path.get_data()), nullptr) || set_private_acl(p_directory, true) != OK) {
			return ERR_CANT_CREATE;
		}
		attributes = GetFileAttributesW(reinterpret_cast<LPCWSTR>(path.get_data()));
	}
	if ((attributes & FILE_ATTRIBUTE_DIRECTORY) == 0 || (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0) {
		return ERR_INVALID_DATA;
	}
	return BridgeRuntime::validate_private_path(p_directory, BridgeRuntime::DIRECTORY_MODE, true);
}

static Error ensure_project_data_directory(const String &p_directory) {
	const Char16String path = path_utf16(p_directory);
	DWORD attributes = GetFileAttributesW(reinterpret_cast<LPCWSTR>(path.get_data()));
	if (attributes == INVALID_FILE_ATTRIBUTES) {
		if (!CreateDirectoryW(reinterpret_cast<LPCWSTR>(path.get_data()), nullptr)) {
			return ERR_CANT_CREATE;
		}
		attributes = GetFileAttributesW(reinterpret_cast<LPCWSTR>(path.get_data()));
	}
	return (attributes & FILE_ATTRIBUTE_DIRECTORY) != 0 && (attributes & FILE_ATTRIBUTE_REPARSE_POINT) == 0 ? OK : ERR_INVALID_DATA;
}

static Error write_handle_all(HANDLE p_handle, const uint8_t *p_bytes, size_t p_size) {
	size_t written = 0;
	while (written < p_size) {
		const DWORD requested = static_cast<DWORD>(MIN<size_t>(p_size - written, UINT32_MAX));
		DWORD chunk = 0;
		if (!WriteFile(p_handle, p_bytes + written, requested, &chunk, nullptr) || chunk == 0) {
			return ERR_FILE_CANT_WRITE;
		}
		written += chunk;
	}
	return OK;
}

static Error atomic_write_private_file(const String &p_target, const String &p_temporary, const uint8_t *p_bytes, size_t p_size, const String &p_parent_directory) {
	remove_path_no_follow(p_temporary);
	const Char16String temporary = path_utf16(p_temporary);
	HANDLE file = CreateFileW(reinterpret_cast<LPCWSTR>(temporary.get_data()), GENERIC_WRITE, 0, nullptr, CREATE_NEW, FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT, nullptr);
	if (file == INVALID_HANDLE_VALUE) {
		return ERR_CANT_CREATE;
	}
	Error error = set_private_acl(p_temporary, false);
	if (error == OK) {
		error = write_handle_all(file, p_bytes, p_size);
	}
	if (error == OK && !FlushFileBuffers(file)) {
		error = ERR_FILE_CANT_WRITE;
	}
	CloseHandle(file);
	if (error != OK) {
		remove_path_no_follow(p_temporary);
		return error;
	}
	const Char16String target = path_utf16(p_target);
	if (!MoveFileExW(reinterpret_cast<LPCWSTR>(temporary.get_data()), reinterpret_cast<LPCWSTR>(target.get_data()), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)) {
		remove_path_no_follow(p_temporary);
		return ERR_CANT_CREATE;
	}
	error = BridgeRuntime::validate_private_path(p_target, BridgeRuntime::PRIVATE_FILE_MODE, false);
	if (error != OK) {
		remove_path_no_follow(p_target);
	}
	return error;
}

static Error read_private_file(const String &p_path, int p_expected_size, PackedByteArray &r_bytes) {
	Error error = BridgeRuntime::validate_private_path(p_path, BridgeRuntime::PRIVATE_FILE_MODE, false);
	if (error != OK) {
		return error;
	}
	const Char16String path = path_utf16(p_path);
	HANDLE file = CreateFileW(reinterpret_cast<LPCWSTR>(path.get_data()), GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT, nullptr);
	if (file == INVALID_HANDLE_VALUE) {
		return ERR_CANT_OPEN;
	}
	LARGE_INTEGER size = {};
	if (!GetFileSizeEx(file, &size) || size.QuadPart < 0 || size.QuadPart > BridgeFrameCodec::MAX_PAYLOAD_BYTES || (p_expected_size >= 0 && size.QuadPart != p_expected_size)) {
		CloseHandle(file);
		return ERR_INVALID_DATA;
	}
	r_bytes.resize(static_cast<int>(size.QuadPart));
	int read = 0;
	while (read < r_bytes.size()) {
		DWORD chunk = 0;
		if (!ReadFile(file, r_bytes.ptrw() + read, r_bytes.size() - read, &chunk, nullptr) || chunk == 0) {
			CloseHandle(file);
			r_bytes.clear();
			return ERR_FILE_CANT_READ;
		}
		read += chunk;
	}
	CloseHandle(file);
	return OK;
}

static bool is_process_alive(int64_t p_pid) {
	if (p_pid <= 0 || p_pid > INT32_MAX) {
		return false;
	}
	HANDLE process = OpenProcess(SYNCHRONIZE, FALSE, static_cast<DWORD>(p_pid));
	if (!process) {
		return GetLastError() == ERROR_ACCESS_DENIED;
	}
	const bool alive = WaitForSingleObject(process, 0) == WAIT_TIMEOUT;
	CloseHandle(process);
	return alive;
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

static bool parse_loopback_endpoint(const String &p_endpoint, int &r_port) {
	static const String prefix = "127.0.0.1:";
	if (!p_endpoint.begins_with(prefix)) {
		return false;
	}
	const String port = p_endpoint.trim_prefix(prefix);
	if (!port.is_valid_int()) {
		return false;
	}
	r_port = port.to_int();
	return r_port > 0 && r_port <= 65535 && p_endpoint == prefix + itos(r_port);
}

static bool validate_relative_endpoint(const String &p_endpoint) {
	int port = 0;
	return parse_loopback_endpoint(p_endpoint, port);
}

static bool wait_for_connected(const Ref<BridgeStreamPeer> &p_peer, uint64_t p_deadline) {
	while (OS::get_singleton()->get_ticks_usec() < p_deadline) {
		if (p_peer->poll() != OK) {
			return false;
		}
		if (p_peer->get_status() == BridgeStreamPeer::STATUS_CONNECTED) {
			return true;
		}
		if (p_peer->get_status() == BridgeStreamPeer::STATUS_ERROR || p_peer->get_status() == BridgeStreamPeer::STATUS_NONE) {
			return false;
		}
		OS::get_singleton()->delay_usec(1000);
	}
	return false;
}

static bool send_frame(const Ref<BridgeStreamPeer> &p_peer, const PackedByteArray &p_frame, uint64_t p_deadline) {
	int offset = 0;
	while (offset < p_frame.size() && OS::get_singleton()->get_ticks_usec() < p_deadline) {
		int sent = 0;
		if (p_peer->put_partial_data(p_frame.ptr() + offset, p_frame.size() - offset, sent) != OK) {
			return false;
		}
		offset += sent;
		if (sent == 0) {
			OS::get_singleton()->delay_usec(1000);
		}
	}
	return offset == p_frame.size();
}

static bool receive_object(const Ref<BridgeStreamPeer> &p_peer, uint64_t p_deadline, Dictionary &r_object) {
	BridgeFrameCodec codec;
	while (OS::get_singleton()->get_ticks_usec() < p_deadline) {
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
	if (number < static_cast<double>(p_minimum) || number > static_cast<double>(p_maximum)) {
		return false;
	}
	r_value = static_cast<int64_t>(number);
	return static_cast<double>(r_value) == number;
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
			!get_string_field(p_discovery, "transport", transport) || transport != "tcp_loopback" ||
			!get_string_field(p_discovery, "endpoint", endpoint) || !validate_relative_endpoint(endpoint) ||
			!get_string_field(p_discovery, "token_file", token_file) || token_file != ".godot/codex/session.token" ||
			!get_string_field(p_discovery, "project_id", project_id) || project_id != p_expected_project_id || !project_id.begins_with("project:sha256:") || !is_lower_hex(project_id.trim_prefix("project:sha256:"), 64) ||
			!get_string_field(p_discovery, "editor_session_id", editor_session_id) || !editor_session_id.begins_with("editor:") || !is_lower_hex(editor_session_id.trim_prefix("editor:"), 32) ||
			!get_string_field(p_discovery, "created_at", created_at) || created_at.is_empty() || created_at.length() > 64 ||
			!p_discovery.has("protocol_versions") || p_discovery["protocol_versions"].get_type() != Variant::ARRAY) {
		return false;
	}
	const Array versions = p_discovery["protocol_versions"];
	for (int index = 0; index < versions.size(); index++) {
		if (versions[index].get_type() == Variant::STRING && (String(versions[index]) == "1.0" || String(versions[index]) == "1.1")) {
			return true;
		}
	}
	return false;
}

static bool probe_authenticated_endpoint(const String &p_project_root, const Dictionary &p_discovery) {
	String endpoint;
	String token_relative;
	String project_id;
	String editor_session_id;
	int port = 0;
	if (!get_string_field(p_discovery, "endpoint", endpoint) || !parse_loopback_endpoint(endpoint, port) ||
			!get_string_field(p_discovery, "token_file", token_relative) || token_relative != ".godot/codex/session.token" ||
			!get_string_field(p_discovery, "project_id", project_id) || !get_string_field(p_discovery, "editor_session_id", editor_session_id)) {
		return false;
	}
	PackedByteArray token;
	if (read_private_file(p_project_root.path_join(token_relative), BridgeCrypto::RANDOM_VALUE_BYTES, token) != OK) {
		return false;
	}
	Ref<BridgeStreamPeer> peer;
	peer.instantiate();
	if (peer->connect_to_host(IPAddress("127.0.0.1"), port) != OK) {
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
	String selected_version;
	if (!receive_object(peer, deadline, challenge) || !get_string_field(challenge, "kind", kind) || kind != "handshake.server_challenge" ||
			!get_string_field(challenge, "selected_protocol_version", selected_version) || selected_version != "1.0" ||
			!get_string_field(challenge, "server_nonce", server_nonce_encoded) || !get_string_field(challenge, "server_proof", server_proof_encoded)) {
		return false;
	}
	PackedByteArray server_nonce;
	PackedByteArray received_server_proof;
	PackedStringArray offered;
	offered.push_back("1.0");
	PackedByteArray transcript;
	PackedByteArray expected_server_proof;
	if (BridgeCrypto::base64url_decode_32(server_nonce_encoded, server_nonce) != OK || BridgeCrypto::base64url_decode_32(server_proof_encoded, received_server_proof) != OK ||
			BridgeCrypto::build_handshake_transcript("1.0", offered, "1.0", project_id, editor_session_id, client_nonce, server_nonce, transcript) != OK ||
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
	return receive_object(peer, deadline, ready) && get_string_field(ready, "kind", kind) && kind == "handshake.server_ready";
}

static Error read_discovery(const String &p_path, Dictionary &r_discovery) {
	PackedByteArray bytes;
	const Error error = read_private_file(p_path, -1, bytes);
	return error == OK ? BridgeJson::parse_strict_object(bytes, r_discovery) : error;
}

#endif // WINDOWS_ENABLED

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
#elif defined(WINDOWS_ENABLED)
	if (p_project_root.is_empty()) {
		return ERR_INVALID_PARAMETER;
	}
	const Char16String source = path_utf16(p_project_root);
	HANDLE directory = CreateFileW(reinterpret_cast<LPCWSTR>(source.get_data()), 0, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, nullptr);
	if (directory == INVALID_HANDLE_VALUE) {
		return ERR_FILE_NOT_FOUND;
	}
	BY_HANDLE_FILE_INFORMATION information = {};
	if (!GetFileInformationByHandle(directory, &information) || (information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) == 0 || (information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0) {
		CloseHandle(directory);
		return ERR_INVALID_DATA;
	}
	const DWORD length = GetFinalPathNameByHandleW(directory, nullptr, 0, FILE_NAME_NORMALIZED | VOLUME_NAME_DOS);
	if (length == 0) {
		CloseHandle(directory);
		return ERR_CANT_RESOLVE;
	}
	Char16String resolved;
	resolved.resize_uninitialized(length);
	if (GetFinalPathNameByHandleW(directory, reinterpret_cast<LPWSTR>(resolved.ptrw()), length, FILE_NAME_NORMALIZED | VOLUME_NAME_DOS) == 0) {
		CloseHandle(directory);
		return ERR_CANT_RESOLVE;
	}
	CloseHandle(directory);
	String canonical = String::utf16(resolved.ptr()).replace_char('\\', '/');
	if (canonical.begins_with("//?/UNC/")) {
		canonical = "//" + canonical.trim_prefix("//?/UNC/");
	} else {
		canonical = canonical.trim_prefix("//?/");
	}
	canonical = canonical.trim_suffix("/");
	const String project_file = canonical.path_join("project.godot");
	const Char16String project_file_path = path_utf16(project_file);
	const DWORD project_attributes = GetFileAttributesW(reinterpret_cast<LPCWSTR>(project_file_path.get_data()));
	if (project_attributes == INVALID_FILE_ATTRIBUTES || (project_attributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) != 0) {
		return ERR_FILE_NOT_FOUND;
	}
	r_canonical_root = canonical;
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
#elif defined(WINDOWS_ENABLED)
	(void)p_mode;
	if (p_socket) {
		return ERR_INVALID_PARAMETER;
	}
	const Char16String path = path_utf16(p_path);
	const DWORD attributes = GetFileAttributesW(reinterpret_cast<LPCWSTR>(path.get_data()));
	if (attributes == INVALID_FILE_ATTRIBUTES || (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0 || !validate_private_acl(p_path, p_directory)) {
		return ERR_UNAUTHORIZED;
	}
	if (p_directory != ((attributes & FILE_ATTRIBUTE_DIRECTORY) != 0)) {
		return ERR_INVALID_DATA;
	}
	return OK;
#else
	return ERR_UNAVAILABLE;
#endif
}

bool BridgeRuntime::probe_authenticated_runtime(const String &p_project_root) {
#if defined(UNIX_ENABLED) || defined(WINDOWS_ENABLED)
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
#elif defined(WINDOWS_ENABLED)
	const bool existed = path_exists_no_follow(lock_path);
	if (existed && validate_private_path(lock_path, PRIVATE_FILE_MODE, false) != OK) {
		return ERR_UNAUTHORIZED;
	}
	const Char16String path = path_utf16(lock_path);
	HANDLE handle = CreateFileW(reinterpret_cast<LPCWSTR>(path.get_data()), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_DELETE, nullptr, OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT, nullptr);
	if (handle == INVALID_HANDLE_VALUE) {
		return GetLastError() == ERROR_SHARING_VIOLATION ? ERR_ALREADY_IN_USE : ERR_CANT_OPEN;
	}
	lock_handle = handle;
	if (!existed && set_private_acl(lock_path, false) != OK) {
		_release_lock();
		return ERR_UNAUTHORIZED;
	}
	if (validate_private_path(lock_path, PRIVATE_FILE_MODE, false) != OK) {
		_release_lock();
		return ERR_UNAUTHORIZED;
	}
	return OK;
#else
	return ERR_UNAVAILABLE;
#endif
}

Error BridgeRuntime::_remove_or_reject_stale_runtime() {
#if defined(UNIX_ENABLED) || defined(WINDOWS_ENABLED)
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
#ifdef UNIX_ENABLED
		if (get_string_field(discovery, "endpoint", stale_endpoint) && validate_relative_endpoint(stale_endpoint)) {
			const String stale_endpoint_path = canonical_project_root.path_join(stale_endpoint);
			if (path_exists_no_follow(stale_endpoint_path) && validate_private_path(stale_endpoint_path, PRIVATE_FILE_MODE, false, true) != OK) {
				return ERR_UNAUTHORIZED;
			}
			remove_path_no_follow(stale_endpoint_path);
		}
#endif
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
#elif defined(WINDOWS_ENABLED)
	ERR_FAIL_NULL_V(lock_handle, ERR_UNCONFIGURED);
	Dictionary metadata;
	metadata["editor_session_id"] = editor_session_id;
	metadata["pid"] = OS::get_singleton()->get_process_id();
	const CharString json = JSON::stringify(metadata, "", true).utf8();
	HANDLE handle = static_cast<HANDLE>(lock_handle);
	LARGE_INTEGER start = {};
	if (!SetFilePointerEx(handle, start, nullptr, FILE_BEGIN) || !SetEndOfFile(handle) || write_handle_all(handle, reinterpret_cast<const uint8_t *>(json.get_data()), json.length()) != OK || !FlushFileBuffers(handle)) {
		return ERR_FILE_CANT_WRITE;
	}
	return OK;
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
#elif defined(WINDOWS_ENABLED)
	server.instantiate();
	const Error error = server->listen(0, IPAddress("127.0.0.1"));
	if (error != OK) {
		server.unref();
		return error;
	}
	const int port = server->get_local_port();
	if (port <= 0 || port > 65535) {
		server->stop();
		server.unref();
		return ERR_CANT_CREATE;
	}
	endpoint_relative_path = "127.0.0.1:" + itos(port);
	endpoint_path = endpoint_relative_path;
	return OK;
#else
	return ERR_UNAVAILABLE;
#endif
}

Error BridgeRuntime::_publish_token() {
#if defined(UNIX_ENABLED) || defined(WINDOWS_ENABLED)
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
#if defined(UNIX_ENABLED) || defined(WINDOWS_ENABLED)
	Dictionary discovery;
	discovery["created_at"] = Time::get_singleton()->get_datetime_string_from_system(true, false) + "Z";
	discovery["discovery_schema"] = 1;
	discovery["editor_session_id"] = editor_session_id;
	discovery["endpoint"] = endpoint_relative_path;
	discovery["pid"] = OS::get_singleton()->get_process_id();
	discovery["project_id"] = project_id;
	Array versions;
	versions.push_back("1.1");
	versions.push_back("1.0");
	discovery["protocol_versions"] = versions;
	discovery["token_file"] = ".godot/codex/session.token";
#ifdef WINDOWS_ENABLED
	discovery["transport"] = "tcp_loopback";
#else
	discovery["transport"] = "uds";
#endif
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
#elif defined(WINDOWS_ENABLED)
	if (lock_handle) {
		CloseHandle(static_cast<HANDLE>(lock_handle));
		lock_handle = nullptr;
	}
#endif
}

Error BridgeRuntime::initialize(const String &p_project_root) {
	bool has_lock = lock_fd >= 0;
#ifdef WINDOWS_ENABLED
	has_lock = has_lock || lock_handle;
#endif
	ERR_FAIL_COND_V(server.is_valid() || has_lock, ERR_ALREADY_IN_USE);
#if defined(UNIX_ENABLED) || defined(WINDOWS_ENABLED)
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
#ifdef UNIX_ENABLED
	endpoint_relative_path = ".godot/codex/run/bridge-" + session_hex + ".sock";
	endpoint_path = canonical_project_root.path_join(endpoint_relative_path);
#endif

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
#if defined(UNIX_ENABLED) || defined(WINDOWS_ENABLED)
	if (server.is_valid()) {
		server->stop();
		server.unref();
	}
#ifdef UNIX_ENABLED
	if (!endpoint_path.is_empty()) {
		remove_path_no_follow(endpoint_path);
	}
#endif
	if (discovery_published) {
		Dictionary discovery;
		String published_session;
		if (read_discovery(discovery_path, discovery) == OK && get_string_field(discovery, "editor_session_id", published_session) && published_session == editor_session_id) {
			remove_path_no_follow(discovery_path);
		}
	}
	if (token_published && (lock_fd >= 0
#ifdef WINDOWS_ENABLED
				|| lock_handle
#endif
				)) {
		remove_path_no_follow(token_path);
	}
	discovery_published = false;
	token_published = false;
	if (lock_fd >= 0
#ifdef WINDOWS_ENABLED
			|| lock_handle
#endif
			) {
		remove_path_no_follow(lock_path);
#ifdef UNIX_ENABLED
		sync_directory(codex_directory);
#endif
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

Ref<BridgeStreamPeer> BridgeRuntime::take_connection() {
	return server.is_valid() ? server->take_connection() : Ref<BridgeStreamPeer>();
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
