/**************************************************************************/
/*  resource_delta_journal.h                                              */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#pragma once

#include "core/templates/hash_set.h"
#include "core/templates/list.h"
#include "core/variant/variant.h"

class ResourceGraphAdapter;

class ResourceDeltaJournal {
public:
	enum QueryStatus {
		QUERY_CURRENT,
		QUERY_BATCH,
		QUERY_GAP,
		QUERY_FUTURE,
	};

	struct QueryResult {
		QueryStatus status = QUERY_CURRENT;
		uint64_t current_resource_revision = 0;
		uint64_t oldest_available_resource_revision = 0;
		Dictionary batch;
	};

	struct PreparedBatch {
		Dictionary value;
		uint64_t encoded_bytes = 0;
	};

	static constexpr uint32_t MAX_ENTRIES = 4096;
	static constexpr uint64_t MAX_BYTES = 16 * 1024 * 1024;
	static constexpr uint64_t MAX_BATCH_BYTES = 512 * 1024;

private:
	friend class ResourceGraphAdapter;
	friend struct ResourceDeltaJournalTestAccess;

	struct StoredBatch {
		Dictionary value;
		uint64_t previous_resource_revision = 0;
		uint64_t resource_revision = 0;
		uint64_t encoded_bytes = 0;
	};
	struct RetiredVariant;
	struct FallbackCleanupThread;

	List<StoredBatch> batches;
	List<int64_t> cleanup_tasks;
	List<FallbackCleanupThread *> cleanup_threads;
	uint64_t total_bytes = 0;
	uint64_t current_resource_revision = 1;
	bool invalidating = false;

	static void _destroy_retired_variant_thread(void *p_userdata);
	void _schedule_retired_variant(RetiredVariant *p_retired);
	void _retire_dictionary(Dictionary &r_value);
	void _retire_array(Array &r_value);
	bool _retire_front_batch();
	void _reap_cleanup_step();
	void _wait_for_cleanup();
	void _retire_prepared_batch(PreparedBatch &r_prepared);

	static String _resource_ref_key(const Dictionary &p_resource_ref);
	static String _operation_key(const Dictionary &p_operation);
	static Dictionary _without_internal_fields(const Dictionary &p_operation);

public:
	~ResourceDeltaJournal();

	void initialize(uint64_t p_initial_resource_revision = 1);

	static Array coalesce_operations(const Array &p_operations, const HashSet<String> &p_preexisting_keys);
	static String resource_ref_key(const Dictionary &p_resource_ref);
	static Error prepare_batch(uint64_t p_next_resource_revision, const Array &p_operations, const HashSet<String> &p_preexisting_keys, PreparedBatch &r_prepared);

	Error commit(uint64_t p_next_resource_revision, uint64_t p_project_revision, const Array &p_operations, const HashSet<String> &p_preexisting_keys, Dictionary &r_batch, bool &r_invalidated);
	Error commit_prepared(uint64_t p_next_resource_revision, uint64_t p_project_revision, const PreparedBatch &p_prepared, Dictionary &r_batch, bool &r_invalidated);
	QueryResult query_after(uint64_t p_after_resource_revision) const;
	void invalidate_to(uint64_t p_resource_revision);
	bool drain_invalidation_step();
	bool is_invalidating() const;

	uint64_t get_current_resource_revision() const;
	uint64_t get_oldest_available_resource_revision() const;
	uint32_t get_entry_count() const;
	uint64_t get_total_bytes() const;
};
