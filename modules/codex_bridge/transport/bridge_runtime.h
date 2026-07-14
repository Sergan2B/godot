/**************************************************************************/
/*  bridge_runtime.h                                                      */
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

#include "core/io/stream_peer_uds.h"
#include "core/io/uds_server.h"
#include "core/variant/variant.h"

class BridgeRuntime {
	String canonical_project_root;
	String codex_directory;
	String run_directory;
	String discovery_path;
	String token_path;
	String lock_path;
	String endpoint_path;
	String endpoint_relative_path;
	String project_id;
	String editor_session_id;
	PackedByteArray token;
	Ref<UDSServer> server;
	int lock_fd = -1;
	bool discovery_published = false;
	bool token_published = false;

	Error _acquire_lock();
	Error _remove_or_reject_stale_runtime();
	Error _write_lock_metadata();
	Error _bind_server();
	Error _publish_token();
	Error _publish_discovery();
	void _release_lock();

public:
	static constexpr uint32_t DIRECTORY_MODE = 0700;
	static constexpr uint32_t PRIVATE_FILE_MODE = 0600;

	static Error canonicalize_project_root(const String &p_project_root, String &r_canonical_root);
	static Error validate_private_path(const String &p_path, uint32_t p_mode, bool p_directory, bool p_socket = false);
	static bool probe_authenticated_runtime(const String &p_project_root);

	Error initialize(const String &p_project_root);
	void cleanup();

	bool is_listening() const;
	bool is_connection_available() const;
	Ref<StreamPeerUDS> take_connection();

	const String &get_canonical_project_root() const;
	const String &get_project_id() const;
	const String &get_editor_session_id() const;
	const String &get_endpoint_path() const;
	const PackedByteArray &get_token() const;

	~BridgeRuntime();
};
