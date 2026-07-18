/**************************************************************************/
/*  script_graph_adapter.cpp                                              */
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

#include "script_graph_adapter.h"

#include "bridge_revision_clock.h"

#include "core/io/file_access.h"
#include "core/io/json.h"
#include "core/io/resource_uid.h"
#include "core/object/worker_thread_pool.h"
#include "core/os/os.h"
#include "core/os/thread.h"
#include "editor/file_system/editor_file_system.h"

#include "modules/codex_bridge/protocol/bridge_crypto.h"

String ScriptGraphAdapter::_make_snapshot_id() {
	PackedByteArray random;
	if (BridgeCrypto::random_bytes(16, random) != OK) {
		return "snapshot:" + String("0").repeat(32);
	}
	return "snapshot:" + BridgeCrypto::bytes_to_lower_hex(random);
}

Dictionary ScriptGraphAdapter::_make_script_ref(const String &p_path, int64_t p_uid) {
	Dictionary script_ref;
	if (p_uid != ResourceUID::INVALID_ID && ResourceUID::get_singleton()) {
		script_ref["uid"] = ResourceUID::get_singleton()->id_to_text(p_uid);
	} else {
		script_ref["uid_missing"] = true;
		script_ref["path"] = p_path;
	}
	return script_ref;
}

Dictionary ScriptGraphAdapter::_make_raw_script_ref(const String &p_path) {
	ResourceUID *registry = ResourceUID::get_singleton();
	ResourceUID::ID uid = registry ? registry->get_path_id(p_path) : ResourceUID::INVALID_ID;
	if (uid == ResourceUID::INVALID_ID && registry) {
		const String uid_text = FileAccess::get_file_as_string(p_path + ".uid").strip_edges();
		const ResourceUID::ID sidecar_uid = registry->text_to_id(uid_text);
		if (sidecar_uid != ResourceUID::INVALID_ID && registry->id_to_text(sidecar_uid) == uid_text) {
			uid = sidecar_uid;
		}
	}
	return _make_script_ref(p_path, uid);
}

String ScriptGraphAdapter::_script_ref_key(const Dictionary &p_script_ref) {
	if (p_script_ref.has("uid") && p_script_ref["uid"].get_type() == Variant::STRING) {
		return "uid:" + String(p_script_ref["uid"]);
	}
	if (p_script_ref.has("uid_missing") && p_script_ref["uid_missing"].get_type() == Variant::BOOL && bool(p_script_ref["uid_missing"]) &&
			p_script_ref.has("path") && p_script_ref["path"].get_type() == Variant::STRING) {
		return "path:" + String(p_script_ref["path"]);
	}
	return String();
}

bool ScriptGraphAdapter::_is_script_path(const String &p_path) {
	return ScriptSemanticAdapter::_is_valid_script_path(p_path);
}

uint64_t ScriptGraphAdapter::_bundle_count(const Dictionary &p_bundle, const String &p_key) {
	if (!p_bundle.has(p_key) || p_bundle[p_key].get_type() != Variant::ARRAY) {
		return UINT64_MAX;
	}
	return Array(p_bundle[p_key]).size();
}

bool ScriptGraphAdapter::_account_bundle(const Dictionary &p_bundle) {
	const uint64_t symbols = _bundle_count(p_bundle, "symbols");
	const uint64_t relations = _bundle_count(p_bundle, "relations");
	const uint64_t diagnostics = _bundle_count(p_bundle, "diagnostics");
	if (symbols == UINT64_MAX || relations == UINT64_MAX || diagnostics == UINT64_MAX ||
			symbols > ScriptSemanticAdapter::MAX_SYMBOLS_PER_DOCUMENT || relations > ScriptSemanticAdapter::MAX_RELATIONS_PER_DOCUMENT || diagnostics > ScriptSemanticAdapter::MAX_DIAGNOSTICS_PER_DOCUMENT ||
			observed_symbol_count > ScriptSemanticAdapter::MAX_SYMBOLS - symbols || observed_relation_count > ScriptSemanticAdapter::MAX_RELATIONS - relations || observed_diagnostic_count > ScriptSemanticAdapter::MAX_DIAGNOSTICS - diagnostics) {
		return false;
	}
	observed_symbol_count += symbols;
	observed_relation_count += relations;
	observed_diagnostic_count += diagnostics;
	return true;
}

void ScriptGraphAdapter::_reset_refresh() {
	ERR_FAIL_COND(!refresh_files.is_empty());
	ERR_FAIL_COND(!refresh_paths.is_empty());
	ERR_FAIL_COND(!observed_catalog.is_empty());
	ERR_FAIL_COND(!retiring_catalog.is_empty() || !pending_retiring_catalog.is_empty() || !retiring_bundles.is_empty());
	ERR_FAIL_COND(projection_preparing || projection_task >= 0 || !projection_result.bundle.is_empty());
	directory_stack.clear();
	raw_directory_stack.clear();
	raw_scan_started = false;
	refresh_file = nullptr;
	projection_file = RefreshFile();
	projection_resource_revision = 1;
	projection_error = OK;
	observed_symbol_count = 0;
	observed_relation_count = 0;
	observed_diagnostic_count = 0;
	refresh_limit_exceeded = false;
	refresh_projection_failed = false;
	drain_resume_phase = REFRESH_IDLE;
	refresh_phase = REFRESH_IDLE;
}

void ScriptGraphAdapter::_enqueue_catalog_retirement(RBMap<String, CatalogRecord> &r_source) {
	if (r_source.is_empty()) {
		return;
	}
	if (retiring_catalog.is_empty()) {
		retiring_catalog = std::move(r_source);
		return;
	}
	ERR_FAIL_COND_MSG(!pending_retiring_catalog.is_empty(), "ScriptGraphAdapter retirement queue overflowed.");
	pending_retiring_catalog = std::move(r_source);
}

void ScriptGraphAdapter::_flush_retirement_batch() {
	if (!retiring_bundles.is_empty()) {
		journal._retire_array(retiring_bundles);
	}
	retiring_source_bytes = 0;
}

bool ScriptGraphAdapter::_drain_retired_catalog_step() {
	if (retiring_catalog.is_empty() && !pending_retiring_catalog.is_empty()) {
		retiring_catalog = std::move(pending_retiring_catalog);
	}
	RBMap<String, CatalogRecord>::Element *record = retiring_catalog.front();
	if (!record) {
		_flush_retirement_batch();
		return false;
	}
	const uint64_t source_bytes = record->value().source_bytes;
	if (!retiring_bundles.is_empty() &&
			(retiring_bundles.size() >= (int)MAX_RETIRE_RECORDS_PER_BATCH ||
					retiring_source_bytes > MAX_RETIRE_SOURCE_BYTES_PER_BATCH - MIN(source_bytes, MAX_RETIRE_SOURCE_BYTES_PER_BATCH))) {
		_flush_retirement_batch();
	}
	Dictionary bundle = record->value().bundle;
	record->value().bundle = Dictionary();
	retiring_bundles.push_back(bundle);
	retiring_source_bytes += MIN(source_bytes, MAX_RETIRE_SOURCE_BYTES_PER_BATCH);
	retiring_catalog.erase(record);
	if (retiring_bundles.size() >= (int)MAX_RETIRE_RECORDS_PER_BATCH || retiring_source_bytes >= MAX_RETIRE_SOURCE_BYTES_PER_BATCH || (retiring_catalog.is_empty() && pending_retiring_catalog.is_empty())) {
		_flush_retirement_batch();
	}
	return true;
}

void ScriptGraphAdapter::_begin_refresh_drain(RefreshPhase p_resume_phase) {
	directory_stack.clear();
	raw_directory_stack.clear();
	refresh_file = nullptr;
	drain_resume_phase = p_resume_phase;
	if (!refresh_files.is_empty()) {
		refresh_phase = REFRESH_DRAIN_FILES;
	} else if (!retiring_catalog.is_empty() || !pending_retiring_catalog.is_empty() || !retiring_bundles.is_empty()) {
		refresh_phase = REFRESH_DRAIN_CATALOG;
	} else {
		_finish_refresh_drain();
	}
}

void ScriptGraphAdapter::_finish_refresh_drain() {
	ERR_FAIL_COND(!refresh_files.is_empty());
	ERR_FAIL_COND(!refresh_paths.is_empty());
	ERR_FAIL_COND(!observed_catalog.is_empty());
	ERR_FAIL_COND(!retiring_catalog.is_empty() || !pending_retiring_catalog.is_empty() || !retiring_bundles.is_empty());
	refresh_file = nullptr;
	observed_symbol_count = 0;
	observed_relation_count = 0;
	observed_diagnostic_count = 0;
	refresh_limit_exceeded = false;
	refresh_projection_failed = false;
	const RefreshPhase resume_phase = drain_resume_phase;
	drain_resume_phase = REFRESH_IDLE;
	refresh_phase = resume_phase;
}

bool ScriptGraphAdapter::_begin_refresh() {
	EditorFileSystem *filesystem = EditorFileSystem::get_singleton();
	if (!filesystem || filesystem->doing_first_scan() || filesystem->is_scanning() || filesystem->is_importing() || !filesystem->get_filesystem()) {
		return false;
	}
	_reset_refresh();
	refresh_requested = false;
	target_resource_revision = revision_clock ? revision_clock->get_resource_revision() : 1;
	target_script_graph_revision = catalog_ready ? (revision_clock ? revision_clock->get_script_graph_revision() + 1 : journal.get_current_script_graph_revision() + 1) : (revision_clock ? revision_clock->get_script_graph_revision() : journal.get_current_script_graph_revision());
	DirectoryCursor root;
	root.directory = filesystem->get_filesystem();
	directory_stack.push_back(root);
	refresh_phase = REFRESH_COLLECT;
	return true;
}

bool ScriptGraphAdapter::_begin_raw_scan() {
	raw_scan_started = true;
	Error open_error = OK;
	Ref<DirAccess> root = DirAccess::open("res://", &open_error);
	if (open_error != OK || root.is_null()) {
		refresh_projection_failed = true;
		refresh_phase = REFRESH_RECONCILE_BEGIN;
		return false;
	}
	root->set_include_hidden(false);
	root->set_include_navigational(false);
	root->list_dir_begin();
	RawDirectoryCursor cursor;
	cursor.directory = root;
	cursor.path = "res://";
	raw_directory_stack.push_back(cursor);
	return true;
}

bool ScriptGraphAdapter::_collect_one_raw_path() {
	while (!raw_directory_stack.is_empty()) {
		RawDirectoryCursor &cursor = raw_directory_stack.write[raw_directory_stack.size() - 1];
		if (cursor.directory.is_null()) {
			refresh_projection_failed = true;
			refresh_phase = REFRESH_RECONCILE_BEGIN;
			return false;
		}
		const String entry = cursor.directory->get_next();
		if (entry.is_empty()) {
			cursor.directory->list_dir_end();
			raw_directory_stack.resize(raw_directory_stack.size() - 1);
			return true;
		}
		if (entry.begins_with(".") || cursor.directory->is_link(entry)) {
			return true;
		}
		const String path = cursor.path.path_join(entry);
		if (cursor.directory->current_is_dir()) {
			Error open_error = OK;
			Ref<DirAccess> child = DirAccess::open(path, &open_error);
			if (open_error != OK || child.is_null()) {
				refresh_projection_failed = true;
				refresh_phase = REFRESH_RECONCILE_BEGIN;
				return false;
			}
			if (child->file_exists(".gdignore")) {
				return true;
			}
			child->set_include_hidden(false);
			child->set_include_navigational(false);
			child->list_dir_begin();
			RawDirectoryCursor child_cursor;
			child_cursor.directory = child;
			child_cursor.path = path;
			raw_directory_stack.push_back(child_cursor);
			return true;
		}
		if (!path.ends_with(".cs")) {
			return true;
		}
		if (refresh_paths.has(path)) {
			return true;
		}
		if (refresh_files.size() >= (int)ScriptSemanticAdapter::MAX_DOCUMENTS) {
			refresh_limit_exceeded = true;
			refresh_phase = REFRESH_RECONCILE_BEGIN;
			return false;
		}
		const Dictionary script_ref = _make_raw_script_ref(path);
		const String key = _script_ref_key(script_ref);
		if (key.is_empty()) {
			refresh_projection_failed = true;
			refresh_phase = REFRESH_RECONCILE_BEGIN;
			return false;
		}
		if (refresh_files.has(key)) {
			const RefreshFile &existing = refresh_files[key];
			if (existing.path != path) {
				refresh_limit_exceeded = true;
				refresh_phase = REFRESH_RECONCILE_BEGIN;
				return false;
			}
			return true;
		}
		RefreshFile file;
		file.key = key;
		file.path = path;
		file.script_ref = script_ref;
		refresh_files.insert(key, file);
		refresh_paths.insert(path);
		return true;
	}
	refresh_file = refresh_files.front();
	refresh_phase = REFRESH_PROJECT;
	return false;
}

bool ScriptGraphAdapter::_collect_one_path() {
	while (!directory_stack.is_empty()) {
		DirectoryCursor &cursor = directory_stack.write[directory_stack.size() - 1];
		if (!cursor.directory) {
			refresh_limit_exceeded = true;
			refresh_phase = REFRESH_RECONCILE_BEGIN;
			return false;
		}
		if (cursor.file_index < cursor.directory->get_file_count()) {
			const int file_index = cursor.file_index++;
			const String path = cursor.directory->get_file_path(file_index);
			if (!_is_script_path(path)) {
				return true;
			}
			if (refresh_files.size() >= (int)ScriptSemanticAdapter::MAX_DOCUMENTS) {
				refresh_limit_exceeded = true;
				refresh_phase = REFRESH_RECONCILE_BEGIN;
				return false;
			}
			const Dictionary script_ref = _make_script_ref(path, cursor.directory->get_file_uid(file_index));
			const String key = _script_ref_key(script_ref);
			if (key.is_empty() || refresh_files.has(key) || refresh_paths.has(path)) {
				refresh_limit_exceeded = true;
				refresh_phase = REFRESH_RECONCILE_BEGIN;
				return false;
			}
			RefreshFile file;
			file.key = key;
			file.path = path;
			file.script_ref = script_ref;
			refresh_files.insert(key, file);
			refresh_paths.insert(path);
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
	if (!raw_scan_started && !_begin_raw_scan()) {
		return false;
	}
	return _collect_one_raw_path();
}

void ScriptGraphAdapter::_project_document_thread(void *p_userdata) {
	ScriptGraphAdapter *adapter = static_cast<ScriptGraphAdapter *>(p_userdata);
	adapter->projection_error = ScriptSemanticAdapter::project_saved_document(
			adapter->projection_file.path,
			adapter->projection_file.script_ref,
			adapter->projection_resource_revision,
			adapter->target_script_graph_revision,
			adapter->projection_result);
}

void ScriptGraphAdapter::_wait_for_projection() {
	if (projection_task < 0) {
		return;
	}
	WorkerThreadPool *worker_pool = WorkerThreadPool::get_singleton();
	if (worker_pool) {
		worker_pool->wait_for_task_completion(projection_task);
	}
	projection_task = -1;
}

bool ScriptGraphAdapter::_project_one_document() {
	if (!projection_preparing) {
		if (!refresh_file) {
			refresh_phase = REFRESH_RECONCILE_BEGIN;
			return false;
		}
		projection_file = refresh_file->value();
		projection_resource_revision = target_resource_revision;
		projection_error = OK;
		projection_result = ScriptSemanticAdapter::DocumentProjection();
		projection_preparing = true;
		WorkerThreadPool *worker_pool = WorkerThreadPool::get_singleton();
		if (worker_pool) {
			projection_task = worker_pool->add_native_task(_project_document_thread, this, false, SNAME("CodexScriptProjection"));
		}
		if (projection_task < 0) {
			_project_document_thread(this);
		}
		return false;
	}

	WorkerThreadPool *worker_pool = WorkerThreadPool::get_singleton();
	if (projection_task >= 0 && worker_pool && !worker_pool->is_task_completed(projection_task)) {
		return false;
	}
	_wait_for_projection();
	projection_preparing = false;
	const RefreshFile file = projection_file;
	projection_file = RefreshFile();
	projection_resource_revision = 1;
	const Error error = projection_error;
	projection_error = OK;
	ScriptSemanticAdapter::DocumentProjection projection = projection_result;
	projection_result = ScriptSemanticAdapter::DocumentProjection();
	RBMap<String, RefreshFile>::Element *active_file = refresh_files.find(file.key);
	if (!active_file || active_file != refresh_file) {
		refresh_paths.erase(file.path);
		if (!projection.bundle.is_empty()) {
			journal._retire_dictionary(projection.bundle);
		}
		refresh_projection_failed = true;
		refresh_phase = REFRESH_RECONCILE_BEGIN;
		return false;
	}
	RBMap<String, RefreshFile>::Element *next_file = active_file->next();
	if (error == ERR_BUSY) {
		if (!projection.bundle.is_empty()) {
			journal._retire_dictionary(projection.bundle);
		}
		refresh_requested = true;
		_enqueue_catalog_retirement(observed_catalog);
		_begin_refresh_drain(REFRESH_IDLE);
		return false;
	}
	if (error != OK || projection.bundle.is_empty() || projection.facts_checksum.is_empty()) {
		if (!projection.bundle.is_empty()) {
			journal._retire_dictionary(projection.bundle);
		}
		refresh_limit_exceeded = error == ERR_OUT_OF_MEMORY;
		refresh_projection_failed = !refresh_limit_exceeded;
		refresh_phase = REFRESH_RECONCILE_BEGIN;
		return false;
	}

	CatalogRecord record;
	record.bundle = projection.bundle;
	projection.bundle = Dictionary();
	record.facts_checksum = projection.facts_checksum;
	record.source_modified_time = projection.source_modified_time;
	record.source_bytes = projection.source_bytes;
	const RBMap<String, CatalogRecord>::Element *existing = catalog.find(file.key);
	if (existing && existing->value().facts_checksum == record.facts_checksum) {
		journal._retire_dictionary(record.bundle);
		record = existing->value();
	}
	if (!_account_bundle(record.bundle)) {
		journal._retire_dictionary(record.bundle);
		refresh_limit_exceeded = true;
		refresh_phase = REFRESH_RECONCILE_BEGIN;
		return false;
	}
	observed_catalog.insert(file.key, record);
	refresh_paths.erase(file.path);
	refresh_files.erase(active_file);
	refresh_file = next_file;
	return true;
}

void ScriptGraphAdapter::_reset_reconcile() {
	journal._retire_array(reconcile_operations);
	journal._retire_prepared_batch(journal_prepared_batch);
	reconcile_observed = nullptr;
	reconcile_existing = nullptr;
	reconcile_previous_revision = 0;
	reconcile_next_revision = 0;
	reconcile_preparing = false;
	journal_prepare_task = -1;
	journal_prepare_error = OK;
}

void ScriptGraphAdapter::_prepare_journal_batch_thread(void *p_userdata) {
	ScriptGraphAdapter *adapter = static_cast<ScriptGraphAdapter *>(p_userdata);
	adapter->journal_prepare_error = ScriptDeltaJournal::prepare_batch(adapter->reconcile_next_revision, adapter->reconcile_operations, adapter->journal_prepared_batch);
}

void ScriptGraphAdapter::_wait_for_journal_preparation() {
	if (journal_prepare_task < 0) {
		return;
	}
	WorkerThreadPool *worker_pool = WorkerThreadPool::get_singleton();
	if (worker_pool) {
		worker_pool->wait_for_task_completion(journal_prepare_task);
	}
	journal_prepare_task = -1;
}

bool ScriptGraphAdapter::_reconcile(RefreshOutcome &r_outcome) {
	switch (refresh_phase) {
		case REFRESH_RECONCILE_BEGIN: {
			if (refresh_limit_exceeded || refresh_projection_failed) {
				refresh_requested = false;
				const uint64_t previous = revision_clock ? revision_clock->get_script_graph_revision() : journal.get_current_script_graph_revision();
				const uint64_t current = revision_clock ? revision_clock->record_script_graph_change() : previous + 1;
				journal.invalidate_to(current);
				catalog_limit_exceeded = refresh_limit_exceeded;
				catalog_ready = false;
				r_outcome.invalidated = true;
				r_outcome.last_contiguous_script_graph_revision = previous;
				r_outcome.current_script_graph_revision = current;
				r_outcome.revisions = revision_clock ? revision_clock->get_revision_vector() : Dictionary();
				_reset_reconcile();
				_enqueue_catalog_retirement(catalog);
				_enqueue_catalog_retirement(observed_catalog);
				_begin_refresh_drain(journal.is_invalidating() ? REFRESH_DRAIN_JOURNAL : REFRESH_IDLE);
				return true;
			}
			if (!catalog_ready) {
				catalog = std::move(observed_catalog);
				catalog_ready = true;
				catalog_limit_exceeded = false;
				_reset_reconcile();
				_reset_refresh();
				return true;
			}
			_reset_reconcile();
			reconcile_operations = Array();
			reconcile_observed = observed_catalog.front();
			refresh_phase = REFRESH_RECONCILE_OBSERVED;
			return true;
		}
		case REFRESH_RECONCILE_OBSERVED: {
			if (!reconcile_observed) {
				reconcile_existing = catalog.front();
				refresh_phase = REFRESH_RECONCILE_REMOVED;
				return true;
			}
			RBMap<String, CatalogRecord>::Element *record = reconcile_observed;
			reconcile_observed = reconcile_observed->next();
			const RBMap<String, CatalogRecord>::Element *existing = catalog.find(record->key());
			if (!existing || existing->value().facts_checksum != record->value().facts_checksum) {
				Dictionary operation;
				operation["kind"] = "upsert_document";
				operation["value"] = record->value().bundle;
				reconcile_operations.push_back(operation);
			}
			return true;
		}
		case REFRESH_RECONCILE_REMOVED: {
			if (!reconcile_existing) {
				refresh_phase = REFRESH_RECONCILE_PREPARE;
				return true;
			}
			RBMap<String, CatalogRecord>::Element *record = reconcile_existing;
			reconcile_existing = reconcile_existing->next();
			if (!observed_catalog.has(record->key())) {
				const Dictionary document = record->value().bundle["document"];
				Dictionary operation;
				operation["kind"] = "remove_document";
				operation["script_ref"] = document["script_ref"];
				operation["path"] = document["path"];
				reconcile_operations.push_back(operation);
			}
			return true;
		}
		case REFRESH_RECONCILE_PREPARE: {
			if (reconcile_operations.is_empty()) {
				_enqueue_catalog_retirement(observed_catalog);
				_reset_reconcile();
				_begin_refresh_drain(REFRESH_IDLE);
				return true;
			}

			reconcile_previous_revision = revision_clock ? revision_clock->get_script_graph_revision() : journal.get_current_script_graph_revision();
			reconcile_next_revision = reconcile_previous_revision + 1;
			reconcile_preparing = true;
			journal_prepare_error = OK;
			journal_prepared_batch = ScriptDeltaJournal::PreparedBatch();
			WorkerThreadPool *worker_pool = WorkerThreadPool::get_singleton();
			if (worker_pool) {
				journal_prepare_task = worker_pool->add_native_task(_prepare_journal_batch_thread, this, false, SNAME("CodexScriptDeltaBatch"));
			}
			if (journal_prepare_task < 0) {
				_prepare_journal_batch_thread(this);
			}
			refresh_phase = REFRESH_RECONCILE_WAIT;
			return false;
		}
		case REFRESH_RECONCILE_WAIT: {
			WorkerThreadPool *worker_pool = WorkerThreadPool::get_singleton();
			if (journal_prepare_task >= 0 && worker_pool && !worker_pool->is_task_completed(journal_prepare_task)) {
				return false;
			}
			_wait_for_journal_preparation();
			const uint64_t current = revision_clock ? revision_clock->record_script_graph_change() : reconcile_next_revision;
			const Dictionary revisions = revision_clock ? revision_clock->get_revision_vector() : Dictionary();
			bool invalidated = current != reconcile_next_revision || journal_prepare_error != OK;
			if (!invalidated) {
				Dictionary batch;
				const uint64_t resource_revision = revisions.get("resource_revision", 1);
				const uint64_t scene_graph_revision = revisions.get("scene_graph_revision", 1);
				const uint64_t project_revision = revisions.get("project_revision", 0);
				if (journal.commit_prepared(current, resource_revision, scene_graph_revision, project_revision, journal_prepared_batch, batch, invalidated) != OK) {
					invalidated = true;
				}
			}
			if (invalidated) {
				journal.invalidate_to(current);
			}
			_enqueue_catalog_retirement(catalog);
			catalog = std::move(observed_catalog);
			r_outcome.changed = !invalidated;
			r_outcome.invalidated = invalidated;
			r_outcome.last_contiguous_script_graph_revision = reconcile_previous_revision;
			r_outcome.current_script_graph_revision = current;
			r_outcome.revisions = revisions;
			_reset_reconcile();
			_begin_refresh_drain(journal.is_invalidating() ? REFRESH_DRAIN_JOURNAL : REFRESH_IDLE);
			return true;
		}
		default:
			break;
	}
	return false;
}

bool ScriptGraphAdapter::_process_refresh_step(RefreshOutcome &r_outcome) {
	switch (refresh_phase) {
		case REFRESH_COLLECT:
			return _collect_one_path();
		case REFRESH_PROJECT:
			return _project_one_document();
		case REFRESH_RECONCILE_BEGIN:
		case REFRESH_RECONCILE_OBSERVED:
		case REFRESH_RECONCILE_REMOVED:
		case REFRESH_RECONCILE_PREPARE:
		case REFRESH_RECONCILE_WAIT:
			return _reconcile(r_outcome);
		case REFRESH_DRAIN_FILES: {
			RBMap<String, RefreshFile>::Element *file = refresh_files.front();
			if (file) {
				refresh_paths.erase(file->value().path);
				refresh_files.erase(file);
				return true;
			}
			if (!retiring_catalog.is_empty() || !pending_retiring_catalog.is_empty() || !retiring_bundles.is_empty()) {
				refresh_phase = REFRESH_DRAIN_CATALOG;
			} else {
				_finish_refresh_drain();
			}
			return true;
		}
		case REFRESH_DRAIN_CATALOG:
			if (!_drain_retired_catalog_step()) {
				_finish_refresh_drain();
			}
			return true;
		case REFRESH_DRAIN_JOURNAL:
			if (!journal.drain_invalidation_step()) {
				refresh_phase = REFRESH_IDLE;
			}
			return true;
		case REFRESH_IDLE:
			break;
	}
	return false;
}

void ScriptGraphAdapter::initialize(BridgeRevisionClock *p_revision_clock) {
	revision_clock = p_revision_clock;
	journal.initialize(revision_clock ? revision_clock->get_script_graph_revision() : 1);
	adapter_statuses = ScriptSemanticAdapter::make_adapter_statuses();
	_reset_reconcile();
	_reset_snapshot();
	if (ScriptSemanticAdapter::is_gdscript_available()) {
		request_refresh();
	}
}

void ScriptGraphAdapter::shutdown() {
	_wait_for_projection();
	projection_preparing = false;
	if (!projection_result.bundle.is_empty()) {
		journal._retire_dictionary(projection_result.bundle);
	}
	projection_result = ScriptSemanticAdapter::DocumentProjection();
	projection_file = RefreshFile();
	projection_error = OK;
	_wait_for_journal_preparation();
	_reset_reconcile();
	_reset_snapshot();
	directory_stack.clear();
	raw_directory_stack.clear();
	refresh_file = nullptr;
	while (!refresh_files.is_empty()) {
		RBMap<String, RefreshFile>::Element *file = refresh_files.front();
		refresh_paths.erase(file->value().path);
		refresh_files.erase(file);
	}
	while (_drain_retired_catalog_step()) {
	}
	_enqueue_catalog_retirement(catalog);
	_enqueue_catalog_retirement(observed_catalog);
	while (_drain_retired_catalog_step()) {
	}
	_reset_refresh();
	journal._retire_array(adapter_statuses);
	catalog_ready = false;
	catalog_limit_exceeded = false;
	refresh_requested = false;
	revision_clock = nullptr;
	while (journal.drain_invalidation_step()) {
	}
	journal._wait_for_cleanup();
}

void ScriptGraphAdapter::request_refresh() {
	if (ScriptSemanticAdapter::is_gdscript_available()) {
		refresh_requested = true;
	}
}

bool ScriptGraphAdapter::process_refresh(uint64_t p_budget_usec, RefreshOutcome &r_outcome) {
	r_outcome = RefreshOutcome();
	ERR_FAIL_COND_V_MSG(!Thread::is_main_thread(), false, "ScriptGraphAdapter must run on the main thread.");
	if (refresh_phase == REFRESH_IDLE && refresh_requested && !_begin_refresh()) {
		return false;
	}
	if (refresh_phase == REFRESH_IDLE) {
		return false;
	}
	const uint64_t started = OS::get_singleton()->get_ticks_usec();
	do {
		_process_refresh_step(r_outcome);
		if (r_outcome.changed || r_outcome.invalidated || refresh_phase == REFRESH_IDLE) {
			return true;
		}
		if (refresh_phase == REFRESH_RECONCILE_WAIT || refresh_phase == REFRESH_DRAIN_JOURNAL) {
			return false;
		}
		if (projection_preparing) {
			return false;
		}
	} while (OS::get_singleton()->get_ticks_usec() - started < p_budget_usec);
	return false;
}

void ScriptGraphAdapter::_reset_snapshot_payload() {
	snapshot_documents = Array();
	snapshot_symbols = Array();
	snapshot_relations = Array();
	snapshot_diagnostics = Array();
	snapshot_adapter_statuses = Array();
	Dictionary payload;
	payload["documents"] = snapshot_documents;
	payload["symbols"] = snapshot_symbols;
	payload["relations"] = snapshot_relations;
	payload["diagnostics"] = snapshot_diagnostics;
	payload["adapter_statuses"] = snapshot_adapter_statuses;
	snapshot_payload_bytes = JSON::stringify(payload, "", true, true).utf8().length();
}

void ScriptGraphAdapter::_reset_snapshot() {
	snapshot_active = false;
	snapshot_waiting_for_terminal = false;
	snapshot_request_id = 0;
	snapshot_started_usec = 0;
	snapshot_id.clear();
	snapshot_resource_revision = 0;
	snapshot_scene_graph_revision = 0;
	snapshot_script_graph_revision = 0;
	snapshot_revisions = Dictionary();
	snapshot_context = Dictionary();
	snapshot_record = nullptr;
	snapshot_document_added = false;
	snapshot_symbol_index = 0;
	snapshot_relation_index = 0;
	snapshot_diagnostic_index = 0;
	snapshot_adapter_status_index = 0;
	snapshot_pending_message = Dictionary();
	snapshot_chunk_count = 0;
	snapshot_document_count = 0;
	snapshot_symbol_count = 0;
	snapshot_relation_count = 0;
	snapshot_diagnostic_count = 0;
	snapshot_adapter_status_count = 0;
	snapshot_finishing = false;
	_reset_snapshot_payload();
}

Array ScriptGraphAdapter::_take_abandoned_snapshot_data() {
	Array abandoned;
	if (!snapshot_pending_message.is_empty()) {
		abandoned.push_back(snapshot_pending_message);
	}
	const Array values[] = { snapshot_documents, snapshot_symbols, snapshot_relations, snapshot_diagnostics, snapshot_adapter_statuses };
	for (const Array &value : values) {
		if (!value.is_empty()) {
			abandoned.push_back(value);
		}
	}
	snapshot_pending_message = Dictionary();
	_reset_snapshot_payload();
	return abandoned;
}

bool ScriptGraphAdapter::_emit_pending_snapshot_message(SnapshotCompletion &r_completion) {
	if (snapshot_pending_message.is_empty()) {
		return false;
	}
	r_completion.ready = true;
	r_completion.request_id = snapshot_request_id;
	r_completion.server_message = snapshot_pending_message;
	snapshot_pending_message = Dictionary();
	return true;
}

bool ScriptGraphAdapter::_append_snapshot_value(Array &r_values, const Variant &p_value) {
	const uint64_t encoded_bytes = JSON::stringify(p_value, "", true, true).utf8().length();
	uint64_t additional_bytes = encoded_bytes + (r_values.is_empty() ? 0 : 1);
	if (snapshot_payload_bytes + additional_bytes > SNAPSHOT_CHUNK_BYTES &&
			(!snapshot_documents.is_empty() || !snapshot_symbols.is_empty() || !snapshot_relations.is_empty() || !snapshot_diagnostics.is_empty() || !snapshot_adapter_statuses.is_empty())) {
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

bool ScriptGraphAdapter::_flush_snapshot_payload() {
	if (!snapshot_pending_message.is_empty() || snapshot_chunk_count >= MAX_SNAPSHOT_CHUNKS || snapshot_payload_bytes > SNAPSHOT_CHUNK_BYTES) {
		return false;
	}
	Dictionary payload;
	payload["documents"] = snapshot_documents;
	payload["symbols"] = snapshot_symbols;
	payload["relations"] = snapshot_relations;
	payload["diagnostics"] = snapshot_diagnostics;
	payload["adapter_statuses"] = snapshot_adapter_statuses;
	Dictionary chunk;
	chunk["protocol_version"] = "1.4";
	chunk["kind"] = "chunk";
	chunk["snapshot_id"] = snapshot_id;
	chunk["domain"] = "script_graph";
	chunk["chunk_index"] = (int64_t)snapshot_chunk_count;
	chunk["payload"] = payload;
	chunk["context"] = snapshot_context;
	snapshot_pending_message = chunk;
	snapshot_chunk_count++;
	snapshot_document_count += snapshot_documents.size();
	snapshot_symbol_count += snapshot_symbols.size();
	snapshot_relation_count += snapshot_relations.size();
	snapshot_diagnostic_count += snapshot_diagnostics.size();
	snapshot_adapter_status_count += snapshot_adapter_statuses.size();
	_reset_snapshot_payload();
	return true;
}

void ScriptGraphAdapter::_fail_snapshot(const String &p_code, const String &p_message, bool p_retryable, SnapshotCompletion &r_completion) {
	r_completion.ready = true;
	r_completion.request_id = snapshot_request_id;
	r_completion.is_error = true;
	r_completion.error_code = p_code;
	r_completion.error_message = p_message;
	r_completion.error_retryable = p_retryable;
	r_completion.abandoned_messages = _take_abandoned_snapshot_data();
	_reset_snapshot();
}

void ScriptGraphAdapter::_finish_snapshot(SnapshotCompletion &r_completion) {
	if (!snapshot_finishing) {
		if ((!snapshot_documents.is_empty() || !snapshot_symbols.is_empty() || !snapshot_relations.is_empty() || !snapshot_diagnostics.is_empty() || !snapshot_adapter_statuses.is_empty() || snapshot_chunk_count == 0) && !_flush_snapshot_payload()) {
			_fail_snapshot("script_limit_exceeded", "A script graph record exceeds the snapshot chunk limit.", false, r_completion);
			return;
		}
		snapshot_finishing = true;
		if (_emit_pending_snapshot_message(r_completion)) {
			return;
		}
	}

	Dictionary end_params;
	end_params["snapshot_id"] = snapshot_id;
	end_params["domain"] = "script_graph";
	end_params["resource_revision"] = (int64_t)snapshot_resource_revision;
	end_params["scene_graph_revision"] = (int64_t)snapshot_scene_graph_revision;
	end_params["script_graph_revision"] = (int64_t)snapshot_script_graph_revision;
	end_params["chunk_count"] = (int64_t)snapshot_chunk_count;
	end_params["document_count"] = (int64_t)snapshot_document_count;
	end_params["symbol_count"] = (int64_t)snapshot_symbol_count;
	end_params["relation_count"] = (int64_t)snapshot_relation_count;
	end_params["diagnostic_count"] = (int64_t)snapshot_diagnostic_count;
	end_params["adapter_status_count"] = (int64_t)snapshot_adapter_status_count;
	end_params["checksum"] = "";
	end_params["revisions"] = snapshot_revisions;
	Dictionary end;
	end["protocol_version"] = "1.4";
	end["kind"] = "notification";
	end["method"] = "snapshot.end";
	end["params"] = end_params;
	end["context"] = snapshot_context;

	Dictionary limits;
	limits["script_documents"] = (int64_t)ScriptSemanticAdapter::MAX_DOCUMENTS;
	limits["script_symbols"] = (int64_t)ScriptSemanticAdapter::MAX_SYMBOLS;
	limits["script_relations"] = (int64_t)ScriptSemanticAdapter::MAX_RELATIONS;
	limits["script_diagnostics"] = (int64_t)ScriptSemanticAdapter::MAX_DIAGNOSTICS;
	limits["symbols_per_document"] = (int64_t)ScriptSemanticAdapter::MAX_SYMBOLS_PER_DOCUMENT;
	limits["relations_per_document"] = (int64_t)ScriptSemanticAdapter::MAX_RELATIONS_PER_DOCUMENT;
	limits["diagnostics_per_document"] = (int64_t)ScriptSemanticAdapter::MAX_DIAGNOSTICS_PER_DOCUMENT;
	limits["script_path_bytes"] = (int64_t)ScriptSemanticAdapter::MAX_PATH_BYTES;
	limits["script_name_bytes"] = (int64_t)ScriptSemanticAdapter::MAX_NAME_BYTES;
	limits["script_signature_bytes"] = (int64_t)ScriptSemanticAdapter::MAX_SIGNATURE_BYTES;
	limits["diagnostic_message_bytes"] = (int64_t)ScriptSemanticAdapter::MAX_DIAGNOSTIC_MESSAGE_BYTES;
	limits["snapshot_chunk_bytes"] = (int64_t)SNAPSHOT_CHUNK_BYTES;
	limits["snapshot_window_bytes"] = (int64_t)SNAPSHOT_WINDOW_BYTES;
	limits["snapshot_timeout_ms"] = (int64_t)(SNAPSHOT_TIMEOUT_USEC / 1000);
	limits["adapter_status_count"] = (int64_t)MAX_ADAPTER_STATUSES;
	Dictionary result;
	result["snapshot_id"] = snapshot_id;
	result["domain"] = "script_graph";
	result["resource_revision"] = (int64_t)snapshot_resource_revision;
	result["scene_graph_revision"] = (int64_t)snapshot_scene_graph_revision;
	result["script_graph_revision"] = (int64_t)snapshot_script_graph_revision;
	result["revisions"] = snapshot_revisions;
	result["limits_applied"] = limits;

	r_completion.ready = true;
	r_completion.terminal = true;
	r_completion.request_id = snapshot_request_id;
	r_completion.result = result;
	r_completion.server_message = end;
	snapshot_waiting_for_terminal = true;
	snapshot_record = nullptr;
	snapshot_finishing = false;
}

Error ScriptGraphAdapter::begin_snapshot(uint64_t p_request_id, uint64_t p_now_usec, const Dictionary &p_context, Dictionary &r_error_data) {
	r_error_data.clear();
	if (!ScriptSemanticAdapter::is_gdscript_available()) {
		return ERR_UNAVAILABLE;
	}
	if (snapshot_active) {
		r_error_data["active_snapshot_id"] = snapshot_id;
		return ERR_BUSY;
	}
	if (catalog_limit_exceeded) {
		return ERR_OUT_OF_MEMORY;
	}
	if (!catalog_ready || refresh_requested || refresh_phase != REFRESH_IDLE || journal.is_invalidating()) {
		return ERR_UNAVAILABLE;
	}
	_reset_snapshot();
	snapshot_active = true;
	snapshot_request_id = p_request_id;
	snapshot_started_usec = p_now_usec;
	snapshot_id = _make_snapshot_id();
	snapshot_resource_revision = revision_clock ? revision_clock->get_resource_revision() : 1;
	snapshot_scene_graph_revision = revision_clock ? revision_clock->get_scene_graph_revision() : 1;
	snapshot_script_graph_revision = revision_clock ? revision_clock->get_script_graph_revision() : journal.get_current_script_graph_revision();
	snapshot_revisions = revision_clock ? revision_clock->get_revision_vector() : Dictionary();
	snapshot_context = p_context;
	snapshot_record = catalog.front();
	Dictionary params;
	params["snapshot_id"] = snapshot_id;
	params["domain"] = "script_graph";
	params["resource_revision"] = (int64_t)snapshot_resource_revision;
	params["scene_graph_revision"] = (int64_t)snapshot_scene_graph_revision;
	params["script_graph_revision"] = (int64_t)snapshot_script_graph_revision;
	params["revisions"] = snapshot_revisions;
	Dictionary begin;
	begin["protocol_version"] = "1.4";
	begin["kind"] = "notification";
	begin["method"] = "snapshot.begin";
	begin["params"] = params;
	begin["context"] = snapshot_context;
	snapshot_pending_message = begin;
	return OK;
}

bool ScriptGraphAdapter::process_snapshot(uint64_t p_now_usec, uint64_t p_budget_usec, SnapshotCompletion &r_completion) {
	r_completion = SnapshotCompletion();
	if (!snapshot_active || snapshot_waiting_for_terminal) {
		return false;
	}
	if (_emit_pending_snapshot_message(r_completion)) {
		return true;
	}
	if (p_now_usec - snapshot_started_usec >= SNAPSHOT_TIMEOUT_USEC) {
		_fail_snapshot("snapshot_timeout", "The script graph snapshot exceeded 120 seconds.", true, r_completion);
		return true;
	}
	const uint64_t started = OS::get_singleton()->get_ticks_usec();
	do {
		if (snapshot_record) {
			const Dictionary bundle = snapshot_record->value().bundle;
			if (!bundle.has("document") || bundle["document"].get_type() != Variant::DICTIONARY || _bundle_count(bundle, "symbols") == UINT64_MAX || _bundle_count(bundle, "relations") == UINT64_MAX || _bundle_count(bundle, "diagnostics") == UINT64_MAX) {
				_fail_snapshot("script_limit_exceeded", "The script graph contains an invalid catalog record.", false, r_completion);
				return true;
			}
			const Array symbols = bundle["symbols"];
			const Array relations = bundle["relations"];
			const Array diagnostics = bundle["diagnostics"];
			if (!snapshot_document_added) {
				if (!_append_snapshot_value(snapshot_documents, bundle["document"])) {
					_fail_snapshot("script_limit_exceeded", "A script document exceeds the snapshot chunk limit.", false, r_completion);
					return true;
				}
				snapshot_document_added = true;
			} else if (snapshot_symbol_index < symbols.size()) {
				if (!_append_snapshot_value(snapshot_symbols, symbols[snapshot_symbol_index++])) {
					_fail_snapshot("script_limit_exceeded", "A script symbol exceeds the snapshot chunk limit.", false, r_completion);
					return true;
				}
			} else if (snapshot_relation_index < relations.size()) {
				if (!_append_snapshot_value(snapshot_relations, relations[snapshot_relation_index++])) {
					_fail_snapshot("script_limit_exceeded", "A script relation exceeds the snapshot chunk limit.", false, r_completion);
					return true;
				}
			} else if (snapshot_diagnostic_index < diagnostics.size()) {
				if (!_append_snapshot_value(snapshot_diagnostics, diagnostics[snapshot_diagnostic_index++])) {
					_fail_snapshot("script_limit_exceeded", "A script diagnostic exceeds the snapshot chunk limit.", false, r_completion);
					return true;
				}
			} else {
				snapshot_record = snapshot_record->next();
				snapshot_document_added = false;
				snapshot_symbol_index = 0;
				snapshot_relation_index = 0;
				snapshot_diagnostic_index = 0;
			}
		} else if (snapshot_adapter_status_index < adapter_statuses.size()) {
			if (!_append_snapshot_value(snapshot_adapter_statuses, adapter_statuses[snapshot_adapter_status_index++])) {
				_fail_snapshot("script_limit_exceeded", "A script adapter status exceeds the snapshot chunk limit.", false, r_completion);
				return true;
			}
		} else {
			_finish_snapshot(r_completion);
			return true;
		}
		if (_emit_pending_snapshot_message(r_completion)) {
			return true;
		}
	} while (OS::get_singleton()->get_ticks_usec() - started < p_budget_usec);
	return true;
}

Array ScriptGraphAdapter::cancel_snapshot(uint64_t p_request_id) {
	if (!snapshot_active || snapshot_request_id != p_request_id) {
		return Array();
	}
	Array abandoned = _take_abandoned_snapshot_data();
	_reset_snapshot();
	return abandoned;
}

ScriptDeltaJournal::QueryResult ScriptGraphAdapter::query_delta(uint64_t p_after_script_graph_revision) const {
	return journal.query_after(p_after_script_graph_revision);
}

bool ScriptGraphAdapter::is_catalog_ready() const {
	return ScriptSemanticAdapter::is_gdscript_available() && catalog_ready && !journal.is_invalidating();
}

bool ScriptGraphAdapter::has_catalog_limit_failure() const {
	return catalog_limit_exceeded;
}

bool ScriptGraphAdapter::is_snapshot_active() const {
	return snapshot_active;
}

bool ScriptGraphAdapter::has_pending_work() const {
	return refresh_requested || refresh_phase != REFRESH_IDLE || snapshot_active;
}

uint64_t ScriptGraphAdapter::get_script_graph_revision() const {
	return journal.get_current_script_graph_revision();
}
