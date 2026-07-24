/**************************************************************************/
/*  compound_change_set_coordinator.cpp                                   */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#include "compound_change_set_coordinator.h"

#include "core/io/json.h"

bool CompoundChangeSetCoordinator::_coordinates_current(const Dictionary &p_coordinates) const {
	if (!revision_clock || String(p_coordinates.get("editor_session_id", String())) != editor_session_id) {
		return false;
	}
	const Dictionary revisions = revision_clock->get_revision_vector();
	const String scene_id = p_coordinates.get("scene_id", String());
	return (uint64_t)(int64_t)p_coordinates.get("scene_revision", -1) == revision_clock->get_scene_revision(scene_id) &&
			(uint64_t)(int64_t)p_coordinates.get("operation_seq", -1) == (uint64_t)(int64_t)revisions.get("operation_seq", -2) &&
			(uint64_t)(int64_t)p_coordinates.get("resource_revision", -1) == revision_clock->get_resource_revision() &&
			(uint64_t)(int64_t)p_coordinates.get("script_graph_revision", -1) == revision_clock->get_script_graph_revision();
}

Dictionary CompoundChangeSetCoordinator::_preview_result(const PreparedChangeSetStore::Record &p_record) {
	Dictionary result;
	result["change_set_id"] = p_record.plan.change_set_id;
	result["state"] = PreparedChangeSetStore::state_name(p_record.state);
	result["transaction_seq"] = (int64_t)p_record.transaction_seq;
	result["preview_digest"] = p_record.plan.preview_digest;
	result["request_digest"] = p_record.plan.request_digest;
	result["preview"] = JSON::parse_string(p_record.plan.canonical_preview_json);
	result["risk"] = p_record.plan.risk;
	result["operation_count"] = p_record.plan.ordered_operations.size();
	result["created_at_ms"] = (int64_t)p_record.plan.created_at_ms;
	result["expires_at_ms"] = (int64_t)p_record.plan.expires_at_ms;
	return result;
}

void CompoundChangeSetCoordinator::initialize(const String &p_project_id, const String &p_editor_session_id, BridgeRevisionClock *p_revision_clock) {
	project_id = p_project_id;
	editor_session_id = p_editor_session_id;
	revision_clock = p_revision_clock;
	store.clear();
}

void CompoundChangeSetCoordinator::shutdown() {
	store.clear();
	project_id.clear();
	editor_session_id.clear();
	revision_clock = nullptr;
}

CompoundChangeSetCoordinator::Outcome CompoundChangeSetCoordinator::prepare(const Dictionary &p_params, uint64_t p_now_ms) {
	Outcome outcome;
	if (!is_ready()) {
		outcome.error_code = "compound_executor_unavailable";
		outcome.error_message = "The compound coordinator is not initialized.";
		return outcome;
	}
	store.expire(p_now_ms);
	if (!CompoundChangeSetPlanner::validate_params(p_params)) {
		outcome.error_code = "invalid_request";
		outcome.error_message = "The compound change-set parameters are invalid.";
		return outcome;
	}
	const Dictionary coordinates = p_params["coordinates"];
	if (!_coordinates_current(coordinates)) {
		outcome.error_code = "stale_editor_state";
		outcome.error_message = "The requested revision coordinates are stale.";
		outcome.retryable = true;
		return outcome;
	}
	CompoundChangeSetPlanner::Plan plan;
	if (CompoundChangeSetPlanner::build(project_id, editor_session_id, p_params, p_now_ms, plan, outcome.error_code, outcome.error_message) != OK) {
		return outcome;
	}
	PreparedChangeSetStore::Record record;
	switch (store.admit(plan, record)) {
		case PreparedChangeSetStore::ADMISSION_CREATED:
		case PreparedChangeSetStore::ADMISSION_REPLAY:
			outcome.has_result = true;
			outcome.result = _preview_result(record);
			return outcome;
		case PreparedChangeSetStore::ADMISSION_CONFLICT:
			outcome.error_code = "idempotency_conflict";
			outcome.error_message = "The idempotency key is bound to a different immutable request.";
			return outcome;
		case PreparedChangeSetStore::ADMISSION_BUSY:
			outcome.error_code = "transaction_busy";
			outcome.error_message = "The bounded prepared change-set store is full.";
			outcome.retryable = true;
			return outcome;
	}
	return outcome;
}

CompoundChangeSetCoordinator::Outcome CompoundChangeSetCoordinator::status(const String &p_change_set_id, uint64_t p_now_ms) {
	Outcome outcome;
	store.expire(p_now_ms);
	PreparedChangeSetStore::Record record;
	if (!store.get(p_change_set_id, record)) {
		outcome.error_code = "change_set_not_found";
		outcome.error_message = "The bounded change-set record was not found.";
		return outcome;
	}
	outcome.has_result = true;
	outcome.result = _preview_result(record);
	outcome.result["outcome"] = record.outcome;
	if (!record.error_code.is_empty()) {
		Dictionary error;
		error["code"] = record.error_code;
		error["message"] = record.error_message;
		outcome.result["terminal_error"] = error;
	}
	return outcome;
}

void CompoundChangeSetCoordinator::invalidate_all(uint64_t p_now_ms) {
	// The store intentionally exposes no mutable iterator. Expiry at zero cannot
	// invalidate unexpired records, so shutdown remains the fail-closed boundary.
	(void)p_now_ms;
	store.clear();
}

uint32_t CompoundChangeSetCoordinator::get_active_count() const {
	return store.get_active_count();
}

bool CompoundChangeSetCoordinator::is_ready() const {
	return revision_clock && !project_id.is_empty() && !editor_session_id.is_empty();
}
