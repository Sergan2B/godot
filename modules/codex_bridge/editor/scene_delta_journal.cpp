/**************************************************************************/
/*  scene_delta_journal.cpp                                               */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
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

#include "scene_delta_journal.h"

#include "core/crypto/crypto_core.h"
#include "core/io/json.h"

#include "modules/codex_bridge/protocol/bridge_crypto.h"

static String scene_sha256_hex(const String &p_value) {
	const CharString bytes = p_value.utf8();
	PackedByteArray digest;
	digest.resize(32);
	if (CryptoCore::sha256(reinterpret_cast<const uint8_t *>(bytes.get_data()), bytes.length(), digest.ptrw()) != OK) {
		return String();
	}
	return BridgeCrypto::bytes_to_lower_hex(digest);
}

void SceneDeltaJournal::initialize(uint64_t p_initial_revision) {
	batches.clear();
	encoded_bytes = 0;
	current_revision = MAX((uint64_t)1, p_initial_revision);
	invalidating = false;
}

Error SceneDeltaJournal::prepare_batch(uint64_t p_next_revision, const Array &p_operations, PreparedBatch &r_prepared) {
	r_prepared = PreparedBatch();
	ERR_FAIL_COND_V(p_next_revision == 0 || p_operations.is_empty(), ERR_INVALID_PARAMETER);
	const String operations_json = JSON::stringify(p_operations, "", true, true);
	const uint64_t operations_bytes = operations_json.utf8().length();
	if (operations_bytes > MAX_BATCH_BYTES) {
		return ERR_OUT_OF_MEMORY;
	}
	r_prepared.operations = p_operations;
	r_prepared.batch_id = "scene-batch:" + scene_sha256_hex(String::num_uint64(p_next_revision) + ":" + operations_json).left(32);
	r_prepared.checksum = scene_sha256_hex(operations_json);
	r_prepared.operations_bytes = operations_bytes;
	return OK;
}

Error SceneDeltaJournal::commit_prepared(uint64_t p_next_revision, uint64_t p_resource_revision, uint64_t p_project_revision, const PreparedBatch &p_prepared, Dictionary &r_batch, bool &r_invalidated) {
	r_batch.clear();
	r_invalidated = false;
	ERR_FAIL_COND_V(p_next_revision != current_revision + 1 || p_prepared.operations.is_empty() || p_prepared.batch_id.is_empty() || p_prepared.checksum.is_empty() || p_prepared.operations_bytes == 0, ERR_INVALID_PARAMETER);

	Dictionary batch;
	batch["batch_id"] = p_prepared.batch_id;
	batch["previous_scene_graph_revision"] = (int64_t)current_revision;
	batch["scene_graph_revision"] = (int64_t)p_next_revision;
	batch["resource_revision"] = (int64_t)p_resource_revision;
	batch["project_revision"] = (int64_t)p_project_revision;
	// Encode the potentially large operation array exactly once. The canonical
	// batch envelope is measured with an empty array and its two bytes are
	// replaced by the already measured operations JSON length.
	batch["operations"] = Array();
	batch["source_complete"] = true;
	batch["checksum"] = p_prepared.checksum;
	const uint64_t empty_batch_bytes = JSON::stringify(batch, "", true, true).utf8().length();
	ERR_FAIL_COND_V(empty_batch_bytes < 2, ERR_BUG);
	const uint64_t batch_bytes = empty_batch_bytes - 2 + p_prepared.operations_bytes;
	if (batch_bytes > MAX_BATCH_BYTES) {
		invalidate_to(p_next_revision);
		r_invalidated = true;
		return ERR_OUT_OF_MEMORY;
	}
	batch["operations"] = p_prepared.operations;

	StoredBatch stored;
	stored.previous_revision = current_revision;
	stored.revision = p_next_revision;
	stored.encoded_bytes = batch_bytes;
	stored.value = batch;
	batches.push_back(stored);
	encoded_bytes += batch_bytes;
	current_revision = p_next_revision;
	invalidating = false;
	while (batches.size() > (int)MAX_ENTRIES || encoded_bytes > MAX_BYTES) {
		const StoredBatch oldest = batches[0];
		encoded_bytes -= oldest.encoded_bytes;
		batches.remove_at(0);
		r_invalidated = true;
	}
	r_batch = batch;
	return OK;
}

Error SceneDeltaJournal::commit(uint64_t p_next_revision, uint64_t p_resource_revision, uint64_t p_project_revision, const Array &p_operations, Dictionary &r_batch, bool &r_invalidated) {
	PreparedBatch prepared;
	const Error prepare_error = prepare_batch(p_next_revision, p_operations, prepared);
	if (prepare_error != OK) {
		invalidate_to(p_next_revision);
		r_batch.clear();
		r_invalidated = true;
		return prepare_error;
	}
	return commit_prepared(p_next_revision, p_resource_revision, p_project_revision, prepared, r_batch, r_invalidated);
}

void SceneDeltaJournal::invalidate_to(uint64_t p_revision) {
	batches.clear();
	encoded_bytes = 0;
	current_revision = MAX((uint64_t)1, p_revision);
	// The current catalog remains a valid source for a replacement snapshot.
	// An empty journal is enough to make older delta cursors return QUERY_GAP;
	// keeping this flag set would prevent that recovery snapshot forever.
	invalidating = false;
}

SceneDeltaJournal::QueryResult SceneDeltaJournal::query_after(uint64_t p_after_revision) const {
	QueryResult result;
	result.current_scene_graph_revision = current_revision;
	result.oldest_available_scene_graph_revision = get_oldest_available_revision();
	if (p_after_revision == current_revision) {
		return result;
	}
	if (p_after_revision > current_revision) {
		result.status = QUERY_FUTURE;
		return result;
	}
	if (invalidating || batches.is_empty() || p_after_revision < result.oldest_available_scene_graph_revision) {
		result.status = QUERY_GAP;
		return result;
	}
	for (const StoredBatch &batch : batches) {
		if (batch.previous_revision == p_after_revision) {
			result.status = QUERY_BATCH;
			result.batch = batch.value;
			return result;
		}
	}
	result.status = QUERY_GAP;
	return result;
}

uint64_t SceneDeltaJournal::get_current_revision() const {
	return current_revision;
}

uint64_t SceneDeltaJournal::get_oldest_available_revision() const {
	return batches.is_empty() ? current_revision : batches[0].previous_revision;
}

bool SceneDeltaJournal::is_invalidating() const {
	return invalidating;
}
