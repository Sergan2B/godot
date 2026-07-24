/**************************************************************************/
/*  bridge_change_set_profile.h                                           */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#pragma once

#include "core/variant/variant.h"

class BridgeChangeSetProfile {
public:
	static constexpr int64_t MAX_OPERATIONS = 16;
	static constexpr int64_t MAX_REPORT_PAGE_BYTES = 65536;
	static constexpr int64_t MAX_REPORT_PAGES = 4;
	static constexpr int64_t MAX_RETAINED_REPORT_BYTES = 262144;

	static Dictionary make_capability(const String &p_name, bool p_ready, const String &p_unavailable_reason);
	static Dictionary make_limits();
	static void append_global_limits(Dictionary &r_limits);
};
