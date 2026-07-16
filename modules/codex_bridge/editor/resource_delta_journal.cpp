/**************************************************************************/
/*  resource_delta_journal.cpp                                            */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#include "resource_delta_journal.h"

#include "core/crypto/crypto_core.h"
#include "core/io/json.h"
#include "modules/codex_bridge/protocol/bridge_crypto.h"

namespace {

static String sha256_hex_utf8(const String &p_value) {
	const CharString bytes = p_value.utf8();
	PackedByteArray digest;
	digest.resize(32);
	if (CryptoCore::sha256(reinterpret_cast<const uint8_t *>(bytes.get_data()), bytes.length(), digest.ptrw()) != OK) {
		return String("0").repeat(64);
	}
	return BridgeCrypto::bytes_to_lower_hex(digest);
}

static Dictionary operation_resource_ref(const Dictionary &p_operation) {
	if (p_operation.has("resource_ref") && p_operation["resource_ref"].get_type() == Variant::DICTIONARY) {
		return p_operation["resource_ref"];
	}
	if (!p_operation.has("value") || p_operation["value"].get_type() != Variant::DICTIONARY) {
		return Dictionary();
	}
	const Dictionary value = p_operation["value"];
	if (!value.has("resource") || value["resource"].get_type() != Variant::DICTIONARY) {
		return Dictionary();
	}
	const Dictionary resource = value["resource"];
	return resource.has("resource_ref") && resource["resource_ref"].get_type() == Variant::DICTIONARY ? Dictionary(resource["resource_ref"]) : Dictionary();
}

} // namespace

String ResourceDeltaJournal::_resource_ref_key(const Dictionary &p_resource_ref) {
	if (p_resource_ref.has("uid") && p_resource_ref["uid"].get_type() == Variant::STRING) {
		return "uid:" + String(p_resource_ref["uid"]);
	}
	if (p_resource_ref.has("uid_missing") && bool(p_resource_ref["uid_missing"]) && p_resource_ref.has("path") && p_resource_ref["path"].get_type() == Variant::STRING) {
		return "path:" + String(p_resource_ref["path"]);
	}
	return String();
}

String ResourceDeltaJournal::_operation_key(const Dictionary &p_operation) {
	if (!p_operation.has("kind") || p_operation["kind"].get_type() != Variant::STRING) {
		return String();
	}
	const String kind = p_operation["kind"];
	if (kind == "move") {
		return p_operation.has("uid") && p_operation["uid"].get_type() == Variant::STRING ? "uid:" + String(p_operation["uid"]) : String();
	}
	return _resource_ref_key(operation_resource_ref(p_operation));
}

Dictionary ResourceDeltaJournal::_without_internal_fields(const Dictionary &p_operation) {
	Dictionary operation = p_operation.duplicate(true);
	for (const Variant &key : operation.keys()) {
		if (key.get_type() == Variant::STRING && String(key).begins_with("_")) {
			operation.erase(key);
		}
	}
	return operation;
}

void ResourceDeltaJournal::initialize(uint64_t p_initial_resource_revision) {
	batches.clear();
	total_bytes = 0;
	current_resource_revision = p_initial_resource_revision;
}

Array ResourceDeltaJournal::coalesce_operations(const Array &p_operations, const HashSet<String> &p_preexisting_keys) {
	HashMap<String, Dictionary> pending;
	HashSet<String> ordered_keys;
	Vector<String> order;

	for (const Variant &operation_value : p_operations) {
		if (operation_value.get_type() != Variant::DICTIONARY) {
			continue;
		}
		Dictionary operation = Dictionary(operation_value).duplicate(true);
		const String key = _operation_key(operation);
		if (key.is_empty() || !operation.has("kind") || operation["kind"].get_type() != Variant::STRING) {
			continue;
		}
		const String kind = operation["kind"];
		Dictionary *previous = pending.getptr(key);

		if (kind == "remove") {
			if (previous && !p_preexisting_keys.has(key)) {
				const String previous_kind = (*previous)["kind"];
				if (previous_kind == "upsert" || previous_kind == "reimport" || previous_kind == "move") {
					pending.erase(key);
					continue;
				}
			}
		} else if (kind == "move" && previous) {
			const String previous_kind = (*previous)["kind"];
			if (previous_kind == "move") {
				operation["from_path"] = (*previous)["from_path"];
			} else if (!p_preexisting_keys.has(key) && (previous_kind == "upsert" || previous_kind == "reimport")) {
				Dictionary replacement;
				replacement["kind"] = "upsert";
				replacement["value"] = operation["value"];
				operation = replacement;
			}
		}

		if (!previous && !ordered_keys.has(key)) {
			order.push_back(key);
			ordered_keys.insert(key);
		}
		pending.insert(key, _without_internal_fields(operation));
	}

	Array result;
	for (const String &key : order) {
		const Dictionary *operation = pending.getptr(key);
		if (operation) {
			result.push_back(*operation);
		}
	}
	return result;
}

String ResourceDeltaJournal::resource_ref_key(const Dictionary &p_resource_ref) {
	return _resource_ref_key(p_resource_ref);
}

Error ResourceDeltaJournal::commit(uint64_t p_next_resource_revision, const Array &p_operations, const HashSet<String> &p_preexisting_keys, Dictionary &r_batch, bool &r_invalidated) {
	r_batch.clear();
	r_invalidated = false;
	ERR_FAIL_COND_V(p_next_resource_revision != current_resource_revision + 1, ERR_INVALID_PARAMETER);

	const Array operations = coalesce_operations(p_operations, p_preexisting_keys);
	ERR_FAIL_COND_V(operations.is_empty(), ERR_INVALID_PARAMETER);

	const String operations_json = JSON::stringify(operations, "", true, true);
	const String checksum = sha256_hex_utf8(operations_json);
	Dictionary batch;
	batch["batch_id"] = "resource-batch:" + sha256_hex_utf8(String::num_uint64(p_next_resource_revision) + ":" + operations_json).left(32);
	batch["previous_resource_revision"] = (int64_t)current_resource_revision;
	batch["resource_revision"] = (int64_t)p_next_resource_revision;
	batch["operations"] = operations;
	batch["source_complete"] = true;
	batch["checksum"] = checksum;

	const uint64_t encoded_bytes = JSON::stringify(batch, "", true, true).utf8().length();
	current_resource_revision = p_next_resource_revision;
	r_batch = batch;
	if (encoded_bytes > MAX_BATCH_BYTES) {
		batches.clear();
		total_bytes = 0;
		r_invalidated = true;
		return OK;
	}

	StoredBatch stored;
	stored.value = batch;
	stored.previous_resource_revision = p_next_resource_revision - 1;
	stored.resource_revision = p_next_resource_revision;
	stored.encoded_bytes = encoded_bytes;
	batches.push_back(stored);
	total_bytes += encoded_bytes;

	while ((uint32_t)batches.size() > MAX_ENTRIES || total_bytes > MAX_BYTES) {
		const List<StoredBatch>::Element *front = batches.front();
		ERR_FAIL_NULL_V(front, ERR_BUG);
		total_bytes -= front->get().encoded_bytes;
		batches.pop_front();
		r_invalidated = true;
	}
	return OK;
}

ResourceDeltaJournal::QueryResult ResourceDeltaJournal::query_after(uint64_t p_after_resource_revision) const {
	QueryResult result;
	result.current_resource_revision = current_resource_revision;
	result.oldest_available_resource_revision = get_oldest_available_resource_revision();
	if (p_after_resource_revision == current_resource_revision) {
		result.status = QUERY_CURRENT;
		return result;
	}
	if (p_after_resource_revision > current_resource_revision) {
		result.status = QUERY_FUTURE;
		return result;
	}
	if (batches.is_empty() || p_after_resource_revision < result.oldest_available_resource_revision) {
		result.status = QUERY_GAP;
		return result;
	}
	for (const StoredBatch &batch : batches) {
		if (batch.previous_resource_revision == p_after_resource_revision) {
			result.status = QUERY_BATCH;
			result.batch = batch.value;
			return result;
		}
	}
	result.status = QUERY_GAP;
	return result;
}

void ResourceDeltaJournal::invalidate_to(uint64_t p_resource_revision) {
	batches.clear();
	total_bytes = 0;
	current_resource_revision = p_resource_revision;
}

uint64_t ResourceDeltaJournal::get_current_resource_revision() const {
	return current_resource_revision;
}

uint64_t ResourceDeltaJournal::get_oldest_available_resource_revision() const {
	const List<StoredBatch>::Element *front = batches.front();
	return front ? front->get().previous_resource_revision : current_resource_revision;
}

uint32_t ResourceDeltaJournal::get_entry_count() const {
	return batches.size();
}

uint64_t ResourceDeltaJournal::get_total_bytes() const {
	return total_bytes;
}
