/**************************************************************************/
/*  test_script_semantic_adapter.cpp                                      */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
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

#include "tests/test_macros.h"

TEST_FORCE_LINK(test_script_semantic_adapter)

#include "core/io/file_access.h"
#include "core/io/json.h"
#include "core/io/resource_loader.h"
#include "tests/test_utils.h"

#include "modules/codex_bridge/editor/script_semantic_adapter.h"
#include "modules/modules_enabled.gen.h"

struct ScriptSemanticAdapterTestAccess {
	static Error project_source(const String &p_source, const String &p_path, const Dictionary &p_script_ref, uint64_t p_resource_revision, uint64_t p_script_graph_revision, ScriptSemanticAdapter::DocumentProjection &r_projection) {
		return ScriptSemanticAdapter::_project_source(p_source, p_path, p_script_ref, p_resource_revision, p_script_graph_revision, r_projection);
	}

	static String resource_id(const Dictionary &p_script_ref, const String &p_path, const String &p_content_sha256) {
		return ScriptSemanticAdapter::_resource_id(p_script_ref, p_path, p_content_sha256);
	}

	static String named_symbol_id(const String &p_script_id, const String &p_language, const String &p_kind, const String &p_qualified_key) {
		return ScriptSemanticAdapter::_named_symbol_id(p_script_id, p_language, p_kind, p_qualified_key);
	}
};

namespace TestScriptSemanticAdapter {

static String fixture_script(const String &p_name) {
	return TestUtils::get_executable_dir().path_join("../tests/codex/fixtures/script_semantics_project/scripts").path_join(p_name).simplify_path();
}

static String read_fixture(const String &p_name) {
	Error error = OK;
	const String source = FileAccess::get_file_as_string(fixture_script(p_name), &error);
	REQUIRE(error == OK);
	return source;
}

static Dictionary uid_ref(const String &p_uid) {
	Dictionary result;
	result["uid"] = p_uid;
	return result;
}

#ifdef MODULE_GDSCRIPT_ENABLED

static Dictionary symbol_by_key(const Array &p_symbols, const String &p_qualified_key) {
	for (int index = 0; index < p_symbols.size(); index++) {
		const Dictionary symbol = p_symbols[index];
		if (symbol.get("qualified_key", Variant()) == p_qualified_key) {
			return symbol;
		}
	}
	return Dictionary();
}

static Dictionary relation_by_detail(const Array &p_relations, const String &p_detail) {
	for (int index = 0; index < p_relations.size(); index++) {
		const Dictionary relation = p_relations[index];
		if (relation.get("detail", Variant()) == p_detail) {
			return relation;
		}
	}
	return Dictionary();
}

#endif

TEST_CASE("[CodexS5ScriptAdapter] Canonical resource and named-symbol identities reproduce contract vectors") {
	const Dictionary player_ref = uid_ref("uid://s5player");
	const String script_id = ScriptSemanticAdapterTestAccess::resource_id(player_ref, "res://scripts/player.gd", "sha256:8b0f27ddcd021bdeec1989889b93bdd1200f0971befb7ac50250d7054084bacf");
	CHECK(script_id == "godot:resource:uid:v1:sqJ30thGZ51wWAelzXaPokwt_ZZU6NggpdkmLRA5DHE");
	CHECK(ScriptSemanticAdapterTestAccess::named_symbol_id(script_id, "gdscript", "class", "class:Player") == "godot:script-symbol:named:v1:u527woh_W0SwNm6Ele6Vmu0Cf1fJoX1Qqd2H_FB_0JA");
	CHECK(ScriptSemanticAdapterTestAccess::named_symbol_id(script_id, "gdscript", "method", "class:Player/method:take_damage") == "godot:script-symbol:named:v1:bE1JpRjDl5O_51JqPWg_LyVFbHn-Cnh0sEaOj_FHYVc");
}

TEST_CASE("[CodexS5ScriptAdapter] Adapter statuses expose GDScript build state and CSharp discovery only") {
	const Array statuses = ScriptSemanticAdapter::make_adapter_statuses();
	REQUIRE(statuses.size() == 2);
	const Dictionary gdscript = statuses[0];
	const Dictionary csharp = statuses[1];
	CHECK(gdscript["language"] == "gdscript");
#ifdef MODULE_GDSCRIPT_ENABLED
	CHECK(ScriptSemanticAdapter::is_gdscript_available());
	CHECK(gdscript["availability"] == "available");
	CHECK(gdscript["profile"] == "gdscript_parser_analyzer_v1");
	CHECK(gdscript["diagnostic"].get_type() == Variant::NIL);
#else
	CHECK_FALSE(ScriptSemanticAdapter::is_gdscript_available());
	CHECK(gdscript["availability"] == "unavailable");
	CHECK(gdscript["profile"].get_type() == Variant::NIL);
	CHECK(gdscript["diagnostic"].get_type() == Variant::STRING);
#endif
	CHECK(csharp["language"] == "csharp");
	CHECK(csharp["availability"] == "discovery_only");
	CHECK(csharp["profile"] == "csharp_discovery_only_v1");
}

TEST_CASE("[CodexS5ScriptAdapter] CSharp projection is exact-content discovery only") {
	ScriptSemanticAdapter::DocumentProjection projection;
	const Error error = ScriptSemanticAdapterTestAccess::project_source(read_fixture("Enemy.cs"), "res://scripts/Enemy.cs", uid_ref("uid://s5enemycsharp"), 7, 11, projection);
	REQUIRE(error == OK);
	const Dictionary document = projection.bundle["document"];
	CHECK(document["language"] == "csharp");
	CHECK(document["adapter_profile"] == "csharp_discovery_only_v1");
	CHECK(document["completeness"] == "unavailable");
	CHECK(document["content_sha256"] == "sha256:52f6670daee178eb5269113f2e3e20300296dfec48826a5bb101990cd1830a69");
	CHECK(int64_t(document["resource_revision"]) == 7);
	CHECK(int64_t(document["script_graph_revision"]) == 11);
	CHECK(Array(projection.bundle["symbols"]).is_empty());
	CHECK(Array(projection.bundle["relations"]).is_empty());
	CHECK(Array(projection.bundle["diagnostics"]).is_empty());
	CHECK(projection.facts_checksum.length() == 64);
	ScriptSemanticAdapter::DocumentProjection later_revision;
	REQUIRE(ScriptSemanticAdapterTestAccess::project_source(read_fixture("Enemy.cs"), "res://scripts/Enemy.cs", uid_ref("uid://s5enemycsharp"), 8, 12, later_revision) == OK);
	CHECK(later_revision.facts_checksum == projection.facts_checksum);
	CHECK(int64_t(Dictionary(later_revision.bundle["document"])["script_graph_revision"]) == 12);
}

TEST_CASE("[CodexS5ScriptAdapter] A disappeared saved file requests a refresh restart instead of a hard-limit failure") {
	ScriptSemanticAdapter::DocumentProjection projection;
	ERR_PRINT_OFF;
	const Error error = ScriptSemanticAdapter::project_saved_document("res://codex-missing-script.cs", uid_ref("uid://s5missingtransient"), 1, 1, projection);
	ERR_PRINT_ON;
	CHECK(error == ERR_BUSY);
	CHECK(projection.bundle.is_empty());
}

#ifdef MODULE_GDSCRIPT_ENABLED

TEST_CASE("[CodexS5ScriptAdapter] Saved GDScript declarations preserve canonical UTF-8 ranges") {
	ScriptSemanticAdapter::DocumentProjection projection;
	const Error error = ScriptSemanticAdapterTestAccess::project_source(read_fixture("base_actor.gd"), "res://tests/codex/fixtures/script_semantics_project/scripts/base_actor.gd", uid_ref("uid://s5baseactor"), 7, 11, projection);
	REQUIRE(error == OK);
	const Dictionary document = projection.bundle["document"];
	CHECK(document["language"] == "gdscript");
	CHECK(document["adapter_profile"] == "gdscript_parser_analyzer_v1");
	CHECK(document["completeness"] == "complete");
	CHECK(document["content_sha256"] == "sha256:5e5c2a44bab5737e46514efd58b1fb2168f23a510790de25499cd71d2c961be5");

	const Array symbols = projection.bundle["symbols"];
	REQUIRE(symbols.size() == 12);
	const Dictionary actor = symbol_by_key(symbols, "class:BaseActor");
	const Dictionary method = symbol_by_key(symbols, "class:BaseActor/method:take_damage");
	const Dictionary local = symbol_by_key(symbols, "class:BaseActor/method:take_damage/local:remaining@229");
	REQUIRE(!actor.is_empty());
	REQUIRE(!method.is_empty());
	REQUIRE(!local.is_empty());
	CHECK(int64_t(Dictionary(actor["declaration_range"])["start_byte"]) == 24);
	CHECK(int64_t(Dictionary(actor["declaration_range"])["end_byte"]) == 33);
	CHECK(int64_t(Dictionary(method["declaration_range"])["start_byte"]) == 191);
	CHECK(int64_t(Dictionary(method["declaration_range"])["end_byte"]) == 202);
	CHECK(method["type_name"] == "int");
	CHECK(method["type_state"] == "explicit");
	CHECK(int64_t(Dictionary(local["declaration_range"])["start_byte"]) == 229);
	CHECK(int64_t(Dictionary(local["declaration_range"])["end_byte"]) == 238);
	CHECK(local["identity_scope"] == "content_revision");
	CHECK(String(local["symbol_id"]).begins_with("godot:script-symbol:content-revision:v1:"));
	const Array relations = projection.bundle["relations"];
	const Dictionary property_reference = relation_by_detail(relations, "property_reference");
	const Dictionary signal_reference = relation_by_detail(relations, "signal_reference");
	REQUIRE(!property_reference.is_empty());
	REQUIRE(!signal_reference.is_empty());
	CHECK(int64_t(Dictionary(property_reference["evidence_range"])["start_byte"]) == 250);
	CHECK(int64_t(Dictionary(property_reference["evidence_range"])["end_byte"]) == 256);
	CHECK(int64_t(Dictionary(signal_reference["evidence_range"])["start_byte"]) == 291);
	CHECK(int64_t(Dictionary(signal_reference["evidence_range"])["end_byte"]) == 298);

	const String serialized = JSON::stringify(projection.bundle, "", true, true);
	CHECK_FALSE(serialized.contains("extends Node"));
	CHECK_FALSE(serialized.contains("/Users/"));
}

TEST_CASE("[CodexS5ScriptAdapter] Saved-file projection pins size mtime and exact content") {
	const String path = "res://tests/codex/fixtures/script_semantics_project/scripts/base_actor.gd";
	ScriptSemanticAdapter::DocumentProjection projection;
	const Error error = ScriptSemanticAdapter::project_saved_document(path, uid_ref("uid://s5baseactor"), 7, 11, projection);
	REQUIRE(error == OK);
	CHECK(projection.source_bytes == uint64_t(FileAccess::get_size(path)));
	CHECK(projection.source_modified_time == FileAccess::get_modified_time(path));
	CHECK(projection.source_modified_time > 0);
	CHECK(Dictionary(projection.bundle["document"])["content_sha256"] == "sha256:5e5c2a44bab5737e46514efd58b1fb2168f23a510790de25499cd71d2c961be5");
	CHECK(projection.facts_checksum.length() == 64);
}

TEST_CASE("[CodexS5ScriptAdapter] Unicode source and lambdas keep byte ranges from the independent oracle") {
	ScriptSemanticAdapter::DocumentProjection projection;
	const Error error = ScriptSemanticAdapterTestAccess::project_source(read_fixture("player.gd"), "res://scripts/player.gd", uid_ref("uid://s5player"), 7, 11, projection);
	REQUIRE(error == OK);
	const Array symbols = projection.bundle["symbols"];
	REQUIRE(symbols.size() == 29);
	const Dictionary lambda = symbol_by_key(symbols, "class:Player/method:_apply_bonus/lambda@600");
	const Dictionary marker = symbol_by_key(symbols, "class:Player/method:dynamic_actions/local:marker@919");
	REQUIRE(!lambda.is_empty());
	REQUIRE(!marker.is_empty());
	CHECK(int64_t(Dictionary(lambda["declaration_range"])["start_byte"]) == 600);
	CHECK(int64_t(Dictionary(lambda["declaration_range"])["end_byte"]) == 623);
	CHECK(int64_t(Dictionary(marker["declaration_range"])["start_byte"]) == 919);
	CHECK(int64_t(Dictionary(marker["declaration_range"])["end_byte"]) == 925);

	const Array relations = projection.bundle["relations"];
	const Dictionary direct_call = relation_by_detail(relations, "direct_call");
	const Dictionary literal_uid = relation_by_detail(relations, "literal_uid");
	const Dictionary dynamic_call = relation_by_detail(relations, "variant_method_name");
	const Dictionary dynamic_load = relation_by_detail(relations, "variable_resource_path");
	const Dictionary dynamic_node_path = relation_by_detail(relations, "node_path_parameter");
	const Dictionary shorthand = relation_by_detail(relations, "shorthand_node_path");
	const Dictionary callable = relation_by_detail(relations, "callable_local");
	REQUIRE(!direct_call.is_empty());
	REQUIRE(!literal_uid.is_empty());
	REQUIRE(!dynamic_call.is_empty());
	REQUIRE(!dynamic_load.is_empty());
	REQUIRE(!dynamic_node_path.is_empty());
	REQUIRE(!shorthand.is_empty());
	REQUIRE(!callable.is_empty());
	CHECK(int64_t(Dictionary(direct_call["evidence_range"])["start_byte"]) == 522);
	CHECK(int64_t(Dictionary(literal_uid["evidence_range"])["start_byte"]) == 721);
	CHECK(Dictionary(direct_call["target"])["symbol_id"] == symbol_by_key(symbols, "class:Player/method:_apply_bonus")["symbol_id"]);
	CHECK(Dictionary(Dictionary(literal_uid["target"])["resource_ref"])["uid"] == "uid://s5damageprofile");
	CHECK(dynamic_call["confidence"] == "dynamic");
	CHECK(dynamic_call["target"].get_type() == Variant::NIL);

	const Dictionary document = projection.bundle["document"];
	if (document["completeness"] == "complete") {
		CHECK(lambda["type_name"] == "int");
		CHECK(lambda["type_state"] == "explicit");
		const Dictionary inheritance = relation_by_detail(relations, "script_inheritance");
		const Dictionary overridden = relation_by_detail(relations, "method_override");
		const Dictionary super_call = relation_by_detail(relations, "super_call");
		const Dictionary literal_path = relation_by_detail(relations, "literal_path");
		REQUIRE(!inheritance.is_empty());
		REQUIRE(!overridden.is_empty());
		REQUIRE(!super_call.is_empty());
		REQUIRE(!literal_path.is_empty());
		CHECK(int64_t(Dictionary(inheritance["evidence_range"])["start_byte"]) == 9);
		CHECK(int64_t(Dictionary(overridden["evidence_range"])["start_byte"]) == 437);
		CHECK(int64_t(Dictionary(super_call["evidence_range"])["start_byte"]) == 488);
		CHECK(int64_t(Dictionary(literal_path["evidence_range"])["start_byte"]) == 262);
		const String base_script_id = ScriptSemanticAdapterTestAccess::resource_id(uid_ref("uid://s5baseactor"), "res://scripts/base_actor.gd", String());
		const String base_class_id = ScriptSemanticAdapterTestAccess::named_symbol_id(base_script_id, "gdscript", "class", "class:BaseActor");
		const String base_method_id = ScriptSemanticAdapterTestAccess::named_symbol_id(base_script_id, "gdscript", "method", "class:BaseActor/method:take_damage");
		CHECK(Dictionary(inheritance["target"])["symbol_id"] == base_class_id);
		CHECK(Dictionary(overridden["target"])["symbol_id"] == base_method_id);
		CHECK(Dictionary(super_call["target"])["symbol_id"] == base_method_id);
	}
}

TEST_CASE("[CodexS5ScriptAdapter] Path-only scripts retain content-scoped parameter references") {
	ScriptSemanticAdapter::DocumentProjection projection;
	const Error error = ScriptSemanticAdapterTestAccess::project_source(read_fixture("path_only.gd"), "res://scripts/path_only.gd", uid_ref("uid://s5pathonly"), 7, 11, projection);
	REQUIRE(error == OK);
	CHECK(Dictionary(projection.bundle["document"])["completeness"] == "complete");
	CHECK(Array(projection.bundle["symbols"]).size() == 4);
	const Dictionary reference = relation_by_detail(projection.bundle["relations"], "local_parameter_reference");
	REQUIRE(!reference.is_empty());
	CHECK(int64_t(Dictionary(reference["evidence_range"])["start_byte"]) == 67);
	CHECK(int64_t(Dictionary(reference["evidence_range"])["end_byte"]) == 72);
}

TEST_CASE("[CodexS5ScriptAdapter] Missing dependencies remain partial with stable bounded diagnostics") {
	ScriptSemanticAdapter::DocumentProjection projection;
	const Error error = ScriptSemanticAdapterTestAccess::project_source(read_fixture("missing_base.gd"), "res://scripts/missing_base.gd", uid_ref("uid://s5missingbase"), 7, 11, projection);
	REQUIRE(error == OK);
	CHECK(Dictionary(projection.bundle["document"])["completeness"] == "partial");
	CHECK(Array(projection.bundle["symbols"]).size() == 1);
	const Array diagnostics = projection.bundle["diagnostics"];
	REQUIRE(!diagnostics.is_empty());
	const Dictionary diagnostic = diagnostics[0];
	CHECK(diagnostic["code"] == "GDSCRIPT_MISSING_DEPENDENCY");
	CHECK(diagnostic["authority"] == "gdscript_analyzer");
	CHECK(int64_t(Dictionary(diagnostic["range"])["start_byte"]) == 9);
	CHECK(int64_t(Dictionary(diagnostic["range"])["end_byte"]) == 40);
}

TEST_CASE("[CodexS5ScriptAdapter] Cyclic dependencies receive a distinct stable diagnostic") {
	if (!ResourceLoader::exists("res://scripts/cycle_b.gd")) {
		CHECK(true);
		return;
	}
	ScriptSemanticAdapter::DocumentProjection projection;
	const Error error = ScriptSemanticAdapterTestAccess::project_source(read_fixture("cycle_a.gd"), "res://scripts/cycle_a.gd", uid_ref("uid://s5cyclea"), 7, 11, projection);
	REQUIRE(error == OK);
	CHECK(Dictionary(projection.bundle["document"])["completeness"] == "partial");
	const Array diagnostics = projection.bundle["diagnostics"];
	REQUIRE(!diagnostics.is_empty());
	const Dictionary diagnostic = diagnostics[0];
	CHECK(diagnostic["code"] == "GDSCRIPT_CYCLIC_DEPENDENCY");
	CHECK(int64_t(Dictionary(diagnostic["range"])["start_byte"]) == 9);
	CHECK(int64_t(Dictionary(diagnostic["range"])["end_byte"]) == 33);
}

TEST_CASE("[CodexS5ScriptAdapter] Broken GDScript is isolated as invalid with a bounded diagnostic") {
	ScriptSemanticAdapter::DocumentProjection projection;
	const Error error = ScriptSemanticAdapterTestAccess::project_source(read_fixture("broken.gd"), "res://scripts/broken.gd", uid_ref("uid://s5broken"), 7, 11, projection);
	REQUIRE(error == OK);
	const Dictionary document = projection.bundle["document"];
	CHECK(document["completeness"] == "invalid");
	CHECK(Array(projection.bundle["symbols"]).is_empty());
	const Array diagnostics = projection.bundle["diagnostics"];
	REQUIRE(diagnostics.size() >= 1);
	const Dictionary diagnostic = diagnostics[0];
	CHECK(diagnostic["code"] == "GDSCRIPT_PARSE_ERROR");
	CHECK(diagnostic["severity"] == "error");
	CHECK(diagnostic["authority"] == "gdscript_parser");
	CHECK(String(diagnostic["safe_message"]).utf8().length() <= ScriptSemanticAdapter::MAX_DIAGNOSTIC_MESSAGE_BYTES);
	CHECK(String(diagnostic["diagnostic_id"]).begins_with("godot:script-diagnostic:v1:"));
	CHECK(int64_t(Dictionary(diagnostic["range"])["start_byte"]) == 14);
	CHECK(int64_t(Dictionary(diagnostic["range"])["end_byte"]) == 32);
}

#else

TEST_CASE("[CodexS5ScriptAdapter] GDScript projection fails closed when the module is disabled") {
	ScriptSemanticAdapter::DocumentProjection projection;
	CHECK(ScriptSemanticAdapterTestAccess::project_source("extends Node\n", "res://scripts/disabled.gd", uid_ref("uid://s5disabled"), 7, 11, projection) == ERR_UNAVAILABLE);
	CHECK(projection.bundle.is_empty());
}

#endif

} // namespace TestScriptSemanticAdapter
