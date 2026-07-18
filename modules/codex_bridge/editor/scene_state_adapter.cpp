/**************************************************************************/
/*  scene_state_adapter.cpp                                               */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
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

#include "scene_state_adapter.h"

#include "bounded_variant_projector.h"
#include "bridge_revision_clock.h"

#include "core/config/project_settings.h"
#include "core/crypto/crypto_core.h"
#include "core/input/input_event.h"
#include "core/io/file_access.h"
#include "core/io/json.h"
#include "core/io/resource_loader.h"
#include "core/io/resource_uid.h"
#include "core/object/property_info.h"
#include "core/object/worker_thread_pool.h"
#include "core/os/os.h"
#include "core/os/thread.h"
#include "editor/file_system/editor_file_system.h"
#include "scene/resources/animation_library.h"
#include "scene/resources/packed_scene.h"

#include "modules/codex_bridge/protocol/bridge_crypto.h"

bool SceneStateAdapter::_is_scene_path(const String &p_path) {
	return p_path.begins_with("res://") && (p_path.ends_with(".tscn") || p_path.ends_with(".scn")) && p_path.utf8().length() <= (int)MAX_PATH_BYTES && !p_path.contains("\\") && !p_path.contains("\0") && !p_path.contains("/../");
}

Dictionary SceneStateAdapter::_make_resource_ref(const String &p_path) {
	Dictionary result;
	if (p_path.begins_with("res://")) {
		const ResourceUID::ID uid = ResourceUID::get_singleton()->get_path_id(p_path);
		if (uid != ResourceUID::INVALID_ID && ResourceUID::get_singleton()->has_id(uid)) {
			result["uid"] = ResourceUID::get_singleton()->id_to_text(uid);
			return result;
		}
	}
	result["uid_missing"] = true;
	result["path"] = p_path;
	return result;
}

String SceneStateAdapter::_make_snapshot_id() {
	PackedByteArray random;
	if (BridgeCrypto::random_bytes(16, random) != OK) {
		return "snapshot:00000000000000000000000000000000";
	}
	return "snapshot:" + BridgeCrypto::bytes_to_lower_hex(random);
}

String SceneStateAdapter::_sha256_hex(const String &p_value) {
	const CharString bytes = p_value.utf8();
	PackedByteArray digest;
	digest.resize(32);
	if (CryptoCore::sha256(reinterpret_cast<const uint8_t *>(bytes.get_data()), bytes.length(), digest.ptrw()) != OK) {
		return String();
	}
	return BridgeCrypto::bytes_to_lower_hex(digest);
}

String SceneStateAdapter::_canonical_node_path(const String &p_path) {
	if (p_path == ".") {
		return p_path;
	}
	return p_path.trim_prefix("./");
}

bool SceneStateAdapter::_is_safe_node_path(const String &p_path, bool p_allow_empty) {
	if (p_path.is_empty()) {
		return p_allow_empty;
	}
	if (p_path == ".") {
		return true;
	}
	return !p_path.begins_with("/") && !p_path.contains("\\") && !p_path.contains(":") && !p_path.contains("\0") && !p_path.contains("//") && !p_path.contains("../") && p_path.utf8().length() <= 2048;
}

void SceneStateAdapter::_collect_resource_ownership(const Variant &p_value, const String &p_path, int p_depth, HashMap<ObjectID, RBSet<String>> &r_ownership, Vector<Ref<Resource>> &r_subresources) {
	if (p_depth > BoundedVariantProjector::MAX_DEPTH) {
		return;
	}
	if (p_value.get_type() == Variant::OBJECT) {
		const Ref<Resource> resource = p_value;
		if (resource.is_valid()) {
			RBSet<String> *paths = r_ownership.getptr(resource->get_instance_id());
			const bool first_observation = paths == nullptr;
			if (!paths) {
				r_ownership.insert(resource->get_instance_id(), RBSet<String>());
				paths = r_ownership.getptr(resource->get_instance_id());
			}
			paths->insert(p_path);
			if (first_observation && resource->get_path().contains("::")) {
				r_subresources.push_back(resource);
			}
		}
		return;
	}
	if (p_value.get_type() == Variant::ARRAY) {
		const Array values = p_value;
		const int count = MIN(values.size(), BoundedVariantProjector::MAX_CONTAINER_ITEMS);
		for (int index = 0; index < count; index++) {
			_collect_resource_ownership(values[index], p_path + "/" + String::num_int64(index), p_depth + 1, r_ownership, r_subresources);
		}
		return;
	}
	if (p_value.get_type() == Variant::DICTIONARY) {
		const Dictionary values = p_value;
		const Array keys = values.keys();
		const int count = MIN(keys.size(), BoundedVariantProjector::MAX_CONTAINER_ITEMS);
		for (int index = 0; index < count; index++) {
			String key = keys[index].stringify().replace("/", "_").left(256);
			_collect_resource_ownership(values[keys[index]], p_path + "/" + key, p_depth + 1, r_ownership, r_subresources);
		}
	}
}

bool SceneStateAdapter::_append_diagnostic(Array &r_diagnostics, const Dictionary &p_diagnostic) {
	if (++observed_diagnostic_count > MAX_DIAGNOSTICS) {
		refresh_limit_exceeded = true;
		return false;
	}
	r_diagnostics.push_back(p_diagnostic);
	return true;
}

void SceneStateAdapter::_reset_active_scene() {
	active_path.clear();
	active_load_requested = false;
	active_scene.unref();
	active_state.unref();
	active_record = CatalogRecord();
	active_nodes.clear();
	active_connections.clear();
	active_editable_instances.clear();
	active_subresources.clear();
	active_animation_tracks.clear();
	active_node_paths.clear();
	active_resource_ownership.clear();
	active_node_index = 0;
	active_property_index = 0;
	active_node.clear();
	active_node_properties.clear();
	active_deferred_properties.clear();
	active_connection_index = 0;
	active_subresource_values.clear();
	active_subresource_index = 0;
}

bool SceneStateAdapter::_collect_one_path() {
	while (!directory_stack.is_empty()) {
		DirectoryCursor &cursor = directory_stack.write[directory_stack.size() - 1];
		if (cursor.file_index < cursor.directory->get_file_count()) {
			const String path = cursor.directory->get_file_path(cursor.file_index++);
			if (!_is_scene_path(path)) {
				return true;
			}
			if (scene_paths.size() >= (int)MAX_SCENES) {
				refresh_limit_exceeded = true;
				return false;
			}
			scene_paths.push_back(path);
			return true;
		}
		if (cursor.subdirectory_index < cursor.directory->get_subdir_count()) {
			DirectoryCursor child;
			child.directory = cursor.directory->get_subdir(cursor.subdirectory_index++);
			directory_stack.push_back(child);
			continue;
		}
		directory_stack.resize(directory_stack.size() - 1);
	}
	scene_paths.sort();
	refresh_phase = REFRESH_BEGIN_SCENE;
	return false;
}

bool SceneStateAdapter::_begin_active_scene() {
	if (active_path.is_empty()) {
		if (scene_path_index >= scene_paths.size()) {
			refresh_phase = REFRESH_PROJECT_CONTEXT;
			return false;
		}
		active_path = scene_paths[scene_path_index++];
		const Error request_error = ResourceLoader::load_threaded_request(active_path, "PackedScene", true, ResourceFormatLoader::CACHE_MODE_REUSE);
		if (request_error != OK) {
			Dictionary diagnostic;
			diagnostic["code"] = "scene_load_failed";
			diagnostic["subject"] = active_path;
			diagnostic["scene_graph_revision"] = (int64_t)target_scene_graph_revision;
			_append_diagnostic(observed_diagnostics, diagnostic);
			refresh_phase = REFRESH_SCENE_FINISH;
			return true;
		}
		active_load_requested = true;
		return true;
	}
	ERR_FAIL_COND_V(!active_load_requested, false);
	const ResourceLoader::ThreadLoadStatus load_status = ResourceLoader::load_threaded_get_status(active_path);
	if (load_status == ResourceLoader::THREAD_LOAD_IN_PROGRESS) {
		return false;
	}
	if (load_status == ResourceLoader::THREAD_LOAD_LOADED) {
		active_scene = ResourceLoader::load_threaded_get(active_path);
	}
	active_load_requested = false;
	if (active_scene.is_null() || !active_scene->can_instantiate()) {
		Dictionary diagnostic;
		diagnostic["code"] = "scene_load_failed";
		diagnostic["subject"] = active_path;
		diagnostic["scene_graph_revision"] = (int64_t)target_scene_graph_revision;
		_append_diagnostic(observed_diagnostics, diagnostic);
		refresh_phase = REFRESH_SCENE_FINISH;
		return true;
	}
	active_state = active_scene->get_state();
	if (active_state.is_null()) {
		Dictionary diagnostic;
		diagnostic["code"] = "scene_load_failed";
		diagnostic["subject"] = active_path;
		diagnostic["detail"] = "PackedScene did not expose a SceneState.";
		diagnostic["scene_graph_revision"] = (int64_t)target_scene_graph_revision;
		_append_diagnostic(observed_diagnostics, diagnostic);
		refresh_phase = REFRESH_SCENE_FINISH;
		return true;
	}
	active_subresource_values = active_state->get_sub_resources();
	for (const Ref<Resource> &resource : active_subresource_values) {
		if (resource.is_valid()) {
			active_resource_ownership.insert(resource->get_instance_id(), RBSet<String>());
		}
	}

	Dictionary observation;
	const Dictionary scene_ref = _make_resource_ref(active_path);
	const String content_hash = FileAccess::get_sha256(active_path);
	if (content_hash.length() != 64) {
		Dictionary diagnostic;
		diagnostic["code"] = "scene_load_failed";
		diagnostic["subject"] = active_path;
		diagnostic["detail"] = "Scene content could not be hashed.";
		diagnostic["scene_graph_revision"] = (int64_t)target_scene_graph_revision;
		_append_diagnostic(observed_diagnostics, diagnostic);
		refresh_phase = REFRESH_SCENE_FINISH;
		return true;
	}
	observation["scene_ref"] = scene_ref;
	observation["path"] = active_path;
	observation["content_generation"] = "sha256:" + content_hash;
	observation["identity_scope"] = scene_ref.has("uid") ? "persistent" : "content_revision";
	const Ref<SceneState> base_state = active_state->get_base_scene_state();
	const String base_path = base_state.is_valid() ? base_state->get_path() : String();
	observation["base_scene_ref"] = _is_scene_path(base_path) ? Variant(_make_resource_ref(base_path)) : Variant();
	observation["authority"] = "packed_scene_state";
	observation["resource_revision"] = (int64_t)(revision_clock ? revision_clock->get_resource_revision() : 1);
	observation["scene_graph_revision"] = (int64_t)target_scene_graph_revision;
	observation["source_complete"] = true;
	active_record.value = observation;
	active_record.facts_checksum = content_hash;
	if (!scene_ref.has("uid")) {
		Dictionary diagnostic;
		diagnostic["code"] = "weak_scene_identity";
		diagnostic["subject"] = active_path;
		diagnostic["scene_graph_revision"] = (int64_t)target_scene_graph_revision;
		_append_diagnostic(active_record.diagnostics, diagnostic);
	}
	refresh_phase = REFRESH_NODE_BEGIN;
	return true;
}

bool SceneStateAdapter::_begin_active_node() {
	if (active_node_index >= active_state->get_node_count()) {
		refresh_phase = REFRESH_CONNECTION;
		return false;
	}
	if (++observed_node_count > MAX_NODES) {
		refresh_limit_exceeded = true;
		return false;
	}
	const int index = active_node_index;
	const String path = _canonical_node_path(String(active_state->get_node_path(index)));
	const bool root = path == ".";
	const String parent = root ? String() : _canonical_node_path(String(active_state->get_node_path(index, true)));
	const String owner = _canonical_node_path(String(active_state->get_node_owner_path(index)));
	const String name = String(active_state->get_node_name(index));
	const String godot_type = String(active_state->get_node_type(index));
	if (!_is_safe_node_path(path) || !_is_safe_node_path(parent, true) || !_is_safe_node_path(owner, true) || name.is_empty() || name.length() > 512 || godot_type.length() > 256 || active_state->get_node_property_count(index) > 16384) {
		refresh_limit_exceeded = true;
		return false;
	}
	active_node_paths.insert(path);
	active_node["node_path"] = path;
	active_node["parent_path"] = parent.is_empty() ? Variant() : Variant(parent);
	active_node["owner_path"] = owner.is_empty() ? Variant() : Variant(owner);
	active_node["name"] = name;
	active_node["godot_type"] = godot_type;
	active_node["node_index"] = active_state->get_node_index(index);
	const int32_t unique_id = active_state->get_node_unique_id(index);
	active_node["unique_scene_id"] = unique_id > 0 ? Variant(unique_id) : Variant();
	active_node["identity_scope"] = unique_id > 0 ? "persistent" : "content_revision";
	active_node["owned"] = root || !owner.is_empty();
	active_node["internal"] = !root && owner.is_empty();
	const Ref<PackedScene> instance = root ? Ref<PackedScene>() : active_state->get_node_instance(index);
	const String instance_path = instance.is_valid() ? instance->get_path().get_slice("::", 0) : String();
	active_node["instance"] = _is_scene_path(instance_path) ? Variant(_make_resource_ref(instance_path)) : Variant();
	const String placeholder = active_state->get_node_instance_placeholder(index);
	if (!placeholder.is_empty() && !_is_scene_path(placeholder)) {
		refresh_limit_exceeded = true;
		return false;
	}
	active_node["instance_placeholder"] = placeholder.is_empty() ? Variant() : Variant(placeholder);
	Vector<StringName> raw_groups = active_state->get_node_groups(index);
	if (raw_groups.size() > 1024) {
		refresh_limit_exceeded = true;
		return false;
	}
	Vector<String> groups;
	HashSet<String> unique_groups;
	for (const StringName &group : raw_groups) {
		const String value = String(group);
		if (value.is_empty() || value.length() > 512 || unique_groups.has(value)) {
			refresh_limit_exceeded = true;
			return false;
		}
		unique_groups.insert(value);
		groups.push_back(value);
	}
	groups.sort();
	Array group_values;
	for (const String &group : groups) {
		group_values.push_back(group);
	}
	active_node["groups"] = group_values;
	active_node["attached_script"] = Variant();
	active_property_index = 0;
	active_node_properties.clear();
	active_deferred_properties = active_state->get_node_deferred_nodepath_properties(index);
	refresh_phase = REFRESH_NODE_PROPERTY;
	return true;
}

bool SceneStateAdapter::_observe_active_property() {
	const int property_count = active_state->get_node_property_count(active_node_index);
	if (active_property_index >= property_count) {
		refresh_phase = REFRESH_NODE_FINISH;
		return false;
	}
	if (++observed_property_count > MAX_PROPERTIES) {
		refresh_limit_exceeded = true;
		return false;
	}
	const String property_name = String(active_state->get_node_property_name(active_node_index, active_property_index));
	if (property_name.is_empty() || property_name.length() > 512) {
		refresh_limit_exceeded = true;
		return false;
	}
	const Variant value = active_state->get_node_property_value(active_node_index, active_property_index);
	_collect_resource_ownership(value, String(active_node["node_path"]) + ":" + property_name, 0, active_resource_ownership, active_subresource_values);
	bool deferred = false;
	for (const String &candidate : active_deferred_properties) {
		if (candidate == property_name) {
			deferred = true;
			break;
		}
	}
	Dictionary property;
	property["name"] = property_name;
	property["value"] = BoundedVariantProjector::project_typed(value);
	property["deferred_node_path"] = deferred;
	active_node_properties.push_back(property);
	if (property_name == "script" && value.get_type() == Variant::OBJECT) {
		const Ref<Resource> script = value;
		const String script_path = script.is_valid() ? script->get_path().get_slice("::", 0) : String();
		if (script_path.begins_with("res://")) {
			active_node["attached_script"] = _make_resource_ref(script_path);
		}
	}
	active_property_index++;
	return true;
}

bool SceneStateAdapter::_finish_active_node() {
	active_node["properties"] = active_node_properties.duplicate(true);
	if (String(active_node["identity_scope"]) == "content_revision") {
		Dictionary diagnostic;
		diagnostic["code"] = "weak_node_identity";
		diagnostic["subject"] = active_path + "#" + String(active_node["node_path"]);
		diagnostic["scene_graph_revision"] = (int64_t)target_scene_graph_revision;
		_append_diagnostic(active_record.diagnostics, diagnostic);
	}
	active_nodes.push_back(active_node.duplicate(true));
	active_node.clear();
	active_node_properties.clear();
	active_node_index++;
	refresh_phase = REFRESH_NODE_BEGIN;
	return true;
}

bool SceneStateAdapter::_observe_connection() {
	if (active_connection_index >= active_state->get_connection_count()) {
		refresh_phase = REFRESH_SUBRESOURCE;
		return false;
	}
	if (++observed_relation_count > MAX_RELATIONS) {
		refresh_limit_exceeded = true;
		return false;
	}
	Dictionary connection;
	connection["emitter"] = _canonical_node_path(String(active_state->get_connection_source(active_connection_index)));
	connection["signal"] = String(active_state->get_connection_signal(active_connection_index));
	connection["receiver"] = _canonical_node_path(String(active_state->get_connection_target(active_connection_index)));
	connection["method"] = String(active_state->get_connection_method(active_connection_index));
	const Array raw_binds = active_state->get_connection_binds(active_connection_index);
	if (!_is_safe_node_path(connection["emitter"]) || !_is_safe_node_path(connection["receiver"]) || String(connection["signal"]).is_empty() || String(connection["signal"]).length() > 512 || String(connection["method"]).is_empty() || String(connection["method"]).length() > 512 || raw_binds.size() > 1024) {
		refresh_limit_exceeded = true;
		return false;
	}
	const int flags = active_state->get_connection_flags(active_connection_index);
	const int unbinds = active_state->get_connection_unbinds(active_connection_index);
	if (flags < 0 || unbinds < 0 || unbinds > 1024) {
		refresh_limit_exceeded = true;
		return false;
	}
	connection["flags"] = flags;
	connection["unbinds"] = unbinds;
	Array binds;
	for (const Variant &bind : raw_binds) {
		binds.push_back(BoundedVariantProjector::project_typed(bind));
	}
	connection["binds"] = binds;
	active_connections.push_back(connection);
	active_connection_index++;
	return true;
}

bool SceneStateAdapter::_observe_subresource() {
	if (active_subresource_index >= active_subresource_values.size()) {
		refresh_phase = REFRESH_SCENE_FINISH;
		return false;
	}
	if (++observed_relation_count > MAX_RELATIONS) {
		refresh_limit_exceeded = true;
		return false;
	}
	const Ref<Resource> resource = active_subresource_values[active_subresource_index++];
	if (resource.is_null()) {
		return true;
	}
	const String unique_id = resource->get_scene_unique_id();
	const String godot_type = resource->get_class();
	if (unique_id.length() > 256 || godot_type.is_empty() || godot_type.length() > 256) {
		refresh_limit_exceeded = true;
		return false;
	}
	Dictionary observation;
	observation["scene_unique_id"] = unique_id.is_empty() ? Variant() : Variant(unique_id);
	observation["godot_type"] = godot_type;
	observation["identity_scope"] = unique_id.is_empty() ? "content_revision" : "persistent";
	Array ownership_paths;
	const RBSet<String> *known_paths = active_resource_ownership.getptr(resource->get_instance_id());
	if (known_paths) {
		for (const String &path : *known_paths) {
			ownership_paths.push_back(path);
		}
	}
	if (ownership_paths.is_empty()) {
		ownership_paths.push_back("scene:subresource/" + (unique_id.is_empty() ? String::num_int64(active_subresource_index - 1) : unique_id));
	}
	if (ownership_paths.size() > 4096) {
		refresh_limit_exceeded = true;
		return false;
	}
	observation["ownership_paths"] = ownership_paths;
	active_subresources.push_back(observation);
	if (unique_id.is_empty()) {
		Dictionary diagnostic;
		diagnostic["code"] = "weak_subresource_identity";
		diagnostic["subject"] = active_path + "::" + resource->get_class();
		diagnostic["scene_graph_revision"] = (int64_t)target_scene_graph_revision;
		_append_diagnostic(active_record.diagnostics, diagnostic);
	}

	// SceneState exposes the subresources directly referenced by node properties,
	// but nested built-ins (for example a Gradient owned by a
	// GradientTexture1D, or an Animation owned by an AnimationLibrary) must be
	// reached through the owning Resource's serialized properties. Keep this
	// traversal bounded and append newly discovered built-ins to the same work
	// queue so every nested resource receives its own identity and ownership path.
	List<PropertyInfo> properties;
	resource->get_property_list(&properties);
	int stored_property_count = 0;
	const String ownership_root = ownership_paths[0];
	for (const PropertyInfo &property : properties) {
		if (!(property.usage & PROPERTY_USAGE_STORAGE)) {
			continue;
		}
		if (++stored_property_count > BoundedVariantProjector::MAX_CONTAINER_ITEMS) {
			refresh_limit_exceeded = true;
			return false;
		}
		const String property_name = String(property.name);
		if (property_name.is_empty() || property_name.length() > 512) {
			refresh_limit_exceeded = true;
			return false;
		}
		_collect_resource_ownership(resource->get(property.name), ownership_root + ":" + property_name, 1, active_resource_ownership, active_subresource_values);
	}

	AnimationLibrary *library = Object::cast_to<AnimationLibrary>(resource.ptr());
	if (!library) {
		return true;
	}
	String mixer = ".";
	String library_name;
	if (known_paths && known_paths->front()) {
		const String ownership = known_paths->front()->get();
		mixer = ownership.get_slice(":", 0);
		library_name = ownership.get_slice("/", ownership.get_slice_count("/") - 1);
	}
	LocalVector<StringName> animation_names;
	library->get_animation_list(&animation_names);
	for (const StringName &animation_name : animation_names) {
		const Ref<Animation> animation = library->get_animation(animation_name);
		if (animation.is_null()) {
			continue;
		}
		for (int track_index = 0; track_index < animation->get_track_count(); track_index++) {
			Dictionary track;
			const NodePath track_path = animation->track_get_path(track_index);
			const String target_path = String(track_path.get_concatenated_names());
			if (!_is_safe_node_path(mixer) || library_name.length() > 512 || String(animation_name).is_empty() || String(animation_name).length() > 512 || String(track_path).is_empty() || String(track_path).length() > 2048) {
				refresh_limit_exceeded = true;
				return false;
			}
			track["mixer"] = mixer;
			track["library"] = library_name;
			track["animation"] = String(animation_name);
			track["track_index"] = track_index;
			track["node_path"] = String(track_path);
			track["resolution"] = active_node_paths.has(target_path) ? "resolved" : "unresolved";
			active_animation_tracks.push_back(track);
		}
	}
	return true;
}

bool SceneStateAdapter::_finish_active_scene() {
	if (active_scene.is_valid() && active_state.is_valid() && !active_record.value.is_empty()) {
		for (const NodePath &path : active_state->get_editable_instances()) {
			const String value = _canonical_node_path(String(path));
			if (!_is_safe_node_path(value) || active_editable_instances.size() >= (int)MAX_NODES) {
				refresh_limit_exceeded = true;
				return false;
			}
			active_editable_instances.push_back(value);
		}
		active_record.value["nodes"] = active_nodes.duplicate(true);
		active_record.value["connections"] = active_connections.duplicate(true);
		active_record.value["editable_instances"] = active_editable_instances.duplicate(true);
		active_record.value["subresources"] = active_subresources.duplicate(true);
		active_record.value["animation_tracks"] = active_animation_tracks.duplicate(true);
		observed_catalog.insert(active_path, active_record);
	}
	_reset_active_scene();
	refresh_phase = REFRESH_BEGIN_SCENE;
	return true;
}

void SceneStateAdapter::_reset_project_context_capture() {
	project_context_phase = PROJECT_CONTEXT_COLLECT_KEYS;
	project_context_source_index = 0;
	project_context_values.clear();
	project_context_actions.clear();
	project_context_action_index = 0;
}

bool SceneStateAdapter::_reject_project_context_capture() {
	refresh_limit_exceeded = true;
	_reset_project_context_capture();
	refresh_phase = REFRESH_RECONCILE;
	return true;
}

bool SceneStateAdapter::_capture_project_context() {
	ProjectSettings *settings = ProjectSettings::get_singleton();
	ERR_FAIL_NULL_V(settings, _reject_project_context_capture());

	if (project_context_phase == PROJECT_CONTEXT_COLLECT_KEYS) {
		if (!project_context_keys_valid) {
			List<PropertyInfo> properties;
			settings->get_property_list(&properties);
			if (properties.size() > (int)MAX_PROPERTIES) {
				return _reject_project_context_capture();
			}
			project_context_source_keys.clear();
			for (const PropertyInfo &property : properties) {
				project_context_source_keys.push_back(String(property.name));
			}
			project_context_keys_valid = true;
		}
		project_context_phase = PROJECT_CONTEXT_SETTINGS;
		return true;
	}

	if (project_context_phase == PROJECT_CONTEXT_SETTINGS) {
		for (int processed = 0; processed < 64 && project_context_source_index < project_context_source_keys.size(); processed++) {
			const String source_key = project_context_source_keys[project_context_source_index++];
			if (source_key.begins_with("input/") && settings->has_setting(source_key) && !settings->is_builtin_setting(source_key)) {
				if (project_context_actions.size() >= (int)MAX_SCENES) {
					return _reject_project_context_capture();
				}
				project_context_actions.push_back(source_key.trim_prefix("input/"));
				continue;
			}
			const bool layer_setting = source_key.begins_with("layer_names/2d_physics/layer_") || source_key.begins_with("layer_names/2d_render/layer_") ||
					source_key.begins_with("layer_names/3d_physics/layer_") || source_key.begins_with("layer_names/3d_render/layer_") ||
					source_key.begins_with("layer_names/navigation/layer_");
			const bool allowed = source_key == "application/run/main_scene" || source_key.begins_with("autoload/") || layer_setting;
			if (!allowed || !settings->has_setting(source_key)) {
				continue;
			}
			const Variant setting_value = settings->get_setting(source_key);
			if (layer_setting && String(setting_value).is_empty()) {
				continue;
			}
			String key = source_key;
			if (layer_setting) {
				const String layer_number = source_key.get_file().trim_prefix("layer_");
				if (!layer_number.is_valid_int() || layer_number.to_int() < 1) {
					return _reject_project_context_capture();
				}
				key = source_key.get_base_dir().path_join(layer_number);
			}
			const String autoload_name = key.begins_with("autoload/") ? key.trim_prefix("autoload/") : String();
			if (key.length() > 512 || (key.begins_with("autoload/") && (autoload_name.is_empty() || autoload_name.contains("/")))) {
				return _reject_project_context_capture();
			}
			Dictionary fact;
			fact["key"] = key;
			fact["value"] = BoundedVariantProjector::project_typed(setting_value);
			fact["authority"] = "project_settings";
			fact["scene_graph_revision"] = (int64_t)target_scene_graph_revision;
			project_context_values.insert(key, fact);
		}
		if (project_context_source_index >= project_context_source_keys.size()) {
			project_context_actions.sort();
			project_context_phase = PROJECT_CONTEXT_ACTIONS;
		}
		return true;
	}

	if (project_context_phase == PROJECT_CONTEXT_ACTIONS) {
		for (int processed = 0; processed < 16 && project_context_action_index < project_context_actions.size(); processed++) {
			const String action = project_context_actions[project_context_action_index++];
			if (action.is_empty() || action.contains("/") || action.length() > 506 || project_context_values.size() >= (int)MAX_SCENES) {
				return _reject_project_context_capture();
			}
			const Variant setting = settings->get_setting("input/" + action);
			if (setting.get_type() != Variant::DICTIONARY) {
				return _reject_project_context_capture();
			}
			const Dictionary action_setting = setting;
			Dictionary input_value;
			input_value["deadzone"] = action_setting.get("deadzone", 0.5);
			Array events;
			const Array action_events = action_setting.get("events", Array());
			int count = 0;
			for (const Variant &event_value : action_events) {
				if (count++ >= BoundedVariantProjector::MAX_CONTAINER_ITEMS) {
					break;
				}
				const Ref<InputEvent> event = event_value;
				events.push_back(event.is_valid() ? event->as_text().left(1024) : String());
			}
			input_value["events"] = events;
			Dictionary fact;
			fact["key"] = "input/" + action;
			fact["value"] = BoundedVariantProjector::project_typed(input_value);
			fact["authority"] = "input_map";
			fact["scene_graph_revision"] = (int64_t)target_scene_graph_revision;
			project_context_values.insert("input/" + action, fact);
		}
		if (project_context_action_index >= project_context_actions.size()) {
			project_context_phase = PROJECT_CONTEXT_FINALIZE;
		}
		return true;
	}

	project_context.clear();
	project_context_checksum.clear();
	Array checksum_context;
	for (const KeyValue<String, Dictionary> &entry : project_context_values) {
		project_context.push_back(entry.value);
		Dictionary checksum_fact = entry.value.duplicate(true);
		checksum_fact.erase("scene_graph_revision");
		checksum_context.push_back(checksum_fact);
	}
	project_context_checksum = _sha256_hex(JSON::stringify(checksum_context, "", true, true));
	Array checksum_diagnostics = observed_diagnostics.duplicate(true);
	for (int index = 0; index < checksum_diagnostics.size(); index++) {
		Dictionary diagnostic = checksum_diagnostics[index];
		diagnostic.erase("scene_graph_revision");
		checksum_diagnostics[index] = diagnostic;
	}
	observed_diagnostics_checksum = _sha256_hex(JSON::stringify(checksum_diagnostics, "", true, true));
	_reset_project_context_capture();
	refresh_phase = REFRESH_RECONCILE;
	return true;
}

void SceneStateAdapter::_reset_reconcile() {
	reconcile_operations = Array();
	reconcile_previous_revision = 0;
	reconcile_next_revision = 0;
	reconcile_preparing = false;
	journal_prepare_task = -1;
	journal_prepare_error = OK;
	journal_prepared_batch = SceneDeltaJournal::PreparedBatch();
}

void SceneStateAdapter::_prepare_journal_batch_thread(void *p_userdata) {
	SceneStateAdapter *adapter = static_cast<SceneStateAdapter *>(p_userdata);
	adapter->journal_prepare_error = SceneDeltaJournal::prepare_batch(
			adapter->reconcile_next_revision,
			adapter->reconcile_operations,
			adapter->journal_prepared_batch);
}

void SceneStateAdapter::_wait_for_journal_preparation() {
	if (journal_prepare_task < 0) {
		return;
	}
	WorkerThreadPool *worker_pool = WorkerThreadPool::get_singleton();
	if (worker_pool) {
		worker_pool->wait_for_task_completion(journal_prepare_task);
	}
	journal_prepare_task = -1;
}

bool SceneStateAdapter::_reconcile(RefreshOutcome &r_outcome) {
	if (reconcile_preparing) {
		WorkerThreadPool *worker_pool = WorkerThreadPool::get_singleton();
		if (journal_prepare_task >= 0 && worker_pool && !worker_pool->is_task_completed(journal_prepare_task)) {
			return false;
		}
		_wait_for_journal_preparation();
		const uint64_t current = revision_clock ? revision_clock->record_scene_graph_change() : reconcile_next_revision;
		const Dictionary revisions = revision_clock ? revision_clock->get_revision_vector() : Dictionary();
		bool invalidated = current != reconcile_next_revision || journal_prepare_error != OK;
		if (invalidated) {
			journal.invalidate_to(current);
		} else {
			Dictionary batch;
			const uint64_t resource_revision = revisions.get("resource_revision", 1);
			const uint64_t project_revision = revisions.get("project_revision", 0);
			if (journal.commit_prepared(current, resource_revision, project_revision, journal_prepared_batch, batch, invalidated) != OK) {
				journal.invalidate_to(current);
				invalidated = true;
			}
		}
		catalog = std::move(observed_catalog);
		diagnostics = observed_diagnostics.duplicate(true);
		diagnostics_checksum = observed_diagnostics_checksum;
		active_project_context_checksum = project_context_checksum;
		r_outcome.changed = !invalidated;
		r_outcome.invalidated = invalidated;
		r_outcome.last_contiguous_scene_graph_revision = reconcile_previous_revision;
		r_outcome.current_scene_graph_revision = current;
		r_outcome.revisions = revisions;
		_reset_reconcile();
		refresh_phase = REFRESH_IDLE;
		return true;
	}
	if (refresh_limit_exceeded) {
		const uint64_t previous = revision_clock ? revision_clock->get_scene_graph_revision() : journal.get_current_revision();
		const uint64_t current = revision_clock ? revision_clock->record_scene_graph_change() : previous + 1;
		journal.invalidate_to(current);
		r_outcome.invalidated = true;
		r_outcome.last_contiguous_scene_graph_revision = previous;
		r_outcome.current_scene_graph_revision = current;
		r_outcome.revisions = revision_clock ? revision_clock->get_revision_vector() : Dictionary();
		observed_catalog.clear();
		catalog_ready = false;
		refresh_phase = REFRESH_IDLE;
		return true;
	}

	Array operations;
	for (const KeyValue<String, CatalogRecord> &entry : observed_catalog) {
		const RBMap<String, CatalogRecord>::Element *existing = catalog.find(entry.key);
		if (!existing || existing->value().facts_checksum != entry.value.facts_checksum) {
			Dictionary operation;
			operation["kind"] = "upsert";
			operation["value"] = entry.value.value;
			operations.push_back(operation);
		}
	}
	for (const KeyValue<String, CatalogRecord> &entry : catalog) {
		if (!observed_catalog.has(entry.key)) {
			Dictionary operation;
			operation["kind"] = "remove";
			operation["scene_ref"] = _make_resource_ref(entry.key);
			operation["path"] = entry.key;
			operations.push_back(operation);
		}
	}
	if (!catalog_ready || active_project_context_checksum != project_context_checksum) {
		Dictionary operation;
		operation["kind"] = "project_context";
		operation["values"] = project_context.duplicate(true);
		operations.push_back(operation);
	}

	if (!catalog_ready) {
		catalog = std::move(observed_catalog);
		diagnostics = observed_diagnostics.duplicate(true);
		diagnostics_checksum = observed_diagnostics_checksum;
		active_project_context_checksum = project_context_checksum;
		catalog_ready = true;
		refresh_phase = REFRESH_IDLE;
		return true;
	}
	if (diagnostics_checksum != observed_diagnostics_checksum) {
		const uint64_t previous = revision_clock ? revision_clock->get_scene_graph_revision() : journal.get_current_revision();
		const uint64_t current = revision_clock ? revision_clock->record_scene_graph_change() : previous + 1;
		catalog = std::move(observed_catalog);
		diagnostics = observed_diagnostics.duplicate(true);
		diagnostics_checksum = observed_diagnostics_checksum;
		active_project_context_checksum = project_context_checksum;
		journal.invalidate_to(current);
		r_outcome.invalidated = true;
		r_outcome.last_contiguous_scene_graph_revision = previous;
		r_outcome.current_scene_graph_revision = current;
		r_outcome.revisions = revision_clock ? revision_clock->get_revision_vector() : Dictionary();
		refresh_phase = REFRESH_IDLE;
		return true;
	}
	if (operations.is_empty()) {
		observed_catalog.clear();
		refresh_phase = REFRESH_IDLE;
		return true;
	}

	reconcile_operations = operations;
	reconcile_previous_revision = revision_clock ? revision_clock->get_scene_graph_revision() : journal.get_current_revision();
	reconcile_next_revision = reconcile_previous_revision + 1;
	reconcile_preparing = true;
	journal_prepare_error = OK;
	journal_prepared_batch = SceneDeltaJournal::PreparedBatch();
	WorkerThreadPool *worker_pool = WorkerThreadPool::get_singleton();
	if (worker_pool) {
		journal_prepare_task = worker_pool->add_native_task(
				_prepare_journal_batch_thread,
				this,
				false,
				SNAME("CodexSceneDeltaBatch"));
	}
	if (journal_prepare_task < 0) {
		_prepare_journal_batch_thread(this);
	}
	return false;
}

bool SceneStateAdapter::_process_refresh_step(RefreshOutcome &r_outcome) {
	switch (refresh_phase) {
		case REFRESH_COLLECT:
			return _collect_one_path();
		case REFRESH_BEGIN_SCENE:
			return _begin_active_scene();
		case REFRESH_NODE_BEGIN:
			return _begin_active_node();
		case REFRESH_NODE_PROPERTY:
			return _observe_active_property();
		case REFRESH_NODE_FINISH:
			return _finish_active_node();
		case REFRESH_CONNECTION:
			return _observe_connection();
		case REFRESH_SUBRESOURCE:
			return _observe_subresource();
		case REFRESH_SCENE_FINISH:
			return _finish_active_scene();
		case REFRESH_PROJECT_CONTEXT:
			return _capture_project_context();
		case REFRESH_RECONCILE:
			return _reconcile(r_outcome);
		case REFRESH_IDLE:
			break;
	}
	return false;
}

void SceneStateAdapter::initialize(BridgeRevisionClock *p_revision_clock) {
	revision_clock = p_revision_clock;
	journal.initialize(revision_clock ? revision_clock->get_scene_graph_revision() : 1);
	_reset_reconcile();
	request_refresh();
}

void SceneStateAdapter::shutdown() {
	_wait_for_journal_preparation();
	_reset_reconcile();
	_reset_active_scene();
	_reset_snapshot();
	catalog.clear();
	observed_catalog.clear();
	diagnostics.clear();
	observed_diagnostics.clear();
	diagnostics_checksum.clear();
	observed_diagnostics_checksum.clear();
	directory_stack.clear();
	scene_paths.clear();
	project_context.clear();
	project_context_checksum.clear();
	active_project_context_checksum.clear();
	_reset_project_context_capture();
	project_context_source_keys.clear();
	project_context_keys_valid = false;
	refresh_phase = REFRESH_IDLE;
	refresh_requested = false;
	catalog_ready = false;
	revision_clock = nullptr;
}

void SceneStateAdapter::request_refresh() {
	refresh_requested = true;
}

void SceneStateAdapter::invalidate_project_context() {
	project_context_keys_valid = false;
	request_refresh();
}

bool SceneStateAdapter::process_refresh(uint64_t p_budget_usec, RefreshOutcome &r_outcome) {
	r_outcome = RefreshOutcome();
	ERR_FAIL_COND_V_MSG(!Thread::is_main_thread(), false, "SceneStateAdapter must run on the main thread.");
	if (refresh_phase == REFRESH_IDLE && refresh_requested) {
		EditorFileSystem *filesystem = EditorFileSystem::get_singleton();
		if (!filesystem || filesystem->doing_first_scan() || filesystem->is_scanning() || filesystem->is_importing() || !filesystem->get_filesystem()) {
			return false;
		}
		refresh_requested = false;
		refresh_limit_exceeded = false;
		observed_catalog.clear();
		observed_diagnostics.clear();
		observed_diagnostics_checksum.clear();
		directory_stack.clear();
		scene_paths.clear();
		scene_path_index = 0;
		observed_node_count = 0;
		observed_property_count = 0;
		observed_relation_count = 0;
		observed_diagnostic_count = 0;
		target_scene_graph_revision = catalog_ready ? (revision_clock ? revision_clock->get_scene_graph_revision() + 1 : journal.get_current_revision() + 1) : (revision_clock ? revision_clock->get_scene_graph_revision() : journal.get_current_revision());
		DirectoryCursor root;
		root.directory = filesystem->get_filesystem();
		directory_stack.push_back(root);
		refresh_phase = REFRESH_COLLECT;
	}
	if (refresh_phase == REFRESH_IDLE) {
		return false;
	}
	const uint64_t started = OS::get_singleton()->get_ticks_usec();
	do {
		_process_refresh_step(r_outcome);
		if (r_outcome.changed || r_outcome.invalidated || refresh_phase == REFRESH_IDLE) {
			return true;
		}
		// ProjectSettings enumeration and reconciliation are bounded but denser
		// than an ordinary node/property step. Start each on a fresh frame rather
		// than appending it to the tail of the final scene slice.
		if (refresh_phase == REFRESH_PROJECT_CONTEXT || refresh_phase == REFRESH_RECONCILE) {
			return false;
		}
	} while (OS::get_singleton()->get_ticks_usec() - started < p_budget_usec);
	return false;
}

void SceneStateAdapter::_reset_snapshot() {
	snapshot_active = false;
	snapshot_request_id = 0;
	snapshot_started_usec = 0;
	snapshot_id.clear();
	snapshot_resource_revision = 0;
	snapshot_scene_graph_revision = 0;
	snapshot_revisions = Dictionary();
	snapshot_context = Dictionary();
	snapshot_record = nullptr;
	snapshot_pending_message = Dictionary();
	snapshot_scenes = Array();
	snapshot_diagnostics = Array();
	snapshot_chunk_count = 0;
	snapshot_phase = 0;
}

Array SceneStateAdapter::_take_abandoned_snapshot_data() {
	Array abandoned;
	if (!snapshot_pending_message.is_empty()) {
		abandoned.push_back(snapshot_pending_message);
	}
	if (!snapshot_scenes.is_empty()) {
		abandoned.push_back(snapshot_scenes);
	}
	if (!snapshot_diagnostics.is_empty()) {
		abandoned.push_back(snapshot_diagnostics);
	}
	snapshot_pending_message = Dictionary();
	snapshot_scenes = Array();
	snapshot_diagnostics = Array();
	return abandoned;
}

bool SceneStateAdapter::_emit_pending_snapshot_message(SnapshotCompletion &r_completion) {
	if (snapshot_pending_message.is_empty()) {
		return false;
	}
	r_completion.ready = true;
	r_completion.request_id = snapshot_request_id;
	r_completion.server_message = snapshot_pending_message;
	snapshot_pending_message = Dictionary();
	return true;
}

bool SceneStateAdapter::_flush_snapshot_chunk() {
	if (!snapshot_pending_message.is_empty() || snapshot_chunk_count >= MAX_SCENES + 1) {
		return false;
	}
	Dictionary payload;
	payload["scenes"] = snapshot_scenes;
	payload["project_context"] = snapshot_phase >= 2 ? project_context : Array();
	payload["diagnostics"] = snapshot_diagnostics;
	Dictionary chunk;
	chunk["protocol_version"] = "1.3";
	chunk["kind"] = "chunk";
	chunk["snapshot_id"] = snapshot_id;
	chunk["domain"] = "scene_graph";
	chunk["chunk_index"] = (int64_t)snapshot_chunk_count;
	chunk["payload"] = payload;
	chunk["context"] = snapshot_context;
	// Canonical JSON, exact chunk/window limits, and checksums are prepared by
	// the transport worker so no full scene record is serialized on the editor
	// main thread.
	snapshot_pending_message = chunk;
	snapshot_chunk_count++;
	// Detach the builders rather than mutating the arrays now owned by payload.
	snapshot_scenes = Array();
	snapshot_diagnostics = Array();
	return true;
}

void SceneStateAdapter::_fail_snapshot(const String &p_code, const String &p_message, bool p_retryable, SnapshotCompletion &r_completion) {
	r_completion.ready = true;
	r_completion.request_id = snapshot_request_id;
	r_completion.is_error = true;
	r_completion.error_code = p_code;
	r_completion.error_message = p_message;
	r_completion.error_retryable = p_retryable;
	r_completion.abandoned_messages = _take_abandoned_snapshot_data();
	_reset_snapshot();
}

void SceneStateAdapter::_finish_snapshot(SnapshotCompletion &r_completion) {
	Dictionary end_params;
	end_params["snapshot_id"] = snapshot_id;
	end_params["domain"] = "scene_graph";
	end_params["resource_revision"] = (int64_t)snapshot_resource_revision;
	end_params["scene_graph_revision"] = (int64_t)snapshot_scene_graph_revision;
	end_params["revisions"] = snapshot_revisions;
	end_params["chunk_count"] = (int64_t)snapshot_chunk_count;
	// Filled by the transport worker after it serializes each exact payload.
	end_params["checksum"] = "";
	Dictionary end;
	end["protocol_version"] = "1.3";
	end["kind"] = "notification";
	end["method"] = "snapshot.end";
	end["params"] = end_params;
	end["context"] = snapshot_context;

	Dictionary limits;
	limits["scene_records"] = (int64_t)MAX_SCENES;
	limits["scene_nodes"] = (int64_t)MAX_NODES;
	limits["scene_properties"] = (int64_t)MAX_PROPERTIES;
	limits["scene_relations"] = (int64_t)MAX_RELATIONS;
	limits["instance_depth"] = (int64_t)MAX_INSTANCE_DEPTH;
	limits["snapshot_chunk_bytes"] = (int64_t)SNAPSHOT_CHUNK_BYTES;
	limits["snapshot_window_bytes"] = (int64_t)SNAPSHOT_WINDOW_BYTES;
	limits["snapshot_timeout_ms"] = (int64_t)(SNAPSHOT_TIMEOUT_USEC / 1000);
	Dictionary result;
	result["snapshot_id"] = snapshot_id;
	result["domain"] = "scene_graph";
	result["resource_revision"] = (int64_t)snapshot_resource_revision;
	result["scene_graph_revision"] = (int64_t)snapshot_scene_graph_revision;
	result["revisions"] = snapshot_revisions;
	result["limits_applied"] = limits;
	r_completion.ready = true;
	r_completion.terminal = true;
	r_completion.request_id = snapshot_request_id;
	r_completion.result = result;
	r_completion.server_message = end;
	_reset_snapshot();
}

Error SceneStateAdapter::begin_snapshot(uint64_t p_request_id, uint64_t p_now_usec, const Dictionary &p_context, Dictionary &r_error_data) {
	r_error_data.clear();
	if (snapshot_active) {
		r_error_data["active_request_id"] = (int64_t)snapshot_request_id;
		return ERR_BUSY;
	}
	if (!catalog_ready || journal.is_invalidating()) {
		return ERR_UNAVAILABLE;
	}
	_reset_snapshot();
	snapshot_active = true;
	snapshot_request_id = p_request_id;
	snapshot_started_usec = p_now_usec;
	snapshot_id = _make_snapshot_id();
	snapshot_resource_revision = revision_clock ? revision_clock->get_resource_revision() : 1;
	snapshot_scene_graph_revision = revision_clock ? revision_clock->get_scene_graph_revision() : journal.get_current_revision();
	snapshot_revisions = revision_clock ? revision_clock->get_revision_vector() : Dictionary();
	snapshot_context = p_context;
	snapshot_record = catalog.front();
	return OK;
}

bool SceneStateAdapter::process_snapshot(uint64_t p_now_usec, uint64_t p_budget_usec, SnapshotCompletion &r_completion) {
	r_completion = SnapshotCompletion();
	(void)p_budget_usec;
	if (!snapshot_active) {
		return false;
	}
	if (_emit_pending_snapshot_message(r_completion)) {
		return true;
	}
	if (p_now_usec - snapshot_started_usec >= SNAPSHOT_TIMEOUT_USEC) {
		_fail_snapshot("deadline_exceeded", "The scene graph snapshot exceeded its bounded lifetime.", true, r_completion);
		return true;
	}
	// Advance exactly one bounded snapshot unit per frame. Canonical encoding
	// and checksum work happens after each unit reaches the transport worker.
	if (snapshot_phase == 0) {
		Dictionary params;
		params["snapshot_id"] = snapshot_id;
		params["domain"] = "scene_graph";
		params["resource_revision"] = (int64_t)snapshot_resource_revision;
		params["scene_graph_revision"] = (int64_t)snapshot_scene_graph_revision;
		params["revisions"] = snapshot_revisions;
		Dictionary begin;
		begin["protocol_version"] = "1.3";
		begin["kind"] = "notification";
		begin["method"] = "snapshot.begin";
		begin["params"] = params;
		begin["context"] = snapshot_context;
		snapshot_pending_message = begin;
		snapshot_phase = 1;
	} else if (snapshot_phase == 1) {
		if (snapshot_record) {
			snapshot_scenes.push_back(snapshot_record->value().value);
			for (const Variant &diagnostic : snapshot_record->value().diagnostics) {
				snapshot_diagnostics.push_back(diagnostic);
			}
			if (!_flush_snapshot_chunk()) {
				_fail_snapshot("scene_limit_exceeded", "A scene graph snapshot chunk exceeded its negotiated limit.", false, r_completion);
				return true;
			}
			snapshot_record = snapshot_record->next();
		} else {
			snapshot_phase = 2;
		}
	} else if (snapshot_phase == 2) {
		for (const Variant &diagnostic : diagnostics) {
			snapshot_diagnostics.push_back(diagnostic);
		}
		if (!_flush_snapshot_chunk()) {
			_fail_snapshot("scene_limit_exceeded", "Project context exceeded its negotiated snapshot limit.", false, r_completion);
			return true;
		}
		snapshot_phase = 3;
	} else {
		_finish_snapshot(r_completion);
		return true;
	}
	return false;
}

Array SceneStateAdapter::cancel_snapshot(uint64_t p_request_id) {
	if (!snapshot_active || snapshot_request_id != p_request_id) {
		return Array();
	}
	Array abandoned = _take_abandoned_snapshot_data();
	_reset_snapshot();
	return abandoned;
}

SceneDeltaJournal::QueryResult SceneStateAdapter::query_delta(uint64_t p_after_scene_graph_revision) const {
	return journal.query_after(p_after_scene_graph_revision);
}

bool SceneStateAdapter::is_catalog_ready() const {
	return catalog_ready && !journal.is_invalidating();
}

bool SceneStateAdapter::is_snapshot_active() const {
	return snapshot_active;
}

bool SceneStateAdapter::has_pending_work() const {
	return refresh_requested || refresh_phase != REFRESH_IDLE || snapshot_active;
}

uint64_t SceneStateAdapter::get_scene_graph_revision() const {
	return journal.get_current_revision();
}
