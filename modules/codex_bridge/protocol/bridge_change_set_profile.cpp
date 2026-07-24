/**************************************************************************/
/*  bridge_change_set_profile.cpp                                         */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#include "bridge_change_set_profile.h"

Dictionary BridgeChangeSetProfile::make_limits() {
	Dictionary limits;
	limits["operations"] = MAX_OPERATIONS;
	limits["report_page_bytes"] = MAX_REPORT_PAGE_BYTES;
	limits["report_pages"] = MAX_REPORT_PAGES;
	limits["retained_report_bytes"] = MAX_RETAINED_REPORT_BYTES;
	return limits;
}

Dictionary BridgeChangeSetProfile::make_capability(const String &p_name, bool p_ready, const String &p_unavailable_reason) {
	Dictionary capability;
	capability["name"] = p_name;
	capability["version"] = "1.0";
	capability["readiness"] = p_ready ? "ready" : "unavailable";
	capability["reason"] = p_ready ? "ready" : p_unavailable_reason;
	capability["limits"] = make_limits();
	return capability;
}

void BridgeChangeSetProfile::append_global_limits(Dictionary &r_limits) {
	const Dictionary limits = make_limits();
	const Array keys = limits.keys();
	for (int index = 0; index < keys.size(); index++) {
		r_limits["change_set_" + String(keys[index])] = limits[keys[index]];
	}
}
