/**************************************************************************/
/*  scene_delta_journal.h                                                 */
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

#pragma once

#include "core/templates/vector.h"
#include "core/variant/variant.h"

class SceneDeltaJournal {
public:
	static constexpr uint32_t MAX_ENTRIES = 4096;
	static constexpr uint64_t MAX_BYTES = 16 * 1024 * 1024;
	static constexpr uint64_t MAX_BATCH_BYTES = 512 * 1024;

	enum QueryStatus {
		QUERY_CURRENT,
		QUERY_BATCH,
		QUERY_GAP,
		QUERY_FUTURE,
	};

	struct QueryResult {
		QueryStatus status = QUERY_CURRENT;
		uint64_t current_scene_graph_revision = 0;
		uint64_t oldest_available_scene_graph_revision = 0;
		Dictionary batch;
	};

private:
	struct StoredBatch {
		uint64_t previous_revision = 0;
		uint64_t revision = 0;
		uint64_t encoded_bytes = 0;
		Dictionary value;
	};

	Vector<StoredBatch> batches;
	uint64_t current_revision = 1;
	uint64_t encoded_bytes = 0;
	bool invalidating = false;

public:
	void initialize(uint64_t p_initial_revision = 1);
	Error commit(uint64_t p_next_revision, uint64_t p_resource_revision, uint64_t p_project_revision, const Array &p_operations, Dictionary &r_batch, bool &r_invalidated);
	void invalidate_to(uint64_t p_revision);
	QueryResult query_after(uint64_t p_after_revision) const;
	uint64_t get_current_revision() const;
	uint64_t get_oldest_available_revision() const;
	bool is_invalidating() const;
};
