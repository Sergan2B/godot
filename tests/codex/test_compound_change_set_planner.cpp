/**************************************************************************/
/*  test_compound_change_set_planner.cpp                                  */
/**************************************************************************/

#include "tests/test_macros.h"

TEST_FORCE_LINK(test_compound_change_set_planner)

#include "modules/modules_enabled.gen.h"

#ifdef MODULE_CODEX_BRIDGE_ENABLED

#include "core/io/json.h"

#include "modules/codex_bridge/editor/compound_change_set_planner.h"
#include "modules/codex_bridge/editor/compound_change_set_coordinator.h"
#include "modules/codex_bridge/editor/prepared_change_set_store.h"

namespace TestCompoundChangeSetPlanner {

static const String PROJECT_ID = "project:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
static const String EDITOR_ID = "editor:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

static Dictionary int_value(int64_t p_value) {
	Dictionary value;
	value["type"] = "int";
	value["value"] = p_value;
	return value;
}

static Dictionary property(const String &p_name, const Dictionary &p_value) {
	Dictionary result;
	result["name"] = p_name;
	result["value"] = p_value;
	return result;
}

static Dictionary create_resource(const String &p_alias, const String &p_path) {
	Dictionary operation;
	operation["kind"] = "create_resource";
	operation["alias"] = p_alias;
	operation["path"] = p_path;
	operation["resource_class"] = "Gradient";
	Array properties;
	properties.push_back(property("interpolation_mode", int_value(1)));
	operation["properties"] = properties;
	return operation;
}

static Dictionary update_resource(const String &p_reference) {
	Dictionary operation;
	operation["kind"] = "update_resource";
	operation["resource"] = p_reference;
	operation["expected_hash"] = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
	Array properties;
	properties.push_back(property("interpolation_color_space", int_value(0)));
	operation["properties"] = properties;
	return operation;
}

static Dictionary params_for(const Array &p_operations) {
	Dictionary coordinates;
	coordinates["editor_session_id"] = EDITOR_ID;
	coordinates["scene_id"] = "scene:cccccccccccccccccccccccccccccccc";
	coordinates["scene_revision"] = 7;
	coordinates["operation_seq"] = 12;
	coordinates["resource_revision"] = 4;
	coordinates["script_graph_revision"] = 9;
	Dictionary save_scope;
	Array paths;
	paths.push_back("res://validation/accent.tres");
	save_scope["paths"] = paths;
	Dictionary policy;
	policy["rollback"] = "on_required_failure";
	policy["warnings"] = "fail_on_introduced";
	policy["runtime"] = "skip";
	Dictionary params;
	params["idempotency_key"] = "idempotency:dddddddddddddddddddddddddddddddd";
	params["coordinates"] = coordinates;
	params["operations"] = p_operations;
	params["save_scope"] = save_scope;
	params["validation_policy"] = policy;
	return params;
}

TEST_CASE("[CodexS10CompoundLifecycle] Planner produces a stable immutable alias-ordered preview") {
	Array operations;
	operations.push_back(update_resource("alias:accent"));
	operations.push_back(create_resource("alias:accent", "res://validation/accent.tres"));
	const Dictionary params = params_for(operations);
	const String before = JSON::stringify(params, "", true, true);
	CompoundChangeSetPlanner::Plan plan;
	String code;
	String message;
	REQUIRE(CompoundChangeSetPlanner::build(PROJECT_ID, EDITOR_ID, params, 1000, plan, code, message) == OK);
	CHECK(plan.ordered_operations.size() == 2);
	CHECK(Dictionary(plan.ordered_operations[0])["kind"] == "create_resource");
	CHECK(Dictionary(plan.ordered_operations[1])["kind"] == "update_resource");
	CHECK((int64_t)plan.original_order[0] == 1);
	CHECK((int64_t)plan.original_order[1] == 0);
	CHECK(plan.change_set_id.begins_with("change-set:"));
	CHECK(plan.request_digest.begins_with("sha256:"));
	CHECK(plan.preview_digest.begins_with("sha256:"));
	CHECK(plan.risk == "destructive");
	CHECK(plan.canonical_preview_json.utf8().length() <= CompoundChangeSetPlanner::MAX_PREVIEW_BYTES);
	CHECK_FALSE(plan.canonical_preview_json.contains("interpolation_color_space"));
	CHECK_FALSE(plan.canonical_preview_json.contains("\"value\":0"));
	CHECK(JSON::stringify(params, "", true, true) == before);
}

TEST_CASE("[CodexS10CompoundLifecycle] Operation bounds aliases and conflicting writes fail before admission") {
	String code;
	String message;
	CompoundChangeSetPlanner::Plan plan;
	Array empty;
	CHECK(CompoundChangeSetPlanner::build(PROJECT_ID, EDITOR_ID, params_for(empty), 1000, plan, code, message) == ERR_INVALID_DATA);

	Array maximum;
	for (int index = 0; index < 16; index++) {
		maximum.push_back(create_resource("alias:item_" + itos(index), "res://validation/item_" + itos(index) + ".tres"));
	}
	Dictionary maximum_params = params_for(maximum);
	REQUIRE(CompoundChangeSetPlanner::build(PROJECT_ID, EDITOR_ID, maximum_params, 1000, plan, code, message) == OK);
	Array overflow = maximum.duplicate(true);
	overflow.push_back(create_resource("alias:overflow", "res://validation/overflow.tres"));
	CHECK(CompoundChangeSetPlanner::build(PROJECT_ID, EDITOR_ID, params_for(overflow), 1000, plan, code, message) == ERR_INVALID_DATA);

	Array missing_alias;
	missing_alias.push_back(update_resource("alias:missing"));
	CHECK(CompoundChangeSetPlanner::build(PROJECT_ID, EDITOR_ID, params_for(missing_alias), 1000, plan, code, message) == ERR_DOES_NOT_EXIST);
	CHECK(code == "alias_not_found");

	Array duplicates;
	duplicates.push_back(create_resource("alias:first", "res://validation/accent.tres"));
	duplicates.push_back(create_resource("alias:second", "res://validation/accent.tres"));
	CHECK(CompoundChangeSetPlanner::build(PROJECT_ID, EDITOR_ID, params_for(duplicates), 1000, plan, code, message) == ERR_ALREADY_EXISTS);
	CHECK(code == "conflicting_writes");
}

TEST_CASE("[CodexS10CompoundLifecycle] Memory-only scene changes require an empty save scope") {
	Array operations;
	Dictionary operation;
	operation["kind"] = "create_node";
	operation["parent_node_id"] = "node:11111111111111111111111111111111";
	operation["godot_type"] = "Node2D";
	operation["name"] = "MemoryOnly";
	operations.push_back(operation);
	Dictionary params = params_for(operations);
	Dictionary save_scope;
	save_scope["paths"] = Array();
	params["save_scope"] = save_scope;
	CHECK(CompoundChangeSetPlanner::validate_params(params));

	Array persistent;
	persistent.push_back(create_resource("alias:accent", "res://validation/accent.tres"));
	params["operations"] = persistent;
	CHECK_FALSE(CompoundChangeSetPlanner::validate_params(params));
}

TEST_CASE("[CodexS10CompoundLifecycle] Prepared store is bounded idempotent immutable and expiry-safe") {
	Array operations;
	operations.push_back(create_resource("alias:accent", "res://validation/accent.tres"));
	CompoundChangeSetPlanner::Plan plan;
	String code;
	String message;
	REQUIRE(CompoundChangeSetPlanner::build(PROJECT_ID, EDITOR_ID, params_for(operations), 1000, plan, code, message) == OK);
	PreparedChangeSetStore store;
	PreparedChangeSetStore::Record record;
	CHECK(store.admit(plan, record) == PreparedChangeSetStore::ADMISSION_CREATED);
	CHECK(store.get_active_count() == 1);
	CHECK(store.admit(plan, record) == PreparedChangeSetStore::ADMISSION_REPLAY);
	CHECK(store.get_total_count() == 1);

	CompoundChangeSetPlanner::Plan conflict = plan;
	conflict.request_digest = "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
	CHECK(store.admit(conflict, record) == PreparedChangeSetStore::ADMISSION_CONFLICT);
	CHECK(PreparedChangeSetStore::legal_transition(PreparedChangeSetStore::STATE_PREVIEWED, PreparedChangeSetStore::STATE_APPLYING));
	CHECK_FALSE(PreparedChangeSetStore::legal_transition(PreparedChangeSetStore::STATE_PREVIEWED, PreparedChangeSetStore::STATE_COMMITTED));
	const Vector<String> expired = store.expire(plan.expires_at_ms);
	CHECK(expired.size() == 1);
	REQUIRE(store.get(plan.change_set_id, record));
	CHECK(record.state == PreparedChangeSetStore::STATE_EXPIRED);
	CHECK(record.plan.preview_digest == plan.preview_digest);
	CHECK(store.get_active_count() == 0);
}

TEST_CASE("[CodexS10CompoundLifecycle] Coordinator rejects stale revisions and replays the exact preview") {
	BridgeRevisionClock revisions;
	revisions.initialize(EDITOR_ID);
	Array operations;
	operations.push_back(create_resource("alias:accent", "res://validation/accent.tres"));
	Dictionary params = params_for(operations);
	Dictionary coordinates = params["coordinates"];
	coordinates["scene_revision"] = 0;
	coordinates["operation_seq"] = 0;
	coordinates["resource_revision"] = 1;
	coordinates["script_graph_revision"] = 1;
	params["coordinates"] = coordinates;
	CompoundChangeSetCoordinator coordinator;
	coordinator.initialize(PROJECT_ID, EDITOR_ID, &revisions);
	CompoundChangeSetCoordinator::Outcome first = coordinator.prepare(params, 1000);
	REQUIRE(first.has_result);
	CHECK(first.result["state"] == "previewed");
	CHECK(coordinator.get_active_count() == 1);
	const String id = first.result["change_set_id"];
	const String digest = first.result["preview_digest"];
	CompoundChangeSetCoordinator::Outcome replay = coordinator.prepare(params, 1001);
	REQUIRE(replay.has_result);
	CHECK(replay.result["change_set_id"] == id);
	CHECK(replay.result["preview_digest"] == digest);
	CHECK(coordinator.get_active_count() == 1);

	coordinates["resource_revision"] = 2;
	params["coordinates"] = coordinates;
	CompoundChangeSetCoordinator::Outcome stale = coordinator.prepare(params, 1002);
	CHECK_FALSE(stale.has_result);
	CHECK(stale.error_code == "stale_editor_state");
	CHECK(stale.retryable);
	CompoundChangeSetCoordinator::Outcome status = coordinator.status(id, 1003);
	REQUIRE(status.has_result);
	CHECK(status.result["preview_digest"] == digest);
	coordinator.shutdown();
}

} // namespace TestCompoundChangeSetPlanner

#endif // MODULE_CODEX_BRIDGE_ENABLED
