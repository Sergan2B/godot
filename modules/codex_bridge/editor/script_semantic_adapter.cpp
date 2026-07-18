/**************************************************************************/
/*  script_semantic_adapter.cpp                                           */
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

#include "script_semantic_adapter.h"

#include "core/config/project_settings.h"
#include "core/crypto/crypto_core.h"
#include "core/io/file_access.h"
#include "core/io/json.h"
#include "core/io/resource_loader.h"
#include "core/io/resource_uid.h"
#include "core/templates/hash_map.h"
#include "core/templates/hash_set.h"
#include "core/version.h"

#include "modules/modules_enabled.gen.h"

#ifdef MODULE_GDSCRIPT_ENABLED
#include "modules/gdscript/gdscript.h"
#include "modules/gdscript/gdscript_analyzer.h"
#include "modules/gdscript/gdscript_parser.h"
#endif

#include <cstring>

namespace {

static bool utf8_within(const String &p_value, uint32_t p_limit) {
	return (uint64_t)p_value.utf8().length() <= p_limit;
}

static String sha256_hex(const uint8_t *p_data, uint64_t p_size) {
	PackedByteArray digest;
	digest.resize(32);
	if (CryptoCore::sha256(p_data, p_size, digest.ptrw()) != OK) {
		return String();
	}
	return String::hex_encode_buffer(digest.ptr(), digest.size());
}

static String sha256_hex(const String &p_value) {
	const CharString bytes = p_value.utf8();
	return sha256_hex(reinterpret_cast<const uint8_t *>(bytes.get_data()), bytes.length());
}

static String domain_digest(const String &p_domain, const Vector<String> &p_parts) {
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
	PackedByteArray digest;
	digest.resize(32);
	if (CryptoCore::sha256(input.ptr(), input.size(), digest.ptrw()) != OK) {
		return String();
	}
	return CryptoCore::b64_encode_str(digest.ptr(), digest.size()).replace("+", "-").replace("/", "_").trim_suffix("=");
}

static String make_resource_id(const Dictionary &p_script_ref, const String &p_path, const String &p_content_sha256) {
	if (p_script_ref.has("uid") && p_script_ref["uid"].get_type() == Variant::STRING) {
		return "godot:resource:uid:v1:" + domain_digest("godot-codex/resource-entity/uid/v1", { p_script_ref["uid"] });
	}
	return "godot:resource:path-content:v1:" + domain_digest("godot-codex/resource-entity/path-content/v1", { p_path, p_content_sha256 });
}

static String make_named_symbol_id(const String &p_script_id, const String &p_language, const String &p_kind, const String &p_qualified_key) {
	return "godot:script-symbol:named:v1:" + domain_digest("godot-codex/script-symbol/named/v1", { p_script_id, p_language, p_kind, p_qualified_key });
}

static String make_content_symbol_id(const String &p_script_id, const String &p_language, const String &p_content_sha256, const String &p_owner_qualified_key, const String &p_kind, uint64_t p_start_byte, uint64_t p_end_byte) {
	return "godot:script-symbol:content-revision:v1:" + domain_digest("godot-codex/script-symbol/content-revision/v1", { p_script_id, p_language, p_content_sha256, p_owner_qualified_key, p_kind, String::num_uint64(p_start_byte), String::num_uint64(p_end_byte) });
}

static String make_diagnostic_id(const String &p_script_id, const String &p_language, const String &p_content_sha256, const String &p_severity, const String &p_code, uint64_t p_start_byte, uint64_t p_end_byte, const String &p_message) {
	return "godot:script-diagnostic:v1:" + domain_digest("godot-codex/script-diagnostic/content-revision/v1", { p_script_id, p_language, p_content_sha256, p_severity, p_code, String::num_uint64(p_start_byte), String::num_uint64(p_end_byte), p_message });
}

#ifdef MODULE_GDSCRIPT_ENABLED

static String normalize_safe_message(const String &p_message) {
	String value = p_message;
	if (ProjectSettings::get_singleton()) {
		const String root = ProjectSettings::get_singleton()->get_resource_path().replace("\\", "/").trim_suffix("/");
		if (!root.is_empty()) {
			value = value.replace(root, "res://");
		}
	}
	String normalized;
	bool pending_space = false;
	for (int index = 0; index < value.length(); index++) {
		const char32_t character = value[index];
		if (character == ' ' || character == '\t' || character == '\r' || character == '\n') {
			pending_space = !normalized.is_empty();
			continue;
		}
		if (character < 0x20 || character == 0x7f) {
			continue;
		}
		if (pending_space) {
			normalized += " ";
			pending_space = false;
		}
		normalized += String::chr(character);
	}
	normalized = normalized.strip_edges();
	if (normalized.is_empty()) {
		normalized = "GDScript diagnostic without a safe message.";
	}
	if (!utf8_within(normalized, ScriptSemanticAdapter::MAX_DIAGNOSTIC_MESSAGE_BYTES)) {
		normalized = "GDScript diagnostic exceeded the safe message limit.";
	}
	return normalized;
}

static String symbol_visibility(const String &p_name) {
	return p_name.begins_with("_") ? "private" : "public";
}

#endif

static Dictionary make_adapter_status(const String &p_language, const String &p_availability, const Variant &p_profile, const Variant &p_version, const Variant &p_diagnostic) {
	Dictionary status;
	status["language"] = p_language;
	status["availability"] = p_availability;
	status["profile"] = p_profile;
	status["version"] = p_version;
	status["diagnostic"] = p_diagnostic;
	return status;
}

#ifdef MODULE_GDSCRIPT_ENABLED

struct ProjectionContext {
	String source;
	String path;
	Dictionary script_ref;
	String content_sha256;
	String script_id;
	uint64_t resource_revision = 0;
	uint64_t script_graph_revision = 0;
	Vector<String> lines;
	Vector<uint64_t> line_start_bytes;
	Array symbols;
	Array relations;
	Array diagnostics;
	HashMap<const GDScriptParser::Node *, String> declaration_symbols;
	HashSet<String> lambda_backed_symbols;
	HashSet<String> relation_keys;

	bool initialize_lines() {
		lines = source.split("\n", true);
		if (lines.is_empty()) {
			lines.push_back(String());
		}
		uint64_t offset = 0;
		for (int index = 0; index < lines.size(); index++) {
			line_start_bytes.push_back(offset);
			offset += lines[index].utf8().length();
			if (index + 1 < lines.size()) {
				offset++;
			}
		}
		return offset == (uint64_t)source.utf8().length();
	}

	bool byte_offset(int p_line, int p_column, uint64_t &r_offset) const {
		if (p_line < 1 || p_line > lines.size() || p_column < 1) {
			return false;
		}
		const String &line = lines[p_line - 1];
		const int character_offset = p_column - 1;
		if (character_offset > line.length()) {
			return false;
		}
		r_offset = line_start_bytes[p_line - 1] + line.substr(0, character_offset).utf8().length();
		return r_offset <= (uint64_t)source.utf8().length();
	}

	bool make_range(int p_start_line, int p_start_column, int p_end_line, int p_end_column, Dictionary &r_range) const {
		uint64_t start_byte = 0;
		uint64_t end_byte = 0;
		if (!byte_offset(p_start_line, p_start_column, start_byte) || !byte_offset(p_end_line, p_end_column, end_byte) || end_byte < start_byte) {
			return false;
		}
		r_range["path"] = path;
		r_range["content_sha256"] = content_sha256;
		r_range["start_byte"] = (int64_t)start_byte;
		r_range["end_byte"] = (int64_t)end_byte;
		r_range["start_line"] = p_start_line;
		r_range["start_column"] = p_start_column;
		r_range["end_line"] = p_end_line;
		r_range["end_column"] = p_end_column;
		return true;
	}

	bool make_range(const GDScriptParser::Node *p_node, Dictionary &r_range) const {
		return p_node && make_range(p_node->start_line, p_node->start_column, p_node->end_line, p_node->end_column, r_range);
	}

	bool append_symbol(const String &p_kind, const Variant &p_name, const String &p_qualified_key, const Variant &p_owner_symbol_id, bool p_persistent, const String &p_identity_owner_key, const GDScriptParser::Node *p_range_node, const Variant &p_signature, const Variant &p_type_name, const String &p_type_state, const Array &p_modifiers, bool p_documentation_present, String &r_symbol_id) {
		if ((uint32_t)symbols.size() >= ScriptSemanticAdapter::MAX_SYMBOLS_PER_DOCUMENT || p_qualified_key.is_empty() || !utf8_within(p_qualified_key, 2048) ||
				(p_name.get_type() == Variant::STRING && (String(p_name).is_empty() || !utf8_within(p_name, 512))) ||
				(p_signature.get_type() == Variant::STRING && !utf8_within(p_signature, ScriptSemanticAdapter::MAX_SIGNATURE_BYTES)) ||
				(p_type_name.get_type() == Variant::STRING && !utf8_within(p_type_name, 1024))) {
			return false;
		}
		Dictionary range;
		if (!make_range(p_range_node, range)) {
			return false;
		}
		const uint64_t start_byte = (uint64_t)(int64_t)range["start_byte"];
		const uint64_t end_byte = (uint64_t)(int64_t)range["end_byte"];
		r_symbol_id = p_persistent ? make_named_symbol_id(script_id, "gdscript", p_kind, p_qualified_key) : make_content_symbol_id(script_id, "gdscript", content_sha256, p_identity_owner_key, p_kind, start_byte, end_byte);
		if (r_symbol_id.is_empty()) {
			return false;
		}
		Dictionary symbol;
		symbol["symbol_id"] = r_symbol_id;
		symbol["script_ref"] = script_ref;
		symbol["language"] = "gdscript";
		symbol["kind"] = p_kind;
		symbol["name"] = p_name;
		symbol["qualified_key"] = p_qualified_key;
		symbol["owner_symbol_id"] = p_owner_symbol_id;
		symbol["identity_scope"] = p_persistent ? "persistent" : "content_revision";
		symbol["signature"] = p_signature;
		symbol["type_name"] = p_type_name;
		symbol["type_state"] = p_type_state;
		symbol["visibility"] = p_name.get_type() == Variant::STRING ? symbol_visibility(p_name) : "private";
		symbol["modifiers"] = p_modifiers;
		symbol["declaration_range"] = range;
		symbol["documentation_present"] = p_documentation_present;
		symbol["script_graph_revision"] = (int64_t)script_graph_revision;
		symbols.push_back(symbol);
		return true;
	}

	void bind_declaration(const GDScriptParser::Node *p_node, const GDScriptParser::IdentifierNode *p_identifier, const String &p_symbol_id) {
		if (p_node) {
			declaration_symbols.insert(p_node, p_symbol_id);
		}
		if (p_identifier) {
			declaration_symbols.insert(p_identifier, p_symbol_id);
		}
	}

	String find_declaration(const GDScriptParser::Node *p_node) const {
		const String *symbol_id = p_node ? declaration_symbols.getptr(p_node) : nullptr;
		return symbol_id ? *symbol_id : String();
	}

	bool append_relation(const String &p_source_symbol_id, const String &p_predicate, const Variant &p_target, const String &p_confidence, const GDScriptParser::Node *p_evidence_node, const String &p_detail) {
		if ((uint32_t)relations.size() >= ScriptSemanticAdapter::MAX_RELATIONS_PER_DOCUMENT || p_source_symbol_id.is_empty() || p_predicate.is_empty() ||
				(p_confidence != "exact" && p_confidence != "dynamic") || (p_confidence == "exact" && p_target.get_type() != Variant::DICTIONARY) ||
				(p_confidence == "dynamic" && p_target.get_type() != Variant::NIL) || !utf8_within(p_detail, 128)) {
			return false;
		}
		Dictionary evidence_range;
		if (!make_range(p_evidence_node, evidence_range)) {
			return false;
		}
		const String relation_key = p_source_symbol_id + "\n" + p_predicate + "\n" + p_confidence + "\n" + p_detail + "\n" +
				String::num_int64(evidence_range["start_byte"]) + "\n" + String::num_int64(evidence_range["end_byte"]);
		if (relation_keys.has(relation_key)) {
			return true;
		}
		relation_keys.insert(relation_key);
		Dictionary source;
		source["kind"] = "symbol";
		source["symbol_id"] = p_source_symbol_id;
		Dictionary relation;
		relation["source"] = source;
		relation["predicate"] = p_predicate;
		relation["target"] = p_target;
		relation["confidence"] = p_confidence;
		relation["evidence_range"] = evidence_range;
		relation["detail"] = p_detail;
		relation["authority"] = "gdscript_parser_analyzer";
		relation["script_graph_revision"] = (int64_t)script_graph_revision;
		relations.push_back(relation);
		return true;
	}

	bool append_diagnostic(const String &p_code, const String &p_severity, const String &p_message, const String &p_authority, int p_start_line, int p_start_column, int p_end_line, int p_end_column) {
		if ((uint32_t)diagnostics.size() >= ScriptSemanticAdapter::MAX_DIAGNOSTICS_PER_DOCUMENT) {
			return false;
		}
		Dictionary range;
		const bool has_range = make_range(p_start_line, p_start_column, p_end_line, p_end_column, range);
		const uint64_t start_byte = has_range ? (uint64_t)(int64_t)range["start_byte"] : 0;
		const uint64_t end_byte = has_range ? (uint64_t)(int64_t)range["end_byte"] : 0;
		const String safe_message = normalize_safe_message(p_message);
		Dictionary diagnostic;
		diagnostic["diagnostic_id"] = make_diagnostic_id(script_id, "gdscript", content_sha256, p_severity, p_code, start_byte, end_byte, safe_message);
		diagnostic["script_ref"] = script_ref;
		diagnostic["language"] = "gdscript";
		diagnostic["content_sha256"] = content_sha256;
		diagnostic["code"] = p_code;
		diagnostic["severity"] = p_severity;
		diagnostic["safe_message"] = safe_message;
		diagnostic["range"] = has_range ? Variant(range) : Variant();
		diagnostic["authority"] = p_authority;
		diagnostic["script_graph_revision"] = (int64_t)script_graph_revision;
		diagnostics.push_back(diagnostic);
		return true;
	}
};

static void data_type_projection(const GDScriptParser::DataType &p_data_type, Variant &r_type_name, String &r_type_state) {
	if (!p_data_type.is_set() || p_data_type.is_variant()) {
		r_type_name = Variant();
		r_type_state = "dynamic";
		return;
	}
	const String type_name = p_data_type.to_string();
	if (type_name.is_empty() || type_name == "Variant") {
		r_type_name = Variant();
		r_type_state = "dynamic";
		return;
	}
	r_type_name = type_name;
	switch (p_data_type.type_source) {
		case GDScriptParser::DataType::ANNOTATED_EXPLICIT:
			r_type_state = "explicit";
			break;
		case GDScriptParser::DataType::ANNOTATED_INFERRED:
		case GDScriptParser::DataType::INFERRED:
			r_type_state = "inferred";
			break;
		case GDScriptParser::DataType::UNDETECTED:
			r_type_state = "dynamic";
			break;
	}
}

static String function_signature(const GDScriptParser::FunctionNode *p_function, const String &p_name) {
	String signature = p_name + "(";
	int emitted_parameters = 0;
	for (int index = 0; index < p_function->parameters.size(); index++) {
		const GDScriptParser::ParameterNode *parameter = p_function->parameters[index];
		if (p_function->source_lambda && parameter && parameter->identifier &&
				(parameter->identifier->start_line > p_function->header_end_line ||
						(parameter->identifier->start_line == p_function->header_end_line && parameter->identifier->start_column >= p_function->header_end_column))) {
			continue;
		}
		if (emitted_parameters++ > 0) {
			signature += ", ";
		}
		signature += parameter->identifier ? String(parameter->identifier->name) : "_";
		if (parameter->type_constraint.is_hard_type()) {
			signature += ": " + parameter->type_constraint.to_string();
		}
	}
	if (p_function->is_vararg() && p_function->rest_parameter && p_function->rest_parameter->identifier) {
		if (emitted_parameters > 0) {
			signature += ", ";
		}
		signature += "..." + String(p_function->rest_parameter->identifier->name);
	}
	signature += ")";
	if (p_function->return_type_constraint.is_hard_type()) {
		signature += " -> " + p_function->return_type_constraint.to_string();
	}
	return signature;
}

static bool project_suite(ProjectionContext &r_context, const GDScriptParser::SuiteNode *p_suite, const String &p_owner_qualified_key, const String &p_owner_symbol_id);

static bool project_lambda(ProjectionContext &r_context, const GDScriptParser::LambdaNode *p_lambda, const String &p_owner_qualified_key, const String &p_owner_symbol_id) {
	if (!p_lambda || !p_lambda->function) {
		return false;
	}
	Dictionary range;
	const GDScriptParser::FunctionNode *function = p_lambda->function;
	if (!r_context.make_range(function->start_line, function->start_column, function->header_end_line, function->header_end_column, range)) {
		return false;
	}
	const uint64_t start_byte = (uint64_t)(int64_t)range["start_byte"];
	const String qualified_key = p_owner_qualified_key + "/lambda@" + String::num_uint64(start_byte);
	Array modifiers;
	modifiers.push_back("static");
	String lambda_id;
	// The function header is the declaration coordinate for an anonymous lambda;
	// the body remains evidence for its child locals, not for its identity span.
	GDScriptParser::Node range_node = *static_cast<const GDScriptParser::Node *>(function);
	range_node.end_line = function->header_end_line;
	range_node.end_column = function->header_end_column;
	Variant return_type_name;
	String return_type_state;
	data_type_projection(function->return_type_constraint, return_type_name, return_type_state);
	if (!r_context.append_symbol("lambda", Variant(), qualified_key, p_owner_symbol_id, false, p_owner_qualified_key, &range_node, function_signature(function, "<lambda>"), return_type_name, return_type_state, modifiers, false, lambda_id)) {
		return false;
	}
	r_context.bind_declaration(p_lambda, function->identifier, lambda_id);
	r_context.bind_declaration(function, function->identifier, lambda_id);
	for (const GDScriptParser::ParameterNode *parameter : function->parameters) {
		if (!parameter || !parameter->identifier) {
			return false;
		}
		if (parameter->identifier->start_line > function->header_end_line ||
				(parameter->identifier->start_line == function->header_end_line && parameter->identifier->start_column >= function->header_end_column)) {
			continue;
		}
		Variant type_name;
		String type_state;
		data_type_projection(parameter->type_constraint, type_name, type_state);
		const String name = parameter->identifier->name;
		const String key = qualified_key + "/parameter:" + name + "@" + itos(parameter->identifier->start_line) + ":" + itos(parameter->identifier->start_column);
		String parameter_id;
		if (!r_context.append_symbol("parameter", name, key, lambda_id, false, qualified_key, parameter->identifier, Variant(), type_name, type_state, Array(), false, parameter_id)) {
			return false;
		}
		r_context.bind_declaration(parameter, parameter->identifier, parameter_id);
	}
	return project_suite(r_context, function->body, qualified_key, lambda_id);
}

static bool project_local(ProjectionContext &r_context, const GDScriptParser::SuiteNode::Local &p_local, const String &p_owner_qualified_key, const String &p_owner_symbol_id) {
	const GDScriptParser::Node *range_node = nullptr;
	const GDScriptParser::VariableNode *variable = nullptr;
	Variant type_name;
	String type_state;
	Array modifiers;
	bool documentation_present = false;
	switch (p_local.type) {
		case GDScriptParser::SuiteNode::Local::PARAMETER:
			return true;
		case GDScriptParser::SuiteNode::Local::CONSTANT:
			range_node = p_local.constant ? p_local.constant->identifier : nullptr;
			if (p_local.constant) {
				data_type_projection(p_local.constant->type_constraint, type_name, type_state);
				modifiers.push_back("const");
#ifdef TOOLS_ENABLED
				documentation_present = !p_local.constant->doc_data.description.is_empty();
#endif
			}
			break;
		case GDScriptParser::SuiteNode::Local::VARIABLE:
			variable = p_local.variable;
			range_node = variable ? variable->identifier : nullptr;
			if (variable) {
				data_type_projection(variable->type_constraint, type_name, type_state);
#ifdef TOOLS_ENABLED
				documentation_present = !variable->doc_data.description.is_empty();
#endif
			}
			break;
		case GDScriptParser::SuiteNode::Local::FOR_VARIABLE:
		case GDScriptParser::SuiteNode::Local::PATTERN_BIND:
			range_node = p_local.bind;
			if (p_local.bind) {
				data_type_projection(p_local.bind->type_constraint, type_name, type_state);
			}
			break;
		case GDScriptParser::SuiteNode::Local::UNDEFINED:
			return false;
	}
	if (!range_node || p_local.name.is_empty()) {
		return false;
	}
	Dictionary range;
	if (!r_context.make_range(range_node, range)) {
		return false;
	}
	const String name = p_local.name;
	const String qualified_key = p_owner_qualified_key + "/local:" + name + "@" + String::num_int64(range["start_byte"]);
	String local_id;
	if (!r_context.append_symbol("local", name, qualified_key, p_owner_symbol_id, false, p_owner_qualified_key, range_node, Variant(), type_name, type_state, modifiers, documentation_present, local_id)) {
		return false;
	}
	const GDScriptParser::Node *declaration_node = range_node;
	if (p_local.type == GDScriptParser::SuiteNode::Local::CONSTANT) {
		declaration_node = p_local.constant;
	} else if (p_local.type == GDScriptParser::SuiteNode::Local::VARIABLE) {
		declaration_node = p_local.variable;
	}
	r_context.bind_declaration(declaration_node, dynamic_cast<const GDScriptParser::IdentifierNode *>(range_node), local_id);
	if (variable && variable->initializer && variable->initializer->type == GDScriptParser::Node::LAMBDA) {
		r_context.lambda_backed_symbols.insert(local_id);
		return project_lambda(r_context, static_cast<const GDScriptParser::LambdaNode *>(variable->initializer), p_owner_qualified_key, local_id);
	}
	return true;
}

static bool project_suite(ProjectionContext &r_context, const GDScriptParser::SuiteNode *p_suite, const String &p_owner_qualified_key, const String &p_owner_symbol_id) {
	if (!p_suite) {
		return true;
	}
	for (const GDScriptParser::SuiteNode::Local &local : p_suite->locals) {
		if (!project_local(r_context, local, p_owner_qualified_key, p_owner_symbol_id)) {
			return false;
		}
	}
	for (const GDScriptParser::Node *statement : p_suite->statements) {
		if (!statement) {
			continue;
		}
		switch (statement->type) {
			case GDScriptParser::Node::IF: {
				const GDScriptParser::IfNode *node = static_cast<const GDScriptParser::IfNode *>(statement);
				if (!project_suite(r_context, node->true_block, p_owner_qualified_key, p_owner_symbol_id) || !project_suite(r_context, node->false_block, p_owner_qualified_key, p_owner_symbol_id)) {
					return false;
				}
			} break;
			case GDScriptParser::Node::FOR:
				if (!project_suite(r_context, static_cast<const GDScriptParser::ForNode *>(statement)->loop, p_owner_qualified_key, p_owner_symbol_id)) {
					return false;
				}
				break;
			case GDScriptParser::Node::WHILE:
				if (!project_suite(r_context, static_cast<const GDScriptParser::WhileNode *>(statement)->loop, p_owner_qualified_key, p_owner_symbol_id)) {
					return false;
				}
				break;
			case GDScriptParser::Node::MATCH: {
				const GDScriptParser::MatchNode *node = static_cast<const GDScriptParser::MatchNode *>(statement);
				for (const GDScriptParser::MatchBranchNode *branch : node->branches) {
					if (branch && !project_suite(r_context, branch->block, p_owner_qualified_key, p_owner_symbol_id)) {
						return false;
					}
				}
			} break;
			default:
				break;
		}
	}
	return true;
}

static bool project_function(ProjectionContext &r_context, const GDScriptParser::FunctionNode *p_function, const String &p_class_key, const Variant &p_class_symbol_id, bool p_has_class_symbol) {
	if (!p_function || !p_function->identifier) {
		return false;
	}
	const String name = p_function->identifier->name;
	const String kind = p_function->is_static || !p_has_class_symbol ? "function" : "method";
	const String qualified_key = p_class_key + "/" + kind + ":" + name;
	Array modifiers;
	if (p_function->is_abstract) {
		modifiers.push_back("abstract");
	}
	if (p_function->is_static) {
		modifiers.push_back("static");
	}
	bool documentation_present = false;
#ifdef TOOLS_ENABLED
	documentation_present = !p_function->doc_data.description.is_empty();
#endif
	String function_id;
	Variant return_type_name;
	String return_type_state;
	data_type_projection(p_function->return_type_constraint, return_type_name, return_type_state);
	if (!r_context.append_symbol(kind, name, qualified_key, p_class_symbol_id, true, qualified_key, p_function->identifier, function_signature(p_function, name), return_type_name, return_type_state, modifiers, documentation_present, function_id)) {
		return false;
	}
	r_context.bind_declaration(p_function, p_function->identifier, function_id);
	for (const GDScriptParser::ParameterNode *parameter : p_function->parameters) {
		if (!parameter || !parameter->identifier) {
			return false;
		}
		Variant type_name;
		String type_state;
		data_type_projection(parameter->type_constraint, type_name, type_state);
		const String parameter_name = parameter->identifier->name;
		const String parameter_key = qualified_key + "/parameter:" + parameter_name + "@" + itos(parameter->identifier->start_line) + ":" + itos(parameter->identifier->start_column);
		String parameter_id;
		if (!r_context.append_symbol("parameter", parameter_name, parameter_key, function_id, false, qualified_key, parameter->identifier, Variant(), type_name, type_state, Array(), false, parameter_id)) {
			return false;
		}
		r_context.bind_declaration(parameter, parameter->identifier, parameter_id);
	}
	return project_suite(r_context, p_function->body, qualified_key, function_id);
}

static bool project_class(ProjectionContext &r_context, const GDScriptParser::ClassNode *p_class, const String &p_parent_key, const Variant &p_parent_symbol_id, bool p_root) {
	if (!p_class) {
		return false;
	}
	const bool has_symbol = p_class->identifier != nullptr;
	String class_key = p_root ? "script" : p_parent_key;
	Variant class_symbol_id = p_parent_symbol_id;
	if (has_symbol) {
		const String name = p_class->identifier->name;
		class_key = p_root ? "class:" + name : p_parent_key + "/class:" + name;
		Array modifiers;
		if (p_class->is_abstract) {
			modifiers.push_back("abstract");
		}
		bool documentation_present = false;
#ifdef TOOLS_ENABLED
		documentation_present = !p_class->doc_data.brief.is_empty() || !p_class->doc_data.description.is_empty();
#endif
		String class_id;
		if (!r_context.append_symbol("class", name, class_key, p_parent_symbol_id, true, class_key, p_class->identifier, Variant(), name, "explicit", modifiers, documentation_present, class_id)) {
			return false;
		}
		r_context.bind_declaration(p_class, p_class->identifier, class_id);
		class_symbol_id = class_id;
	}

	for (const GDScriptParser::ClassNode::Member &member : p_class->members) {
		switch (member.type) {
			case GDScriptParser::ClassNode::Member::VARIABLE: {
				const GDScriptParser::VariableNode *variable = member.variable;
				if (!variable || !variable->identifier) {
					return false;
				}
				const String name = variable->identifier->name;
				const String key = class_key + "/property:" + name;
				Variant type_name;
				String type_state;
				data_type_projection(variable->type_constraint, type_name, type_state);
				Array modifiers;
				if (variable->exported) {
					modifiers.push_back("exported");
				}
				if (variable->is_static) {
					modifiers.push_back("static");
				}
				bool documentation_present = false;
#ifdef TOOLS_ENABLED
				documentation_present = !variable->doc_data.description.is_empty();
#endif
				String symbol_id;
				if (!r_context.append_symbol("property", name, key, class_symbol_id, true, key, variable->identifier, Variant(), type_name, type_state, modifiers, documentation_present, symbol_id)) {
					return false;
				}
				r_context.bind_declaration(variable, variable->identifier, symbol_id);
			} break;
			case GDScriptParser::ClassNode::Member::CONSTANT: {
				const GDScriptParser::ConstantNode *constant = member.constant;
				if (!constant || !constant->identifier) {
					return false;
				}
				const String name = constant->identifier->name;
				const String key = class_key + "/constant:" + name;
				Variant type_name;
				String type_state;
				data_type_projection(constant->type_constraint, type_name, type_state);
				Array modifiers;
				modifiers.push_back("const");
				bool documentation_present = false;
#ifdef TOOLS_ENABLED
				documentation_present = !constant->doc_data.description.is_empty();
#endif
				String symbol_id;
				if (!r_context.append_symbol("constant", name, key, class_symbol_id, true, key, constant->identifier, Variant(), type_name, type_state, modifiers, documentation_present, symbol_id)) {
					return false;
				}
				r_context.bind_declaration(constant, constant->identifier, symbol_id);
			} break;
			case GDScriptParser::ClassNode::Member::SIGNAL: {
				const GDScriptParser::SignalNode *signal = member.signal;
				if (!signal || !signal->identifier) {
					return false;
				}
				const String name = signal->identifier->name;
				String signature = name + "(";
				for (int index = 0; index < signal->parameters.size(); index++) {
					if (index > 0) {
						signature += ", ";
					}
					const GDScriptParser::ParameterNode *parameter = signal->parameters[index];
					signature += parameter && parameter->identifier ? String(parameter->identifier->name) : "_";
				}
				signature += ")";
				const String key = class_key + "/signal:" + name;
				bool documentation_present = false;
#ifdef TOOLS_ENABLED
				documentation_present = !signal->doc_data.description.is_empty();
#endif
				String symbol_id;
				if (!r_context.append_symbol("signal", name, key, class_symbol_id, true, key, signal->identifier, signature, "Signal", "inferred", Array(), documentation_present, symbol_id)) {
					return false;
				}
				r_context.bind_declaration(signal, signal->identifier, symbol_id);
			} break;
			case GDScriptParser::ClassNode::Member::ENUM: {
				const GDScriptParser::EnumNode *enumeration = member.m_enum;
				if (!enumeration || !enumeration->identifier) {
					return false;
				}
				const String name = enumeration->identifier->name;
				const String enum_key = class_key + "/enum:" + name;
				String enum_id;
				if (!r_context.append_symbol("enum", name, enum_key, class_symbol_id, true, enum_key, enumeration->identifier, Variant(), name, "explicit", Array(), false, enum_id)) {
					return false;
				}
				r_context.bind_declaration(enumeration, enumeration->identifier, enum_id);
				for (const GDScriptParser::EnumNode::Value &value : enumeration->values) {
					if (!value.identifier) {
						return false;
					}
					const String value_name = value.identifier->name;
					const String value_key = enum_key + "/member:" + value_name;
					String value_id;
					if (!r_context.append_symbol("enum_member", value_name, value_key, enum_id, true, value_key, value.identifier, Variant(), name, "inferred", Array(), false, value_id)) {
						return false;
					}
					r_context.bind_declaration(value.identifier, value.identifier, value_id);
				}
			} break;
			case GDScriptParser::ClassNode::Member::ENUM_VALUE: {
				const GDScriptParser::EnumNode::Value &value = member.enum_value;
				if (!value.identifier) {
					return false;
				}
				const String name = value.identifier->name;
				const String key = class_key + "/enum_member:" + name;
				String symbol_id;
				if (!r_context.append_symbol("enum_member", name, key, class_symbol_id, true, key, value.identifier, Variant(), Variant(), "inferred", Array(), false, symbol_id)) {
					return false;
				}
				r_context.bind_declaration(value.identifier, value.identifier, symbol_id);
			} break;
			case GDScriptParser::ClassNode::Member::FUNCTION:
				if (!project_function(r_context, member.function, class_key, class_symbol_id, has_symbol)) {
					return false;
				}
				break;
			case GDScriptParser::ClassNode::Member::CLASS:
				if (!project_class(r_context, member.m_class, class_key, class_symbol_id, false)) {
					return false;
				}
				break;
			case GDScriptParser::ClassNode::Member::GROUP:
				break;
			case GDScriptParser::ClassNode::Member::UNDEFINED:
				return false;
		}
	}
	return true;
}

static bool is_safe_resource_path(const String &p_path) {
	if (!p_path.begins_with("res://") || p_path.length() <= 6 || !utf8_within(p_path, ScriptSemanticAdapter::MAX_PATH_BYTES) || p_path.contains("\\") || p_path.contains("?") || p_path.contains("#")) {
		return false;
	}
	const PackedStringArray segments = p_path.trim_prefix("res://").split("/", true);
	for (const String &segment : segments) {
		if (segment.is_empty() || segment == "." || segment == "..") {
			return false;
		}
		for (int index = 0; index < segment.length(); index++) {
			if (segment[index] < 0x20 || segment[index] == 0x7f || segment[index] == ':') {
				return false;
			}
		}
	}
	return true;
}

static bool is_safe_resource_uid(const String &p_uid) {
	if (!p_uid.begins_with("uid://") || p_uid.length() <= 6 || !utf8_within(p_uid, 128)) {
		return false;
	}
	for (int index = 6; index < p_uid.length(); index++) {
		const char32_t character = p_uid[index];
		if (!((character >= 'a' && character <= 'z') || (character >= 'A' && character <= 'Z') || (character >= '0' && character <= '9') || character == '_' || character == '-')) {
			return false;
		}
	}
	return true;
}

static bool make_resolved_resource_ref(const String &p_reference, Dictionary &r_resource_ref, String &r_resolved_path) {
	r_resource_ref.clear();
	r_resolved_path = String();
	if (is_safe_resource_uid(p_reference)) {
		ResourceUID *uid_registry = ResourceUID::get_singleton();
		if (uid_registry) {
			const ResourceUID::ID id = uid_registry->text_to_id(p_reference);
			if (id != ResourceUID::INVALID_ID && uid_registry->has_id(id)) {
				r_resolved_path = uid_registry->get_id_path(id);
			}
		}
		r_resource_ref["uid"] = p_reference;
		return true;
	}
	if (!is_safe_resource_path(p_reference) || !ResourceLoader::exists(p_reference)) {
		return false;
	}
	r_resolved_path = p_reference;
	String uid = ResourceUID::path_to_uid(p_reference);
	if (!is_safe_resource_uid(uid)) {
		Error uid_error = OK;
		uid = FileAccess::get_file_as_string(p_reference + ".uid", &uid_error).strip_edges();
		if (uid_error != OK) {
			uid = String();
		}
	}
	if (is_safe_resource_uid(uid)) {
		r_resource_ref["uid"] = uid;
	} else {
		r_resource_ref["uid_missing"] = true;
		r_resource_ref["path"] = p_reference;
	}
	return true;
}

static Dictionary symbol_endpoint(const String &p_symbol_id) {
	Dictionary endpoint;
	endpoint["kind"] = "symbol";
	endpoint["symbol_id"] = p_symbol_id;
	return endpoint;
}

static Dictionary resource_endpoint(const Dictionary &p_resource_ref) {
	Dictionary endpoint;
	endpoint["kind"] = "resource";
	endpoint["resource_ref"] = p_resource_ref;
	return endpoint;
}

static bool text_evidence_node(const ProjectionContext &p_context, int p_line, int p_start_column, int p_end_column, const String &p_text, GDScriptParser::Node &r_node) {
	if (p_line < 1 || p_line > p_context.lines.size() || p_text.is_empty()) {
		return false;
	}
	const String &line = p_context.lines[p_line - 1];
	const int start_index = MAX(0, p_start_column - 1);
	const int end_index = MIN(line.length(), MAX(start_index, p_end_column - 1));
	const int match = line.find(p_text, start_index);
	if (match < start_index || match + p_text.length() > end_index) {
		return false;
	}
	r_node.start_line = p_line;
	r_node.start_column = match + 1;
	r_node.end_line = p_line;
	r_node.end_column = match + p_text.length() + 1;
	return true;
}

static const GDScriptParser::Node *identifier_declaration(const GDScriptParser::IdentifierNode *p_identifier) {
	if (!p_identifier) {
		return nullptr;
	}
	switch (p_identifier->source) {
		case GDScriptParser::IdentifierNode::FUNCTION_PARAMETER:
			return p_identifier->parameter_source;
		case GDScriptParser::IdentifierNode::LOCAL_VARIABLE:
		case GDScriptParser::IdentifierNode::MEMBER_VARIABLE:
		case GDScriptParser::IdentifierNode::INHERITED_VARIABLE:
		case GDScriptParser::IdentifierNode::STATIC_VARIABLE:
			return p_identifier->variable_source;
		case GDScriptParser::IdentifierNode::LOCAL_CONSTANT:
		case GDScriptParser::IdentifierNode::MEMBER_CONSTANT:
			return p_identifier->constant_source;
		case GDScriptParser::IdentifierNode::LOCAL_ITERATOR:
		case GDScriptParser::IdentifierNode::LOCAL_BIND:
			return p_identifier->bind_source;
		case GDScriptParser::IdentifierNode::MEMBER_SIGNAL:
			return p_identifier->signal_source;
		case GDScriptParser::IdentifierNode::MEMBER_FUNCTION:
			return p_identifier->function_source;
		case GDScriptParser::IdentifierNode::MEMBER_CLASS:
		case GDScriptParser::IdentifierNode::NATIVE_CLASS:
		case GDScriptParser::IdentifierNode::UNDEFINED_SOURCE:
			return nullptr;
	}
	return nullptr;
}

static String reference_detail(const GDScriptParser::IdentifierNode *p_identifier) {
	if (!p_identifier) {
		return String();
	}
	switch (p_identifier->source) {
		case GDScriptParser::IdentifierNode::MEMBER_VARIABLE:
		case GDScriptParser::IdentifierNode::INHERITED_VARIABLE:
		case GDScriptParser::IdentifierNode::STATIC_VARIABLE:
			return "property_reference";
		case GDScriptParser::IdentifierNode::MEMBER_SIGNAL:
			return "signal_reference";
		case GDScriptParser::IdentifierNode::MEMBER_CONSTANT:
			return "constant_reference";
		case GDScriptParser::IdentifierNode::FUNCTION_PARAMETER:
			return "local_parameter_reference";
		case GDScriptParser::IdentifierNode::LOCAL_VARIABLE:
		case GDScriptParser::IdentifierNode::LOCAL_CONSTANT:
		case GDScriptParser::IdentifierNode::LOCAL_ITERATOR:
		case GDScriptParser::IdentifierNode::LOCAL_BIND:
			return "local_reference";
		default:
			return String();
	}
}

static Variant external_script_endpoint(const Ref<Script> &p_script, const String &p_kind, const String &p_member_name) {
	if (p_script.is_null()) {
		return Variant();
	}
	Dictionary resource_ref;
	String resolved_path;
	if (!make_resolved_resource_ref(p_script->get_path(), resource_ref, resolved_path)) {
		return Variant();
	}
	const String global_name = p_script->get_global_name();
	if (resource_ref.has("uid") && !global_name.is_empty()) {
		const String script_id = make_resource_id(resource_ref, resolved_path, String());
		String qualified_key = "class:" + global_name;
		if (!p_member_name.is_empty()) {
			qualified_key += "/" + p_kind + ":" + p_member_name;
		}
		const String symbol_id = make_named_symbol_id(script_id, "gdscript", p_member_name.is_empty() ? "class" : p_kind, qualified_key);
		if (!symbol_id.is_empty()) {
			return symbol_endpoint(symbol_id);
		}
	}
	return resource_endpoint(resource_ref);
}

static Variant external_class_node_endpoint(const ProjectionContext &p_context, const GDScriptParser::ClassNode *p_class, const String &p_member_kind = String(), const String &p_member_name = String()) {
	if (!p_class) {
		return Variant();
	}
	const String existing_id = p_context.find_declaration(p_member_name.is_empty() ? static_cast<const GDScriptParser::Node *>(p_class) : static_cast<const GDScriptParser::Node *>(p_class->has_function(p_member_name) ? p_class->get_member(p_member_name).function : nullptr));
	if (!existing_id.is_empty()) {
		return symbol_endpoint(existing_id);
	}
	String path = p_class->self_type.script_path;
	if (path.is_empty()) {
		path = p_class->base_type.script_path;
	}
	Dictionary resource_ref;
	String resolved_path;
	if (!make_resolved_resource_ref(path, resource_ref, resolved_path)) {
		return Variant();
	}
	const String class_name = p_class->identifier ? String(p_class->identifier->name) : String();
	if (resource_ref.has("uid") && !class_name.is_empty()) {
		const String script_id = make_resource_id(resource_ref, resolved_path, String());
		String qualified_key = "class:" + class_name;
		if (!p_member_name.is_empty()) {
			qualified_key += "/" + p_member_kind + ":" + p_member_name;
		}
		return symbol_endpoint(make_named_symbol_id(script_id, "gdscript", p_member_name.is_empty() ? "class" : p_member_kind, qualified_key));
	}
	return resource_endpoint(resource_ref);
}

static Variant base_class_endpoint(const ProjectionContext &p_context, const GDScriptParser::ClassNode *p_class) {
	if (!p_class) {
		return Variant();
	}
	if (p_class->base_type.kind == GDScriptParser::DataType::CLASS) {
		return external_class_node_endpoint(p_context, p_class->base_type.class_type);
	}
	if (p_class->base_type.kind == GDScriptParser::DataType::SCRIPT) {
		return external_script_endpoint(p_class->base_type.script_type, "class", String());
	}
	return Variant();
}

static Variant base_method_endpoint(const ProjectionContext &p_context, const GDScriptParser::ClassNode *p_class, const StringName &p_method) {
	const GDScriptParser::ClassNode *base_class = p_class && p_class->base_type.kind == GDScriptParser::DataType::CLASS ? p_class->base_type.class_type : nullptr;
	while (base_class) {
		if (base_class->has_function(p_method)) {
			const GDScriptParser::ClassNode::Member member = base_class->get_member(p_method);
			const String target_id = p_context.find_declaration(member.function);
			if (!target_id.is_empty()) {
				return symbol_endpoint(target_id);
			}
			return external_class_node_endpoint(p_context, base_class, member.function->is_static ? "function" : "method", p_method);
		}
		base_class = base_class->base_type.kind == GDScriptParser::DataType::CLASS ? base_class->base_type.class_type : nullptr;
	}
	if (p_class && p_class->base_type.kind == GDScriptParser::DataType::SCRIPT && p_class->base_type.script_type.is_valid() && p_class->base_type.script_type->has_method(p_method)) {
		return external_script_endpoint(p_class->base_type.script_type, "method", p_method);
	}
	return Variant();
}

static bool project_node_relations(ProjectionContext &r_context, const GDScriptParser::Node *p_node, const String &p_source_symbol_id, const GDScriptParser::ClassNode *p_owner_class);

static bool project_identifier_relation(ProjectionContext &r_context, const GDScriptParser::IdentifierNode *p_identifier, const String &p_source_symbol_id) {
	const String detail = reference_detail(p_identifier);
	if (detail.is_empty()) {
		return true;
	}
	const String target_id = r_context.find_declaration(identifier_declaration(p_identifier));
	if (target_id.is_empty() || target_id == p_source_symbol_id) {
		return true;
	}
	return r_context.append_relation(p_source_symbol_id, "references_symbol", symbol_endpoint(target_id), "exact", p_identifier, detail);
}

static bool project_call_relation(ProjectionContext &r_context, const GDScriptParser::CallNode *p_call, const String &p_source_symbol_id, const GDScriptParser::ClassNode *p_owner_class) {
	if (!p_call || p_source_symbol_id.is_empty()) {
		return true;
	}
	const StringName function_name = p_call->function_name;
	if (function_name == SNAME("load") && !p_call->arguments.is_empty()) {
		const GDScriptParser::ExpressionNode *argument = p_call->arguments[0];
		if (argument && argument->type == GDScriptParser::Node::LITERAL && static_cast<const GDScriptParser::LiteralNode *>(argument)->value.get_type() == Variant::STRING) {
			const String reference = static_cast<const GDScriptParser::LiteralNode *>(argument)->value;
			Dictionary resource_ref;
			String resolved_path;
			if (make_resolved_resource_ref(reference, resource_ref, resolved_path)) {
				GDScriptParser::Node evidence;
				if (text_evidence_node(r_context, argument->start_line, argument->start_column, argument->end_column, reference, evidence)) {
					return r_context.append_relation(p_source_symbol_id, "loads", resource_endpoint(resource_ref), "exact", &evidence, reference.begins_with("uid://") ? "literal_uid" : "literal_path");
				}
			}
			return true;
		}
		return r_context.append_relation(p_source_symbol_id, "loads", Variant(), "dynamic", p_call, "variable_resource_path");
	}
	if (function_name == SNAME("get_node") && !p_call->arguments.is_empty()) {
		const GDScriptParser::ExpressionNode *argument = p_call->arguments[0];
		const String detail = argument && argument->type == GDScriptParser::Node::LITERAL ? "string_node_path" : "node_path_parameter";
		return r_context.append_relation(p_source_symbol_id, "references_symbol", Variant(), "dynamic", p_call, detail);
	}

	const GDScriptParser::SubscriptNode *subscript = p_call->callee && p_call->callee->type == GDScriptParser::Node::SUBSCRIPT ? static_cast<const GDScriptParser::SubscriptNode *>(p_call->callee) : nullptr;
	if (function_name == SNAME("call") && subscript) {
		String detail = "variant_method_name";
		if (subscript->base && subscript->base->type == GDScriptParser::Node::IDENTIFIER) {
			const String receiver_id = r_context.find_declaration(identifier_declaration(static_cast<const GDScriptParser::IdentifierNode *>(subscript->base)));
			if (!receiver_id.is_empty() && r_context.lambda_backed_symbols.has(receiver_id)) {
				detail = "callable_local";
			}
		}
		return r_context.append_relation(p_source_symbol_id, "calls", Variant(), "dynamic", p_call->callee, detail);
	}

	const GDScriptParser::IdentifierNode *callee_identifier = nullptr;
	if (p_call->callee && p_call->callee->type == GDScriptParser::Node::IDENTIFIER) {
		callee_identifier = static_cast<const GDScriptParser::IdentifierNode *>(p_call->callee);
	} else if (subscript && subscript->is_attribute) {
		callee_identifier = subscript->attribute;
	}
	String target_id = r_context.find_declaration(identifier_declaration(callee_identifier));
	Variant target;
	String detail = "direct_call";
	if (p_call->is_super) {
		target = base_method_endpoint(r_context, p_owner_class, function_name);
		detail = "super_call";
	} else if (!target_id.is_empty()) {
		target = symbol_endpoint(target_id);
	} else if (p_owner_class && p_owner_class->has_function(function_name)) {
		target_id = r_context.find_declaration(p_owner_class->get_member(function_name).function);
		if (!target_id.is_empty()) {
			target = symbol_endpoint(target_id);
		}
	}
	if (target.get_type() == Variant::DICTIONARY) {
		GDScriptParser::Node evidence;
		const GDScriptParser::Node *evidence_node = p_call->is_super ? nullptr : p_call->callee;
		const String evidence_text = p_call->is_super ? "super." + String(function_name) : String(function_name);
		if (text_evidence_node(r_context, p_call->start_line, p_call->start_column, p_call->end_column, evidence_text, evidence)) {
			evidence_node = &evidence;
		}
		return evidence_node ? r_context.append_relation(p_source_symbol_id, "calls", target, "exact", evidence_node, detail) : true;
	}
	return true;
}

static bool project_expression_relations(ProjectionContext &r_context, const GDScriptParser::ExpressionNode *p_expression, const String &p_source_symbol_id, const GDScriptParser::ClassNode *p_owner_class) {
	if (!p_expression) {
		return true;
	}
	switch (p_expression->type) {
		case GDScriptParser::Node::IDENTIFIER:
			return project_identifier_relation(r_context, static_cast<const GDScriptParser::IdentifierNode *>(p_expression), p_source_symbol_id);
		case GDScriptParser::Node::CALL: {
			const GDScriptParser::CallNode *call = static_cast<const GDScriptParser::CallNode *>(p_expression);
			if (!project_call_relation(r_context, call, p_source_symbol_id, p_owner_class) || !project_expression_relations(r_context, call->callee, p_source_symbol_id, p_owner_class)) {
				return false;
			}
			for (const GDScriptParser::ExpressionNode *argument : call->arguments) {
				if (!project_expression_relations(r_context, argument, p_source_symbol_id, p_owner_class)) {
					return false;
				}
			}
			return true;
		}
		case GDScriptParser::Node::PRELOAD: {
			const GDScriptParser::PreloadNode *preload = static_cast<const GDScriptParser::PreloadNode *>(p_expression);
			if (preload->path && preload->path->type == GDScriptParser::Node::LITERAL && static_cast<const GDScriptParser::LiteralNode *>(preload->path)->value.get_type() == Variant::STRING) {
				const String reference = static_cast<const GDScriptParser::LiteralNode *>(preload->path)->value;
				Dictionary resource_ref;
				String resolved_path;
				if (make_resolved_resource_ref(reference, resource_ref, resolved_path)) {
					GDScriptParser::Node evidence;
					if (text_evidence_node(r_context, preload->path->start_line, preload->path->start_column, preload->path->end_column, reference, evidence)) {
						return r_context.append_relation(p_source_symbol_id, "preloads", resource_endpoint(resource_ref), "exact", &evidence, reference.begins_with("uid://") ? "literal_uid" : "literal_path");
					}
				}
			}
			return true;
		}
		case GDScriptParser::Node::GET_NODE: {
			const GDScriptParser::GetNodeNode *get_node = static_cast<const GDScriptParser::GetNodeNode *>(p_expression);
			return r_context.append_relation(p_source_symbol_id, "references_symbol", Variant(), "dynamic", get_node, get_node->use_dollar ? "shorthand_node_path" : "unique_node_path");
		}
		case GDScriptParser::Node::ASSIGNMENT: {
			const GDScriptParser::AssignmentNode *assignment = static_cast<const GDScriptParser::AssignmentNode *>(p_expression);
			return project_expression_relations(r_context, assignment->assignee, p_source_symbol_id, p_owner_class) && project_expression_relations(r_context, assignment->assigned_value, p_source_symbol_id, p_owner_class);
		}
		case GDScriptParser::Node::BINARY_OPERATOR: {
			const GDScriptParser::BinaryOpNode *binary = static_cast<const GDScriptParser::BinaryOpNode *>(p_expression);
			return project_expression_relations(r_context, binary->left_operand, p_source_symbol_id, p_owner_class) && project_expression_relations(r_context, binary->right_operand, p_source_symbol_id, p_owner_class);
		}
		case GDScriptParser::Node::UNARY_OPERATOR:
			return project_expression_relations(r_context, static_cast<const GDScriptParser::UnaryOpNode *>(p_expression)->operand, p_source_symbol_id, p_owner_class);
		case GDScriptParser::Node::TERNARY_OPERATOR: {
			const GDScriptParser::TernaryOpNode *ternary = static_cast<const GDScriptParser::TernaryOpNode *>(p_expression);
			return project_expression_relations(r_context, ternary->condition, p_source_symbol_id, p_owner_class) && project_expression_relations(r_context, ternary->true_expr, p_source_symbol_id, p_owner_class) && project_expression_relations(r_context, ternary->false_expr, p_source_symbol_id, p_owner_class);
		}
		case GDScriptParser::Node::SUBSCRIPT: {
			const GDScriptParser::SubscriptNode *subscript = static_cast<const GDScriptParser::SubscriptNode *>(p_expression);
			return project_expression_relations(r_context, subscript->base, p_source_symbol_id, p_owner_class) && (subscript->is_attribute || project_expression_relations(r_context, subscript->index, p_source_symbol_id, p_owner_class));
		}
		case GDScriptParser::Node::ARRAY: {
			for (const GDScriptParser::ExpressionNode *element : static_cast<const GDScriptParser::ArrayNode *>(p_expression)->elements) {
				if (!project_expression_relations(r_context, element, p_source_symbol_id, p_owner_class)) {
					return false;
				}
			}
			return true;
		}
		case GDScriptParser::Node::DICTIONARY: {
			for (const GDScriptParser::DictionaryNode::Pair &pair : static_cast<const GDScriptParser::DictionaryNode *>(p_expression)->elements) {
				if (!project_expression_relations(r_context, pair.key, p_source_symbol_id, p_owner_class) || !project_expression_relations(r_context, pair.value, p_source_symbol_id, p_owner_class)) {
					return false;
				}
			}
			return true;
		}
		case GDScriptParser::Node::AWAIT:
			return project_expression_relations(r_context, static_cast<const GDScriptParser::AwaitNode *>(p_expression)->to_await, p_source_symbol_id, p_owner_class);
		case GDScriptParser::Node::CAST:
			return project_expression_relations(r_context, static_cast<const GDScriptParser::CastNode *>(p_expression)->operand, p_source_symbol_id, p_owner_class);
		case GDScriptParser::Node::TYPE_TEST:
			return project_expression_relations(r_context, static_cast<const GDScriptParser::TypeTestNode *>(p_expression)->operand, p_source_symbol_id, p_owner_class);
		case GDScriptParser::Node::LAMBDA: {
			const GDScriptParser::LambdaNode *lambda = static_cast<const GDScriptParser::LambdaNode *>(p_expression);
			const String lambda_id = r_context.find_declaration(lambda);
			return lambda->function && project_node_relations(r_context, lambda->function->body, lambda_id.is_empty() ? p_source_symbol_id : lambda_id, p_owner_class);
		}
		default:
			return true;
	}
}

static bool project_node_relations(ProjectionContext &r_context, const GDScriptParser::Node *p_node, const String &p_source_symbol_id, const GDScriptParser::ClassNode *p_owner_class) {
	if (!p_node) {
		return true;
	}
	if (p_node->is_expression()) {
		return project_expression_relations(r_context, static_cast<const GDScriptParser::ExpressionNode *>(p_node), p_source_symbol_id, p_owner_class);
	}
	switch (p_node->type) {
		case GDScriptParser::Node::SUITE:
			for (const GDScriptParser::Node *statement : static_cast<const GDScriptParser::SuiteNode *>(p_node)->statements) {
				if (!project_node_relations(r_context, statement, p_source_symbol_id, p_owner_class)) {
					return false;
				}
			}
			return true;
		case GDScriptParser::Node::VARIABLE:
			return project_expression_relations(r_context, static_cast<const GDScriptParser::VariableNode *>(p_node)->initializer, p_source_symbol_id, p_owner_class);
		case GDScriptParser::Node::CONSTANT:
			return project_expression_relations(r_context, static_cast<const GDScriptParser::ConstantNode *>(p_node)->initializer, p_source_symbol_id, p_owner_class);
		case GDScriptParser::Node::RETURN:
			return project_expression_relations(r_context, static_cast<const GDScriptParser::ReturnNode *>(p_node)->return_value, p_source_symbol_id, p_owner_class);
		case GDScriptParser::Node::ASSERT: {
			const GDScriptParser::AssertNode *assertion = static_cast<const GDScriptParser::AssertNode *>(p_node);
			return project_expression_relations(r_context, assertion->condition, p_source_symbol_id, p_owner_class) && project_expression_relations(r_context, assertion->message, p_source_symbol_id, p_owner_class);
		}
		case GDScriptParser::Node::IF: {
			const GDScriptParser::IfNode *condition = static_cast<const GDScriptParser::IfNode *>(p_node);
			return project_expression_relations(r_context, condition->condition, p_source_symbol_id, p_owner_class) && project_node_relations(r_context, condition->true_block, p_source_symbol_id, p_owner_class) && project_node_relations(r_context, condition->false_block, p_source_symbol_id, p_owner_class);
		}
		case GDScriptParser::Node::FOR: {
			const GDScriptParser::ForNode *loop = static_cast<const GDScriptParser::ForNode *>(p_node);
			return project_expression_relations(r_context, loop->list, p_source_symbol_id, p_owner_class) && project_node_relations(r_context, loop->loop, p_source_symbol_id, p_owner_class);
		}
		case GDScriptParser::Node::WHILE: {
			const GDScriptParser::WhileNode *loop = static_cast<const GDScriptParser::WhileNode *>(p_node);
			return project_expression_relations(r_context, loop->condition, p_source_symbol_id, p_owner_class) && project_node_relations(r_context, loop->loop, p_source_symbol_id, p_owner_class);
		}
		case GDScriptParser::Node::MATCH: {
			const GDScriptParser::MatchNode *match = static_cast<const GDScriptParser::MatchNode *>(p_node);
			if (!project_expression_relations(r_context, match->test, p_source_symbol_id, p_owner_class)) {
				return false;
			}
			for (const GDScriptParser::MatchBranchNode *branch : match->branches) {
				if (branch && !project_node_relations(r_context, branch->block, p_source_symbol_id, p_owner_class)) {
					return false;
				}
			}
			return true;
		}
		default:
			return true;
	}
}

static bool project_class_relations(ProjectionContext &r_context, const GDScriptParser::ClassNode *p_class) {
	if (!p_class) {
		return false;
	}
	const String class_id = r_context.find_declaration(p_class);
	const Variant base_endpoint = base_class_endpoint(r_context, p_class);
	if (!class_id.is_empty() && base_endpoint.get_type() == Variant::DICTIONARY) {
		GDScriptParser::Node evidence;
		const GDScriptParser::Node *evidence_node = p_class;
		if (!p_class->extends_path.is_empty() && text_evidence_node(r_context, p_class->extends_start_line, p_class->extends_start_column, p_class->extends_end_column, p_class->extends_path, evidence)) {
			evidence_node = &evidence;
		}
		if (!r_context.append_relation(class_id, "inherits", base_endpoint, "exact", evidence_node, "script_inheritance")) {
			return false;
		}
	}

	for (const GDScriptParser::ClassNode::Member &member : p_class->members) {
		switch (member.type) {
			case GDScriptParser::ClassNode::Member::VARIABLE: {
				const String source_id = r_context.find_declaration(member.variable);
				if (!project_expression_relations(r_context, member.variable->initializer, source_id, p_class)) {
					return false;
				}
			} break;
			case GDScriptParser::ClassNode::Member::CONSTANT: {
				const String source_id = r_context.find_declaration(member.constant);
				if (!project_expression_relations(r_context, member.constant->initializer, source_id, p_class)) {
					return false;
				}
			} break;
			case GDScriptParser::ClassNode::Member::FUNCTION: {
				const String source_id = r_context.find_declaration(member.function);
				const Variant overridden = base_method_endpoint(r_context, p_class, member.function->identifier->name);
				if (overridden.get_type() == Variant::DICTIONARY && !r_context.append_relation(source_id, "overrides", overridden, "exact", member.function->identifier, "method_override")) {
					return false;
				}
				if (!project_node_relations(r_context, member.function->body, source_id, p_class)) {
					return false;
				}
			} break;
			case GDScriptParser::ClassNode::Member::CLASS:
				if (!project_class_relations(r_context, member.m_class)) {
					return false;
				}
				break;
			default:
				break;
		}
	}
	return true;
}

static String analyzer_diagnostic_code(const GDScriptParser::ParserError &p_error, const GDScriptParser::ClassNode *p_root) {
	const String normalized = p_error.message.to_lower();
	if (normalized.contains("cyclic") || normalized.contains("cycle")) {
		return "GDSCRIPT_CYCLIC_DEPENDENCY";
	}
	if (p_root && !p_root->extends_path.is_empty() &&
			(normalized.contains("not found") || normalized.contains("does not exist") || normalized.contains("could not find") || normalized.contains("cannot find") || normalized.contains("couldn't load") || normalized.contains("could not load") || normalized.contains("could not resolve"))) {
		return "GDSCRIPT_MISSING_DEPENDENCY";
	}
	return "GDSCRIPT_ANALYZER_ERROR";
}

#endif // MODULE_GDSCRIPT_ENABLED

} // namespace

bool ScriptSemanticAdapter::_is_valid_script_path(const String &p_path) {
	if (!p_path.begins_with("res://") || p_path.length() <= 6 || !utf8_within(p_path, MAX_PATH_BYTES) || (!p_path.ends_with(".gd") && !p_path.ends_with(".cs")) || p_path.contains("\\") || p_path.contains("?") || p_path.contains("#")) {
		return false;
	}
	const PackedStringArray segments = p_path.trim_prefix("res://").split("/", true);
	for (const String &segment : segments) {
		if (segment.is_empty() || segment == "." || segment == "..") {
			return false;
		}
		for (int index = 0; index < segment.length(); index++) {
			if (segment[index] < 0x20 || segment[index] == 0x7f || segment[index] == ':') {
				return false;
			}
		}
	}
	return true;
}

bool ScriptSemanticAdapter::_is_valid_script_ref(const Dictionary &p_script_ref, const String &p_path) {
	if (p_script_ref.size() == 1 && p_script_ref.has("uid") && p_script_ref["uid"].get_type() == Variant::STRING) {
		const String uid = p_script_ref["uid"];
		if (!uid.begins_with("uid://") || uid.length() <= 6 || !utf8_within(uid, 128)) {
			return false;
		}
		for (int index = 6; index < uid.length(); index++) {
			const char32_t character = uid[index];
			if (!((character >= 'a' && character <= 'z') || (character >= 'A' && character <= 'Z') || (character >= '0' && character <= '9') || character == '_' || character == '-')) {
				return false;
			}
		}
		return true;
	}
	return p_script_ref.size() == 2 && p_script_ref.has("uid_missing") && p_script_ref["uid_missing"].get_type() == Variant::BOOL && bool(p_script_ref["uid_missing"]) &&
			p_script_ref.has("path") && p_script_ref["path"].get_type() == Variant::STRING && p_script_ref["path"] == p_path;
}

String ScriptSemanticAdapter::_resource_id(const Dictionary &p_script_ref, const String &p_path, const String &p_content_sha256) {
	return make_resource_id(p_script_ref, p_path, p_content_sha256);
}

String ScriptSemanticAdapter::_named_symbol_id(const String &p_script_id, const String &p_language, const String &p_kind, const String &p_qualified_key) {
	return make_named_symbol_id(p_script_id, p_language, p_kind, p_qualified_key);
}

String ScriptSemanticAdapter::_content_symbol_id(const String &p_script_id, const String &p_language, const String &p_content_sha256, const String &p_owner_qualified_key, const String &p_kind, uint64_t p_start_byte, uint64_t p_end_byte) {
	return make_content_symbol_id(p_script_id, p_language, p_content_sha256, p_owner_qualified_key, p_kind, p_start_byte, p_end_byte);
}

String ScriptSemanticAdapter::_diagnostic_id(const String &p_script_id, const String &p_language, const String &p_content_sha256, const String &p_severity, const String &p_code, uint64_t p_start_byte, uint64_t p_end_byte, const String &p_message) {
	return make_diagnostic_id(p_script_id, p_language, p_content_sha256, p_severity, p_code, p_start_byte, p_end_byte, p_message);
}

bool ScriptSemanticAdapter::is_gdscript_available() {
#ifdef MODULE_GDSCRIPT_ENABLED
	return true;
#else
	return false;
#endif
}

Array ScriptSemanticAdapter::make_adapter_statuses() {
	Array statuses;
#ifdef MODULE_GDSCRIPT_ENABLED
	statuses.push_back(make_adapter_status("gdscript", "available", "gdscript_parser_analyzer_v1", itos(GODOT_VERSION_MAJOR) + "." + itos(GODOT_VERSION_MINOR), Variant()));
#else
	statuses.push_back(make_adapter_status("gdscript", "unavailable", Variant(), Variant(), "The GDScript module is disabled in this editor build."));
#endif
	statuses.push_back(make_adapter_status("csharp", "discovery_only", "csharp_discovery_only_v1", "1.0", Variant()));
	return statuses;
}

Error ScriptSemanticAdapter::_project_source(const String &p_source, const String &p_path, const Dictionary &p_script_ref, uint64_t p_resource_revision, uint64_t p_script_graph_revision, DocumentProjection &r_projection) {
	r_projection = DocumentProjection();
	ERR_FAIL_COND_V(!_is_valid_script_path(p_path) || !_is_valid_script_ref(p_script_ref, p_path) || p_resource_revision > MAX_SAFE_INTEGER || p_script_graph_revision > MAX_SAFE_INTEGER, ERR_INVALID_PARAMETER);
	const CharString source_bytes = p_source.utf8();
	ERR_FAIL_COND_V((uint64_t)source_bytes.length() > MAX_SOURCE_BYTES, ERR_OUT_OF_MEMORY);
	ERR_FAIL_COND_V(p_source.find_char(0) >= 0, ERR_INVALID_DATA);
	const String content_sha256 = "sha256:" + sha256_hex(reinterpret_cast<const uint8_t *>(source_bytes.get_data()), source_bytes.length());
	ERR_FAIL_COND_V(content_sha256.length() != 71, ERR_CANT_CREATE);

	Dictionary document;
	document["script_ref"] = p_script_ref;
	document["path"] = p_path;
	document["content_sha256"] = content_sha256;
	document["resource_revision"] = (int64_t)p_resource_revision;
	document["script_graph_revision"] = (int64_t)p_script_graph_revision;
	Array symbols;
	Array relations;
	Array diagnostics;

	if (p_path.ends_with(".cs")) {
		document["language"] = "csharp";
		document["adapter_profile"] = "csharp_discovery_only_v1";
		document["completeness"] = "unavailable";
	} else {
#ifndef MODULE_GDSCRIPT_ENABLED
		return ERR_UNAVAILABLE;
#else
		document["language"] = "gdscript";
		document["adapter_profile"] = "gdscript_parser_analyzer_v1";
		ProjectionContext context;
		context.source = p_source;
		context.path = p_path;
		context.script_ref = p_script_ref;
		context.content_sha256 = content_sha256;
		context.script_id = make_resource_id(p_script_ref, p_path, content_sha256);
		context.resource_revision = p_resource_revision;
		context.script_graph_revision = p_script_graph_revision;
		ERR_FAIL_COND_V(context.script_id.is_empty() || !context.initialize_lines(), ERR_INVALID_DATA);

		GDScriptParser parser;
		const Error parse_error = parser.parse(p_source, p_path, false);
		Error analyzer_error = OK;
		const GDScriptParser::ClassNode *root = nullptr;
		if (parse_error == OK) {
			root = dynamic_cast<const GDScriptParser::ClassNode *>(parser.get_tree());
			ERR_FAIL_NULL_V(root, ERR_INVALID_DATA);
			GDScriptAnalyzer analyzer(&parser);
			analyzer_error = analyzer.analyze();
		}
		document["completeness"] = parse_error != OK ? "invalid" : (analyzer_error == OK ? "complete" : "partial");

		if (parse_error == OK) {
			ERR_FAIL_COND_V(!project_class(context, root, String(), Variant(), true) || !project_class_relations(context, root), ERR_INVALID_DATA);
		}
		const String error_authority = parse_error == OK ? "gdscript_analyzer" : "gdscript_parser";
		for (const GDScriptParser::ParserError &error : parser.get_errors()) {
			const String error_code = parse_error == OK ? analyzer_diagnostic_code(error, root) : "GDSCRIPT_PARSE_ERROR";
			int start_line = error.start_line;
			int start_column = error.start_column;
			int end_line = error.end_line;
			int end_column = error.end_column;
			if (parse_error != OK && start_line >= 1 && start_line <= context.lines.size()) {
				// A syntax failure often points only at the unexpected token. The
				// declaration line is the stable, useful evidence span and remains
				// fully bound to the exact content hash.
				const String &line = context.lines[start_line - 1];
				int first_content_column = 1;
				while (first_content_column <= line.length() && (line[first_content_column - 1] == ' ' || line[first_content_column - 1] == '\t')) {
					first_content_column++;
				}
				start_column = first_content_column;
				end_line = start_line;
				end_column = line.length() + 1;
			} else if (parse_error == OK && root && !root->extends_path.is_empty() && (error_code == "GDSCRIPT_MISSING_DEPENDENCY" || error_code == "GDSCRIPT_CYCLIC_DEPENDENCY")) {
				GDScriptParser::Node evidence;
				if (text_evidence_node(context, root->extends_start_line, root->extends_start_column, root->extends_end_column, root->extends_path, evidence)) {
					start_line = evidence.start_line;
					start_column = evidence.start_column;
					end_line = evidence.end_line;
					end_column = evidence.end_column;
				}
			}
			ERR_FAIL_COND_V(!context.append_diagnostic(error_code, "error", error.message, error_authority, start_line, start_column, end_line, end_column), ERR_OUT_OF_MEMORY);
		}
		for (const GDScriptWarning &warning : parser.get_warnings()) {
			ERR_FAIL_COND_V(!context.append_diagnostic(String(warning.get_name()).to_upper(), "warning", warning.get_message(), "gdscript_analyzer", warning.start_line, warning.start_column, warning.end_line, warning.end_column), ERR_OUT_OF_MEMORY);
		}
		symbols = context.symbols;
		relations = context.relations;
		diagnostics = context.diagnostics;
#endif
	}

	Dictionary bundle;
	bundle["document"] = document;
	bundle["symbols"] = symbols;
	bundle["relations"] = relations;
	bundle["diagnostics"] = diagnostics;
	r_projection.bundle = bundle;
	r_projection.source_bytes = source_bytes.length();
	Dictionary facts_document = document.duplicate();
	facts_document.erase("resource_revision");
	facts_document.erase("script_graph_revision");
	Array facts_symbols;
	for (const Variant &value : symbols) {
		Dictionary symbol = Dictionary(value).duplicate();
		symbol.erase("script_graph_revision");
		facts_symbols.push_back(symbol);
	}
	Array facts_relations;
	for (const Variant &value : relations) {
		Dictionary relation = Dictionary(value).duplicate();
		relation.erase("script_graph_revision");
		facts_relations.push_back(relation);
	}
	Array facts_diagnostics;
	for (const Variant &value : diagnostics) {
		Dictionary diagnostic = Dictionary(value).duplicate();
		diagnostic.erase("script_graph_revision");
		facts_diagnostics.push_back(diagnostic);
	}
	Dictionary facts;
	facts["document"] = facts_document;
	facts["symbols"] = facts_symbols;
	facts["relations"] = facts_relations;
	facts["diagnostics"] = facts_diagnostics;
	r_projection.facts_checksum = sha256_hex(JSON::stringify(facts, "", true, true));
	return r_projection.facts_checksum.is_empty() ? ERR_CANT_CREATE : OK;
}

Error ScriptSemanticAdapter::project_saved_document(const String &p_path, const Dictionary &p_script_ref, uint64_t p_resource_revision, uint64_t p_script_graph_revision, DocumentProjection &r_projection) {
	r_projection = DocumentProjection();
	ERR_FAIL_COND_V(!_is_valid_script_path(p_path) || !_is_valid_script_ref(p_script_ref, p_path), ERR_INVALID_PARAMETER);
	const int64_t size_before = FileAccess::get_size(p_path);
	const uint64_t modified_before = FileAccess::get_modified_time(p_path);
	ERR_FAIL_COND_V(size_before < 0, ERR_BUSY);
	ERR_FAIL_COND_V((uint64_t)size_before > MAX_SOURCE_BYTES, ERR_OUT_OF_MEMORY);
	Error read_error = OK;
	const PackedByteArray bytes = FileAccess::get_file_as_bytes(p_path, &read_error);
	ERR_FAIL_COND_V(read_error != OK || bytes.size() != size_before, read_error == OK ? ERR_FILE_CORRUPT : read_error);
	const int64_t size_after = FileAccess::get_size(p_path);
	const uint64_t modified_after = FileAccess::get_modified_time(p_path);
	ERR_FAIL_COND_V(size_before != size_after || modified_before != modified_after, ERR_BUSY);
	const String source = bytes.is_empty() ? String() : String::utf8(reinterpret_cast<const char *>(bytes.ptr()), bytes.size());
	const CharString round_trip = source.utf8();
	ERR_FAIL_COND_V(round_trip.length() != bytes.size() || (!bytes.is_empty() && std::memcmp(round_trip.get_data(), bytes.ptr(), bytes.size()) != 0), ERR_INVALID_DATA);
	const Error projection_error = _project_source(source, p_path, p_script_ref, p_resource_revision, p_script_graph_revision, r_projection);
	if (projection_error == OK) {
		r_projection.source_modified_time = modified_after;
	}
	return projection_error;
}
