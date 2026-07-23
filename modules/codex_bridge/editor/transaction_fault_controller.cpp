/**************************************************************************/
/*  transaction_fault_controller.cpp                                    */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "transaction_fault_controller.h"

#ifdef CODEX_BRIDGE_TESTS_ENABLED

#include "core/config/project_settings.h"
#include "core/io/dir_access.h"
#include "core/io/file_access.h"
#include "core/io/json.h"

#include "modules/codex_bridge/protocol/bridge_frame_codec.h"

namespace {

static constexpr char SCHEMA_VERSION[] = "s9-transaction-fault/1.0";

static bool is_safe_case_id(const String &p_value) {
	if (p_value.is_empty() || p_value.length() > 64) {
		return false;
	}
	for (int index = 0; index < p_value.length(); index++) {
		const char32_t character = p_value[index];
		if (!((character >= 'a' && character <= 'z') || (character >= '0' && character <= '9') || character == '-' || character == '_' || character == '.')) {
			return false;
		}
	}
	return true;
}

static bool is_transaction_id(const String &p_value) {
	if (!p_value.begins_with("transaction:") || p_value.length() != 44) {
		return false;
	}
	for (int index = 12; index < p_value.length(); index++) {
		const char32_t character = p_value[index];
		if (!((character >= '0' && character <= '9') || (character >= 'a' && character <= 'f'))) {
			return false;
		}
	}
	return true;
}

static bool is_point(const String &p_value) {
	return p_value == "after_prepare" ||
			p_value == "after_approval_before_preflight" ||
			p_value == "after_action_created_before_commit" ||
			p_value == "before_commit" ||
			p_value == "after_commit_before_response";
}

static bool is_mode(const String &p_value) {
	return p_value == "pause" || p_value == "fail" || p_value == "drop_response" || p_value == "terminate" || p_value == "observe";
}

static bool exact_marker(const Dictionary &p_marker) {
	return p_marker.size() == 6 &&
			p_marker.has("schema_version") &&
			p_marker.has("case_id") &&
			p_marker.has("transaction_id") &&
			p_marker.has("point") &&
			p_marker.has("mode") &&
			p_marker.has("state");
}

static bool read_marker(const String &p_path, Dictionary &r_marker) {
	if (!FileAccess::exists(p_path)) {
		return false;
	}
	const PackedByteArray bytes = FileAccess::get_file_as_bytes(p_path);
	return !bytes.is_empty() && BridgeJson::parse_strict_object(bytes, r_marker) == OK && exact_marker(r_marker);
}

static bool write_marker_atomic(const String &p_path, const Dictionary &p_marker) {
	const String temporary = p_path + ".tmp";
	DirAccess::remove_absolute(temporary);
	Error error = OK;
	Ref<FileAccess> file = FileAccess::open(temporary, FileAccess::WRITE, &error);
	if (error != OK || file.is_null()) {
		return false;
	}
	file->store_string(JSON::stringify(p_marker, "", true, true));
	file->flush();
	file.unref();
	DirAccess::remove_absolute(p_path);
	return DirAccess::rename_absolute(temporary, p_path) == OK;
}

} // namespace

String TransactionFaultController::_path(const String &p_name) const {
	return root_path.path_join(p_name);
}

bool TransactionFaultController::_load_armed() {
	Dictionary marker;
	if (!read_marker(_path("armed.json"), marker) ||
			marker.get("schema_version", String()) != SCHEMA_VERSION ||
			marker.get("state", String()) != "armed") {
		return false;
	}
	const String candidate_case = marker.get("case_id", String());
	const String candidate_transaction = marker.get("transaction_id", String());
	const String candidate_point = marker.get("point", String());
	const String candidate_mode = marker.get("mode", String());
	if (!is_safe_case_id(candidate_case) || !is_transaction_id(candidate_transaction) || !is_point(candidate_point) || !is_mode(candidate_mode)) {
		return false;
	}
	case_id = candidate_case;
	transaction_id = candidate_transaction;
	point = candidate_point;
	mode = candidate_mode;
	active = true;
	reached = false;
	return true;
}

bool TransactionFaultController::_release_matches() const {
	Dictionary marker;
	return read_marker(_path("release.json"), marker) &&
			marker.get("schema_version", String()) == SCHEMA_VERSION &&
			marker.get("case_id", String()) == case_id &&
			marker.get("transaction_id", String()) == transaction_id &&
			marker.get("point", String()) == point &&
			marker.get("mode", String()) == mode &&
			marker.get("state", String()) == "released";
}

bool TransactionFaultController::_write_reached() {
	if (DirAccess::make_dir_recursive_absolute(root_path) != OK) {
		return false;
	}
	Dictionary marker;
	marker["schema_version"] = SCHEMA_VERSION;
	marker["case_id"] = case_id;
	marker["transaction_id"] = transaction_id;
	marker["point"] = point;
	marker["mode"] = mode;
	marker["state"] = "reached";
	return write_marker_atomic(_path("reached.json"), marker);
}

void TransactionFaultController::_consume() {
	DirAccess::remove_absolute(_path("armed.json"));
	DirAccess::remove_absolute(_path("release.json"));
	active = false;
	reached = false;
	case_id.clear();
	transaction_id.clear();
	point.clear();
	mode.clear();
}

void TransactionFaultController::initialize(const String &p_root_path) {
	reset();
	if (!p_root_path.is_empty()) {
		root_path = p_root_path.simplify_path();
	} else {
		root_path = ProjectSettings::get_singleton()->globalize_path("res://.godot/codex/test-faults").simplify_path();
	}
}

void TransactionFaultController::reset() {
	root_path.clear();
	case_id.clear();
	transaction_id.clear();
	point.clear();
	mode.clear();
	active = false;
	reached = false;
}

TransactionFaultController::Action TransactionFaultController::hit(const String &p_transaction_id, const String &p_point) {
	if (root_path.is_empty() || (!active && !_load_armed())) {
		return ACTION_NO_MATCH;
	}
	if (p_transaction_id != transaction_id || p_point != point) {
		return ACTION_NO_MATCH;
	}
	if (!reached) {
		if (!_write_reached()) {
			_consume();
			return ACTION_NO_MATCH;
		}
		reached = true;
	}
	if (mode == "pause") {
		if (!_release_matches()) {
			return ACTION_WAIT;
		}
		_consume();
		return ACTION_CONTINUE;
	}
	if (mode == "terminate") {
		return ACTION_TERMINATE;
	}
	const Action result = mode == "fail" ? ACTION_FAIL : mode == "drop_response" ? ACTION_DROP_RESPONSE
																				  : ACTION_CONTINUE;
	_consume();
	return result;
}

String TransactionFaultController::get_root_path() const {
	return root_path;
}

String TransactionFaultController::get_case_id() const {
	return case_id;
}

bool TransactionFaultController::is_active() const {
	return active;
}

#endif // CODEX_BRIDGE_TESTS_ENABLED
