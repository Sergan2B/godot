/**************************************************************************/
/*  transaction_scene_resolver.h                                        */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#pragma once

#include "transaction_preview_builder.h"

#include "core/object/object_id.h"
#include "core/templates/hash_map.h"
#include "core/templates/list.h"

class TransactionSceneResolver {
public:
	typedef uint64_t (*Clock)(void *p_userdata);

	struct SceneEvidence {
		int scene_index = -1;
		int native_history_id = -1;
		uint64_t history_version = 0;
		int history_action_position = -1;
	};

	struct Job {
		String editor_session_id;
		String scene_id;
		String history_id;
		Dictionary operation;
		ObjectID root_id;
		SceneEvidence scene_evidence;
		List<ObjectID> pending_nodes;
		HashMap<String, ObjectID> objects_by_node_id;
		HashMap<ObjectID, String> node_ids_by_object;
		uint32_t visited_nodes = 0;
		bool complete = false;
	};

	struct ProcessOutcome {
		bool complete = false;
		bool failed = false;
		String error_code;
		String error_message;
		TransactionPreviewBuilder::Resolution resolution;
	};

	static constexpr uint32_t MAX_STRUCTURAL_NODES = 1000;
	static constexpr uint32_t MAX_NODES_PER_SLICE = 128;
	static constexpr uint64_t MAX_SLICE_USEC = 600;

	static Error begin(const String &p_editor_session_id, const String &p_scene_id, const String &p_history_id, const Dictionary &p_operation, Job &r_job, String &r_error_code, String &r_error_message);
	static ProcessOutcome process(Job &r_job, uint32_t p_max_nodes = MAX_NODES_PER_SLICE, uint64_t p_budget_usec = MAX_SLICE_USEC, Clock p_clock = nullptr, void *p_clock_userdata = nullptr);
	static bool binding_is_current(const Job &p_job);

private:
	static uint64_t _default_clock(void *p_userdata);
};
