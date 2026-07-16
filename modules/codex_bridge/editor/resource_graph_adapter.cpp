/**************************************************************************/
/*  resource_graph_adapter.cpp                                            */
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

#include "resource_graph_adapter.h"

#include "bridge_revision_clock.h"

#include "core/crypto/crypto_core.h"
#include "core/io/dir_access.h"
#include "core/io/file_access.h"
#include "core/io/json.h"
#include "core/io/resource_loader.h"
#include "core/io/resource_uid.h"
#include "core/object/worker_thread_pool.h"
#include "core/os/os.h"
#include "core/string/print_string.h"
#include "editor/file_system/editor_file_system.h"

#include "modules/codex_bridge/protocol/bridge_crypto.h"

#include <initializer_list>
#include <utility>

namespace {

static bool has_exact_keys(const Dictionary &p_value, std::initializer_list<const char *> p_keys) {
	if (p_value.size() != (int)p_keys.size()) {
		return false;
	}
	for (const char *key : p_keys) {
		if (!p_value.has(key)) {
			return false;
		}
	}
	return true;
}

static bool is_safe_integer(const Variant &p_value, bool p_require_positive = false) {
	if (p_value.get_type() != Variant::INT) {
		return false;
	}
	const int64_t value = p_value;
	return value >= (p_require_positive ? 1 : 0) && (uint64_t)value <= ResourceGraphAdapter::MAX_SAFE_INTEGER;
}

static bool is_one_of(const String &p_value, std::initializer_list<const char *> p_allowed) {
	for (const char *allowed : p_allowed) {
		if (p_value == allowed) {
			return true;
		}
	}
	return false;
}

} // namespace

bool ResourceGraphAdapter::_utf8_within_limit(const String &p_value, uint32_t p_limit, uint32_t *r_length) {
	uint32_t length = 0;
	for (int index = 0; index < p_value.length(); index++) {
		const char32_t codepoint = p_value.unicode_at(index);
		uint32_t width = 0;
		if (codepoint <= 0x7f) {
			width = 1;
		} else if (codepoint <= 0x7ff) {
			width = 2;
		} else if (codepoint >= 0xd800 && codepoint <= 0xdfff) {
			return false;
		} else if (codepoint <= 0xffff) {
			width = 3;
		} else if (codepoint <= 0x10ffff) {
			width = 4;
		} else {
			return false;
		}
		if (width > p_limit || length > p_limit - width) {
			return false;
		}
		length += width;
	}
	if (r_length) {
		*r_length = length;
	}
	return true;
}

bool ResourceGraphAdapter::_is_valid_resource_path(const String &p_path) {
	uint32_t byte_length = 0;
	if (!_utf8_within_limit(p_path, MAX_PATH_BYTES, &byte_length) || byte_length <= 6 || !p_path.begins_with("res://")) {
		return false;
	}
	for (int index = 0; index < p_path.length(); index++) {
		const char32_t codepoint = p_path.unicode_at(index);
		if (codepoint <= 0x1f || codepoint == 0x7f || codepoint == '\\' || codepoint == '?' || codepoint == '#') {
			return false;
		}
	}
	const PackedStringArray segments = p_path.substr(6).split("/", true);
	if (segments.is_empty()) {
		return false;
	}
	for (const String &segment : segments) {
		if (segment.is_empty() || segment == "." || segment == "..") {
			return false;
		}
	}
	return true;
}

bool ResourceGraphAdapter::_is_valid_resource_uid(const String &p_uid) {
	uint32_t byte_length = 0;
	if (!_utf8_within_limit(p_uid, MAX_UID_BYTES, &byte_length) || byte_length <= 6 || !p_uid.begins_with("uid://")) {
		return false;
	}
	for (int index = 6; index < p_uid.length(); index++) {
		const char32_t codepoint = p_uid.unicode_at(index);
		if (!((codepoint >= 'a' && codepoint <= 'z') || (codepoint >= 'A' && codepoint <= 'Z') ||
					(codepoint >= '0' && codepoint <= '9') || codepoint == '_' || codepoint == '-')) {
			return false;
		}
	}
	return true;
}

bool ResourceGraphAdapter::_is_valid_raw_dependency_spec(const String &p_spec) {
	uint32_t byte_length = 0;
	return _utf8_within_limit(p_spec, MAX_RAW_DEPENDENCY_BYTES, &byte_length) && byte_length > 0;
}

bool ResourceGraphAdapter::_paths_match_lexically(const String &p_left, const String &p_right, bool p_case_sensitive) {
	const String left = p_left.simplify_path();
	const String right = p_right.simplify_path();
	return p_case_sensitive ? left == right : left.nocasecmp_to(right) == 0;
}

bool ResourceGraphAdapter::_paths_match_on_volume(const String &p_left, const String &p_right) {
	const Ref<DirAccess> dir = DirAccess::create_for_path(p_left);
	const bool case_sensitive = dir.is_null() || dir->is_case_sensitive(p_left);
	return _paths_match_lexically(p_left, p_right, case_sensitive);
}

bool ResourceGraphAdapter::_validate_resource_ref(const Dictionary &p_resource_ref) {
	if (has_exact_keys(p_resource_ref, { "uid" })) {
		return p_resource_ref["uid"].get_type() == Variant::STRING && _is_valid_resource_uid(p_resource_ref["uid"]);
	}
	return has_exact_keys(p_resource_ref, { "uid_missing", "path" }) &&
			p_resource_ref["uid_missing"].get_type() == Variant::BOOL && bool(p_resource_ref["uid_missing"]) &&
			p_resource_ref["path"].get_type() == Variant::STRING && _is_valid_resource_path(p_resource_ref["path"]);
}

bool ResourceGraphAdapter::_validate_resource_observation(const Dictionary &p_resource) {
	if (!has_exact_keys(p_resource, { "resource_ref", "path", "godot_type", "source_kind", "import_state", "modified_time_unix_seconds", "byte_size", "validity", "authority", "resource_revision" }) ||
			p_resource["resource_ref"].get_type() != Variant::DICTIONARY || !_validate_resource_ref(p_resource["resource_ref"]) ||
			p_resource["path"].get_type() != Variant::STRING || !_is_valid_resource_path(p_resource["path"]) ||
			p_resource["godot_type"].get_type() != Variant::STRING || String(p_resource["godot_type"]).is_empty() || !_utf8_within_limit(p_resource["godot_type"], MAX_TYPE_BYTES) ||
			p_resource["source_kind"].get_type() != Variant::STRING || !is_one_of(p_resource["source_kind"], { "source", "imported_source" }) ||
			p_resource["import_state"].get_type() != Variant::STRING || !is_one_of(p_resource["import_state"], { "not_imported", "valid", "invalid" }) ||
			!is_safe_integer(p_resource["modified_time_unix_seconds"]) || !is_safe_integer(p_resource["byte_size"]) ||
			p_resource["validity"].get_type() != Variant::STRING || !is_one_of(p_resource["validity"], { "valid", "partial", "invalid" }) ||
			p_resource["authority"].get_type() != Variant::STRING || p_resource["authority"] != "editor_file_system" ||
			!is_safe_integer(p_resource["resource_revision"], true)) {
		return false;
	}
	const Dictionary resource_ref = p_resource["resource_ref"];
	return !resource_ref.has("path") || resource_ref["path"] == p_resource["path"];
}

bool ResourceGraphAdapter::_validate_dependency_observation(const Dictionary &p_dependency) {
	if (!has_exact_keys(p_dependency, { "source_ref", "target_uid", "fallback_path", "resolved_path", "declared_type", "resolution", "authority", "resource_revision" }) ||
			p_dependency["source_ref"].get_type() != Variant::DICTIONARY || !_validate_resource_ref(p_dependency["source_ref"]) ||
			p_dependency["fallback_path"].get_type() != Variant::STRING || !_is_valid_resource_path(p_dependency["fallback_path"]) ||
			p_dependency["resolution"].get_type() != Variant::STRING || !is_one_of(p_dependency["resolution"], { "resolved", "missing", "stale_uid" }) ||
			p_dependency["authority"].get_type() != Variant::STRING || p_dependency["authority"] != "resource_loader_dependencies" ||
			!is_safe_integer(p_dependency["resource_revision"], true)) {
		return false;
	}
	const Variant target_uid = p_dependency["target_uid"];
	if (target_uid.get_type() != Variant::NIL && (target_uid.get_type() != Variant::STRING || !_is_valid_resource_uid(target_uid))) {
		return false;
	}
	const Variant resolved_path = p_dependency["resolved_path"];
	if (resolved_path.get_type() != Variant::NIL && (resolved_path.get_type() != Variant::STRING || !_is_valid_resource_path(resolved_path))) {
		return false;
	}
	const Variant declared_type = p_dependency["declared_type"];
	return declared_type.get_type() == Variant::NIL || (declared_type.get_type() == Variant::STRING && _utf8_within_limit(declared_type, MAX_TYPE_BYTES));
}

bool ResourceGraphAdapter::_validate_diagnostic(const Dictionary &p_diagnostic) {
	const bool has_target = p_diagnostic.has("target_reference");
	if (!(has_target ? has_exact_keys(p_diagnostic, { "code", "subject", "target_reference", "resource_revision" }) : has_exact_keys(p_diagnostic, { "code", "subject", "resource_revision" })) ||
			p_diagnostic["code"].get_type() != Variant::STRING || !is_one_of(p_diagnostic["code"], { "missing_dependency", "stale_resource_uid", "resource_uid_path_mismatch", "duplicate_resource_uid", "invalid_import", "resource_limit_exceeded" }) ||
			p_diagnostic["subject"].get_type() != Variant::DICTIONARY || !_validate_resource_ref(p_diagnostic["subject"]) ||
			!is_safe_integer(p_diagnostic["resource_revision"], true)) {
		return false;
	}
	return !has_target || (p_diagnostic["target_reference"].get_type() == Variant::STRING && _utf8_within_limit(p_diagnostic["target_reference"], MAX_PATH_BYTES));
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

bool ResourceGraphAdapter::_can_append_diagnostics(uint64_t p_current_count, uint64_t p_additional_count) {
	return p_current_count <= MAX_DIAGNOSTICS && p_additional_count <= (uint64_t)MAX_DIAGNOSTICS - p_current_count;
}

bool ResourceGraphAdapter::_append_active_diagnostic(const Dictionary &p_diagnostic) {
	if (!_validate_diagnostic(p_diagnostic) || !_can_append_diagnostics(observed_diagnostic_count, 1)) {
		return false;
	}
	active_observed_record.diagnostics.push_back(p_diagnostic);
	observed_diagnostic_count++;
	return true;
}

bool ResourceGraphAdapter::_update_active_facts_hash(const String &p_name, const String &p_value) {
	if (!active_facts_hash_started) {
		return false;
	}
	const CharString name = p_name.utf8();
	const CharString value = p_value.utf8();
	uint8_t lengths[16];
	const uint64_t name_length = name.length();
	const uint64_t value_length = value.length();
	for (int index = 0; index < 8; index++) {
		lengths[7 - index] = (uint8_t)(name_length >> (index * 8));
		lengths[15 - index] = (uint8_t)(value_length >> (index * 8));
	}
	if (active_facts_hash.update(lengths, sizeof(lengths)) != OK ||
			(name_length > 0 && active_facts_hash.update(reinterpret_cast<const uint8_t *>(name.get_data()), name_length) != OK) ||
			(value_length > 0 && active_facts_hash.update(reinterpret_cast<const uint8_t *>(value.get_data()), value_length) != OK)) {
		return false;
	}
	return true;
}

void ResourceGraphAdapter::_reset_active_observation() {
	if (active_facts_hash_started) {
		uint8_t discarded[32];
		active_facts_hash.finish(discarded);
	}
	active_facts_hash_started = false;
	active_refresh_file = RefreshFile();
	active_observed_record = CatalogRecord();
	active_resource_ref = Dictionary();
	active_dependency_index = 0;
	active_dependency_count = 0;
	active_resource_revision = 0;
}

bool ResourceGraphAdapter::_release_active_observation_step() {
	if (active_observed_record.value.has("dependencies")) {
		Array dependencies = active_observed_record.value["dependencies"];
		if (!dependencies.is_empty()) {
			dependencies.pop_back();
			active_observed_record.value["dependencies"] = dependencies;
			return true;
		}
	}
	if (!active_observed_record.diagnostics.is_empty()) {
		active_observed_record.diagnostics.pop_back();
		return true;
	}
	_reset_active_observation();
	return true;
}

bool ResourceGraphAdapter::_release_catalog_front_step(RBMap<String, CatalogRecord> &r_catalog, const RBMap<String, CatalogRecord> *p_preserved_catalog) {
	RBMap<String, CatalogRecord>::Element *first = r_catalog.front();
	if (!first) {
		return false;
	}

	CatalogRecord &record = first->value();
	bool value_is_shared = false;
	bool diagnostics_are_shared = false;
	if (p_preserved_catalog) {
		const RBMap<String, CatalogRecord>::Element *preserved = p_preserved_catalog->find(first->key());
		if (preserved) {
			value_is_shared = record.value.id() == preserved->value().value.id();
			diagnostics_are_shared = record.diagnostics.id() == preserved->value().diagnostics.id();
		}
	}
	if (!value_is_shared && record.value.has("dependencies")) {
		Array dependencies = record.value["dependencies"];
		if (!dependencies.is_empty()) {
			dependencies.pop_back();
			record.value["dependencies"] = dependencies;
			return true;
		}
	}
	if (!diagnostics_are_shared && !record.diagnostics.is_empty()) {
		record.diagnostics.pop_back();
		return true;
	}
	r_catalog.erase(first);
	return true;
}

void ResourceGraphAdapter::_reset_reconcile() {
	reconcile_record = nullptr;
	reconcile_resume_phase = REFRESH_IDLE;
	reconcile_next_revision = 0;
	reconcile_operation_count = 0;
	reconcile_dependency_count = 0;
	reconcile_changed = false;
	reconcile_invalidated = false;
	reconcile_commit_started = false;
	reconcile_previous_revision = 0;
	journal_prepare_task = -1;
	journal_prepare_error = OK;
	journal._retire_prepared_batch(journal_prepared_batch);
}

void ResourceGraphAdapter::_prepare_journal_batch_thread(void *p_userdata) {
	ResourceGraphAdapter *adapter = static_cast<ResourceGraphAdapter *>(p_userdata);
	HashSet<String> preexisting_keys;
	adapter->journal_prepare_error = ResourceDeltaJournal::prepare_batch(
			adapter->reconcile_next_revision,
			adapter->reconcile_operations,
			preexisting_keys,
			adapter->journal_prepared_batch);
}

void ResourceGraphAdapter::_wait_for_journal_preparation() {
	if (journal_prepare_task < 0) {
		return;
	}
	WorkerThreadPool *worker_pool = WorkerThreadPool::get_singleton();
	if (worker_pool) {
		worker_pool->wait_for_task_completion(journal_prepare_task);
	}
	journal_prepare_task = -1;
}

void ResourceGraphAdapter::initialize(BridgeRevisionClock *p_revision_clock) {
	revision_clock = p_revision_clock;
	journal.initialize(p_revision_clock ? p_revision_clock->get_resource_revision() : 1);
	catalog.clear();
	catalog_ready = false;
	catalog_limit_exceeded = false;
	refresh_phase = REFRESH_IDLE;
	refresh_drain_disposition = REFRESH_DRAIN_NONE;
	refresh_root = nullptr;
	observed_dependency_count = 0;
	observed_diagnostic_count = 0;
	_reset_active_observation();
	_reset_reconcile();
	// The module starts before EditorFileSystem's first scan. The authoritative
	// filesystem_changed signal requests the initial catalog once that scan has
	// installed its complete directory tree.
	refresh_requested = false;
	pending_reimport_paths.clear();
	snapshot_active = false;
	snapshot_waiting_for_terminal = false;
	snapshot_pending_message.clear();
	snapshot_chunk_count = 0;
	snapshot_finishing = false;
}

void ResourceGraphAdapter::shutdown() {
	_wait_for_journal_preparation();
	revision_clock = nullptr;
	catalog.clear();
	catalog_ready = false;
	catalog_limit_exceeded = false;
	refresh_requested = false;
	refresh_phase = REFRESH_IDLE;
	refresh_drain_disposition = REFRESH_DRAIN_NONE;
	refresh_root = nullptr;
	observed_dependency_count = 0;
	observed_diagnostic_count = 0;
	directory_stack.clear();
	refresh_files.clear();
	observed_catalog.clear();
	_reset_active_observation();
	next_catalog.clear();
	journal._retire_array(reconcile_operations);
	_reset_reconcile();
	pending_reimport_paths.clear();
	active_reimport_paths.clear();
	snapshot_active = false;
	snapshot_waiting_for_terminal = false;
	snapshot_record = nullptr;
	snapshot_pending_message.clear();
	snapshot_chunk_count = 0;
	snapshot_finishing = false;
	snapshot_resources.clear();
	snapshot_dependencies.clear();
	snapshot_diagnostics.clear();
	journal._wait_for_cleanup();
}

void ResourceGraphAdapter::request_refresh() {
	refresh_requested = true;
}

void ResourceGraphAdapter::mark_reimported(const Vector<String> &p_paths) {
	for (const String &path : p_paths) {
		if (_is_valid_resource_path(path)) {
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
	if (!directory_stack.is_empty() || !refresh_files.is_empty() || !observed_catalog.is_empty() || !next_catalog.is_empty() ||
			!reconcile_operations.is_empty() || !active_reimport_paths.is_empty() || active_facts_hash_started ||
			!active_observed_record.value.is_empty() || !active_observed_record.diagnostics.is_empty()) {
		_start_refresh_drain(REFRESH_DRAIN_RESTART);
		return false;
	}
	refresh_requested = false;
	refresh_phase = REFRESH_COLLECT_PATHS;
	refresh_root = filesystem->get_filesystem();
	DirectoryCursor root;
	root.directory = refresh_root;
	directory_stack.push_back(root);
	_reset_reconcile();
	active_reimport_paths = std::move(pending_reimport_paths);
	observed_dependency_count = 0;
	observed_diagnostic_count = 0;
	refresh_limit_exceeded = false;
	return true;
}

void ResourceGraphAdapter::_abort_refresh() {
	refresh_requested = true;
	_start_refresh_drain(REFRESH_DRAIN_RESTART);
}

void ResourceGraphAdapter::_start_refresh_drain(RefreshDrainDisposition p_disposition) {
	refresh_drain_disposition = p_disposition;
	refresh_root = nullptr;
	refresh_phase = REFRESH_DRAIN_OBSERVATION;
}

bool ResourceGraphAdapter::_process_refresh_drain_step(RefreshOutcome &r_outcome) {
	if (refresh_phase == REFRESH_DRAIN_OBSERVATION) {
		_release_active_observation_step();
		if (!active_facts_hash_started && active_observed_record.value.is_empty() && active_observed_record.diagnostics.is_empty()) {
			refresh_phase = REFRESH_DRAIN_DIRECTORIES;
		}
		return true;
	}
	if (refresh_phase == REFRESH_DRAIN_DIRECTORIES) {
		if (!directory_stack.is_empty()) {
			directory_stack.resize(directory_stack.size() - 1);
			return true;
		}
		refresh_phase = REFRESH_DRAIN_FILES;
		return true;
	}
	if (refresh_phase == REFRESH_DRAIN_FILES) {
		RBMap<String, RefreshFile>::Element *first = refresh_files.front();
		if (first) {
			refresh_files.erase(first);
			return true;
		}
		refresh_phase = REFRESH_DRAIN_OBSERVED;
		return true;
	}
	if (refresh_phase == REFRESH_DRAIN_OBSERVED) {
		if (_release_catalog_front_step(observed_catalog, &next_catalog)) {
			return true;
		}
		refresh_phase = REFRESH_DRAIN_OPERATIONS;
		return true;
	}
	if (refresh_phase == REFRESH_DRAIN_OPERATIONS) {
		journal._retire_array(reconcile_operations);
		refresh_phase = REFRESH_DRAIN_NEXT;
		return true;
	}
	if (refresh_phase == REFRESH_DRAIN_NEXT) {
		if (_release_catalog_front_step(next_catalog, &catalog)) {
			return true;
		}
		refresh_phase = REFRESH_DRAIN_REIMPORTS;
		return true;
	}
	if (refresh_phase == REFRESH_DRAIN_REIMPORTS) {
		RBSet<String>::Element *first = active_reimport_paths.front();
		if (first) {
			pending_reimport_paths.insert(first->get());
			active_reimport_paths.erase(first);
			return true;
		}
		refresh_phase = REFRESH_DRAIN_FINALIZE;
		return true;
	}
	if (refresh_phase != REFRESH_DRAIN_FINALIZE) {
		return false;
	}

	const RefreshDrainDisposition disposition = refresh_drain_disposition;
	refresh_drain_disposition = REFRESH_DRAIN_NONE;
	refresh_phase = REFRESH_IDLE;
	observed_dependency_count = 0;
	observed_diagnostic_count = 0;
	refresh_limit_exceeded = false;
	_reset_reconcile();
	if (disposition == REFRESH_DRAIN_RESTART) {
		refresh_requested = true;
		return true;
	}
	if (disposition == REFRESH_DRAIN_LIMIT_FAILURE) {
		catalog_limit_exceeded = true;
		const uint64_t previous = revision_clock ? revision_clock->get_resource_revision() : journal.get_current_resource_revision();
		const uint64_t current = revision_clock ? revision_clock->record_resource_change() : previous + 1;
		journal.invalidate_to(current);
		r_outcome.invalidated = true;
		r_outcome.last_contiguous_resource_revision = previous;
		r_outcome.current_resource_revision = current;
		r_outcome.revisions = revision_clock ? revision_clock->get_revision_vector() : Dictionary();
		if (journal.is_invalidating()) {
			refresh_phase = REFRESH_DRAIN_JOURNAL;
		}
	}
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
			if (!_is_valid_resource_path(path) || refresh_files.size() >= (int)MAX_RESOURCES || refresh_files.has(path)) {
				refresh_limit_exceeded = true;
				return false;
			}
			RefreshFile file;
			file.path = path;
			file.directory = cursor.directory;
			file.file_index = file_index;
			refresh_files.insert(path, file);
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
	refresh_phase = REFRESH_OBSERVE;
	return false;
}

bool ResourceGraphAdapter::_begin_observe_resource(const RefreshFile &p_file) {
	_reset_active_observation();
	const String &path = p_file.path;
	if (!_is_valid_resource_path(path)) {
		return false;
	}
	EditorFileSystemDirectory *directory = p_file.directory;
	const int file_index = p_file.file_index;
	if (!directory || file_index < 0 || file_index >= directory->get_file_count() || directory->get_file_path(file_index) != path) {
		return false;
	}
	const int dependency_count = directory->get_file_dep_count(file_index);
	if (dependency_count < 0 || (uint32_t)dependency_count > MAX_DEPENDENCIES_PER_RESOURCE || observed_dependency_count + (uint64_t)dependency_count > MAX_DEPENDENCIES) {
		return false;
	}

	const ResourceUID::ID uid = directory->get_file_uid(file_index);
	active_resource_ref = _make_resource_ref(path, uid);
	if (!_validate_resource_ref(active_resource_ref)) {
		return false;
	}
	const bool imported = FileAccess::exists(path + ".import");
	const bool import_valid = !imported || directory->get_file_import_is_valid(file_index);
	String godot_type = directory->get_file_type(file_index);
	if (godot_type.is_empty()) {
		godot_type = ResourceLoader::get_resource_type(path);
	}
	if (godot_type.is_empty()) {
		godot_type = "Resource";
	}
	if (godot_type.is_empty() || !_utf8_within_limit(godot_type, MAX_TYPE_BYTES)) {
		return false;
	}

	const uint64_t current_revision = revision_clock ? revision_clock->get_resource_revision() : journal.get_current_resource_revision();
	active_resource_revision = catalog_ready ? current_revision + 1 : current_revision;
	const uint64_t modified_time = directory->get_file_modified_time(file_index);
	const int64_t raw_byte_size = FileAccess::get_size(path);
	const int64_t byte_size = MAX((int64_t)0, raw_byte_size);
	if (modified_time > MAX_SAFE_INTEGER || byte_size > (int64_t)MAX_SAFE_INTEGER) {
		return false;
	}
	Dictionary resource;
	resource["resource_ref"] = active_resource_ref;
	resource["path"] = path;
	resource["godot_type"] = godot_type;
	resource["source_kind"] = imported ? "imported_source" : "source";
	resource["import_state"] = imported ? (import_valid ? "valid" : "invalid") : "not_imported";
	resource["modified_time_unix_seconds"] = (int64_t)modified_time;
	resource["byte_size"] = byte_size;
	resource["validity"] = import_valid ? "valid" : "invalid";
	resource["authority"] = "editor_file_system";
	resource["resource_revision"] = (int64_t)active_resource_revision;
	if (!_validate_resource_observation(resource)) {
		return false;
	}
	active_observed_record.value["resource"] = resource;
	active_observed_record.value["dependencies"] = Array();
	active_observed_record.diagnostics = Array();
	active_refresh_file = p_file;
	active_dependency_count = dependency_count;
	if (active_facts_hash.start() != OK) {
		_reset_active_observation();
		return false;
	}
	active_facts_hash_started = true;
	return _update_active_facts_hash("domain", "resource_graph_facts_v2") &&
			_update_active_facts_hash("dependency_count", String::num_int64(dependency_count));
}

bool ResourceGraphAdapter::_observe_one_dependency() {
	if (!active_facts_hash_started || active_dependency_index < 0 || active_dependency_index >= active_dependency_count) {
		return false;
	}
	EditorFileSystemDirectory *directory = active_refresh_file.directory;
	const int file_index = active_refresh_file.file_index;
	if (!directory || file_index < 0 || file_index >= directory->get_file_count() || directory->get_file_path(file_index) != active_refresh_file.path) {
		return false;
	}

	const String raw_dependency = directory->get_file_dep_raw(file_index, active_dependency_index);
	if (!_is_valid_raw_dependency_spec(raw_dependency)) {
		return false;
	}
	const EditorFileSystemDependency parsed_dependency = EditorFileSystemDependency::from_cache(raw_dependency);
	const String target_uid = parsed_dependency.uid;
	const String declared_type = parsed_dependency.declared_type;
	String fallback_path = parsed_dependency.fallback_path;
	if ((!target_uid.is_empty() && !_is_valid_resource_uid(target_uid)) ||
			!_utf8_within_limit(declared_type, MAX_TYPE_BYTES) ||
			(!fallback_path.is_empty() && !_is_valid_resource_path(fallback_path)) ||
			(target_uid.is_empty() && fallback_path.is_empty())) {
		return false;
	}

	String resolved_path;
	String resolution = "missing";
	bool uid_path_mismatch = false;
	if (!target_uid.is_empty()) {
		const ResourceUID::ID target_id = ResourceUID::get_singleton()->text_to_id(target_uid);
		if (target_id != ResourceUID::INVALID_ID && ResourceUID::get_singleton()->has_id(target_id)) {
			const String candidate = ResourceUID::get_singleton()->get_id_path(target_id);
			const bool candidate_valid = _is_valid_resource_path(candidate);
			if (fallback_path.is_empty() && candidate_valid) {
				fallback_path = candidate;
			}
			if (!_is_valid_resource_path(fallback_path)) {
				return false;
			}
			const bool candidate_exists = candidate_valid && (FileAccess::exists(candidate) || ResourceLoader::exists(candidate));
			const bool fallback_exists = candidate == fallback_path ? candidate_exists : (FileAccess::exists(fallback_path) || ResourceLoader::exists(fallback_path));
			uid_path_mismatch = candidate_exists && fallback_exists && !_paths_match_on_volume(candidate, fallback_path);
			if (uid_path_mismatch) {
				resolution = "stale_uid";
			} else if (candidate_exists) {
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
	if (!_is_valid_resource_path(fallback_path)) {
		return false;
	}

	Dictionary dependency;
	dependency["source_ref"] = active_resource_ref;
	dependency["target_uid"] = target_uid.is_empty() ? Variant() : Variant(target_uid);
	dependency["fallback_path"] = fallback_path;
	dependency["resolved_path"] = resolved_path.is_empty() ? Variant() : Variant(resolved_path);
	dependency["declared_type"] = declared_type.is_empty() ? Variant() : Variant(declared_type);
	dependency["resolution"] = resolution;
	dependency["authority"] = "resource_loader_dependencies";
	dependency["resource_revision"] = (int64_t)active_resource_revision;
	if (!_validate_dependency_observation(dependency) || dependency["source_ref"] != active_resource_ref) {
		return false;
	}
	Array dependencies = active_observed_record.value["dependencies"];
	dependencies.push_back(dependency);
	active_observed_record.value["dependencies"] = dependencies;

	String diagnostic_code;
	if (resolution != "resolved") {
		diagnostic_code = uid_path_mismatch ? "resource_uid_path_mismatch" : (resolution == "stale_uid" ? "stale_resource_uid" : "missing_dependency");
		Dictionary diagnostic;
		diagnostic["code"] = diagnostic_code;
		diagnostic["subject"] = active_resource_ref;
		diagnostic["target_reference"] = uid_path_mismatch ? fallback_path : (resolution == "stale_uid" ? target_uid : fallback_path);
		diagnostic["resource_revision"] = (int64_t)active_resource_revision;
		if (!_append_active_diagnostic(diagnostic)) {
			return false;
		}
		Dictionary resource = active_observed_record.value["resource"];
		if (resource["validity"] == "valid") {
			resource["validity"] = "partial";
			active_observed_record.value["resource"] = resource;
		}
	}

	if (!_update_active_facts_hash("dependency.target_uid", target_uid) ||
			!_update_active_facts_hash("dependency.declared_type", declared_type) ||
			!_update_active_facts_hash("dependency.fallback_path", fallback_path) ||
			!_update_active_facts_hash("dependency.resolved_path", resolved_path) ||
			!_update_active_facts_hash("dependency.resolution", resolution) ||
			!_update_active_facts_hash("dependency.diagnostic_code", diagnostic_code)) {
		return false;
	}
	active_dependency_index++;
	observed_dependency_count++;
	return true;
}

bool ResourceGraphAdapter::_finish_observe_resource(CatalogRecord &r_record) {
	if (!active_facts_hash_started || active_dependency_index != active_dependency_count) {
		return false;
	}
	EditorFileSystemDirectory *directory = active_refresh_file.directory;
	const int file_index = active_refresh_file.file_index;
	if (!directory || file_index < 0 || file_index >= directory->get_file_count() || directory->get_file_path(file_index) != active_refresh_file.path) {
		return false;
	}
	Dictionary resource = active_observed_record.value["resource"];
	if (String(resource["import_state"]) == "invalid") {
		Dictionary diagnostic;
		diagnostic["code"] = "invalid_import";
		diagnostic["subject"] = active_resource_ref;
		diagnostic["resource_revision"] = (int64_t)active_resource_revision;
		if (!_append_active_diagnostic(diagnostic)) {
			return false;
		}
		if (!_update_active_facts_hash("diagnostic", "invalid_import")) {
			return false;
		}
	}
	const ResourceUID::ID uid = directory->get_file_uid(file_index);
	if (uid != ResourceUID::INVALID_ID && ResourceUID::get_singleton()->has_id(uid)) {
		const String uid_path = ResourceUID::get_singleton()->get_id_path(uid);
		const bool uid_path_valid = _is_valid_resource_path(uid_path);
		const bool uid_path_mismatch = !uid_path_valid || !_paths_match_on_volume(uid_path, active_refresh_file.path);
		if (uid_path_mismatch) {
			Dictionary diagnostic;
			diagnostic["code"] = "resource_uid_path_mismatch";
			diagnostic["subject"] = active_resource_ref;
			if (uid_path_valid) {
				diagnostic["target_reference"] = uid_path;
			}
			diagnostic["resource_revision"] = (int64_t)active_resource_revision;
			if (!_append_active_diagnostic(diagnostic)) {
				return false;
			}
			resource["validity"] = "partial";
			if (!_update_active_facts_hash("diagnostic", "resource_uid_path_mismatch") || (uid_path_valid && !_update_active_facts_hash("diagnostic.target", uid_path))) {
				return false;
			}
		}
	}
	active_observed_record.value["resource"] = resource;
	if (!has_exact_keys(active_observed_record.value, { "resource", "dependencies" }) || !_validate_resource_observation(resource)) {
		return false;
	}
	const String resource_key = ResourceDeltaJournal::resource_ref_key(active_resource_ref);
	if (resource_key.is_empty() ||
			!_update_active_facts_hash("resource_ref", resource_key) ||
			!_update_active_facts_hash("path", resource["path"]) ||
			!_update_active_facts_hash("godot_type", resource["godot_type"]) ||
			!_update_active_facts_hash("source_kind", resource["source_kind"]) ||
			!_update_active_facts_hash("import_state", resource["import_state"]) ||
			!_update_active_facts_hash("modified_time", String::num_int64((int64_t)resource["modified_time_unix_seconds"])) ||
			!_update_active_facts_hash("byte_size", String::num_int64((int64_t)resource["byte_size"])) ||
			!_update_active_facts_hash("validity", resource["validity"])) {
		return false;
	}
	PackedByteArray digest;
	digest.resize(32);
	if (active_facts_hash.finish(digest.ptrw()) != OK) {
		return false;
	}
	active_facts_hash_started = false;
	active_observed_record.facts_checksum = BridgeCrypto::bytes_to_lower_hex(digest);
	r_record = std::move(active_observed_record);
	_reset_active_observation();
	return true;
}

void ResourceGraphAdapter::_finish_refresh(RefreshOutcome &r_outcome) {
	refresh_root = nullptr;

	if (refresh_limit_exceeded || !directory_stack.is_empty() || !refresh_files.is_empty() || active_facts_hash_started ||
			!active_observed_record.value.is_empty() || !active_observed_record.diagnostics.is_empty()) {
		_start_refresh_drain(REFRESH_DRAIN_LIMIT_FAILURE);
		return;
	}

	if (!catalog_ready) {
		catalog = std::move(observed_catalog);
		refresh_phase = REFRESH_INITIAL_RELEASE_REIMPORTS;
		return;
	}

	reconcile_next_revision = revision_clock ? revision_clock->get_resource_revision() + 1 : journal.get_current_resource_revision() + 1;
	const int64_t record_delta = (int64_t)observed_catalog.size() - (int64_t)catalog.size();
	reconcile_invalidated = record_delta >= (int64_t)BULK_INVALIDATION_RECORD_DELTA || record_delta <= -(int64_t)BULK_INVALIDATION_RECORD_DELTA;
	reconcile_record = observed_catalog.front();
	refresh_phase = REFRESH_RECONCILE_OBSERVED;
}

void ResourceGraphAdapter::_release_reconcile_operations_then(RefreshPhase p_resume_phase) {
	reconcile_resume_phase = p_resume_phase;
	refresh_phase = REFRESH_RELEASE_RECONCILE_OPERATIONS;
}

bool ResourceGraphAdapter::_process_reconcile_step(RefreshOutcome &r_outcome) {
	if (refresh_phase == REFRESH_RECONCILE_OBSERVED) {
		if (!reconcile_record) {
			refresh_phase = REFRESH_RELEASE_OBSERVED;
			return true;
		}
		const String key = reconcile_record->key();
		CatalogRecord record = reconcile_record->value();
		reconcile_record = reconcile_record->next();
		const RBMap<String, CatalogRecord>::Element *previous_element = catalog.find(key);
		const CatalogRecord *previous = previous_element ? &previous_element->value() : nullptr;
		const Dictionary resource = record.value["resource"];
		const String path = resource["path"];
		const bool reimported = active_reimport_paths.has(path);
		if (previous && !reimported && previous->facts_checksum == record.facts_checksum) {
			next_catalog.insert(key, *previous);
			return true;
		}
		next_catalog.insert(key, record);
		reconcile_changed = true;
		if (reconcile_invalidated) {
			return true;
		}
		const Array dependencies = record.value["dependencies"];
		if (reconcile_operation_count >= MAX_INCREMENTAL_OPERATIONS || reconcile_dependency_count + (uint32_t)dependencies.size() > MAX_INCREMENTAL_DEPENDENCIES) {
			reconcile_invalidated = true;
			_release_reconcile_operations_then(REFRESH_RECONCILE_OBSERVED);
			return true;
		}
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
		reconcile_operations.push_back(operation);
		reconcile_operation_count++;
		reconcile_dependency_count += dependencies.size();
		return true;
	}
	if (refresh_phase == REFRESH_RELEASE_OBSERVED) {
		if (observed_catalog.is_empty()) {
			reconcile_record = catalog.front();
			refresh_phase = REFRESH_RECONCILE_REMOVED;
			return true;
		}
		_release_catalog_front_step(observed_catalog, &next_catalog);
		return true;
	}
	if (refresh_phase == REFRESH_RECONCILE_REMOVED) {
		if (!reconcile_record) {
			refresh_phase = REFRESH_RELEASE_CATALOG;
			return true;
		}
		const String key = reconcile_record->key();
		const CatalogRecord removed = reconcile_record->value();
		reconcile_record = reconcile_record->next();
		if (next_catalog.has(key)) {
			return true;
		}
		reconcile_changed = true;
		if (reconcile_invalidated) {
			return true;
		}
		if (reconcile_operation_count >= MAX_INCREMENTAL_OPERATIONS) {
			reconcile_invalidated = true;
			_release_reconcile_operations_then(REFRESH_RECONCILE_REMOVED);
			return true;
		}
		const Dictionary resource = removed.value["resource"];
		Dictionary operation;
		operation["kind"] = "remove";
		operation["resource_ref"] = resource["resource_ref"];
		operation["path"] = resource["path"];
		reconcile_operations.push_back(operation);
		reconcile_operation_count++;
		return true;
	}
	if (refresh_phase == REFRESH_RELEASE_CATALOG) {
		if (catalog.is_empty()) {
			refresh_phase = REFRESH_RELEASE_REIMPORTS;
			return true;
		}
		_release_catalog_front_step(catalog, &next_catalog);
		return true;
	}
	if (refresh_phase == REFRESH_RELEASE_REIMPORTS) {
		RBSet<String>::Element *first = active_reimport_paths.front();
		if (!first) {
			refresh_phase = REFRESH_COMMIT;
			return true;
		}
		active_reimport_paths.erase(first);
		return true;
	}
	if (refresh_phase == REFRESH_COMMIT) {
		_commit_reconcile(r_outcome);
		return true;
	}
	if (refresh_phase == REFRESH_DRAIN_JOURNAL) {
		if (journal.drain_invalidation_step()) {
			return true;
		}
		_release_reconcile_operations_then(REFRESH_IDLE);
		return true;
	}
	if (refresh_phase == REFRESH_RELEASE_RECONCILE_OPERATIONS) {
		journal._retire_array(reconcile_operations);
		const RefreshPhase resume_phase = reconcile_resume_phase;
		reconcile_resume_phase = REFRESH_IDLE;
		reconcile_operation_count = 0;
		reconcile_dependency_count = 0;
		refresh_phase = resume_phase;
		if (resume_phase == REFRESH_IDLE) {
			_reset_reconcile();
		}
		return true;
	}
	return false;
}

void ResourceGraphAdapter::_commit_reconcile(RefreshOutcome &r_outcome) {
	if (!reconcile_commit_started) {
		reconcile_commit_started = true;
		reconcile_previous_revision = revision_clock ? revision_clock->get_resource_revision() : journal.get_current_resource_revision();
		catalog = std::move(next_catalog);
		catalog_limit_exceeded = false;
		if (!reconcile_changed) {
			_release_reconcile_operations_then(REFRESH_IDLE);
			return;
		}
		if (!reconcile_invalidated) {
			journal_prepare_error = OK;
			journal._retire_prepared_batch(journal_prepared_batch);
			WorkerThreadPool *worker_pool = WorkerThreadPool::get_singleton();
			if (worker_pool) {
				journal_prepare_task = worker_pool->add_native_task(
						_prepare_journal_batch_thread,
						this,
						false,
						SNAME("CodexResourceDeltaBatch"));
			}
			if (journal_prepare_task >= 0) {
				return;
			}
			_prepare_journal_batch_thread(this);
		}
	}

	if (journal_prepare_task >= 0) {
		WorkerThreadPool *worker_pool = WorkerThreadPool::get_singleton();
		if (worker_pool && !worker_pool->is_task_completed(journal_prepare_task)) {
			return;
		}
		_wait_for_journal_preparation();
	}

	const uint64_t committed_revision = revision_clock ? revision_clock->record_resource_change() : reconcile_next_revision;
	const Dictionary committed_revisions = revision_clock ? revision_clock->get_revision_vector() : Dictionary();
	bool journal_invalidated = reconcile_invalidated;
	if (committed_revision != reconcile_next_revision || journal_prepare_error != OK) {
		journal_invalidated = true;
	}
	if (journal_invalidated) {
		journal.invalidate_to(committed_revision);
	} else {
		const uint64_t project_revision = committed_revisions.has("project_revision") ? (uint64_t)(int64_t)committed_revisions["project_revision"] : committed_revision;
		Dictionary batch;
		if (journal.commit_prepared(committed_revision, project_revision, journal_prepared_batch, batch, journal_invalidated) != OK) {
			journal.invalidate_to(committed_revision);
			journal_invalidated = true;
		}
	}
	r_outcome.changed = true;
	r_outcome.invalidated = journal_invalidated;
	r_outcome.last_contiguous_resource_revision = reconcile_previous_revision;
	r_outcome.current_resource_revision = committed_revision;
	r_outcome.revisions = committed_revisions;
	if (journal.is_invalidating()) {
		refresh_phase = REFRESH_DRAIN_JOURNAL;
	} else {
		_release_reconcile_operations_then(REFRESH_IDLE);
	}
}

bool ResourceGraphAdapter::process_refresh(uint64_t p_budget_usec, RefreshOutcome &r_outcome) {
	r_outcome = RefreshOutcome();
	// Keep the catalog generation frozen until the transport reports terminal
	// ACK, disconnect, cancellation, or timeout for the active snapshot.
	if (snapshot_active) {
		return false;
	}
	EditorFileSystem *filesystem = EditorFileSystem::get_singleton();
	const bool filesystem_ready = filesystem && !filesystem->doing_first_scan() && !filesystem->is_scanning() && !filesystem->is_importing() && filesystem->get_filesystem();
	const bool capture_active = refresh_phase == REFRESH_COLLECT_PATHS || refresh_phase == REFRESH_OBSERVE ||
			refresh_phase == REFRESH_OBSERVE_DEPENDENCIES || refresh_phase == REFRESH_OBSERVE_FINISH;
	if (capture_active && (refresh_requested || !filesystem_ready || filesystem->get_filesystem() != refresh_root)) {
		_abort_refresh();
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
			RBMap<String, RefreshFile>::Element *next_file = refresh_files.front();
			if (!next_file) {
				_finish_refresh(r_outcome);
				return true;
			}
			const RefreshFile file = next_file->value();
			refresh_files.erase(next_file);
			if (!_begin_observe_resource(file)) {
				refresh_limit_exceeded = true;
				_finish_refresh(r_outcome);
				return true;
			}
			refresh_phase = active_dependency_count > 0 ? REFRESH_OBSERVE_DEPENDENCIES : REFRESH_OBSERVE_FINISH;
		} else if (refresh_phase == REFRESH_OBSERVE_DEPENDENCIES) {
			if (!_observe_one_dependency()) {
				refresh_limit_exceeded = true;
				_finish_refresh(r_outcome);
				return true;
			}
			if (active_dependency_index == active_dependency_count) {
				refresh_phase = REFRESH_OBSERVE_FINISH;
			}
		} else if (refresh_phase == REFRESH_OBSERVE_FINISH) {
			CatalogRecord record;
			if (!_finish_observe_resource(record)) {
				refresh_limit_exceeded = true;
				_finish_refresh(r_outcome);
				return true;
			}
			const Dictionary resource = record.value["resource"];
			const String key = ResourceDeltaJournal::resource_ref_key(resource["resource_ref"]);
			if (key.is_empty() || observed_catalog.has(key)) {
				active_observed_record = std::move(record);
				refresh_limit_exceeded = true;
				_finish_refresh(r_outcome);
				return true;
			}
			observed_catalog.insert(key, record);
			refresh_phase = REFRESH_OBSERVE;
		} else if (refresh_phase == REFRESH_INITIAL_RELEASE_REIMPORTS) {
			RBSet<String>::Element *first = active_reimport_paths.front();
			if (first) {
				active_reimport_paths.erase(first);
			} else {
				const uint64_t initial_revision = revision_clock ? revision_clock->get_resource_revision() : 1;
				catalog_ready = true;
				catalog_limit_exceeded = false;
				journal.initialize(initial_revision);
				refresh_phase = REFRESH_IDLE;
				print_verbose(vformat("[codex_bridge] Resource catalog initialized with %d records.", catalog.size()));
				return true;
			}
		} else if (_process_refresh_drain_step(r_outcome)) {
			if (refresh_phase == REFRESH_IDLE) {
				return true;
			}
		} else if (_process_reconcile_step(r_outcome)) {
			if (refresh_phase == REFRESH_IDLE) {
				return true;
			}
		}
	} while (OS::get_singleton()->get_ticks_usec() - started < p_budget_usec);
	return did_work;
}

Error ResourceGraphAdapter::begin_snapshot(uint64_t p_request_id, uint64_t p_now_usec, const Dictionary &p_context, Dictionary &r_error_data) {
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
	if (refresh_requested || refresh_phase != REFRESH_IDLE) {
		return ERR_UNAVAILABLE;
	}
	snapshot_active = true;
	snapshot_waiting_for_terminal = false;
	snapshot_request_id = p_request_id;
	snapshot_started_usec = p_now_usec;
	snapshot_id = _make_snapshot_id();
	snapshot_resource_revision = revision_clock ? revision_clock->get_resource_revision() : journal.get_current_resource_revision();
	snapshot_revisions = revision_clock ? revision_clock->get_revision_vector() : Dictionary();
	snapshot_context = p_context;
	snapshot_record = catalog.front();
	print_verbose(vformat("[codex_bridge] Resource snapshot %s froze %d records at revision %d.", snapshot_id, catalog.size(), snapshot_resource_revision));
	snapshot_resource_added = false;
	snapshot_dependency_index = 0;
	snapshot_diagnostic_index = 0;
	snapshot_pending_message.clear();
	snapshot_chunk_count = 0;
	snapshot_finishing = false;
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
	begin["context"] = snapshot_context;
	snapshot_pending_message = begin;
	snapshot_resource_count = 0;
	snapshot_dependency_count = 0;
	snapshot_diagnostic_count = 0;
	_reset_snapshot_payload();
	return OK;
}

void ResourceGraphAdapter::_reset_snapshot_payload() {
	snapshot_resources = Array();
	snapshot_dependencies = Array();
	snapshot_diagnostics = Array();
	Dictionary payload;
	payload["resources"] = snapshot_resources;
	payload["dependencies"] = snapshot_dependencies;
	payload["diagnostics"] = snapshot_diagnostics;
	snapshot_payload_bytes = JSON::stringify(payload, "", true, true).utf8().length();
}

Array ResourceGraphAdapter::_take_abandoned_snapshot_data() {
	Array abandoned;
	if (!snapshot_pending_message.is_empty()) {
		abandoned.push_back(snapshot_pending_message);
	}
	if (!snapshot_resources.is_empty()) {
		abandoned.push_back(snapshot_resources);
	}
	if (!snapshot_dependencies.is_empty()) {
		abandoned.push_back(snapshot_dependencies);
	}
	if (!snapshot_diagnostics.is_empty()) {
		abandoned.push_back(snapshot_diagnostics);
	}
	snapshot_pending_message = Dictionary();
	snapshot_resources = Array();
	snapshot_dependencies = Array();
	snapshot_diagnostics = Array();
	return abandoned;
}

bool ResourceGraphAdapter::_emit_pending_snapshot_message(SnapshotCompletion &r_completion) {
	if (snapshot_pending_message.is_empty()) {
		return false;
	}
	r_completion.ready = true;
	r_completion.request_id = snapshot_request_id;
	r_completion.server_message = snapshot_pending_message;
	snapshot_pending_message = Dictionary();
	return true;
}

bool ResourceGraphAdapter::_append_snapshot_value(Array &r_values, const Variant &p_value) {
	const uint64_t encoded_bytes = JSON::stringify(p_value, "", true, true).utf8().length();
	uint64_t additional_bytes = encoded_bytes + (r_values.is_empty() ? 0 : 1);
	if (snapshot_payload_bytes + additional_bytes > SNAPSHOT_BUILD_CHUNK_BYTES &&
			(!snapshot_resources.is_empty() || !snapshot_dependencies.is_empty() || !snapshot_diagnostics.is_empty())) {
		if (!_flush_snapshot_payload()) {
			return false;
		}
		additional_bytes = encoded_bytes;
	}
	if (snapshot_payload_bytes + additional_bytes > SNAPSHOT_CHUNK_BYTES) {
		return false;
	}
	r_values.push_back(p_value);
	snapshot_payload_bytes += additional_bytes;
	return true;
}

bool ResourceGraphAdapter::_flush_snapshot_payload() {
	if (!snapshot_pending_message.is_empty() || snapshot_chunk_count >= MAX_SNAPSHOT_CHUNKS) {
		return false;
	}
	Dictionary payload;
	payload["resources"] = snapshot_resources;
	payload["dependencies"] = snapshot_dependencies;
	payload["diagnostics"] = snapshot_diagnostics;
	if (snapshot_payload_bytes > SNAPSHOT_CHUNK_BYTES) {
		return false;
	}
	Dictionary chunk;
	chunk["protocol_version"] = "1.2";
	chunk["kind"] = "chunk";
	chunk["snapshot_id"] = snapshot_id;
	chunk["domain"] = "resource_graph";
	chunk["chunk_index"] = (int64_t)snapshot_chunk_count;
	chunk["payload"] = payload;
	chunk["context"] = snapshot_context;
	snapshot_pending_message = chunk;
	snapshot_chunk_count++;
	snapshot_resource_count += snapshot_resources.size();
	snapshot_dependency_count += snapshot_dependencies.size();
	snapshot_diagnostic_count += snapshot_diagnostics.size();
	_reset_snapshot_payload();
	return true;
}

void ResourceGraphAdapter::_fail_snapshot(const String &p_code, const String &p_message, bool p_retryable, SnapshotCompletion &r_completion) {
	r_completion.ready = true;
	r_completion.request_id = snapshot_request_id;
	r_completion.is_error = true;
	r_completion.error_code = p_code;
	r_completion.error_message = p_message;
	r_completion.error_retryable = p_retryable;
	r_completion.abandoned_messages = _take_abandoned_snapshot_data();
	snapshot_active = false;
	snapshot_waiting_for_terminal = false;
	snapshot_record = nullptr;
	snapshot_chunk_count = 0;
	snapshot_finishing = false;
}

void ResourceGraphAdapter::_finish_snapshot(SnapshotCompletion &r_completion) {
	if (!snapshot_finishing) {
		if ((!snapshot_resources.is_empty() || !snapshot_dependencies.is_empty() || !snapshot_diagnostics.is_empty() || snapshot_chunk_count == 0) && !_flush_snapshot_payload()) {
			_fail_snapshot("resource_limit_exceeded", "A resource graph record exceeds the snapshot chunk limit.", false, r_completion);
			return;
		}
		snapshot_finishing = true;
		if (_emit_pending_snapshot_message(r_completion)) {
			return;
		}
	}

	Dictionary end_params;
	end_params["snapshot_id"] = snapshot_id;
	end_params["domain"] = "resource_graph";
	end_params["resource_revision"] = (int64_t)snapshot_resource_revision;
	end_params["chunk_count"] = (int64_t)snapshot_chunk_count;
	end_params["resource_count"] = (int64_t)snapshot_resource_count;
	end_params["dependency_count"] = (int64_t)snapshot_dependency_count;
	end_params["diagnostic_count"] = (int64_t)snapshot_diagnostic_count;
	// The transport worker fills the checksum after serializing each payload off
	// the editor main thread.
	end_params["checksum"] = "";
	end_params["revisions"] = snapshot_revisions;
	Dictionary end;
	end["protocol_version"] = "1.2";
	end["kind"] = "notification";
	end["method"] = "snapshot.end";
	end["params"] = end_params;
	end["context"] = snapshot_context;

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
	r_completion.terminal = true;
	r_completion.request_id = snapshot_request_id;
	r_completion.result = result;
	r_completion.server_message = end;
	// The frozen generation remains active while the transport applies ACK
	// backpressure. COMMAND_CANCEL releases it on every terminal path.
	snapshot_waiting_for_terminal = true;
	snapshot_record = nullptr;
	snapshot_finishing = false;
}

bool ResourceGraphAdapter::process_snapshot(uint64_t p_now_usec, uint64_t p_budget_usec, SnapshotCompletion &r_completion) {
	r_completion = SnapshotCompletion();
	if (!snapshot_active) {
		return false;
	}
	if (snapshot_waiting_for_terminal) {
		return false;
	}
	if (_emit_pending_snapshot_message(r_completion)) {
		return true;
	}
	if (p_now_usec - snapshot_started_usec >= SNAPSHOT_TIMEOUT_USEC) {
		_fail_snapshot("snapshot_timeout", "The resource graph snapshot exceeded 120 seconds.", true, r_completion);
		return true;
	}
	const uint64_t started = OS::get_singleton()->get_ticks_usec();
	do {
		if (!snapshot_record) {
			_finish_snapshot(r_completion);
			return true;
		}
		const CatalogRecord &record = snapshot_record->value();
		if (!has_exact_keys(record.value, { "resource", "dependencies" }) || record.value["resource"].get_type() != Variant::DICTIONARY || record.value["dependencies"].get_type() != Variant::ARRAY) {
			_fail_snapshot("resource_limit_exceeded", "The resource graph contains an invalid catalog record.", false, r_completion);
			return true;
		}
		const Dictionary resource = record.value["resource"];
		const Array dependencies = record.value["dependencies"];
		if (!snapshot_resource_added) {
			if (!_validate_resource_observation(resource) || !_append_snapshot_value(snapshot_resources, resource)) {
				_fail_snapshot("resource_limit_exceeded", "A resource graph snapshot value exceeds the chunk limit.", false, r_completion);
				return true;
			}
			snapshot_resource_added = true;
		} else if (snapshot_dependency_index < dependencies.size()) {
			const Variant dependency_value = dependencies[snapshot_dependency_index++];
			if (dependency_value.get_type() != Variant::DICTIONARY) {
				_fail_snapshot("resource_limit_exceeded", "The resource graph contains an invalid dependency record.", false, r_completion);
				return true;
			}
			const Dictionary dependency = dependency_value;
			if (!_validate_dependency_observation(dependency) || dependency["source_ref"] != resource["resource_ref"] || !_append_snapshot_value(snapshot_dependencies, dependency)) {
				_fail_snapshot("resource_limit_exceeded", "A resource graph snapshot value exceeds the chunk limit.", false, r_completion);
				return true;
			}
		} else if (snapshot_diagnostic_index < record.diagnostics.size()) {
			const Variant diagnostic_value = record.diagnostics[snapshot_diagnostic_index++];
			if (diagnostic_value.get_type() != Variant::DICTIONARY || !_validate_diagnostic(diagnostic_value) || !_append_snapshot_value(snapshot_diagnostics, diagnostic_value)) {
				_fail_snapshot("resource_limit_exceeded", "A resource graph snapshot value exceeds the chunk limit.", false, r_completion);
				return true;
			}
		} else {
			snapshot_record = snapshot_record->next();
			snapshot_resource_added = false;
			snapshot_dependency_index = 0;
			snapshot_diagnostic_index = 0;
		}
		if (_emit_pending_snapshot_message(r_completion)) {
			return true;
		}
	} while (OS::get_singleton()->get_ticks_usec() - started < p_budget_usec);
	return true;
}

Array ResourceGraphAdapter::cancel_snapshot(uint64_t p_request_id) {
	if (!snapshot_active || snapshot_request_id != p_request_id) {
		return Array();
	}
	Array abandoned = _take_abandoned_snapshot_data();
	snapshot_active = false;
	snapshot_waiting_for_terminal = false;
	snapshot_record = nullptr;
	snapshot_chunk_count = 0;
	snapshot_finishing = false;
	return abandoned;
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
