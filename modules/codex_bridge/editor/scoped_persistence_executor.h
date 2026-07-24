/**************************************************************************/
/*  scoped_persistence_executor.h                                         */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#pragma once

#include "compound_change_set_planner.h"

#include "core/object/object_id.h"
#include "core/templates/vector.h"

class ScopedPersistenceExecutor {
public:
	struct FileRecord {
		String target_path;
		String stage_path;
		String escrow_path;
		String preimage_digest;
		String postimage_digest;
		bool existed = false;
		bool written = false;
		bool scene = false;
		ObjectID scene_root_id;
	};

	struct Prepared {
		String change_set_id;
		String stage_directory;
		String escrow_directory;
		Vector<FileRecord> files;
		bool staged = false;
		bool commit_point_entered = false;
	};

	static constexpr int64_t MAX_SCRIPT_BYTES = 1048576;
	static constexpr int64_t MAX_TOTAL_STAGED_BYTES = 4194304;

	static Error stage(const CompoundChangeSetPlanner::Plan &p_plan, const Dictionary &p_resource_paths, const String &p_scene_path, ObjectID p_scene_root_id, Prepared &r_prepared, String &r_error_code, String &r_error_message);
	static Error commit(Prepared &r_prepared, String &r_error_code, String &r_error_message);
	static Error restore(Prepared &r_prepared, String &r_error_code, String &r_error_message);
	static bool verify_postimages(const Prepared &p_prepared);
	static bool verify_preimages(const Prepared &p_prepared);
	static void cleanup(Prepared &r_prepared);
	static bool property_allowed(const String &p_class, const String &p_property);
};
