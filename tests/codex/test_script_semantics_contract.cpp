/**************************************************************************/
/*  test_script_semantics_contract.cpp                                    */
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

TEST_FORCE_LINK(test_script_semantics_contract)

#include "core/crypto/crypto_core.h"
#include "core/io/file_access.h"
#include "core/io/json.h"
#include "core/os/os.h"
#include "core/templates/hash_map.h"
#include "core/templates/vector.h"
#include "tests/test_utils.h"

#include "modules/modules_enabled.gen.h"

#ifdef MODULE_GDSCRIPT_ENABLED
#include "modules/gdscript/language_server/gdscript_extend_parser.h"
#endif

namespace TestScriptSemanticsContract {

static String contract_path() {
	return TestUtils::get_executable_dir().path_join("../tests/codex/fixtures/script_semantics_contract/contract-vectors.json").simplify_path();
}

static Dictionary load_contract() {
	Error error = OK;
	const String text = FileAccess::get_file_as_string(contract_path(), &error);
	REQUIRE(error == OK);
	const Variant parsed = JSON::parse_string(text);
	REQUIRE(parsed.get_type() == Variant::DICTIONARY);
	return parsed;
}

static String sha256(const String &p_value) {
	const CharString bytes = p_value.utf8();
	PackedByteArray digest;
	digest.resize(32);
	REQUIRE(CryptoCore::sha256(reinterpret_cast<const uint8_t *>(bytes.get_data()), bytes.length(), digest.ptrw()) == OK);
	return "sha256:" + String::hex_encode_buffer(digest.ptr(), digest.size());
}

static String digest(const String &p_domain, const Vector<String> &p_parts) {
	Vector<uint8_t> input;
	const CharString domain = p_domain.utf8();
	for (int index = 0; index < domain.length(); index++) {
		input.push_back(domain[index]);
	}
	input.push_back(0);
	for (int part_index = 0; part_index < p_parts.size(); part_index++) {
		if (part_index > 0) {
			input.push_back(0);
		}
		const CharString part = p_parts[part_index].utf8();
		for (int index = 0; index < part.length(); index++) {
			input.push_back(part[index]);
		}
	}
	PackedByteArray output;
	output.resize(32);
	REQUIRE(CryptoCore::sha256(input.ptr(), input.size(), output.ptrw()) == OK);
	return CryptoCore::b64_encode_str(output.ptr(), output.size()).replace("+", "-").replace("/", "_").trim_suffix("=");
}

static HashMap<String, Dictionary> source_map(const Dictionary &p_contract) {
	HashMap<String, Dictionary> result;
	const Array sources = p_contract["sources"];
	for (int index = 0; index < sources.size(); index++) {
		const Dictionary source = sources[index];
		const String name = source["name"];
		const String content = source["content"];
		const String expected_hash = source["sha256"];
		REQUIRE(!result.has(name));
		REQUIRE(sha256(content) == expected_hash);
		result.insert(name, source);
	}
	return result;
}

static String compute_identity(const Dictionary &p_vector, const Dictionary &p_algorithms, const HashMap<String, Dictionary> &p_sources) {
	const String kind = p_vector["kind"];
	const String script_id = p_vector["script_id"];
	const String language = p_vector["language"];
	if (kind == "named_symbol") {
		return "godot:script-symbol:named:v1:" + digest(p_algorithms["named_symbol_domain"], { script_id, language, p_vector["symbol_kind"], p_vector["qualified_key"] });
	}
	const String source_name = p_vector["source"];
	const HashMap<String, Dictionary>::ConstIterator source = p_sources.find(source_name);
	REQUIRE(source);
	if (kind == "content_symbol") {
		return "godot:script-symbol:content-revision:v1:" + digest(p_algorithms["content_symbol_domain"], { script_id, language, source->value["sha256"], p_vector["owner_qualified_key"], p_vector["symbol_kind"], itos(p_vector["start_byte"]), itos(p_vector["end_byte"]) });
	}
	REQUIRE(kind == "diagnostic");
	return "godot:script-diagnostic:v1:" + digest(p_algorithms["diagnostic_domain"], { script_id, language, source->value["sha256"], p_vector["severity"], p_vector["code"], itos(p_vector["start_byte"]), itos(p_vector["end_byte"]), p_vector["normalized_message"] });
}

static Vector2i position(const String &p_content, int p_character_offset) {
	const String prefix = p_content.substr(0, p_character_offset);
	const int last_newline = prefix.rfind("\n");
	const int line = prefix.count("\n") + 1;
	const int column = last_newline < 0 ? prefix.length() + 1 : prefix.length() - last_newline;
	return Vector2i(line, column);
}

static Pair<String, bool> classify(const Dictionary &p_vector) {
	const String authority = p_vector["authority"];
	const String resolution = p_vector["resolution"];
	String confidence;
	if ((authority == "gdscript_analyzer" || authority == "godot_resource_loader") && resolution == "resolved_unambiguous") {
		confidence = "exact";
	} else if (authority == "gdscript_parser" && (resolution == "dynamic_target" || resolution == "string_node_path")) {
		confidence = "dynamic";
	} else {
		FAIL("Unsupported confidence vector.");
	}
	return Pair<String, bool>(confidence, String(p_vector["freshness"]) == "current");
}

TEST_CASE("[CodexScriptContract] Identity vectors reproduce exactly") {
	const Dictionary contract = load_contract();
	const Dictionary algorithms = contract["algorithms"];
	const HashMap<String, Dictionary> sources = source_map(contract);
	const Array vectors = contract["identity_vectors"];
	CHECK(vectors.size() == 9);
	HashMap<String, String> computed;
	for (int index = 0; index < vectors.size(); index++) {
		const Dictionary vector = vectors[index];
		const String name = vector["name"];
		const String actual = compute_identity(vector, algorithms, sources);
		CHECK(actual == vector["expected_id"]);
		computed.insert(name, actual);
	}
	for (int index = 0; index < vectors.size(); index++) {
		const Dictionary vector = vectors[index];
		const String name = vector["name"];
		if (vector.has("same_identity_as")) {
			CHECK(computed[name] == computed[String(vector["same_identity_as"])]);
		}
		if (vector.has("different_identity_from")) {
			CHECK(computed[name] != computed[String(vector["different_identity_from"])]);
		}
	}
}

TEST_CASE("[CodexScriptContract] UTF-8 byte ranges reproduce exactly") {
	const Dictionary contract = load_contract();
	const HashMap<String, Dictionary> sources = source_map(contract);
	const Array vectors = contract["range_vectors"];
	CHECK(vectors.size() == 5);
	for (int index = 0; index < vectors.size(); index++) {
		const Dictionary vector = vectors[index];
		const String source_name = vector["source"];
		const HashMap<String, Dictionary>::ConstIterator source = sources.find(source_name);
		REQUIRE(source);
		const String content = source->value["content"];
		const String needle = vector["needle"];
		int character_offset = 0;
		for (int occurrence = 0; occurrence <= int(vector["occurrence"]); occurrence++) {
			character_offset = content.find(needle, occurrence == 0 ? 0 : character_offset + 1);
			REQUIRE(character_offset >= 0);
		}
		const int end_character_offset = character_offset + needle.length();
		const int start_byte = content.substr(0, character_offset).utf8().length();
		const int end_byte = content.substr(0, end_character_offset).utf8().length();
		const Vector2i start = position(content, character_offset);
		const Vector2i end = position(content, end_character_offset);
		CHECK(start_byte == int(vector["start_byte"]));
		CHECK(end_byte == int(vector["end_byte"]));
		CHECK(start.x == int(vector["start_line"]));
		CHECK(start.y == int(vector["start_column"]));
		CHECK(end.x == int(vector["end_line"]));
		CHECK(end.y == int(vector["end_column"]));
	}
}

TEST_CASE("[CodexScriptContract] Confidence remains independent from freshness") {
	const Dictionary contract = load_contract();
	const Array vectors = contract["confidence_vectors"];
	CHECK(vectors.size() == 6);
	for (int index = 0; index < vectors.size(); index++) {
		const Dictionary vector = vectors[index];
		const Pair<String, bool> actual = classify(vector);
		CHECK(actual.first == vector["expected_confidence"]);
		CHECK(actual.second == bool(vector["servable_as_current"]));
	}
}

#ifdef MODULE_GDSCRIPT_ENABLED
static int count_symbols(const LSP::DocumentSymbol &p_symbol) {
	int result = 1;
	for (const LSP::DocumentSymbol &child : p_symbol.children) {
		result += count_symbols(child);
	}
	return result;
}

TEST_CASE("[CodexScriptContract] D-07 bridge-owned projection works without an LSP client") {
	const String source =
			"extends RefCounted\n"
			"signal damaged(amount: int)\n"
			"const MAX_HEALTH := 100\n"
			"var health: int = MAX_HEALTH\n"
			"func take_damage(amount: int) -> int:\n"
			"\thealth -= amount\n"
			"\tdamaged.emit(amount)\n"
			"\treturn health\n";
	const String modified = source.replace("MAX_HEALTH := 100", "MAX_HEALTH := 120");

	const uint64_t cold_start = OS::get_singleton()->get_ticks_usec();
	ExtendGDScriptParser *cold = memnew(ExtendGDScriptParser);
	cold->parse(source, "res://scripts/d07_spike.gd");
	const uint64_t cold_usec = OS::get_singleton()->get_ticks_usec() - cold_start;
	REQUIRE(cold->parse_result == OK);
	CHECK(cold->get_diagnostics().is_empty());
	CHECK(count_symbols(cold->get_symbols()) >= 7);

	HashMap<String, ExtendGDScriptParser *> cache;
	const String cache_key = sha256(source);
	cache.insert(cache_key, cold);
	const uint64_t warm_start = OS::get_singleton()->get_ticks_usec();
	for (int iteration = 0; iteration < 1000; iteration++) {
		const HashMap<String, ExtendGDScriptParser *>::ConstIterator cached = cache.find(cache_key);
		REQUIRE(cached);
		REQUIRE(cached->value == cold);
	}
	const uint64_t warm_total_usec = OS::get_singleton()->get_ticks_usec() - warm_start;

	const uint64_t incremental_start = OS::get_singleton()->get_ticks_usec();
	ExtendGDScriptParser *incremental = memnew(ExtendGDScriptParser);
	incremental->parse(modified, "res://scripts/d07_spike.gd");
	const uint64_t incremental_usec = OS::get_singleton()->get_ticks_usec() - incremental_start;
	REQUIRE(incremental->parse_result == OK);
	CHECK(count_symbols(incremental->get_symbols()) == count_symbols(cold->get_symbols()));

	ExtendGDScriptParser invalid;
	invalid.parse("extends RefCounted\nfunc broken(\n", "res://scripts/d07_broken.gd");
	CHECK(invalid.parse_result == ERR_PARSE_ERROR);
	CHECK(!invalid.get_diagnostics().is_empty());

	const uint64_t memory_before = OS::get_singleton()->get_static_memory_usage();
	Vector<ExtendGDScriptParser *> retained;
	for (int index = 0; index < 32; index++) {
		ExtendGDScriptParser *parser = memnew(ExtendGDScriptParser);
		parser->parse(source, "res://scripts/d07_memory_" + itos(index) + ".gd");
		REQUIRE(parser->parse_result == OK);
		retained.push_back(parser);
	}
	const uint64_t memory_during = OS::get_singleton()->get_static_memory_usage();
	const uint64_t retained_memory_delta_bytes = memory_during >= memory_before ? memory_during - memory_before : 0;
	const uint64_t retained_memory_budget_bytes = 8 * 1024 * 1024;
	CHECK(retained_memory_delta_bytes <= retained_memory_budget_bytes);
	for (ExtendGDScriptParser *parser : retained) {
		memdelete(parser);
	}

	Dictionary metrics;
	metrics["schema_version"] = 1;
	metrics["selected"] = "bridge_owned_exact_content_cache";
	metrics["rejected"] = "active_lsp_peer_cache";
	metrics["selected_path_requires_lsp_client"] = false;
	metrics["execution_thread"] = "godot_test_main_thread";
	metrics["valid_projection"] = true;
	metrics["parse_error_diagnostic"] = true;
	metrics["cold_parse_usec"] = cold_usec;
	metrics["warm_lookup_iterations"] = 1000;
	metrics["warm_lookup_total_usec"] = warm_total_usec;
	metrics["incremental_parse_usec"] = incremental_usec;
	metrics["retained_projection_count"] = 32;
	metrics["retained_memory_delta_bytes"] = retained_memory_delta_bytes;
	metrics["retained_memory_budget_bytes"] = retained_memory_budget_bytes;
	metrics["retained_memory_within_budget"] = retained_memory_delta_bytes <= retained_memory_budget_bytes;
	metrics["symbol_count"] = count_symbols(cold->get_symbols());
	print_line("[codex_d07_spike] " + JSON::stringify(metrics));

	cache.clear();
	memdelete(incremental);
	memdelete(cold);
}
#endif

} // namespace TestScriptSemanticsContract
