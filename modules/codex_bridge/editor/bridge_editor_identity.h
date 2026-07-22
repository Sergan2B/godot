/**************************************************************************/
/*  bridge_editor_identity.h                                             */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#pragma once

#include "core/string/ustring.h"

class Node;

class BridgeEditorIdentity {
public:
	static String make_opaque_id(const String &p_prefix, const String &p_domain, const String &p_value);
	static String make_scene_id(const String &p_editor_session_id, const Node *p_scene_root);
	static String make_node_id(const String &p_editor_session_id, const String &p_scene_id, const String &p_node_path);
	static String make_history_id(const String &p_editor_session_id, int p_native_history_id);
	static String make_script_id(const String &p_editor_session_id, const String &p_identity);
};
