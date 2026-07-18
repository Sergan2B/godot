/**************************************************************************/
/*  test_script_semantics_fixture.cpp                                     */
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

TEST_FORCE_LINK(test_script_semantics_fixture)

#include "core/crypto/crypto_core.h"
#include "core/io/file_access.h"
#include "core/io/json.h"
#include "core/os/os.h"
#include "core/templates/hash_map.h"
#include "tests/test_utils.h"

#include "modules/modules_enabled.gen.h"

#ifdef MODULE_GDSCRIPT_ENABLED
#include "modules/gdscript/gdscript_parser.h"
#endif

namespace TestScriptSemanticsFixture {

static String fixture_root() {
	return TestUtils::get_executable_dir().path_join("../tests/codex/fixtures/script_semantics_project").simplify_path();
}

static String oracle_root() {
	return TestUtils::get_executable_dir().path_join("../tests/codex/fixtures/script_semantics_oracle").simplify_path();
}

static Dictionary load_json_object(const String &p_path) {
	Error error = OK;
	const String text = FileAccess::get_file_as_string(p_path, &error);
	REQUIRE(error == OK);
	const Variant parsed = JSON::parse_string(text);
	REQUIRE(parsed.get_type() == Variant::DICTIONARY);
	return parsed;
}

static String sha256_file(const String &p_path) {
	Error error = OK;
	const PackedByteArray bytes = FileAccess::get_file_as_bytes(p_path, &error);
	REQUIRE(error == OK);
	PackedByteArray digest;
	digest.resize(32);
	REQUIRE(CryptoCore::sha256(bytes.ptr(), bytes.size(), digest.ptrw()) == OK);
	return String::hex_encode_buffer(digest.ptr(), digest.size());
}

static Vector2i position(const String &p_content, int p_character_offset) {
	const String prefix = p_content.substr(0, p_character_offset);
	const int last_newline = prefix.rfind("\n");
	const int line = prefix.count("\n") + 1;
	const int column = last_newline < 0 ? prefix.length() + 1 : prefix.length() - last_newline;
	return Vector2i(line, column);
}

TEST_CASE("[CodexScriptOracle] Manifest hashes and golden ranges reproduce exactly") {
	const Dictionary manifest = load_json_object(oracle_root().path_join("fixture-manifest.json"));
	const Dictionary golden = load_json_object(oracle_root().path_join("golden-script-graph.json"));
	const Array files = manifest["files"];
	CHECK(files.size() == 23);
	for (int index = 0; index < files.size(); index++) {
		const Dictionary file = files[index];
		CHECK(sha256_file(fixture_root().path_join(file["path"])) == String(file["sha256"]));
	}

	const Array documents = golden["documents"];
	HashMap<String, Dictionary> document_map;
	for (int index = 0; index < documents.size(); index++) {
		const Dictionary document = documents[index];
		document_map.insert(document["oracle_id"], document);
	}
	CHECK(document_map.size() == 8);
	const Array ranges = golden["ranges"];
	CHECK(ranges.size() == 65);
	for (int index = 0; index < ranges.size(); index++) {
		const Dictionary range = ranges[index];
		const HashMap<String, Dictionary>::ConstIterator document = document_map.find(range["document"]);
		REQUIRE(document);
		const String relative = String(document->value["path"]).trim_prefix("res://");
		Error error = OK;
		const String content = FileAccess::get_file_as_string(fixture_root().path_join(relative), &error);
		REQUIRE(error == OK);
		const String needle = range["needle"];
		int character_offset = 0;
		for (int occurrence = 0; occurrence <= int(range["occurrence"]); occurrence++) {
			character_offset = content.find(needle, occurrence == 0 ? 0 : character_offset + 1);
			REQUIRE(character_offset >= 0);
		}
		const int end_character_offset = character_offset + needle.length();
		const int start_byte = content.substr(0, character_offset).utf8().length();
		const int end_byte = content.substr(0, end_character_offset).utf8().length();
		const Array expected_bytes = range["bytes"];
		const Array expected_start = range["start"];
		const Array expected_end = range["end"];
		const Vector2i start = position(content, character_offset);
		const Vector2i end = position(content, end_character_offset);
		CHECK(start_byte == int(expected_bytes[0]));
		CHECK(end_byte == int(expected_bytes[1]));
		CHECK(start.x == int(expected_start[0]));
		CHECK(start.y == int(expected_start[1]));
		CHECK(end.x == int(expected_end[0]));
		CHECK(end.y == int(expected_end[1]));
	}
	CHECK(Array(golden["symbols"]).size() == 48);
	CHECK(Array(golden["relations"]).size() == 14);
	CHECK(Array(golden["diagnostics"]).size() == 4);
	CHECK(Array(golden["attachments"]).size() == 3);
}

#ifdef MODULE_GDSCRIPT_ENABLED
TEST_CASE("[CodexScriptOracle] Godot parser recognizes valid and broken fixture documents") {
	const Vector<String> valid_paths = {
		"scripts/base_actor.gd",
		"scripts/player.gd",
		"scripts/path_only.gd",
		"scripts/missing_base.gd",
		"scripts/cycle_a.gd",
		"scripts/cycle_b.gd",
	};
	for (const String &relative : valid_paths) {
		Error error = OK;
		const String content = FileAccess::get_file_as_string(fixture_root().path_join(relative), &error);
		REQUIRE(error == OK);
		GDScriptParser parser;
		CHECK(parser.parse(content, "res://" + relative, false) == OK);
	}
	Error broken_error = OK;
	const String broken = FileAccess::get_file_as_string(fixture_root().path_join("scripts/broken.gd"), &broken_error);
	REQUIRE(broken_error == OK);
	GDScriptParser broken_parser;
	CHECK(broken_parser.parse(broken, "res://scripts/broken.gd", false) == ERR_PARSE_ERROR);

	Dictionary metrics;
	metrics["schema_version"] = 1;
	metrics["fixture_files_loaded_absolute"] = true;
	metrics["gdscript_valid_documents"] = valid_paths.size();
	metrics["gdscript_parse_error_documents"] = 1;
	metrics["csharp_discovery_documents"] = FileAccess::exists(fixture_root().path_join("scripts/Enemy.cs")) ? 1 : 0;
	metrics["attachment_scenes"] = 3;
	print_line("[codex_s5_oracle] " + JSON::stringify(metrics));
}
#endif

} // namespace TestScriptSemanticsFixture
