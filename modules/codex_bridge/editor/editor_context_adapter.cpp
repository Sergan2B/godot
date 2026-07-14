/**************************************************************************/
/*  editor_context_adapter.cpp                                            */
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

#include "editor_context_adapter.h"

#include "core/crypto/crypto_core.h"
#include "core/io/json.h"
#include "core/object/property_info.h"
#include "core/object/script_language.h"
#include "core/os/thread.h"
#include "core/templates/hash_set.h"
#include "editor/editor_data.h"
#include "editor/editor_node.h"
#include "editor/editor_undo_redo_manager.h"
#include "modules/codex_bridge/protocol/bridge_crypto.h"
#include "scene/main/node.h"

String EditorContextAdapter::_make_opaque_id(const String &p_prefix, const String &p_domain, const String &p_value) {
	const CharString bytes = (p_domain + "\n" + p_value).utf8();
	PackedByteArray digest;
	digest.resize(32);
	if (CryptoCore::sha256(reinterpret_cast<const uint8_t *>(bytes.get_data()), bytes.length(), digest.ptrw()) != OK) {
		return p_prefix + "00000000000000000000000000000000";
	}
	return p_prefix + BridgeCrypto::bytes_to_lower_hex(digest).left(32);
}

String EditorContextAdapter::make_scene_id(const String &p_editor_session_id, const Node *p_scene_root) {
	if (!p_scene_root) {
		return String();
	}
	String identity = p_scene_root->get_scene_file_path();
	if (identity.is_empty()) {
		identity = String(p_scene_root->get_name());
	}
	return _make_opaque_id("scene:", "godot-codex-scene/v1\n" + p_editor_session_id, identity);
}

String EditorContextAdapter::_bounded_identity(const String &p_value, bool &r_truncated) {
	if (p_value.length() <= MAX_IDENTITY_CHARACTERS) {
		return p_value;
	}
	r_truncated = true;
	return p_value.left(MAX_IDENTITY_CHARACTERS);
}

Variant EditorContextAdapter::_project_variant(const Variant &p_value, int p_depth, bool &r_truncated) {
	if (p_depth > MAX_VARIANT_DEPTH) {
		r_truncated = true;
		Dictionary truncated;
		truncated["type"] = Variant::get_type_name(p_value.get_type());
		truncated["truncated"] = true;
		return truncated;
	}

	switch (p_value.get_type()) {
		case Variant::NIL:
		case Variant::BOOL:
		case Variant::INT:
		case Variant::FLOAT:
			return p_value;
		case Variant::STRING: {
			const String value = p_value;
			if (value.length() > MAX_STRING_CHARACTERS) {
				r_truncated = true;
				return value.left(MAX_STRING_CHARACTERS);
			}
			return value;
		}
		case Variant::STRING_NAME: {
			const StringName value = p_value;
			const String projected = String(value);
			if (projected.length() > MAX_STRING_CHARACTERS) {
				r_truncated = true;
				return projected.left(MAX_STRING_CHARACTERS);
			}
			return projected;
		}
		case Variant::NODE_PATH: {
			const NodePath value = p_value;
			const String projected = String(value);
			if (projected.length() > MAX_STRING_CHARACTERS) {
				r_truncated = true;
				return projected.left(MAX_STRING_CHARACTERS);
			}
			return projected;
		}
		case Variant::VECTOR2: {
			const Vector2 value = p_value;
			Dictionary result;
			result["type"] = "Vector2";
			result["x"] = value.x;
			result["y"] = value.y;
			return result;
		}
		case Variant::VECTOR2I: {
			const Vector2i value = p_value;
			Dictionary result;
			result["type"] = "Vector2i";
			result["x"] = value.x;
			result["y"] = value.y;
			return result;
		}
		case Variant::VECTOR3: {
			const Vector3 value = p_value;
			Dictionary result;
			result["type"] = "Vector3";
			result["x"] = value.x;
			result["y"] = value.y;
			result["z"] = value.z;
			return result;
		}
		case Variant::VECTOR3I: {
			const Vector3i value = p_value;
			Dictionary result;
			result["type"] = "Vector3i";
			result["x"] = value.x;
			result["y"] = value.y;
			result["z"] = value.z;
			return result;
		}
		case Variant::COLOR: {
			const Color value = p_value;
			Dictionary result;
			result["type"] = "Color";
			result["r"] = value.r;
			result["g"] = value.g;
			result["b"] = value.b;
			result["a"] = value.a;
			return result;
		}
		case Variant::ARRAY: {
			const Array source = p_value;
			Array result;
			const int count = MIN(source.size(), MAX_CONTAINER_ITEMS);
			for (int index = 0; index < count; index++) {
				result.push_back(_project_variant(source[index], p_depth + 1, r_truncated));
			}
			if (source.size() > count) {
				r_truncated = true;
			}
			return result;
		}
		case Variant::DICTIONARY: {
			const Dictionary source = p_value;
			Dictionary result;
			const Array keys = source.keys();
			const int count = MIN(keys.size(), MAX_CONTAINER_ITEMS);
			for (int index = 0; index < count; index++) {
				String key = keys[index].stringify();
				if (key.length() > MAX_STRING_CHARACTERS) {
					key = key.left(MAX_STRING_CHARACTERS);
					r_truncated = true;
				}
				result[key] = _project_variant(source[keys[index]], p_depth + 1, r_truncated);
			}
			if (keys.size() > count) {
				r_truncated = true;
			}
			return result;
		}
		default: {
			Dictionary opaque;
			opaque["type"] = Variant::get_type_name(p_value.get_type());
			opaque["opaque"] = true;
			return opaque;
		}
	}
}

Error EditorContextAdapter::capture(const String &p_project_id, const String &p_editor_session_id, const Dictionary &p_revisions, Dictionary &r_snapshot) {
	ERR_FAIL_COND_V_MSG(!Thread::is_main_thread(), ERR_BUG, "Editor context must be captured on the main thread.");
	EditorNode *editor = EditorNode::get_singleton();
	ERR_FAIL_NULL_V(editor, ERR_UNCONFIGURED);
	EditorData &editor_data = EditorNode::get_editor_data();
	Node *scene_root = editor_data.get_edited_scene_root();

	Array entities;
	Dictionary editor_entity;
	editor_entity["kind"] = "editor_state";
	editor_entity["entity_id"] = "editor:" + p_editor_session_id.trim_prefix("editor:");
	editor_entity["project_id"] = p_project_id;
	editor_entity["editor_session_id"] = p_editor_session_id;
	editor_entity["open_scene_count"] = editor_data.get_edited_scene_count();
	editor_entity["revisions"] = p_revisions;
	entities.push_back(editor_entity);

	Array selected_node_ids;
	String scene_id;
	bool dirty = false;
	bool truncated = false;
	int total_inspector_bytes = 0;
	int total_selection_count = 0;
	if (scene_root) {
		scene_id = make_scene_id(p_editor_session_id, scene_root);
		EditorUndoRedoManager *undo_redo = EditorUndoRedoManager::get_singleton();
		dirty = undo_redo && undo_redo->is_history_unsaved(editor_data.get_current_edited_scene_history_id());
		HashSet<ObjectID> selected_ids;
		const List<Node *> selected_nodes = editor->get_editor_selection()->get_full_selected_node_list();
		for (Node *selected : selected_nodes) {
			if (selected) {
				selected_ids.insert(selected->get_instance_id());
			}
		}
		total_selection_count = selected_ids.size();

		List<Node *> pending;
		pending.push_back(scene_root);
		int node_count = 0;
		while (!pending.is_empty() && node_count < MAX_SCENE_NODES) {
			Node *node = pending.front()->get();
			pending.pop_front();
			const String node_path = String(scene_root->get_path_to(node));
			const String node_id = _make_opaque_id("node:", "godot-codex-node/v1\n" + p_editor_session_id + "\n" + scene_id, node_path);
			const bool selected = selected_ids.has(node->get_instance_id());

			Dictionary entity;
			entity["kind"] = "node";
			entity["entity_id"] = node_id;
			entity["scene_id"] = scene_id;
			entity["node_path"] = _bounded_identity(node_path, truncated);
			entity["name"] = _bounded_identity(String(node->get_name()), truncated);
			entity["godot_type"] = String(node->get_class());
			entity["selected"] = selected;
			Node *owner = node->get_owner();
			const String owner_path = owner && (owner == scene_root || scene_root->is_ancestor_of(owner)) ? String(scene_root->get_path_to(owner)) : String();
			entity["owner_path"] = _bounded_identity(owner_path, truncated);
			const Ref<Script> script = node->get_script();
			entity["script_path"] = _bounded_identity(script.is_valid() ? script->get_path() : String(), truncated);

			if (selected) {
				selected_node_ids.push_back(node_id);
				Array properties;
				int property_bytes = 0;
				bool properties_truncated = false;
				List<PropertyInfo> property_list;
				node->get_property_list(&property_list);
				int property_count = 0;
				for (const PropertyInfo &property : property_list) {
					if (property_count >= MAX_CONTAINER_ITEMS) {
						truncated = true;
						properties_truncated = true;
						break;
					}
					if (!(property.usage & PROPERTY_USAGE_EDITOR) || (property.usage & PROPERTY_USAGE_INTERNAL) || property.name == SNAME("script")) {
						continue;
					}
					bool valid = false;
					const Variant value = node->get(property.name, &valid);
					if (!valid) {
						continue;
					}
					Dictionary projected;
					projected["name"] = _bounded_identity(String(property.name), truncated);
					projected["variant_type"] = Variant::get_type_name(property.type);
					projected["value"] = _project_variant(value, 0, truncated);
					projected["source"] = "live_editor_property";
					projected["freshness"] = "current";
					const Dictionary scene_revisions = p_revisions["scene_revisions"];
					projected["scene_revision"] = scene_revisions.get(scene_id, 0);
					int projected_bytes = JSON::stringify(projected, "", true, true).utf8().length();
					if (projected_bytes > MAX_PROJECTED_VALUE_BYTES) {
						Dictionary limited_value;
						limited_value["type"] = Variant::get_type_name(value.get_type());
						limited_value["truncated"] = true;
						projected["value"] = limited_value;
						projected_bytes = JSON::stringify(projected, "", true, true).utf8().length();
						truncated = true;
					}
					if (property_bytes + projected_bytes > MAX_INSPECTOR_BYTES_PER_NODE || total_inspector_bytes + projected_bytes > MAX_TOTAL_INSPECTOR_BYTES) {
						truncated = true;
						properties_truncated = true;
						break;
					}
					properties.push_back(projected);
					property_bytes += projected_bytes;
					total_inspector_bytes += projected_bytes;
					property_count++;
				}
				entity["properties"] = properties;
				entity["properties_truncated"] = properties_truncated;
			}
			entities.push_back(entity);
			node_count++;
			for (int child_index = 0; child_index < node->get_child_count(); child_index++) {
				pending.push_back(node->get_child(child_index));
			}
		}
		if (!pending.is_empty()) {
			truncated = true;
		}
		if (selected_node_ids.size() < total_selection_count) {
			truncated = true;
		}

		Dictionary scene_entity;
		scene_entity["kind"] = "scene";
		scene_entity["entity_id"] = scene_id;
		scene_entity["path"] = _bounded_identity(scene_root->get_scene_file_path(), truncated);
		scene_entity["root_node_path"] = ".";
		scene_entity["dirty"] = dirty;
		scene_entity["node_count"] = node_count;
		scene_entity["selected_node_ids"] = selected_node_ids;
		const Dictionary scene_revisions = p_revisions["scene_revisions"];
		scene_entity["scene_revision"] = scene_revisions.get(scene_id, 0);
		entities.insert(1, scene_entity);
	}

	editor_entity["current_scene_id"] = scene_id;
	editor_entity["current_scene_dirty"] = dirty;
	editor_entity["selection_count"] = total_selection_count;
	editor_entity["projected_selection_count"] = selected_node_ids.size();
	entities[0] = editor_entity;
	r_snapshot["project_id"] = p_project_id;
	r_snapshot["editor_session_id"] = p_editor_session_id;
	r_snapshot["revision_vector"] = p_revisions;
	r_snapshot["current_scene_id"] = scene_id;
	r_snapshot["dirty"] = dirty;
	r_snapshot["selected_node_ids"] = selected_node_ids;
	r_snapshot["entities"] = entities;
	r_snapshot["truncated"] = truncated;
	return OK;
}
