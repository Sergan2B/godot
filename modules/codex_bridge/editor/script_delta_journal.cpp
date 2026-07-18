/**************************************************************************/
/*  script_delta_journal.cpp                                              */
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

#include "script_delta_journal.h"

#include "core/crypto/crypto_core.h"
#include "core/io/json.h"
#include "core/object/worker_thread_pool.h"
#include "core/os/thread.h"
#include "core/templates/hash_set.h"

#include "modules/codex_bridge/protocol/bridge_crypto.h"

namespace {

static String script_sha256_hex(const String &p_value) {
	const CharString bytes = p_value.utf8();
	PackedByteArray digest;
	digest.resize(32);
	if (CryptoCore::sha256(reinterpret_cast<const uint8_t *>(bytes.get_data()), bytes.length(), digest.ptrw()) != OK) {
		return String();
	}
	return BridgeCrypto::bytes_to_lower_hex(digest);
}

} // namespace

struct ScriptDeltaJournal::FallbackCleanupThread {
	Thread thread;
	SafeFlag completed;
};

struct ScriptDeltaJournal::RetiredVariant {
	Variant value;
	FallbackCleanupThread *fallback = nullptr;
};

ScriptDeltaJournal::~ScriptDeltaJournal() {
	while (_retire_front_batch()) {
	}
	_wait_for_cleanup();
}

void ScriptDeltaJournal::_destroy_retired_variant_thread(void *p_userdata) {
	RetiredVariant *retired = static_cast<RetiredVariant *>(p_userdata);
	FallbackCleanupThread *fallback = retired->fallback;
	retired->value = Variant();
	memdelete(retired);
	if (fallback) {
		fallback->completed.set();
	}
}

void ScriptDeltaJournal::_schedule_retired_variant(RetiredVariant *p_retired) {
	ERR_FAIL_NULL(p_retired);
	_reap_cleanup_step();
	WorkerThreadPool *worker_pool = WorkerThreadPool::get_singleton();
	if (worker_pool) {
		const WorkerThreadPool::TaskID task = worker_pool->add_native_task(
				_destroy_retired_variant_thread,
				p_retired,
				false,
				SNAME("CodexScriptDtoCleanup"));
		cleanup_tasks.push_back(task);
		return;
	}

#ifdef THREADS_ENABLED
	FallbackCleanupThread *fallback = memnew(FallbackCleanupThread);
	p_retired->fallback = fallback;
	fallback->thread.start(_destroy_retired_variant_thread, p_retired);
	cleanup_threads.push_back(fallback);
#else
	_destroy_retired_variant_thread(p_retired);
#endif
}

void ScriptDeltaJournal::_retire_dictionary(Dictionary &r_value) {
	if (r_value.is_empty()) {
		r_value = Dictionary();
		return;
	}
	RetiredVariant *retired = memnew(RetiredVariant);
	retired->value = r_value;
	r_value = Dictionary();
	_schedule_retired_variant(retired);
}

void ScriptDeltaJournal::_retire_array(Array &r_value) {
	if (r_value.is_empty()) {
		r_value = Array();
		return;
	}
	RetiredVariant *retired = memnew(RetiredVariant);
	retired->value = r_value;
	r_value = Array();
	_schedule_retired_variant(retired);
}

bool ScriptDeltaJournal::_retire_front_batch() {
	List<StoredBatch>::Element *front = batches.front();
	if (!front) {
		return false;
	}
	_retire_dictionary(front->get().value);
	batches.pop_front();
	return true;
}

void ScriptDeltaJournal::_reap_cleanup_step() {
	WorkerThreadPool *worker_pool = WorkerThreadPool::get_singleton();
	List<int64_t>::Element *task = cleanup_tasks.front();
	if (worker_pool && task && worker_pool->is_task_completed(task->get())) {
		worker_pool->wait_for_task_completion(task->get());
		cleanup_tasks.pop_front();
	}
	List<FallbackCleanupThread *>::Element *fallback_element = cleanup_threads.front();
	if (fallback_element && fallback_element->get()->completed.is_set()) {
		FallbackCleanupThread *fallback = fallback_element->get();
		fallback->thread.wait_to_finish();
		cleanup_threads.pop_front();
		memdelete(fallback);
	}
}

void ScriptDeltaJournal::_wait_for_cleanup() {
	WorkerThreadPool *worker_pool = WorkerThreadPool::get_singleton();
	while (!cleanup_tasks.is_empty()) {
		const int64_t task = cleanup_tasks.front()->get();
		if (worker_pool) {
			worker_pool->wait_for_task_completion(task);
		}
		cleanup_tasks.pop_front();
	}
	while (!cleanup_threads.is_empty()) {
		FallbackCleanupThread *fallback = cleanup_threads.front()->get();
		fallback->thread.wait_to_finish();
		cleanup_threads.pop_front();
		memdelete(fallback);
	}
}

void ScriptDeltaJournal::_retire_prepared_batch(PreparedBatch &r_prepared) {
	_retire_array(r_prepared.operations);
	r_prepared.batch_id.clear();
	r_prepared.checksum.clear();
	r_prepared.operations_bytes = 0;
	r_prepared.script_graph_revision = 0;
}

String ScriptDeltaJournal::_resource_ref_key(const Dictionary &p_resource_ref) {
	if (p_resource_ref.has("uid") && p_resource_ref["uid"].get_type() == Variant::STRING && !String(p_resource_ref["uid"]).is_empty()) {
		return "uid:" + String(p_resource_ref["uid"]);
	}
	if (p_resource_ref.has("uid_missing") && p_resource_ref["uid_missing"].get_type() == Variant::BOOL && bool(p_resource_ref["uid_missing"]) &&
			p_resource_ref.has("path") && p_resource_ref["path"].get_type() == Variant::STRING && String(p_resource_ref["path"]).begins_with("res://")) {
		return "path:" + String(p_resource_ref["path"]);
	}
	return String();
}

String ScriptDeltaJournal::_operation_key(const Dictionary &p_operation) {
	if (!p_operation.has("kind") || p_operation["kind"].get_type() != Variant::STRING) {
		return String();
	}
	const String kind = p_operation["kind"];
	if (kind == "upsert_document") {
		if (p_operation.size() != 2 || !p_operation.has("value") || p_operation["value"].get_type() != Variant::DICTIONARY) {
			return String();
		}
		const Dictionary value = p_operation["value"];
		if (!value.has("document") || value["document"].get_type() != Variant::DICTIONARY) {
			return String();
		}
		const Dictionary document = value["document"];
		if (!document.has("script_ref") || document["script_ref"].get_type() != Variant::DICTIONARY) {
			return String();
		}
		const String key = _resource_ref_key(document["script_ref"]);
		return key.is_empty() ? String() : "document:" + key;
	}
	if (kind == "remove_document") {
		if (p_operation.size() != 3 || !p_operation.has("script_ref") || p_operation["script_ref"].get_type() != Variant::DICTIONARY ||
				!p_operation.has("path") || p_operation["path"].get_type() != Variant::STRING || !String(p_operation["path"]).begins_with("res://")) {
			return String();
		}
		const String key = _resource_ref_key(p_operation["script_ref"]);
		return key.is_empty() ? String() : "document:" + key;
	}
	if (kind == "adapter_status") {
		if (p_operation.size() != 2 || !p_operation.has("value") || p_operation["value"].get_type() != Variant::DICTIONARY) {
			return String();
		}
		const Dictionary value = p_operation["value"];
		if (!value.has("language") || value["language"].get_type() != Variant::STRING) {
			return String();
		}
		const String language = value["language"];
		return language == "gdscript" || language == "csharp" ? "adapter:" + language : String();
	}
	return String();
}

bool ScriptDeltaJournal::_validate_operations(const Array &p_operations) {
	if (p_operations.is_empty() || p_operations.size() > 250000) {
		return false;
	}
	HashSet<String> keys;
	for (const Variant &operation_value : p_operations) {
		if (operation_value.get_type() != Variant::DICTIONARY) {
			return false;
		}
		const String key = _operation_key(operation_value);
		if (key.is_empty() || keys.has(key)) {
			return false;
		}
		keys.insert(key);
	}
	return true;
}

void ScriptDeltaJournal::initialize(uint64_t p_initial_script_graph_revision) {
	while (_retire_front_batch()) {
	}
	total_bytes = 0;
	current_script_graph_revision = MAX((uint64_t)1, p_initial_script_graph_revision);
	invalidating = false;
}

String ScriptDeltaJournal::operation_key(const Dictionary &p_operation) {
	return _operation_key(p_operation);
}

Error ScriptDeltaJournal::prepare_batch(uint64_t p_next_script_graph_revision, const Array &p_operations, PreparedBatch &r_prepared) {
	r_prepared = PreparedBatch();
	ERR_FAIL_COND_V(p_next_script_graph_revision == 0 || !_validate_operations(p_operations), ERR_INVALID_PARAMETER);

	const String operations_json = JSON::stringify(p_operations, "", true, true);
	const uint64_t operations_bytes = operations_json.utf8().length();
	if (operations_bytes > MAX_BATCH_BYTES) {
		return ERR_OUT_OF_MEMORY;
	}
	const String checksum = script_sha256_hex(operations_json);
	ERR_FAIL_COND_V(checksum.is_empty(), ERR_CANT_CREATE);
	r_prepared.operations = p_operations;
	r_prepared.batch_id = "script-batch:" + script_sha256_hex(String::num_uint64(p_next_script_graph_revision) + ":" + operations_json).left(32);
	r_prepared.checksum = checksum;
	r_prepared.operations_bytes = operations_bytes;
	r_prepared.script_graph_revision = p_next_script_graph_revision;
	return OK;
}

Error ScriptDeltaJournal::commit(uint64_t p_next_script_graph_revision, uint64_t p_resource_revision, uint64_t p_scene_graph_revision, uint64_t p_project_revision, const Array &p_operations, Dictionary &r_batch, bool &r_invalidated) {
	PreparedBatch prepared;
	const Error prepare_error = prepare_batch(p_next_script_graph_revision, p_operations, prepared);
	if (prepare_error != OK) {
		r_batch.clear();
		r_invalidated = prepare_error == ERR_OUT_OF_MEMORY;
		if (r_invalidated) {
			invalidate_to(p_next_script_graph_revision);
		}
		return prepare_error;
	}
	return commit_prepared(p_next_script_graph_revision, p_resource_revision, p_scene_graph_revision, p_project_revision, prepared, r_batch, r_invalidated);
}

Error ScriptDeltaJournal::commit_prepared(uint64_t p_next_script_graph_revision, uint64_t p_resource_revision, uint64_t p_scene_graph_revision, uint64_t p_project_revision, const PreparedBatch &p_prepared, Dictionary &r_batch, bool &r_invalidated) {
	r_batch.clear();
	r_invalidated = false;
	ERR_FAIL_COND_V(invalidating, ERR_BUSY);
	ERR_FAIL_COND_V(p_next_script_graph_revision != current_script_graph_revision + 1 || p_prepared.script_graph_revision != p_next_script_graph_revision || p_prepared.operations.is_empty() || p_prepared.batch_id.is_empty() || p_prepared.checksum.is_empty() || p_prepared.operations_bytes == 0, ERR_INVALID_PARAMETER);

	Dictionary batch;
	batch["batch_id"] = p_prepared.batch_id;
	batch["previous_script_graph_revision"] = (int64_t)current_script_graph_revision;
	batch["script_graph_revision"] = (int64_t)p_next_script_graph_revision;
	batch["resource_revision"] = (int64_t)p_resource_revision;
	batch["scene_graph_revision"] = (int64_t)p_scene_graph_revision;
	batch["project_revision"] = (int64_t)p_project_revision;
	batch["operations"] = Array();
	batch["source_complete"] = true;
	batch["checksum"] = p_prepared.checksum;
	const uint64_t empty_batch_bytes = JSON::stringify(batch, "", true, true).utf8().length();
	ERR_FAIL_COND_V(empty_batch_bytes < 2, ERR_BUG);
	const uint64_t batch_bytes = empty_batch_bytes - 2 + p_prepared.operations_bytes;
	if (batch_bytes > MAX_BATCH_BYTES) {
		invalidate_to(p_next_script_graph_revision);
		r_invalidated = true;
		return ERR_OUT_OF_MEMORY;
	}
	batch["operations"] = p_prepared.operations;

	StoredBatch stored;
	stored.value = batch;
	stored.previous_script_graph_revision = current_script_graph_revision;
	stored.script_graph_revision = p_next_script_graph_revision;
	stored.encoded_bytes = batch_bytes;
	batches.push_back(stored);
	total_bytes += batch_bytes;
	current_script_graph_revision = p_next_script_graph_revision;
	invalidating = false;
	r_batch = batch;

	while ((uint32_t)batches.size() > MAX_ENTRIES || total_bytes > MAX_BYTES) {
		const List<StoredBatch>::Element *front = batches.front();
		ERR_FAIL_NULL_V(front, ERR_BUG);
		total_bytes -= front->get().encoded_bytes;
		_retire_front_batch();
		r_invalidated = true;
	}
	return OK;
}

ScriptDeltaJournal::QueryResult ScriptDeltaJournal::query_after(uint64_t p_after_script_graph_revision) const {
	QueryResult result;
	result.current_script_graph_revision = current_script_graph_revision;
	result.oldest_available_script_graph_revision = get_oldest_available_script_graph_revision();
	if (p_after_script_graph_revision == current_script_graph_revision) {
		return result;
	}
	if (p_after_script_graph_revision > current_script_graph_revision) {
		result.status = QUERY_FUTURE;
		return result;
	}
	if (invalidating || batches.is_empty() || p_after_script_graph_revision < result.oldest_available_script_graph_revision) {
		result.status = QUERY_GAP;
		return result;
	}
	for (const StoredBatch &batch : batches) {
		if (batch.previous_script_graph_revision == p_after_script_graph_revision) {
			result.status = QUERY_BATCH;
			result.batch = batch.value;
			return result;
		}
	}
	result.status = QUERY_GAP;
	return result;
}

void ScriptDeltaJournal::invalidate_to(uint64_t p_script_graph_revision) {
	total_bytes = 0;
	current_script_graph_revision = MAX((uint64_t)1, p_script_graph_revision);
	invalidating = !batches.is_empty();
}

bool ScriptDeltaJournal::drain_invalidation_step() {
	_reap_cleanup_step();
	if (batches.is_empty()) {
		invalidating = false;
		return false;
	}
	_retire_front_batch();
	if (batches.is_empty()) {
		invalidating = false;
	}
	return true;
}

bool ScriptDeltaJournal::is_invalidating() const {
	return invalidating;
}

uint64_t ScriptDeltaJournal::get_current_script_graph_revision() const {
	return current_script_graph_revision;
}

uint64_t ScriptDeltaJournal::get_oldest_available_script_graph_revision() const {
	if (invalidating) {
		return current_script_graph_revision;
	}
	const List<StoredBatch>::Element *front = batches.front();
	return front ? front->get().previous_script_graph_revision : current_script_graph_revision;
}

uint32_t ScriptDeltaJournal::get_entry_count() const {
	return invalidating ? 0 : batches.size();
}

uint64_t ScriptDeltaJournal::get_total_bytes() const {
	return total_bytes;
}
