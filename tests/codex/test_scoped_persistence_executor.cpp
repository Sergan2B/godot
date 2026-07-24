/**************************************************************************/
/*  test_scoped_persistence_executor.cpp                                  */
/**************************************************************************/

#include "tests/test_macros.h"

TEST_FORCE_LINK(test_scoped_persistence_executor)

#include "modules/modules_enabled.gen.h"

#ifdef MODULE_CODEX_BRIDGE_ENABLED

#include "core/config/project_settings.h"
#include "core/io/dir_access.h"
#include "core/io/file_access.h"
#include "core/io/resource_loader.h"
#include "scene/main/node.h"
#include "scene/resources/gradient.h"
#include "scene/resources/packed_scene.h"

#include "modules/codex_bridge/editor/scoped_persistence_executor.h"

namespace TestScopedPersistenceExecutor {

static const String FIXTURE_DIRECTORY = "res://tests/codex/.s10-persistence-fixture";
static const String SCRIPT_PATH = FIXTURE_DIRECTORY + "/fixture.gd";
static const String RESOURCE_PATH = FIXTURE_DIRECTORY + "/accent.tres";
static const String SCENE_PATH = FIXTURE_DIRECTORY + "/fixture.tscn";

static Error write_text(const String &p_path, const String &p_text) {
	DirAccess::make_dir_recursive_absolute(ProjectSettings::get_singleton()->globalize_path(p_path.get_base_dir()));
	Error error = OK;
	Ref<FileAccess> file = FileAccess::open(p_path, FileAccess::WRITE, &error);
	if (error != OK || file.is_null()) {
		return error;
	}
	file->store_string(p_text);
	file->flush();
	return file->get_error();
}

static String digest(const String &p_path) {
	return "sha256:" + FileAccess::get_sha256(p_path);
}

static void cleanup_fixture() {
	DirAccess::remove_absolute(ProjectSettings::get_singleton()->globalize_path(SCRIPT_PATH));
	DirAccess::remove_absolute(ProjectSettings::get_singleton()->globalize_path(RESOURCE_PATH));
	DirAccess::remove_absolute(ProjectSettings::get_singleton()->globalize_path(SCENE_PATH));
	DirAccess::remove_absolute(ProjectSettings::get_singleton()->globalize_path(FIXTURE_DIRECTORY));
}

static Dictionary int_value(int64_t p_value) {
	Dictionary value;
	value["type"] = "int";
	value["value"] = p_value;
	return value;
}

static CompoundChangeSetPlanner::Plan plan_for(const String &p_script_hash) {
	Dictionary create;
	create["kind"] = "create_resource";
	create["alias"] = "alias:accent";
	create["path"] = RESOURCE_PATH;
	create["resource_class"] = "Gradient";
	Dictionary mode;
	mode["name"] = "interpolation_mode";
	mode["value"] = int_value(1);
	Array properties;
	properties.push_back(mode);
	create["properties"] = properties;

	Dictionary edit;
	edit["start_byte"] = 26;
	edit["end_byte"] = 31;
	edit["replacement"] = "ready";
	Dictionary script;
	script["kind"] = "update_gdscript";
	script["path"] = SCRIPT_PATH;
	script["expected_hash"] = p_script_hash;
	Array edits;
	edits.push_back(edit);
	script["edits"] = edits;

	CompoundChangeSetPlanner::Plan plan;
	plan.change_set_id = "change-set:1234567890abcdef1234567890abcdef";
	plan.ordered_operations.push_back(create);
	plan.ordered_operations.push_back(script);
	plan.save_scope.push_back(RESOURCE_PATH);
	plan.save_scope.push_back(SCRIPT_PATH);
	return plan;
}

TEST_CASE("[CodexS10Persistence] Staging is read-only and commit restore preserve exact file state") {
	cleanup_fixture();
	const String source = "extends Node\nvar state := false\n";
	REQUIRE(write_text(SCRIPT_PATH, source) == OK);
	const String source_digest = digest(SCRIPT_PATH);
	CompoundChangeSetPlanner::Plan plan = plan_for(source_digest);
	ScopedPersistenceExecutor::Prepared prepared;
	String code;
	String message;
	REQUIRE(ScopedPersistenceExecutor::stage(plan, Dictionary(), String(), ObjectID(), prepared, code, message) == OK);
	CHECK(prepared.staged);
	CHECK(prepared.files.size() == 2);
	CHECK(FileAccess::get_file_as_string(SCRIPT_PATH) == source);
	CHECK(digest(SCRIPT_PATH) == source_digest);
	CHECK_FALSE(FileAccess::exists(RESOURCE_PATH));
	for (const ScopedPersistenceExecutor::FileRecord &record : prepared.files) {
		CHECK(FileAccess::exists(record.stage_path));
		CHECK_FALSE(record.postimage_digest.is_empty());
		CHECK_FALSE(record.stage_path.contains(SCRIPT_PATH));
		CHECK_FALSE(record.escrow_path.contains(SCRIPT_PATH));
	}

	REQUIRE(ScopedPersistenceExecutor::commit(prepared, code, message) == OK);
	CHECK(FileAccess::get_file_as_string(SCRIPT_PATH) == "extends Node\nvar state := ready\n");
	CHECK(FileAccess::exists(RESOURCE_PATH));
	Ref<Gradient> gradient = ResourceLoader::load(RESOURCE_PATH, "Gradient", ResourceFormatLoader::CACHE_MODE_IGNORE);
	REQUIRE(gradient.is_valid());
	CHECK(gradient->get_interpolation_mode() == Gradient::GRADIENT_INTERPOLATE_CONSTANT);

	REQUIRE(ScopedPersistenceExecutor::restore(prepared, code, message) == OK);
	CHECK(FileAccess::get_file_as_string(SCRIPT_PATH) == source);
	CHECK(digest(SCRIPT_PATH) == source_digest);
	CHECK_FALSE(FileAccess::exists(RESOURCE_PATH));
	REQUIRE(ScopedPersistenceExecutor::commit(prepared, code, message) == OK);
	CHECK(FileAccess::get_file_as_string(SCRIPT_PATH) == "extends Node\nvar state := ready\n");
	CHECK(FileAccess::exists(RESOURCE_PATH));
	REQUIRE(ScopedPersistenceExecutor::restore(prepared, code, message) == OK);
	ScopedPersistenceExecutor::cleanup(prepared);
	cleanup_fixture();
}

TEST_CASE("[CodexS10Persistence] Stale preimages and external postimage edits block writes and restore") {
	cleanup_fixture();
	const String source = "extends Node\nvar state := false\n";
	REQUIRE(write_text(SCRIPT_PATH, source) == OK);
	CompoundChangeSetPlanner::Plan stale_plan = plan_for("sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
	ScopedPersistenceExecutor::Prepared prepared;
	String code;
	String message;
	CHECK(ScopedPersistenceExecutor::stage(stale_plan, Dictionary(), String(), ObjectID(), prepared, code, message) == ERR_INVALID_DATA);
	CHECK(code == "script_editor_conflict");
	CHECK(FileAccess::get_file_as_string(SCRIPT_PATH) == source);
	CHECK_FALSE(FileAccess::exists(RESOURCE_PATH));

	CompoundChangeSetPlanner::Plan plan = plan_for(digest(SCRIPT_PATH));
	REQUIRE(ScopedPersistenceExecutor::stage(plan, Dictionary(), String(), ObjectID(), prepared, code, message) == OK);
	REQUIRE(ScopedPersistenceExecutor::commit(prepared, code, message) == OK);
	REQUIRE(write_text(SCRIPT_PATH, "extends Node\n# external edit\n") == OK);
	CHECK(ScopedPersistenceExecutor::restore(prepared, code, message) == ERR_BUSY);
	CHECK(code == "rollback_blocked");
	CHECK(FileAccess::get_file_as_string(SCRIPT_PATH) == "extends Node\n# external edit\n");

	// Put back the exact postimage solely so the test can safely clean up its
	// own generated files without weakening the production rollback guard.
	REQUIRE(write_text(SCRIPT_PATH, "extends Node\nvar state := ready\n") == OK);
	REQUIRE(ScopedPersistenceExecutor::restore(prepared, code, message) == OK);
	ScopedPersistenceExecutor::cleanup(prepared);
	cleanup_fixture();
}

TEST_CASE("[CodexS10Persistence] Resource property allowlist excludes tracks shaders and dynamic parameters") {
	CHECK(ScopedPersistenceExecutor::property_allowed("Gradient", "colors"));
	CHECK(ScopedPersistenceExecutor::property_allowed("Animation", "length"));
	CHECK_FALSE(ScopedPersistenceExecutor::property_allowed("Animation", "tracks"));
	CHECK(ScopedPersistenceExecutor::property_allowed("StandardMaterial3D", "albedo_texture"));
	CHECK_FALSE(ScopedPersistenceExecutor::property_allowed("StandardMaterial3D", "shader"));
	CHECK_FALSE(ScopedPersistenceExecutor::property_allowed("ShaderMaterial", "shader_parameter/foo"));
	CHECK_FALSE(ScopedPersistenceExecutor::property_allowed("ParticleProcessMaterial", "emission_shape"));
}

TEST_CASE("[CodexS10Persistence] Explicit scene scope stages only after in-memory mutation and restores the exact preimage") {
	cleanup_fixture();
	const String preimage = "[gd_scene format=3]\n\n[node name=\"Fixture\" type=\"Node\"]\n";
	REQUIRE(write_text(SCENE_PATH, preimage) == OK);
	const String preimage_digest = digest(SCENE_PATH);

	Node *root = memnew(Node);
	root->set_name("Fixture");
	root->set_scene_file_path(SCENE_PATH);

	Dictionary operation;
	operation["kind"] = "create_node";
	CompoundChangeSetPlanner::Plan plan;
	plan.change_set_id = "change-set:fedcba0987654321fedcba0987654321";
	plan.ordered_operations.push_back(operation);
	plan.save_scope.push_back(SCENE_PATH);

	ScopedPersistenceExecutor::Prepared prepared;
	String code;
	String message;
	REQUIRE(ScopedPersistenceExecutor::stage(plan, Dictionary(), SCENE_PATH, root->get_instance_id(), prepared, code, message) == OK);
	REQUIRE(prepared.files.size() == 1);
	CHECK(prepared.files[0].scene);
	CHECK(prepared.files[0].postimage_digest.is_empty());
	CHECK(digest(SCENE_PATH) == preimage_digest);

	Node *child = memnew(Node);
	child->set_name("PersistedChild");
	root->add_child(child);
	child->set_owner(root);
	REQUIRE(ScopedPersistenceExecutor::commit(prepared, code, message) == OK);
	CHECK(prepared.files[0].postimage_digest.begins_with("sha256:"));
	CHECK(digest(SCENE_PATH) == prepared.files[0].postimage_digest);
	CHECK(digest(SCENE_PATH) != preimage_digest);
	CHECK(ScopedPersistenceExecutor::verify_postimages(prepared));
	Ref<PackedScene> packed = ResourceLoader::load(SCENE_PATH, "PackedScene", ResourceFormatLoader::CACHE_MODE_IGNORE);
	REQUIRE(packed.is_valid());
	Node *instantiated = packed->instantiate();
	REQUIRE(instantiated != nullptr);
	CHECK(instantiated->has_node(NodePath("PersistedChild")));
	memdelete(instantiated);

	REQUIRE(ScopedPersistenceExecutor::restore(prepared, code, message) == OK);
	CHECK(FileAccess::get_file_as_string(SCENE_PATH) == preimage);
	CHECK(digest(SCENE_PATH) == preimage_digest);
	CHECK(ScopedPersistenceExecutor::verify_preimages(prepared));
	ScopedPersistenceExecutor::cleanup(prepared);
	memdelete(root);
	cleanup_fixture();
}

} // namespace TestScopedPersistenceExecutor

#endif // MODULE_CODEX_BRIDGE_ENABLED
