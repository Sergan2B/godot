/**************************************************************************/
/*  resource_graph_adapter.cpp                                            */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#include "resource_graph_adapter.h"

#include "bridge_revision_clock.h"

#include "core/crypto/crypto_core.h"
#include "core/io/file_access.h"
#include "core/io/json.h"
#include "core/io/resource_loader.h"
#include "core/io/resource_uid.h"
#include "core/os/os.h"
#include "core/string/print_string.h"
#include "editor/file_system/editor_file_system.h"
#include "modules/codex_bridge/protocol/bridge_crypto.h"

String ResourceGraphAdapter::_sha256_hex_utf8(const String &p_value) {
	const CharString bytes = p_value.utf8();
	PackedByteArray digest;
	digest.resize(32);
	if (CryptoCore::sha256(reinterpret_cast<const uint8_t *>(bytes.get_data()), bytes.length(), digest.ptrw()) != OK) {
		return String("0").repeat(64);
	}
	return BridgeCrypto::bytes_to_lower_hex(digest);
}

String ResourceGraphAdapter::_make_snapshot_id() {
	PackedByteArray random;
	if (BridgeCrypto::random_bytes(16, random) != OK) {
		return "snapshot:" + String("0").repeat(32);
	}
	return "snapshot:" + BridgeCrypto::bytes_to_lower_hex(random);
}

Dictionary ResourceGraphAdapter::_make_resource_ref(const String &p_path, int64_t p_uid) {
	Dictionary resource_ref;
	if (p_uid != ResourceUID::INVALID_ID) {
		resource_ref["uid"] = ResourceUID::get_singleton()->id_to_text(p_uid);
	} else {
		resource_ref["uid_missing"] = true;
		resource_ref["path"] = p_path;
	}
	return resource_ref;
}

String ResourceGraphAdapter::_facts_json(const CatalogRecord &p_record) {
	Dictionary value = p_record.value.duplicate(true);
	Dictionary resource = value["resource"];
	resource["resource_revision"] = (int64_t)0;
	value["resource"] = resource;
	Array dependencies = value["dependencies"];
	for (int index = 0; index < dependencies.size(); index++) {
		Dictionary dependency = dependencies[index];
		dependency["resource_revision"] = (int64_t)0;
		dependencies[index] = dependency;
	}
	value["dependencies"] = dependencies;
	Array diagnostics = p_record.diagnostics.duplicate(true);
	for (int index = 0; index < diagnostics.size(); index++) {
		Dictionary diagnostic = diagnostics[index];
		diagnostic["resource_revision"] = (int64_t)0;
		diagnostics[index] = diagnostic;
	}
	Dictionary facts;
	facts["value"] = value;
	facts["diagnostics"] = diagnostics;
	return JSON::stringify(facts, "", true, true);
}

void ResourceGraphAdapter::_stamp_record(CatalogRecord &r_record, uint64_t p_resource_revision) {
	Dictionary resource = r_record.value["resource"];
	resource["resource_revision"] = (int64_t)p_resource_revision;
	r_record.value["resource"] = resource;
	Array dependencies = r_record.value["dependencies"];
	for (int index = 0; index < dependencies.size(); index++) {
		Dictionary dependency = dependencies[index];
		dependency["resource_revision"] = (int64_t)p_resource_revision;
		dependencies[index] = dependency;
	}
	r_record.value["dependencies"] = dependencies;
	for (int index = 0; index < r_record.diagnostics.size(); index++) {
		Dictionary diagnostic = r_record.diagnostics[index];
		diagnostic["resource_revision"] = (int64_t)p_resource_revision;
		r_record.diagnostics[index] = diagnostic;
	}
}

void ResourceGraphAdapter::_collect_sorted_keys(const HashMap<String, CatalogRecord> &p_catalog, Vector<String> &r_keys) {
	r_keys.clear();
	r_keys.reserve(p_catalog.size());
	for (const KeyValue<String, CatalogRecord> &entry : p_catalog) {
		r_keys.push_back(entry.key);
	}
	r_keys.sort();
}

void ResourceGraphAdapter::initialize(BridgeRevisionClock *p_revision_clock) {
	revision_clock = p_revision_clock;
	journal.initialize(p_revision_clock ? p_revision_clock->get_resource_revision() : 1);
	catalog.clear();
	catalog_ready = false;
	catalog_limit_exceeded = false;
	refresh_phase = REFRESH_IDLE;
	// The module starts before EditorFileSystem's first scan. The authoritative
	// filesystem_changed signal requests the initial catalog once that scan has
	// installed its complete directory tree.
	refresh_requested = false;
	pending_reimport_paths.clear();
	snapshot_active = false;
	snapshot_waiting_for_terminal = false;
}

void ResourceGraphAdapter::shutdown() {
	revision_clock = nullptr;
	catalog.clear();
	catalog_ready = false;
	catalog_limit_exceeded = false;
	refresh_requested = false;
	refresh_phase = REFRESH_IDLE;
	directory_stack.clear();
	refresh_paths.clear();
	observed_catalog.clear();
	pending_reimport_paths.clear();
	active_reimport_paths.clear();
	snapshot_active = false;
	snapshot_waiting_for_terminal = false;
	snapshot_records.clear();
	snapshot_payloads.clear();
}

void ResourceGraphAdapter::request_refresh() {
	refresh_requested = true;
}

void ResourceGraphAdapter::mark_reimported(const Vector<String> &p_paths) {
	for (const String &path : p_paths) {
		if (path.begins_with("res://")) {
			pending_reimport_paths.insert(path);
		}
	}
	request_refresh();
}

bool ResourceGraphAdapter::_begin_refresh() {
	EditorFileSystem *filesystem = EditorFileSystem::get_singleton();
	if (!filesystem || filesystem->doing_first_scan() || filesystem->is_scanning() || filesystem->is_importing() || !filesystem->get_filesystem()) {
		return false;
	}
	refresh_requested = false;
	refresh_phase = REFRESH_COLLECT_PATHS;
	directory_stack.clear();
	DirectoryCursor root;
	root.directory = filesystem->get_filesystem();
	directory_stack.push_back(root);
	refresh_paths.clear();
	refresh_path_index = 0;
	observed_catalog.clear();
	active_reimport_paths = pending_reimport_paths;
	pending_reimport_paths.clear();
	observed_dependency_count = 0;
	refresh_limit_exceeded = false;
	return true;
}

bool ResourceGraphAdapter::_collect_one_path() {
	while (!directory_stack.is_empty()) {
		DirectoryCursor &cursor = directory_stack.write[directory_stack.size() - 1];
		if (cursor.file_index < cursor.directory->get_file_count()) {
			const int file_index = cursor.file_index++;
			const String path = cursor.directory->get_file_path(file_index);
			// EditorFileSystem also exposes project metadata and extensionless
			// auxiliary files. They are source files, but not ResourceLoader facts
			// and therefore do not belong in the resource graph.
			const String cached_type = cursor.directory->get_file_type(file_index);
			if (path == "res://project.godot" || (cached_type.is_empty() && ResourceLoader::get_resource_type(path).is_empty())) {
				return true;
			}
			if (path.utf8().length() > (int)MAX_PATH_BYTES || refresh_paths.size() >= (int)MAX_RESOURCES) {
				refresh_limit_exceeded = true;
				return false;
			}
			refresh_paths.push_back(path);
			return true;
		}
		if (cursor.subdirectory_index < cursor.directory->get_subdir_count()) {
			DirectoryCursor child;
			child.directory = cursor.directory->get_subdir(cursor.subdirectory_index++);
			directory_stack.push_back(child);
			continue;
		}
		directory_stack.resize(directory_stack.size() - 1);
	}
	refresh_paths.sort();
	refresh_phase = REFRESH_OBSERVE;
	return false;
}

bool ResourceGraphAdapter::_observe_resource(const String &p_path, CatalogRecord &r_record) {
	if (!p_path.begins_with("res://") || p_path.contains("/../") || p_path.utf8().length() > (int)MAX_PATH_BYTES) {
		return false;
	}
	EditorFileSystem *filesystem = EditorFileSystem::get_singleton();
	ERR_FAIL_NULL_V(filesystem, false);
	int file_index = -1;
	EditorFileSystemDirectory *directory = filesystem->find_file(p_path, &file_index);
	if (!directory || file_index < 0) {
		return false;
	}

	const ResourceUID::ID uid = directory->get_file_uid(file_index);
	const Dictionary resource_ref = _make_resource_ref(p_path, uid);
	const bool imported = FileAccess::exists(p_path + ".import");
	const bool import_valid = !imported || directory->get_file_import_is_valid(file_index);
	String godot_type = directory->get_file_type(file_index);
	if (godot_type.is_empty()) {
		godot_type = ResourceLoader::get_resource_type(p_path);
	}
	if (godot_type.is_empty()) {
		godot_type = "Resource";
	}

	Dictionary resource;
	resource["resource_ref"] = resource_ref;
	resource["path"] = p_path;
	resource["godot_type"] = godot_type;
	resource["source_kind"] = imported ? "imported_source" : "source";
	resource["import_state"] = imported ? (import_valid ? "valid" : "invalid") : "not_imported";
	resource["modified_time_unix_seconds"] = (int64_t)directory->get_file_modified_time(file_index);
	resource["byte_size"] = MAX((int64_t)0, FileAccess::get_size(p_path));
	resource["validity"] = import_valid ? "valid" : "invalid";
	resource["authority"] = "editor_file_system";
	resource["resource_revision"] = (int64_t)0;

	Array dependencies;
	Array diagnostics;
	List<String> raw_dependencies;
	ResourceLoader::get_dependencies(p_path, &raw_dependencies, true);
	if ((uint32_t)raw_dependencies.size() > MAX_DEPENDENCIES_PER_RESOURCE || observed_dependency_count + (uint64_t)raw_dependencies.size() > MAX_DEPENDENCIES) {
		return false;
	}
	for (const String &raw_dependency : raw_dependencies) {
		String target_uid;
		String declared_type;
		String fallback_path = raw_dependency;
		const PackedStringArray parts = raw_dependency.split("::", true, 2);
		if (parts.size() == 3) {
			target_uid = parts[0];
			declared_type = parts[1];
			fallback_path = parts[2];
		} else if (parts.size() == 2) {
			fallback_path = parts[0];
			declared_type = parts[1];
		}
		if (!fallback_path.begins_with("res://") || fallback_path.utf8().length() > (int)MAX_PATH_BYTES) {
			return false;
		}

		String resolved_path;
		String resolution = "missing";
		if (!target_uid.is_empty()) {
			const ResourceUID::ID target_id = ResourceUID::get_singleton()->text_to_id(target_uid);
			if (target_id != ResourceUID::INVALID_ID && ResourceUID::get_singleton()->has_id(target_id)) {
				const String candidate = ResourceUID::get_singleton()->get_id_path(target_id);
				if (FileAccess::exists(candidate) || ResourceLoader::exists(candidate)) {
					resolved_path = candidate;
					resolution = "resolved";
				} else {
					resolution = "stale_uid";
				}
			} else {
				resolution = "stale_uid";
			}
		} else if (FileAccess::exists(fallback_path) || ResourceLoader::exists(fallback_path)) {
			resolved_path = fallback_path;
			resolution = "resolved";
		}

		Dictionary dependency;
		dependency["source_ref"] = resource_ref;
		dependency["target_uid"] = target_uid.is_empty() ? Variant() : Variant(target_uid);
		dependency["fallback_path"] = fallback_path;
		dependency["resolved_path"] = resolved_path.is_empty() ? Variant() : Variant(resolved_path);
		dependency["declared_type"] = declared_type.is_empty() ? Variant() : Variant(declared_type);
		dependency["resolution"] = resolution;
		dependency["authority"] = "resource_loader_dependencies";
		dependency["resource_revision"] = (int64_t)0;
		dependencies.push_back(dependency);

		if (resolution != "resolved") {
			Dictionary diagnostic;
			diagnostic["code"] = resolution == "stale_uid" ? "stale_resource_uid" : "missing_dependency";
			diagnostic["subject"] = resource_ref;
			diagnostic["target_reference"] = (resolution == "stale_uid" ? target_uid : fallback_path).left(MAX_PATH_BYTES);
			diagnostic["resource_revision"] = (int64_t)0;
			diagnostics.push_back(diagnostic);
			if (resource["validity"] == "valid") {
				resource["validity"] = "partial";
			}
		}
	}
	observed_dependency_count += dependencies.size();

	if (!import_valid) {
		Dictionary diagnostic;
		diagnostic["code"] = "invalid_import";
		diagnostic["subject"] = resource_ref;
		diagnostic["resource_revision"] = (int64_t)0;
		diagnostics.push_back(diagnostic);
	}
	if (uid != ResourceUID::INVALID_ID && ResourceUID::get_singleton()->has_id(uid) && ResourceUID::get_singleton()->get_id_path(uid) != p_path) {
		Dictionary diagnostic;
		diagnostic["code"] = "resource_uid_path_mismatch";
		diagnostic["subject"] = resource_ref;
		diagnostic["target_reference"] = ResourceUID::get_singleton()->get_id_path(uid);
		diagnostic["resource_revision"] = (int64_t)0;
		diagnostics.push_back(diagnostic);
		resource["validity"] = "partial";
	}

	r_record.value["resource"] = resource;
	r_record.value["dependencies"] = dependencies;
	r_record.diagnostics = diagnostics;
	return true;
}

void ResourceGraphAdapter::_finish_refresh(RefreshOutcome &r_outcome) {
	refresh_phase = REFRESH_IDLE;
	directory_stack.clear();
	refresh_paths.clear();
	refresh_path_index = 0;

	if (refresh_limit_exceeded) {
		catalog_limit_exceeded = true;
		if (revision_clock) {
			const uint64_t previous = revision_clock->get_resource_revision();
			const uint64_t current = revision_clock->record_resource_change();
			journal.invalidate_to(current);
			r_outcome.invalidated = true;
			r_outcome.last_contiguous_resource_revision = previous;
			r_outcome.current_resource_revision = current;
			r_outcome.revisions = revision_clock->get_revision_vector();
		}
		observed_catalog.clear();
		return;
	}

	if (!catalog_ready) {
		const uint64_t initial_revision = revision_clock ? revision_clock->get_resource_revision() : 1;
		for (KeyValue<String, CatalogRecord> &entry : observed_catalog) {
			_stamp_record(entry.value, initial_revision);
		}
		catalog = observed_catalog;
		observed_catalog.clear();
		catalog_ready = true;
		catalog_limit_exceeded = false;
		journal.initialize(initial_revision);
		print_verbose(vformat("[codex_bridge] Resource catalog initialized with %d records.", catalog.size()));
		return;
	}

	Vector<String> observed_keys;
	Vector<String> existing_keys;
	_collect_sorted_keys(observed_catalog, observed_keys);
	_collect_sorted_keys(catalog, existing_keys);
	const uint64_t next_revision = revision_clock ? revision_clock->get_resource_revision() + 1 : journal.get_current_resource_revision() + 1;
	HashMap<String, CatalogRecord> next_catalog;
	HashSet<String> preexisting_keys;
	Array operations;
	for (const String &key : existing_keys) {
		preexisting_keys.insert(key);
	}

	for (const String &key : observed_keys) {
		CatalogRecord record = observed_catalog[key];
		const CatalogRecord *previous = catalog.getptr(key);
		const Dictionary resource = record.value["resource"];
		const String path = resource["path"];
		const bool reimported = active_reimport_paths.has(path);
		if (previous && !reimported && _facts_json(*previous) == _facts_json(record)) {
			next_catalog.insert(key, *previous);
			continue;
		}
		_stamp_record(record, next_revision);
		next_catalog.insert(key, record);
		Dictionary operation;
		if (previous) {
			const Dictionary previous_resource = previous->value["resource"];
			const Dictionary resource_ref = resource["resource_ref"];
			if (resource_ref.has("uid") && String(previous_resource["path"]) != path) {
				operation["kind"] = "move";
				operation["uid"] = resource_ref["uid"];
				operation["from_path"] = previous_resource["path"];
				operation["to_path"] = path;
				operation["value"] = record.value;
			} else {
				operation["kind"] = reimported ? "reimport" : "upsert";
				operation["value"] = record.value;
			}
		} else {
			operation["kind"] = reimported ? "reimport" : "upsert";
			operation["value"] = record.value;
		}
		operations.push_back(operation);
	}

	for (const String &key : existing_keys) {
		if (observed_catalog.has(key)) {
			continue;
		}
		const CatalogRecord &removed = catalog[key];
		const Dictionary resource = removed.value["resource"];
		Dictionary operation;
		operation["kind"] = "remove";
		operation["resource_ref"] = resource["resource_ref"];
		operation["path"] = resource["path"];
		operations.push_back(operation);
	}

	observed_catalog.clear();
	active_reimport_paths.clear();
	if (operations.is_empty()) {
		catalog = next_catalog;
		catalog_limit_exceeded = false;
		return;
	}

	const uint64_t previous_revision = revision_clock ? revision_clock->get_resource_revision() : journal.get_current_resource_revision();
	const uint64_t committed_revision = revision_clock ? revision_clock->record_resource_change() : next_revision;
	const Dictionary committed_revisions = revision_clock ? revision_clock->get_revision_vector() : Dictionary();
	const uint64_t project_revision = committed_revisions.has("project_revision") ? (uint64_t)(int64_t)committed_revisions["project_revision"] : committed_revision;
	Dictionary batch;
	bool journal_invalidated = false;
	if (journal.commit(committed_revision, project_revision, operations, preexisting_keys, batch, journal_invalidated) != OK) {
		journal.invalidate_to(committed_revision);
		journal_invalidated = true;
	}
	catalog = next_catalog;
	catalog_limit_exceeded = false;
	r_outcome.changed = true;
	r_outcome.invalidated = journal_invalidated;
	r_outcome.last_contiguous_resource_revision = previous_revision;
	r_outcome.current_resource_revision = committed_revision;
	r_outcome.revisions = committed_revisions;
}

bool ResourceGraphAdapter::process_refresh(uint64_t p_budget_usec, RefreshOutcome &r_outcome) {
	r_outcome = RefreshOutcome();
	// Keep the catalog generation frozen until the transport reports terminal
	// ACK, disconnect, cancellation, or timeout for the active snapshot.
	if (snapshot_active) {
		return false;
	}
	if (refresh_phase == REFRESH_IDLE && (!refresh_requested || !_begin_refresh())) {
		return false;
	}
	const uint64_t started = OS::get_singleton()->get_ticks_usec();
	bool did_work = false;
	do {
		did_work = true;
		if (refresh_phase == REFRESH_COLLECT_PATHS) {
			_collect_one_path();
			if (refresh_limit_exceeded) {
				_finish_refresh(r_outcome);
				return true;
			}
		} else if (refresh_phase == REFRESH_OBSERVE) {
			if (refresh_path_index >= refresh_paths.size()) {
				_finish_refresh(r_outcome);
				return true;
			}
			CatalogRecord record;
			const String path = refresh_paths[refresh_path_index++];
			if (!_observe_resource(path, record)) {
				refresh_limit_exceeded = true;
				_finish_refresh(r_outcome);
				return true;
			}
			const Dictionary resource = record.value["resource"];
			const String key = ResourceDeltaJournal::resource_ref_key(resource["resource_ref"]);
			if (key.is_empty() || observed_catalog.has(key)) {
				refresh_limit_exceeded = true;
				_finish_refresh(r_outcome);
				return true;
			}
			observed_catalog.insert(key, record);
		}
	} while (OS::get_singleton()->get_ticks_usec() - started < p_budget_usec);
	return did_work;
}

Error ResourceGraphAdapter::begin_snapshot(uint64_t p_request_id, uint64_t p_now_usec, Dictionary &r_error_data) {
	r_error_data.clear();
	if (snapshot_active) {
		r_error_data["active_snapshot_id"] = snapshot_id;
		return ERR_BUSY;
	}
	if (catalog_limit_exceeded) {
		return ERR_OUT_OF_MEMORY;
	}
	if (!catalog_ready) {
		return ERR_UNAVAILABLE;
	}
	snapshot_active = true;
	snapshot_waiting_for_terminal = false;
	snapshot_request_id = p_request_id;
	snapshot_started_usec = p_now_usec;
	snapshot_id = _make_snapshot_id();
	snapshot_resource_revision = revision_clock ? revision_clock->get_resource_revision() : journal.get_current_resource_revision();
	snapshot_revisions = revision_clock ? revision_clock->get_revision_vector() : Dictionary();
	snapshot_records.clear();
	Vector<String> keys;
	_collect_sorted_keys(catalog, keys);
	for (const String &key : keys) {
		snapshot_records.push_back(catalog[key]);
	}
	print_verbose(vformat("[codex_bridge] Resource snapshot %s froze %d records at revision %d.", snapshot_id, snapshot_records.size(), snapshot_resource_revision));
	snapshot_record_index = 0;
	snapshot_payloads.clear();
	snapshot_resources.clear();
	snapshot_dependencies.clear();
	snapshot_diagnostics.clear();
	return OK;
}

bool ResourceGraphAdapter::_flush_snapshot_payload() {
	Dictionary payload;
	// Arrays are reference-counted Variants. Store independent copies so clearing
	// the in-progress chunk below cannot also clear the queued payload.
	payload["resources"] = snapshot_resources.duplicate(true);
	payload["dependencies"] = snapshot_dependencies.duplicate(true);
	payload["diagnostics"] = snapshot_diagnostics.duplicate(true);
	const String payload_json = JSON::stringify(payload, "", true, true);
	if (payload_json.utf8().length() > (int)SNAPSHOT_CHUNK_BYTES) {
		return false;
	}
	snapshot_payloads.push_back(payload);
	snapshot_resources.clear();
	snapshot_dependencies.clear();
	snapshot_diagnostics.clear();
	return true;
}

void ResourceGraphAdapter::_fail_snapshot(const String &p_code, const String &p_message, bool p_retryable, SnapshotCompletion &r_completion) {
	r_completion.ready = true;
	r_completion.request_id = snapshot_request_id;
	r_completion.is_error = true;
	r_completion.error_code = p_code;
	r_completion.error_message = p_message;
	r_completion.error_retryable = p_retryable;
	snapshot_active = false;
	snapshot_waiting_for_terminal = false;
	snapshot_records.clear();
	snapshot_payloads.clear();
}

void ResourceGraphAdapter::_finish_snapshot(SnapshotCompletion &r_completion) {
	if ((!snapshot_resources.is_empty() || !snapshot_dependencies.is_empty() || !snapshot_diagnostics.is_empty() || snapshot_payloads.is_empty()) && !_flush_snapshot_payload()) {
		_fail_snapshot("resource_limit_exceeded", "A resource graph record exceeds the snapshot chunk limit.", false, r_completion);
		return;
	}

	Array messages;
	Dictionary begin_params;
	begin_params["snapshot_id"] = snapshot_id;
	begin_params["domain"] = "resource_graph";
	begin_params["resource_revision"] = (int64_t)snapshot_resource_revision;
	begin_params["revisions"] = snapshot_revisions;
	Dictionary begin;
	begin["protocol_version"] = "1.2";
	begin["kind"] = "notification";
	begin["method"] = "snapshot.begin";
	begin["params"] = begin_params;
	messages.push_back(begin);

	String checksum_input;
	int resource_count = 0;
	int dependency_count = 0;
	int diagnostic_count = 0;
	for (int index = 0; index < snapshot_payloads.size(); index++) {
		const Dictionary payload = snapshot_payloads[index];
		const String payload_json = JSON::stringify(payload, "", true, true);
		const String checksum = _sha256_hex_utf8(payload_json);
		checksum_input += checksum;
		resource_count += Array(payload["resources"]).size();
		dependency_count += Array(payload["dependencies"]).size();
		diagnostic_count += Array(payload["diagnostics"]).size();
		Dictionary chunk;
		chunk["protocol_version"] = "1.2";
		chunk["kind"] = "chunk";
		chunk["snapshot_id"] = snapshot_id;
		chunk["domain"] = "resource_graph";
		chunk["chunk_index"] = index;
		chunk["payload"] = payload;
		chunk["payload_json"] = payload_json;
		chunk["checksum"] = checksum;
		messages.push_back(chunk);
	}

	Dictionary end_params;
	end_params["snapshot_id"] = snapshot_id;
	end_params["domain"] = "resource_graph";
	end_params["resource_revision"] = (int64_t)snapshot_resource_revision;
	end_params["chunk_count"] = snapshot_payloads.size();
	end_params["resource_count"] = resource_count;
	end_params["dependency_count"] = dependency_count;
	end_params["diagnostic_count"] = diagnostic_count;
	end_params["checksum"] = _sha256_hex_utf8(checksum_input);
	end_params["revisions"] = snapshot_revisions;
	Dictionary end;
	end["protocol_version"] = "1.2";
	end["kind"] = "notification";
	end["method"] = "snapshot.end";
	end["params"] = end_params;
	messages.push_back(end);

	Dictionary limits;
	limits["resource_records"] = (int64_t)MAX_RESOURCES;
	limits["resource_dependencies"] = (int64_t)MAX_DEPENDENCIES;
	limits["resource_dependencies_per_record"] = (int64_t)MAX_DEPENDENCIES_PER_RESOURCE;
	limits["resource_path_bytes"] = (int64_t)MAX_PATH_BYTES;
	limits["snapshot_chunk_bytes"] = (int64_t)SNAPSHOT_CHUNK_BYTES;
	limits["snapshot_window_bytes"] = (int64_t)SNAPSHOT_WINDOW_BYTES;
	limits["snapshot_timeout_ms"] = (int64_t)(SNAPSHOT_TIMEOUT_USEC / 1000);
	Dictionary result;
	result["snapshot_id"] = snapshot_id;
	result["domain"] = "resource_graph";
	result["resource_revision"] = (int64_t)snapshot_resource_revision;
	result["revisions"] = snapshot_revisions;
	result["limits_applied"] = limits;

	r_completion.ready = true;
	r_completion.request_id = snapshot_request_id;
	r_completion.result = result;
	r_completion.server_messages = messages;
	// The frozen generation remains active while the transport applies ACK
	// backpressure. COMMAND_CANCEL releases it on every terminal path.
	snapshot_waiting_for_terminal = true;
	snapshot_records.clear();
	snapshot_payloads.clear();
}

bool ResourceGraphAdapter::process_snapshot(uint64_t p_now_usec, uint64_t p_budget_usec, SnapshotCompletion &r_completion) {
	r_completion = SnapshotCompletion();
	if (!snapshot_active) {
		return false;
	}
	if (snapshot_waiting_for_terminal) {
		return false;
	}
	if (p_now_usec - snapshot_started_usec >= SNAPSHOT_TIMEOUT_USEC) {
		_fail_snapshot("snapshot_timeout", "The resource graph snapshot exceeded 120 seconds.", true, r_completion);
		return true;
	}
	const uint64_t started = OS::get_singleton()->get_ticks_usec();
	do {
		if (snapshot_record_index >= snapshot_records.size()) {
			_finish_snapshot(r_completion);
			return true;
		}
		const CatalogRecord &record = snapshot_records[snapshot_record_index];
		const Dictionary resource = record.value["resource"];
		const Array dependencies = record.value["dependencies"];
		const int old_resource_count = snapshot_resources.size();
		const int old_dependency_count = snapshot_dependencies.size();
		const int old_diagnostic_count = snapshot_diagnostics.size();
		snapshot_resources.push_back(resource);
		snapshot_dependencies.append_array(dependencies);
		snapshot_diagnostics.append_array(record.diagnostics);
		Dictionary candidate;
		candidate["resources"] = snapshot_resources;
		candidate["dependencies"] = snapshot_dependencies;
		candidate["diagnostics"] = snapshot_diagnostics;
		if (JSON::stringify(candidate, "", true, true).utf8().length() > (int)SNAPSHOT_CHUNK_BYTES) {
			snapshot_resources.resize(old_resource_count);
			snapshot_dependencies.resize(old_dependency_count);
			snapshot_diagnostics.resize(old_diagnostic_count);
			if (old_resource_count == 0 && old_dependency_count == 0 && old_diagnostic_count == 0) {
				_fail_snapshot("resource_limit_exceeded", "A resource graph record exceeds the snapshot chunk limit.", false, r_completion);
				return true;
			}
			if (!_flush_snapshot_payload()) {
				_fail_snapshot("resource_limit_exceeded", "A resource graph snapshot chunk exceeds its hard limit.", false, r_completion);
				return true;
			}
			continue;
		}
		snapshot_record_index++;
	} while (OS::get_singleton()->get_ticks_usec() - started < p_budget_usec);
	return true;
}

void ResourceGraphAdapter::cancel_snapshot(uint64_t p_request_id) {
	if (!snapshot_active || snapshot_request_id != p_request_id) {
		return;
	}
	snapshot_active = false;
	snapshot_waiting_for_terminal = false;
	snapshot_records.clear();
	snapshot_payloads.clear();
	snapshot_resources.clear();
	snapshot_dependencies.clear();
	snapshot_diagnostics.clear();
}

ResourceDeltaJournal::QueryResult ResourceGraphAdapter::query_delta(uint64_t p_after_resource_revision) const {
	return journal.query_after(p_after_resource_revision);
}

bool ResourceGraphAdapter::is_catalog_ready() const {
	return catalog_ready;
}

bool ResourceGraphAdapter::is_snapshot_active() const {
	return snapshot_active;
}

bool ResourceGraphAdapter::has_pending_work() const {
	const bool snapshot_generation_pending = snapshot_active && !snapshot_waiting_for_terminal;
	return snapshot_generation_pending || refresh_requested || refresh_phase != REFRESH_IDLE;
}

uint64_t ResourceGraphAdapter::get_resource_revision() const {
	return journal.get_current_resource_revision();
}
