/**************************************************************************/
/*  test_transaction_fault_controller.cpp                               */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "tests/test_macros.h"

TEST_FORCE_LINK(test_transaction_fault_controller)

#include "modules/modules_enabled.gen.h"

#ifdef MODULE_CODEX_BRIDGE_ENABLED

#define CODEX_BRIDGE_TESTS_ENABLED

#include "core/io/dir_access.h"
#include "core/io/file_access.h"
#include "core/io/json.h"

#include "modules/codex_bridge/editor/transaction_fault_controller.h"

namespace TestTransactionFaultController {

static const String TRANSACTION_ID = "transaction:11111111111111111111111111111111";

struct TemporaryDirectory {
	String path;

	TemporaryDirectory() {
		Error error = OK;
		Ref<FileAccess> placeholder = FileAccess::create_temp(FileAccess::WRITE_READ, "codex-s9-fault", "tmp", true, &error);
		REQUIRE(error == OK);
		REQUIRE(placeholder.is_valid());
		path = placeholder->get_path_absolute();
		placeholder.unref();
		REQUIRE(DirAccess::remove_absolute(path) == OK);
		REQUIRE(DirAccess::make_dir_recursive_absolute(path) == OK);
	}

	~TemporaryDirectory() {
		DirAccess::remove_absolute(path.path_join("armed.json"));
		DirAccess::remove_absolute(path.path_join("reached.json"));
		DirAccess::remove_absolute(path.path_join("release.json"));
		DirAccess::remove_absolute(path.path_join("armed.json.tmp"));
		DirAccess::remove_absolute(path.path_join("reached.json.tmp"));
		DirAccess::remove_absolute(path.path_join("release.json.tmp"));
		DirAccess::remove_absolute(path);
	}
};

static void marker(const String &p_root, const String &p_state, const String &p_mode, const String &p_point = "before_commit", const String &p_transaction_id = TRANSACTION_ID, const String &p_case_id = "case-1") {
	Dictionary value;
	value["schema_version"] = "s9-transaction-fault/1.0";
	value["case_id"] = p_case_id;
	value["transaction_id"] = p_transaction_id;
	value["point"] = p_point;
	value["mode"] = p_mode;
	value["state"] = p_state;
	Ref<FileAccess> file = FileAccess::open(p_root.path_join(p_state == "released" ? "release.json" : "armed.json"), FileAccess::WRITE);
	REQUIRE(file.is_valid());
	file->store_string(JSON::stringify(value, "", true, true));
	file->flush();
}

TEST_CASE("[CodexS9Fault] Marker handshake is exact, one-shot, and nonblocking") {
	TemporaryDirectory directory;
	TransactionFaultController controller;
	controller.initialize(directory.path);
	marker(directory.path, "armed", "pause");

	CHECK(controller.hit("transaction:22222222222222222222222222222222", "before_commit") == TransactionFaultController::ACTION_NO_MATCH);
	CHECK_FALSE(FileAccess::exists(directory.path.path_join("reached.json")));
	CHECK(controller.hit(TRANSACTION_ID, "before_commit") == TransactionFaultController::ACTION_WAIT);
	CHECK(FileAccess::exists(directory.path.path_join("reached.json")));
	CHECK(controller.is_active());
	CHECK(controller.get_case_id() == "case-1");
	CHECK(controller.hit(TRANSACTION_ID, "before_commit") == TransactionFaultController::ACTION_WAIT);

	marker(directory.path, "released", "pause");
	CHECK(controller.hit(TRANSACTION_ID, "before_commit") == TransactionFaultController::ACTION_CONTINUE);
	CHECK_FALSE(controller.is_active());
	CHECK_FALSE(FileAccess::exists(directory.path.path_join("armed.json")));
	CHECK_FALSE(FileAccess::exists(directory.path.path_join("release.json")));
	CHECK(controller.hit(TRANSACTION_ID, "before_commit") == TransactionFaultController::ACTION_NO_MATCH);
}

TEST_CASE("[CodexS9Fault] Failure, response loss, observation, and termination modes are distinct") {
	struct ModeCase {
		const char *mode = nullptr;
		TransactionFaultController::Action action = TransactionFaultController::ACTION_NO_MATCH;
	};
	const ModeCase cases[] = {
		{ "fail", TransactionFaultController::ACTION_FAIL },
		{ "drop_response", TransactionFaultController::ACTION_DROP_RESPONSE },
		{ "observe", TransactionFaultController::ACTION_CONTINUE },
		{ "terminate", TransactionFaultController::ACTION_TERMINATE },
	};
	for (const ModeCase &entry : cases) {
		TemporaryDirectory directory;
		TransactionFaultController controller;
		controller.initialize(directory.path);
		marker(directory.path, "armed", entry.mode, "after_commit_before_response");
		CHECK(controller.hit(TRANSACTION_ID, "after_commit_before_response") == entry.action);
		CHECK(FileAccess::exists(directory.path.path_join("reached.json")));
		if (String(entry.mode) == "terminate") {
			CHECK(controller.is_active());
		} else {
			CHECK_FALSE(controller.is_active());
			CHECK_FALSE(FileAccess::exists(directory.path.path_join("armed.json")));
		}
	}
}

TEST_CASE("[CodexS9Fault] Malformed, unsafe, and mismatched markers never arm") {
	TemporaryDirectory directory;
	TransactionFaultController controller;
	controller.initialize(directory.path);
	marker(directory.path, "armed", "pause", "before_commit", "transaction:native-42");
	CHECK(controller.hit(TRANSACTION_ID, "before_commit") == TransactionFaultController::ACTION_NO_MATCH);
	CHECK_FALSE(controller.is_active());

	DirAccess::remove_absolute(directory.path.path_join("armed.json"));
	marker(directory.path, "armed", "pause", "unknown_point");
	CHECK(controller.hit(TRANSACTION_ID, "before_commit") == TransactionFaultController::ACTION_NO_MATCH);
	CHECK_FALSE(controller.is_active());

	DirAccess::remove_absolute(directory.path.path_join("armed.json"));
	marker(directory.path, "armed", "pause", "before_commit", TRANSACTION_ID, "../unsafe");
	CHECK(controller.hit(TRANSACTION_ID, "before_commit") == TransactionFaultController::ACTION_NO_MATCH);
	CHECK_FALSE(controller.is_active());
}

} // namespace TestTransactionFaultController

#endif // MODULE_CODEX_BRIDGE_ENABLED
