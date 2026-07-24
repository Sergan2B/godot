/**************************************************************************/
/*  compound_change_set_coordinator.h                                     */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#pragma once

#include "bridge_revision_clock.h"
#include "prepared_change_set_store.h"

class CompoundChangeSetCoordinator {
	String project_id;
	String editor_session_id;
	BridgeRevisionClock *revision_clock = nullptr;
	PreparedChangeSetStore store;

	bool _coordinates_current(const Dictionary &p_coordinates) const;
	static Dictionary _preview_result(const PreparedChangeSetStore::Record &p_record);

public:
	struct Outcome {
		bool has_result = false;
		Dictionary result;
		String error_code;
		String error_message;
		bool retryable = false;
	};

	void initialize(const String &p_project_id, const String &p_editor_session_id, BridgeRevisionClock *p_revision_clock);
	void shutdown();
	Outcome prepare(const Dictionary &p_params, uint64_t p_now_ms);
	Outcome status(const String &p_change_set_id, uint64_t p_now_ms);
	void invalidate_all(uint64_t p_now_ms);
	uint32_t get_active_count() const;
	bool is_ready() const;
};
