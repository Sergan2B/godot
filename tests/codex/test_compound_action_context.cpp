/**************************************************************************/
/*  test_compound_action_context.cpp                                      */
/**************************************************************************/

#include "tests/test_macros.h"

TEST_FORCE_LINK(test_compound_action_context)

#include "modules/modules_enabled.gen.h"

#ifdef MODULE_CODEX_BRIDGE_ENABLED

#include "core/io/resource.h"
#include "core/object/undo_redo.h"
#include "scene/2d/node_2d.h"
#include "scene/resources/gradient.h"

#ifdef MODULE_GDSCRIPT_ENABLED
#include "modules/gdscript/gdscript.h"
#endif

#include "modules/codex_bridge/editor/compound_action_context.h"

namespace TestCompoundActionContext {

TEST_CASE("[CodexS10AtomicitySpike] Mixed scene resource script steps are one exact native action") {
	Node2D *scene_node = memnew(Node2D);
	scene_node->set_position(Vector2(1, 2));
	Ref<Gradient> resource;
	resource.instantiate();
	resource->set_interpolation_mode(Gradient::GRADIENT_INTERPOLATE_LINEAR);

#ifdef MODULE_GDSCRIPT_ENABLED
	Ref<GDScript> script;
	script.instantiate();
	script->set_source_code("extends Node\n");
#else
	Ref<Resource> script;
	script.instantiate();
	script->set_name("script-before");
#endif

	CompoundActionContext context;
	REQUIRE(context.add_property_step("scene", scene_node, SNAME("position"), Vector2(8, 13)) == OK);
	REQUIRE(context.add_property_step("resource", resource.ptr(), SNAME("interpolation_mode"), Gradient::GRADIENT_INTERPOLATE_CONSTANT) == OK);
#ifdef MODULE_GDSCRIPT_ENABLED
	REQUIRE(context.add_property_step("script", script.ptr(), SNAME("source_code"), "extends Node\nvar ready := true\n") == OK);
#else
	REQUIRE(context.add_property_step("script", script.ptr(), SNAME("resource_name"), "script-after") == OK);
#endif
	CHECK(context.get_step_count() == 3);
	CHECK(scene_node->get_position() == Vector2(1, 2));
	CHECK(resource->get_interpolation_mode() == Gradient::GRADIENT_INTERPOLATE_LINEAR);
	REQUIRE(context.seal() == OK);

	UndoRedo history;
	const int history_before = history.get_history_count();
	REQUIRE(context.register_on_history(&history, "Codex compound atomicity spike") == OK);
	CHECK(history.get_history_count() == history_before + 1);
	CHECK(scene_node->get_position() == Vector2(8, 13));
	CHECK(resource->get_interpolation_mode() == Gradient::GRADIENT_INTERPOLATE_CONSTANT);
#ifdef MODULE_GDSCRIPT_ENABLED
	CHECK(script->get_source_code() == "extends Node\nvar ready := true\n");
#endif

	REQUIRE(history.undo());
	CHECK(scene_node->get_position() == Vector2(1, 2));
	CHECK(resource->get_interpolation_mode() == Gradient::GRADIENT_INTERPOLATE_LINEAR);
#ifdef MODULE_GDSCRIPT_ENABLED
	CHECK(script->get_source_code() == "extends Node\n");
#endif
	REQUIRE(history.redo());
	CHECK(scene_node->get_position() == Vector2(8, 13));
	CHECK(resource->get_interpolation_mode() == Gradient::GRADIENT_INTERPOLATE_CONSTANT);
	REQUIRE(history.undo());
	history.clear_history();
	memdelete(scene_node);
}

TEST_CASE("[CodexS10AtomicitySpike] Failed final preflight creates no native action and mutates nothing") {
	Node2D *scene_node = memnew(Node2D);
	scene_node->set_position(Vector2(1, 2));
	Ref<Gradient> resource;
	resource.instantiate();
	resource->set_interpolation_mode(Gradient::GRADIENT_INTERPOLATE_LINEAR);

	CompoundActionContext context;
	REQUIRE(context.add_property_step("scene", scene_node, SNAME("position"), Vector2(8, 13)) == OK);
	REQUIRE(context.add_property_step("resource", resource.ptr(), SNAME("interpolation_mode"), Gradient::GRADIENT_INTERPOLATE_CONSTANT) == OK);
	REQUIRE(context.seal() == OK);
	resource->set_interpolation_mode(Gradient::GRADIENT_INTERPOLATE_CUBIC);

	UndoRedo history;
	CHECK(context.register_on_history(&history, "must not exist") == ERR_BUSY);
	CHECK(history.get_history_count() == 0);
	CHECK(scene_node->get_position() == Vector2(1, 2));
	CHECK(resource->get_interpolation_mode() == Gradient::GRADIENT_INTERPOLATE_CUBIC);
	memdelete(scene_node);
}

} // namespace TestCompoundActionContext

#endif // MODULE_CODEX_BRIDGE_ENABLED
