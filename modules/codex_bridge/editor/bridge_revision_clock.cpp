/**************************************************************************/
/*  bridge_revision_clock.cpp                                             */
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

#include "bridge_revision_clock.h"

void BridgeRevisionClock::initialize(const String &p_editor_session_id) {
	editor_session_id = p_editor_session_id;
	event_seq = 0;
	project_revision = 0;
	operation_seq = 0;
	// Resource graph revision 1 represents the initial editor filesystem
	// catalog, even when the catalog is empty.
	resource_revision = 1;
	scene_graph_revision = 1;
	script_graph_revision = 1;
	scene_revisions.clear();
}

uint64_t BridgeRevisionClock::record_selection_change() {
	return ++event_seq;
}

uint64_t BridgeRevisionClock::record_scene_change(const String &p_scene_id) {
	++event_seq;
	++project_revision;
	uint64_t *scene_revision = scene_revisions.getptr(p_scene_id);
	if (scene_revision) {
		++(*scene_revision);
	} else {
		scene_revisions.insert(p_scene_id, 1);
	}
	return event_seq;
}

uint64_t BridgeRevisionClock::record_resource_change() {
	++event_seq;
	++project_revision;
	return ++resource_revision;
}

uint64_t BridgeRevisionClock::record_scene_graph_change() {
	++event_seq;
	++project_revision;
	return ++scene_graph_revision;
}

uint64_t BridgeRevisionClock::record_script_graph_change() {
	++event_seq;
	++project_revision;
	return ++script_graph_revision;
}

uint64_t BridgeRevisionClock::get_scene_revision(const String &p_scene_id) const {
	const uint64_t *scene_revision = scene_revisions.getptr(p_scene_id);
	return scene_revision ? *scene_revision : 0;
}

uint64_t BridgeRevisionClock::get_resource_revision() const {
	return resource_revision;
}

uint64_t BridgeRevisionClock::get_scene_graph_revision() const {
	return scene_graph_revision;
}

uint64_t BridgeRevisionClock::get_script_graph_revision() const {
	return script_graph_revision;
}

Dictionary BridgeRevisionClock::get_revision_vector() const {
	Dictionary scenes;
	for (const KeyValue<String, uint64_t> &entry : scene_revisions) {
		scenes[entry.key] = (int64_t)entry.value;
	}
	Dictionary revisions;
	revisions["editor_session_id"] = editor_session_id;
	revisions["event_seq"] = (int64_t)event_seq;
	revisions["project_revision"] = (int64_t)project_revision;
	revisions["operation_seq"] = (int64_t)operation_seq;
	revisions["resource_revision"] = (int64_t)resource_revision;
	revisions["scene_graph_revision"] = (int64_t)scene_graph_revision;
	revisions["script_graph_revision"] = (int64_t)script_graph_revision;
	revisions["scene_revisions"] = scenes;
	return revisions;
}
