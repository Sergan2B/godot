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

#include "bounded_variant_projector.h"

#include "core/config/project_settings.h"
#include "core/crypto/crypto_core.h"
#include "core/io/file_access.h"
#include "core/io/json.h"
#include "core/object/property_info.h"
#include "core/object/script_language.h"
#include "core/os/thread.h"
#include "core/os/os.h"
#include "core/templates/hash_set.h"
#include "editor/editor_data.h"
#include "editor/editor_interface.h"
#include "editor/editor_log.h"
#include "editor/editor_main_screen.h"
#include "editor/editor_node.h"
#include "editor/editor_undo_redo_manager.h"
#include "editor/inspector/editor_inspector.h"
#include "editor/script/script_editor_plugin.h"
#include "editor/scene/3d/node_3d_editor_plugin.h"
#include "editor/scene/canvas_item_editor_plugin.h"
#include "scene/main/node.h"

#include "modules/codex_bridge/protocol/bridge_crypto.h"

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
		identity = String(p_scene_root->get_name()) + "\n" + String::num_uint64(p_scene_root->get_instance_id());
	}
	return _make_opaque_id("scene:", "godot-codex-scene/v1\n" + p_editor_session_id, identity);
}

String EditorContextAdapter::_make_node_id(const String &p_editor_session_id, const String &p_scene_id, const String &p_node_path) {
	return _make_opaque_id("node:", "godot-codex-node/v1\n" + p_editor_session_id + "\n" + p_scene_id, p_node_path);
}

String EditorContextAdapter::_make_history_id(const String &p_editor_session_id, int p_native_history_id) {
	return _make_opaque_id("history:", "godot-codex-history/v1\n" + p_editor_session_id, String::num_int64(p_native_history_id));
}

String EditorContextAdapter::_make_script_id(const String &p_editor_session_id, const String &p_identity) {
	return _make_opaque_id("script:", "godot-codex-live-script/v1\n" + p_editor_session_id, p_identity);
}

String EditorContextAdapter::_redact_output_message(const String &p_message, bool &r_redacted) {
	String message = p_message;
	const String lowered = message.to_lower();
	static const char *sensitive_markers[] = { "authorization:", "bearer ", "api_key", "apikey", "password=", "password:", "secret=", "secret:", "token=", "token:" };
	for (const char *marker : sensitive_markers) {
		if (lowered.contains(marker)) {
			r_redacted = true;
			return "<redacted sensitive output>";
		}
	}
	const String project_root = ProjectSettings::get_singleton() ? ProjectSettings::get_singleton()->get_resource_path().replace("\\", "/").trim_suffix("/") : String();
	if (!project_root.is_empty()) {
		const String normalized = message.replace("\\", "/");
		if (normalized.contains(project_root)) {
			message = normalized.replace(project_root, "<project>");
			r_redacted = true;
		}
	}
	String home = OS::get_singleton()->get_environment("HOME");
	if (home.is_empty()) {
		home = OS::get_singleton()->get_environment("USERPROFILE");
	}
	home = home.replace("\\", "/").trim_suffix("/");
	if (!home.is_empty()) {
		const String normalized = message.replace("\\", "/");
		if (normalized.contains(home)) {
			message = normalized.replace(home, "<home>");
			r_redacted = true;
		}
	}
	if (message.utf8().length() > MAX_STRING_CHARACTERS) {
		int low = 0;
		int high = message.length();
		while (low < high) {
			const int middle = (low + high + 1) / 2;
			if (message.left(middle).utf8().length() <= MAX_STRING_CHARACTERS) {
				low = middle;
			} else {
				high = middle - 1;
			}
		}
		message = message.left(low);
		r_redacted = true;
	}
	return message;
}

String EditorContextAdapter::_bounded_identity(const String &p_value, bool &r_truncated) {
	if (p_value.length() <= MAX_IDENTITY_CHARACTERS) {
		return p_value;
	}
	r_truncated = true;
	return p_value.left(MAX_IDENTITY_CHARACTERS);
}

Variant EditorContextAdapter::_project_variant(const Variant &p_value, int p_depth, bool &r_truncated) {
	return BoundedVariantProjector::project_raw(p_value, r_truncated, p_depth);
}

Array EditorContextAdapter::_capture_properties(Object *p_object, int p_limit, const String &p_scene_id, const Dictionary &p_revisions, bool &r_truncated, int &r_total_bytes) {
	Array properties;
	if (!p_object) {
		return properties;
	}
	int property_bytes = 0;
	List<PropertyInfo> property_list;
	p_object->get_property_list(&property_list);
	const Dictionary scene_revisions = p_revisions.get("scene_revisions", Dictionary());
	for (const PropertyInfo &property : property_list) {
		if (!(property.usage & PROPERTY_USAGE_EDITOR) || (property.usage & PROPERTY_USAGE_INTERNAL) || property.name == SNAME("script")) {
			continue;
		}
		if (properties.size() >= p_limit) {
			r_truncated = true;
			break;
		}
		bool valid = false;
		const Variant value = p_object->get(property.name, &valid);
		if (!valid) {
			continue;
		}
		Dictionary projected;
		projected["name"] = _bounded_identity(String(property.name), r_truncated);
		projected["variant_type"] = Variant::get_type_name(property.type);
		projected["editable"] = !(property.usage & PROPERTY_USAGE_READ_ONLY);
		projected["read_only"] = (bool)(property.usage & PROPERTY_USAGE_READ_ONLY);
		projected["can_revert"] = p_object->property_can_revert(property.name);
		projected["hint"] = (int64_t)property.hint;
		projected["hint_string"] = _bounded_identity(property.hint_string, r_truncated);
		projected["value"] = _project_variant(value, 0, r_truncated);
		projected["source"] = "live_editor_property";
		projected["freshness"] = "current";
		if (!p_scene_id.is_empty()) {
			projected["scene_revision"] = scene_revisions.get(p_scene_id, 0);
		}
		int projected_bytes = JSON::stringify(projected, "", true, true).utf8().length();
		if (projected_bytes > MAX_PROJECTED_VALUE_BYTES) {
			Dictionary limited_value;
			limited_value["type"] = BoundedVariantProjector::type_token(value.get_type());
			limited_value["opaque"] = true;
			limited_value["omitted_reason"] = "max_encoded_bytes";
			projected["value"] = limited_value;
			projected_bytes = JSON::stringify(projected, "", true, true).utf8().length();
			r_truncated = true;
		}
		if (property_bytes + projected_bytes > MAX_INSPECTOR_BYTES_PER_NODE || r_total_bytes + projected_bytes > MAX_TOTAL_INSPECTOR_BYTES) {
			r_truncated = true;
			break;
		}
		properties.push_back(projected);
		property_bytes += projected_bytes;
		r_total_bytes += projected_bytes;
	}
	return properties;
}

Error EditorContextAdapter::capture(const String &p_project_id, const String &p_editor_session_id, const Dictionary &p_revisions, Dictionary &r_snapshot, bool p_full_live_context, const Dictionary &p_history_transitions) {
	ERR_FAIL_COND_V_MSG(!Thread::is_main_thread(), ERR_BUG, "Editor context must be captured on the main thread.");
	EditorNode *editor = EditorNode::get_singleton();
	ERR_FAIL_NULL_V(editor, ERR_UNCONFIGURED);
	EditorData &editor_data = EditorNode::get_editor_data();
	const int current_scene_index = editor_data.get_edited_scene();

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
	Array open_scene_ids;
	String current_scene_id;
	bool current_scene_dirty = false;
	bool truncated = false;
	int total_inspector_bytes = 0;
	HashSet<ObjectID> selected_ids;
	const List<Node *> selected_nodes = editor->get_editor_selection()->get_full_selected_node_list();
	for (Node *selected : selected_nodes) {
		if (selected) {
			selected_ids.insert(selected->get_instance_id());
		}
	}
	const int total_selection_count = selected_ids.size();
	Object *inspected_object = nullptr;
	if (EditorInterface::get_singleton() && EditorInterface::get_singleton()->get_inspector()) {
		inspected_object = EditorInterface::get_singleton()->get_inspector()->get_edited_object();
	}
	HashMap<ObjectID, int> node_entity_indices;
	HashMap<ObjectID, String> node_live_ids;
	HashMap<ObjectID, String> node_scene_ids;
	HashMap<ObjectID, String> node_paths;
	EditorUndoRedoManager *undo_redo = EditorUndoRedoManager::get_singleton();
	const Dictionary scene_revisions = p_revisions.get("scene_revisions", Dictionary());
	const int scene_count = editor_data.get_edited_scene_count();
	const int first_scene = p_full_live_context ? 0 : current_scene_index;
	const int last_scene = p_full_live_context ? MIN(scene_count, MAX_OPEN_SCENES) : (current_scene_index >= 0 ? current_scene_index + 1 : 0);
	for (int scene_index = first_scene; scene_index >= 0 && scene_index < last_scene; scene_index++) {
		Node *scene_root = editor_data.get_edited_scene_root(scene_index);
		if (!scene_root) {
			continue;
		}
		const String scene_id = make_scene_id(p_editor_session_id, scene_root);
		const bool current = scene_index == current_scene_index;
		const int native_history_id = editor_data.get_scene_history_id(scene_index);
		const bool dirty = undo_redo && undo_redo->is_history_unsaved(native_history_id);
		open_scene_ids.push_back(scene_id);
		if (current) {
			current_scene_id = scene_id;
			current_scene_dirty = dirty;
		}

		Dictionary scene_entity;
		scene_entity["kind"] = "scene";
		scene_entity["entity_id"] = scene_id;
		scene_entity["identity_scope"] = "editor_session";
		scene_entity["tab_index"] = scene_index;
		scene_entity["title"] = _bounded_identity(editor_data.get_scene_title(scene_index), truncated);
		scene_entity["scene_type"] = _bounded_identity(editor_data.get_scene_type(scene_index), truncated);
		scene_entity["path"] = _bounded_identity(editor_data.get_scene_path(scene_index), truncated);
		scene_entity["root_node_path"] = ".";
		scene_entity["current"] = current;
		scene_entity["dirty"] = dirty;
		scene_entity["history_id"] = _make_history_id(p_editor_session_id, native_history_id);
		scene_entity["scene_revision"] = scene_revisions.get(scene_id, 0);
		const int scene_entity_index = entities.size();
		entities.push_back(scene_entity);

		List<Node *> pending;
		pending.push_back(scene_root);
		int node_count = 0;
		while (!pending.is_empty() && node_count < MAX_SCENE_NODES) {
			Node *node = pending.front()->get();
			pending.pop_front();
			const String node_path = String(scene_root->get_path_to(node));
			const String node_id = _make_node_id(p_editor_session_id, scene_id, node_path);
			Dictionary entity;
			entity["kind"] = "node";
			entity["entity_id"] = node_id;
			entity["identity_scope"] = "editor_session";
			entity["scene_id"] = scene_id;
			entity["node_path"] = _bounded_identity(node_path, truncated);
			entity["name"] = _bounded_identity(String(node->get_name()), truncated);
			entity["godot_type"] = String(node->get_class());
			entity["selected"] = false;
			entity["primary"] = false;
			Node *owner = node->get_owner();
			const String owner_path = owner && (owner == scene_root || scene_root->is_ancestor_of(owner)) ? String(scene_root->get_path_to(owner)) : String();
			entity["owner_path"] = _bounded_identity(owner_path, truncated);
			const Ref<Script> script = node->get_script();
			entity["script_path"] = _bounded_identity(script.is_valid() ? script->get_path() : String(), truncated);
			const int entity_index = entities.size();
			entities.push_back(entity);
			node_entity_indices.insert(node->get_instance_id(), entity_index);
			node_live_ids.insert(node->get_instance_id(), node_id);
			node_scene_ids.insert(node->get_instance_id(), scene_id);
			node_paths.insert(node->get_instance_id(), node_path);
			node_count++;
			for (int child_index = 0; child_index < node->get_child_count(); child_index++) {
				pending.push_back(node->get_child(child_index));
			}
		}
		const bool nodes_truncated = !pending.is_empty();
		truncated = truncated || nodes_truncated;
		scene_entity = entities[scene_entity_index];
		scene_entity["root_node_id"] = _make_node_id(p_editor_session_id, scene_id, ".");
		scene_entity["node_count"] = node_count;
		scene_entity["nodes_truncated"] = nodes_truncated;
		scene_entity["coverage"] = nodes_truncated ? "partial" : "complete";
		entities[scene_entity_index] = scene_entity;
	}
	if (p_full_live_context && scene_count > MAX_OPEN_SCENES) {
		truncated = true;
	}

	Vector<String> sorted_selected_ids;
	HashMap<String, ObjectID> selected_objects_by_live_id;
	for (const ObjectID &selected_id : selected_ids) {
		const String *live_id = node_live_ids.getptr(selected_id);
		if (live_id) {
			sorted_selected_ids.push_back(*live_id);
			selected_objects_by_live_id.insert(*live_id, selected_id);
		}
	}
	sorted_selected_ids.sort();
	const int projected_selection_count = MIN(sorted_selected_ids.size(), MAX_SELECTED_NODES);
	for (int selection_index = 0; selection_index < projected_selection_count; selection_index++) {
		const String &node_id = sorted_selected_ids[selection_index];
		const ObjectID *object_id = selected_objects_by_live_id.getptr(node_id);
		if (!object_id) {
			continue;
		}
		const int *entity_index = node_entity_indices.getptr(*object_id);
		if (!entity_index) {
			continue;
		}
		Dictionary node_entity = entities[*entity_index];
		node_entity["selected"] = true;
		node_entity["primary"] = inspected_object && inspected_object->get_instance_id() == *object_id;
		bool properties_truncated = false;
		node_entity["properties"] = _capture_properties(ObjectDB::get_instance(*object_id), MAX_INSPECTOR_PROPERTIES, node_scene_ids.get(*object_id), p_revisions, properties_truncated, total_inspector_bytes);
		node_entity["properties_truncated"] = properties_truncated;
		truncated = truncated || properties_truncated;
		entities[*entity_index] = node_entity;
		selected_node_ids.push_back(node_id);
	}
	if (projected_selection_count < total_selection_count) {
		truncated = true;
	}
	for (int entity_index = 1; entity_index < entities.size(); entity_index++) {
		Dictionary entity = entities[entity_index];
		if (entity.get("kind", String()) == "scene" && entity.get("entity_id", String()) == current_scene_id) {
			entity["selected_node_ids"] = selected_node_ids;
			entities[entity_index] = entity;
			break;
		}
	}

	if (p_full_live_context) {
		Dictionary inspector_entity;
		inspector_entity["kind"] = "inspector_state";
		inspector_entity["entity_id"] = _make_opaque_id("inspector:", "godot-codex-inspector/v1", p_editor_session_id);
		inspector_entity["has_object"] = inspected_object != nullptr;
		if (inspected_object) {
			const ObjectID inspected_id = inspected_object->get_instance_id();
			const String *inspected_node_id = node_live_ids.getptr(inspected_id);
			if (inspected_node_id) {
				inspector_entity["object_kind"] = "node";
				inspector_entity["object_id"] = *inspected_node_id;
				inspector_entity["scene_id"] = node_scene_ids.get(inspected_id);
				inspector_entity["node_path"] = node_paths.get(inspected_id);
			} else if (Resource *resource = Object::cast_to<Resource>(inspected_object)) {
				inspector_entity["object_kind"] = "resource";
				const String path = resource->get_path();
				const String identity = path.begins_with("res://") ? path : String(resource->get_class()) + "\n" + String::num_uint64(inspected_id);
				inspector_entity["object_id"] = _make_opaque_id("object:", "godot-codex-object/v1\n" + p_editor_session_id, identity);
				inspector_entity["resource_path"] = path.begins_with("res://") ? path.get_slice("::", 0) : String();
			} else {
				inspector_entity["object_kind"] = "object";
				inspector_entity["object_id"] = _make_opaque_id("object:", "godot-codex-object/v1\n" + p_editor_session_id, String(inspected_object->get_class()) + "\n" + String::num_uint64(inspected_id));
			}
			inspector_entity["godot_type"] = inspected_object->get_class();
			bool inspector_truncated = false;
			inspector_entity["properties"] = _capture_properties(inspected_object, MAX_INSPECTOR_PROPERTIES, inspector_entity.get("scene_id", String()), p_revisions, inspector_truncated, total_inspector_bytes);
			inspector_entity["properties_truncated"] = inspector_truncated;
			truncated = truncated || inspector_truncated;
		}
		entities.push_back(inspector_entity);

		ScriptEditor *script_editor = ScriptEditor::get_singleton();
		Array open_script_ids;
		String active_script_id;
		bool scripts_truncated = false;
		if (script_editor) {
			const Vector<Ref<Script>> open_scripts = script_editor->get_open_scripts();
			const PackedStringArray unsaved_files = script_editor->get_unsaved_files();
			HashSet<String> unsaved_paths;
			for (const String &path : unsaved_files) {
				unsaved_paths.insert(path);
			}
			ScriptEditorBase *current_editor = script_editor->get_current_editor();
			const Ref<Resource> current_resource = current_editor ? current_editor->get_edited_resource() : Ref<Resource>();
			const int script_limit = MIN(open_scripts.size(), MAX_OPEN_SCRIPTS);
			for (int script_index = 0; script_index < script_limit; script_index++) {
				const Ref<Script> &script = open_scripts[script_index];
				if (script.is_null()) {
					continue;
				}
				const String raw_path = script->get_path();
				const bool saved_resource = raw_path.begins_with("res://") && raw_path.find("::") < 0;
				const String public_path = saved_resource ? raw_path : String();
				const String identity = saved_resource ? raw_path : String(script->get_class()) + "\n" + raw_path + "\n" + String::num_uint64(script->get_instance_id());
				const String script_id = _make_script_id(p_editor_session_id, identity);
				const bool active = current_resource.is_valid() && current_resource.ptr() == script.ptr();
				const String editor_hash = script->get_source_code().sha256_text();
				const String disk_hash = saved_resource ? FileAccess::get_sha256(raw_path) : String();
				const bool dirty = unsaved_paths.has(raw_path) || (saved_resource && (disk_hash.is_empty() || disk_hash != editor_hash));

				Dictionary script_entity;
				script_entity["kind"] = "script_tab";
				script_entity["entity_id"] = script_id;
				script_entity["identity_scope"] = "editor_session";
				script_entity["tab_index"] = script_index;
				script_entity["path"] = public_path;
				script_entity["built_in"] = !saved_resource;
				ScriptLanguage *language = script->get_language();
				script_entity["language"] = language ? language->get_name() : String();
				script_entity["active"] = active;
				script_entity["dirty"] = dirty;
				script_entity["disk_content_sha256"] = disk_hash;
				script_entity["editor_content_sha256"] = editor_hash;
				script_entity["source_text_included"] = false;
				Array selections;
				bool selections_truncated = false;
				if (active) {
					active_script_id = script_id;
					TextEditorBase *text_editor = Object::cast_to<TextEditorBase>(current_editor);
					CodeEdit *code_edit = text_editor && text_editor->get_code_editor() ? text_editor->get_code_editor()->get_text_editor() : nullptr;
					if (code_edit) {
						const int caret_limit = MIN(code_edit->get_caret_count(), MAX_SCRIPT_SELECTIONS);
						for (int caret_index = 0; caret_index < caret_limit; caret_index++) {
							Dictionary range;
							range["caret_index"] = caret_index;
							if (code_edit->has_selection(caret_index)) {
								range["start_line"] = code_edit->get_selection_from_line(caret_index) + 1;
								range["start_column"] = code_edit->get_selection_from_column(caret_index) + 1;
								range["end_line"] = code_edit->get_selection_to_line(caret_index) + 1;
								range["end_column"] = code_edit->get_selection_to_column(caret_index) + 1;
								range["empty"] = false;
							} else {
								const int line = code_edit->get_caret_line(caret_index) + 1;
								const int column = code_edit->get_caret_column(caret_index) + 1;
								range["start_line"] = line;
								range["start_column"] = column;
								range["end_line"] = line;
								range["end_column"] = column;
								range["empty"] = true;
							}
							selections.push_back(range);
						}
						selections_truncated = code_edit->get_caret_count() > MAX_SCRIPT_SELECTIONS;
					}
				}
				script_entity["selections"] = selections;
				script_entity["selections_truncated"] = selections_truncated;
				truncated = truncated || selections_truncated;
				open_script_ids.push_back(script_id);
				entities.push_back(script_entity);
			}
			scripts_truncated = open_scripts.size() > MAX_OPEN_SCRIPTS;
			truncated = truncated || scripts_truncated;
		}
		Dictionary script_state;
		script_state["kind"] = "script_state";
		script_state["entity_id"] = _make_opaque_id("scripts:", "godot-codex-script-state/v1", p_editor_session_id);
		script_state["open_script_ids"] = open_script_ids;
		script_state["active_script_id"] = active_script_id;
		script_state["open_scripts_truncated"] = scripts_truncated;
		entities.push_back(script_state);

		Dictionary history_state;
		history_state["kind"] = "history_state";
		history_state["entity_id"] = _make_opaque_id("histories:", "godot-codex-history-state/v1", p_editor_session_id);
		Array history_ids;
		if (undo_redo) {
			Vector<int> native_history_ids;
			HashSet<int> unique_history_ids;
			native_history_ids.push_back(EditorUndoRedoManager::GLOBAL_HISTORY);
			unique_history_ids.insert(EditorUndoRedoManager::GLOBAL_HISTORY);
			for (int scene_index = 0; scene_index < MIN(scene_count, MAX_OPEN_SCENES); scene_index++) {
				const int native_history_id = editor_data.get_scene_history_id(scene_index);
				if (!unique_history_ids.has(native_history_id)) {
					native_history_ids.push_back(native_history_id);
					unique_history_ids.insert(native_history_id);
				}
			}
			for (int native_history_id : native_history_ids) {
				if (!undo_redo->has_history(native_history_id)) {
					continue;
				}
				UndoRedo *native_history = undo_redo->get_history_undo_redo(native_history_id);
				if (!native_history) {
					continue;
				}
				const String history_id = _make_history_id(p_editor_session_id, native_history_id);
				Dictionary history_entity;
				history_entity["kind"] = "editor_history";
				history_entity["entity_id"] = history_id;
				history_entity["identity_scope"] = "editor_session";
				history_entity["scope"] = native_history_id == EditorUndoRedoManager::GLOBAL_HISTORY ? "global" : "scene";
				history_entity["native_version"] = (int64_t)native_history->get_version();
				const int action_count = native_history->get_history_count();
				const int current_action = native_history->get_current_action();
				const int described_action = current_action >= 0 ? current_action : (native_history->has_redo() ? current_action + 1 : -1);
				history_entity["action_count"] = action_count;
				history_entity["current_action_index"] = current_action;
				history_entity["action_name"] = _bounded_identity(described_action >= 0 && described_action < action_count ? native_history->get_action_name(described_action) : String(), truncated);
				history_entity["saved_state"] = undo_redo->is_history_unsaved(native_history_id) ? "unsaved" : "saved";
				history_entity["can_undo"] = native_history->has_undo();
				history_entity["can_redo"] = native_history->has_redo();
				const Dictionary transition = p_history_transitions.get(String::num_int64(native_history_id), Dictionary());
				history_entity["transition_kind"] = transition.get("transition_kind", "unknown");
				history_entity["last_operation_seq"] = transition.get("last_operation_seq", p_revisions.get("operation_seq", 0));
				Dictionary operation;
				operation["kind"] = "opaque";
				operation["omitted_reason"] = "native_operation_payload_unavailable";
				history_entity["last_operation"] = operation;
				if (native_history_id != EditorUndoRedoManager::GLOBAL_HISTORY) {
					for (int scene_index = 0; scene_index < MIN(scene_count, MAX_OPEN_SCENES); scene_index++) {
						if (editor_data.get_scene_history_id(scene_index) == native_history_id) {
							Node *scene_root = editor_data.get_edited_scene_root(scene_index);
							if (scene_root) {
								history_entity["scene_id"] = make_scene_id(p_editor_session_id, scene_root);
							}
							break;
						}
					}
				}
				history_ids.push_back(history_id);
				entities.push_back(history_entity);
			}
		}
		history_state["history_ids"] = history_ids;
		history_state["last_operation_seq"] = p_revisions.get("operation_seq", 0);
		entities.push_back(history_state);

		Array diagnostic_ids;
		int diagnostic_bytes = 0;
		int omitted_diagnostics = 0;
		EditorLog *editor_log = EditorNode::get_log();
		if (editor_log) {
			const Array output_messages = editor_log->get_messages_snapshot(MAX_DIAGNOSTICS);
			omitted_diagnostics = MAX(0, editor_log->get_message_count() - output_messages.size());
			for (int message_index = 0; message_index < output_messages.size(); message_index++) {
				const Dictionary raw_message = output_messages[message_index];
				bool redacted = false;
				const String message = _redact_output_message(raw_message.get("text", String()), redacted);
				Dictionary diagnostic;
				diagnostic["kind"] = "editor_diagnostic";
				const int64_t output_seq = raw_message.get("output_seq", 0);
				const String diagnostic_id = _make_opaque_id("diagnostic:", "godot-codex-output/v1\n" + p_editor_session_id, String::num_int64(output_seq));
				diagnostic["entity_id"] = diagnostic_id;
				diagnostic["source"] = "editor_output";
				const int message_type = raw_message.get("type", 0);
				diagnostic["severity"] = message_type == EditorLog::MSG_TYPE_ERROR ? "error" : (message_type == EditorLog::MSG_TYPE_WARNING ? "warning" : "info");
				diagnostic["message"] = message;
				diagnostic["repeat_count"] = raw_message.get("count", 1);
				diagnostic["output_seq"] = output_seq;
				diagnostic["redacted"] = redacted;
				diagnostic["runtime_semantics_inferred"] = false;
				const int encoded_bytes = JSON::stringify(diagnostic, "", true, true).utf8().length();
				if (diagnostic_bytes + encoded_bytes > MAX_DIAGNOSTIC_BYTES) {
					omitted_diagnostics += output_messages.size() - message_index;
					truncated = true;
					break;
				}
				diagnostic_bytes += encoded_bytes;
				diagnostic_ids.push_back(diagnostic_id);
				entities.push_back(diagnostic);
			}
		}
		Dictionary diagnostic_state;
		diagnostic_state["kind"] = "diagnostic_state";
		diagnostic_state["entity_id"] = _make_opaque_id("diagnostics:", "godot-codex-diagnostic-state/v1", p_editor_session_id);
		diagnostic_state["diagnostic_ids"] = diagnostic_ids;
		diagnostic_state["omitted_count"] = omitted_diagnostics;
		diagnostic_state["encoded_bytes"] = diagnostic_bytes;
		entities.push_back(diagnostic_state);

		Dictionary viewport_entity;
		viewport_entity["kind"] = "viewport_state";
		viewport_entity["entity_id"] = _make_opaque_id("viewport:", "godot-codex-viewport/v1", p_editor_session_id);
		EditorMainScreen *main_screen = EditorNode::get_editor_main_screen();
		const int selected_screen = main_screen ? main_screen->get_selected_index() : -1;
		static const char *screen_names[] = { "2d", "3d", "script", "game", "asset_library" };
		viewport_entity["active_kind"] = selected_screen >= 0 && selected_screen < 5 ? screen_names[selected_screen] : "unknown";
		viewport_entity["visible"] = main_screen && main_screen->is_visible_in_tree();
		if (main_screen && main_screen->get_control()) {
			const Size2 logical_size = main_screen->get_control()->get_size();
			viewport_entity["logical_size"] = Vector2i((int)logical_size.x, (int)logical_size.y);
			viewport_entity["scale"] = MAX(1.0, (double)EDSCALE);
		}
		if (CanvasItemEditor::get_singleton()) {
			const Dictionary state_2d = CanvasItemEditor::get_singleton()->get_state();
			Dictionary camera_2d;
			camera_2d["zoom"] = state_2d.get("zoom", 1.0);
			camera_2d["offset"] = state_2d.get("ofs", Vector2());
			viewport_entity["camera_2d"] = _project_variant(camera_2d, 0, truncated);
		}
		if (Node3DEditor::get_singleton()) {
			const Dictionary state_3d = Node3DEditor::get_singleton()->get_state();
			Dictionary projection_3d;
			projection_3d["fov"] = state_3d.get("fov", 0.0);
			projection_3d["znear"] = state_3d.get("znear", 0.0);
			projection_3d["zfar"] = state_3d.get("zfar", 0.0);
			projection_3d["viewport_mode"] = state_3d.get("viewport_mode", 0);
			projection_3d["viewports"] = state_3d.get("viewports", Array());
			viewport_entity["camera_3d"] = _project_variant(projection_3d, 0, truncated);
		}
		viewport_entity["screenshot_available"] = false;
		viewport_entity["screenshot_omitted_reason"] = "deferred_to_sprint_8";
		entities.push_back(viewport_entity);
	}

	editor_entity["current_scene_id"] = current_scene_id;
	editor_entity["current_scene_dirty"] = current_scene_dirty;
	editor_entity["selection_count"] = total_selection_count;
	editor_entity["projected_selection_count"] = selected_node_ids.size();
	editor_entity["selected_node_ids"] = selected_node_ids;
	editor_entity["open_scene_ids"] = open_scene_ids;
	editor_entity["open_scenes_truncated"] = p_full_live_context && scene_count > MAX_OPEN_SCENES;
	editor_entity["inspector_object_id"] = inspected_object ? (node_live_ids.has(inspected_object->get_instance_id()) ? Variant(*node_live_ids.getptr(inspected_object->get_instance_id())) : Variant()) : Variant();
	entities[0] = editor_entity;
	r_snapshot["project_id"] = p_project_id;
	r_snapshot["editor_session_id"] = p_editor_session_id;
	r_snapshot["revision_vector"] = p_revisions;
	r_snapshot["current_scene_id"] = current_scene_id;
	r_snapshot["dirty"] = current_scene_dirty;
	r_snapshot["selected_node_ids"] = selected_node_ids;
	r_snapshot["entities"] = entities;
	r_snapshot["truncated"] = truncated;
	return OK;
}
