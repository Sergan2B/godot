/**************************************************************************/
/*  compound_action_context.cpp                                           */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#include "compound_action_context.h"

#include "core/object/object.h"
#include "core/object/undo_redo.h"

namespace {

static bool valid_domain(const String &p_domain) {
	return p_domain == "scene" || p_domain == "resource" || p_domain == "script";
}

} // namespace

Error CompoundActionContext::_preflight() const {
	ERR_FAIL_COND_V(!sealed, ERR_UNCONFIGURED);
	ERR_FAIL_COND_V(steps.is_empty() || steps.size() > MAX_STEPS, ERR_INVALID_DATA);
	for (const PropertyStep &step : steps) {
		Object *target = ObjectDB::get_instance(step.target);
		ERR_FAIL_NULL_V(target, ERR_DOES_NOT_EXIST);
		bool valid = false;
		const Variant current = target->get(step.property, &valid);
		ERR_FAIL_COND_V(!valid || current != step.before, ERR_BUSY);
	}
	return OK;
}

Error CompoundActionContext::add_property_step(const String &p_domain, Object *p_target, const StringName &p_property, const Variant &p_after) {
	ERR_FAIL_COND_V(sealed, ERR_ALREADY_IN_USE);
	ERR_FAIL_COND_V(!valid_domain(p_domain) || !p_target || p_property.is_empty(), ERR_INVALID_PARAMETER);
	ERR_FAIL_COND_V(steps.size() >= MAX_STEPS, ERR_OUT_OF_MEMORY);
	bool valid = false;
	const Variant before = p_target->get(p_property, &valid);
	ERR_FAIL_COND_V(!valid, ERR_INVALID_PARAMETER);
	ERR_FAIL_COND_V(before == p_after, ERR_ALREADY_EXISTS);
	PropertyStep step;
	step.domain = p_domain;
	step.target = p_target->get_instance_id();
	step.property = p_property;
	step.before = before;
	step.after = p_after;
	steps.push_back(step);
	return OK;
}

Error CompoundActionContext::seal() {
	ERR_FAIL_COND_V(sealed, ERR_ALREADY_IN_USE);
	ERR_FAIL_COND_V(steps.is_empty(), ERR_INVALID_DATA);
	sealed = true;
	const Error error = _preflight();
	if (error != OK) {
		sealed = false;
	}
	return error;
}

Error CompoundActionContext::register_on_history(UndoRedo *p_history, const String &p_action_name) {
	ERR_FAIL_NULL_V(p_history, ERR_INVALID_PARAMETER);
	ERR_FAIL_COND_V(p_action_name.is_empty(), ERR_INVALID_PARAMETER);
	const Error preflight_error = _preflight();
	if (preflight_error != OK) {
		return preflight_error;
	}
	p_history->create_action(p_action_name, UndoRedo::MERGE_DISABLE, true);
	for (const PropertyStep &step : steps) {
		Object *target = ObjectDB::get_instance(step.target);
		CRASH_COND_MSG(!target, "Compound target disappeared inside the native commit window.");
		p_history->add_do_property(target, step.property, step.after);
		p_history->add_undo_property(target, step.property, step.before);
	}
	p_history->commit_action(true);
	return OK;
}

int64_t CompoundActionContext::get_step_count() const {
	return steps.size();
}

bool CompoundActionContext::is_sealed() const {
	return sealed;
}
