/**************************************************************************/
/*  transaction_fault_controller.h                                      */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#pragma once

#ifdef CODEX_BRIDGE_TESTS_ENABLED

#include "core/string/ustring.h"

class TransactionFaultController {
public:
	enum Action {
		ACTION_NO_MATCH,
		ACTION_CONTINUE,
		ACTION_WAIT,
		ACTION_FAIL,
		ACTION_DROP_RESPONSE,
		ACTION_TERMINATE,
	};

private:
	String root_path;
	String case_id;
	String transaction_id;
	String point;
	String mode;
	bool active = false;
	bool reached = false;

	String _path(const String &p_name) const;
	bool _load_armed();
	bool _release_matches() const;
	bool _write_reached();
	void _consume();

public:
	void initialize(const String &p_root_path = String());
	void reset();
	Action hit(const String &p_transaction_id, const String &p_point);

	String get_root_path() const;
	String get_case_id() const;
	bool is_active() const;
};

#endif // CODEX_BRIDGE_TESTS_ENABLED
