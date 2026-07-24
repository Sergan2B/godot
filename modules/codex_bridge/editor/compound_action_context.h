/**************************************************************************/
/*  compound_action_context.h                                             */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#pragma once

#include "core/object/object_id.h"
#include "core/templates/vector.h"
#include "core/variant/variant.h"

class Object;
class UndoRedo;

// A detached, tests-enabled proof of the Sprint 10 atomicity boundary.
// Planning and seal are read-only. Only register_on_history() is allowed to
// create a native action, after every step has passed the final preflight.
class CompoundActionContext {
public:
	struct PropertyStep {
		String domain;
		ObjectID target;
		StringName property;
		Variant before;
		Variant after;
	};

	static constexpr int64_t MAX_STEPS = 16;

private:
	Vector<PropertyStep> steps;
	bool sealed = false;

	Error _preflight() const;

public:
	Error add_property_step(const String &p_domain, Object *p_target, const StringName &p_property, const Variant &p_after);
	Error seal();
	Error register_on_history(UndoRedo *p_history, const String &p_action_name);

	int64_t get_step_count() const;
	bool is_sealed() const;
};
