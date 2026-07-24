/**************************************************************************/
/*  scoped_persistence_executor.cpp                                       */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/

#include "scoped_persistence_executor.h"

#include "core/config/project_settings.h"
#include "core/io/dir_access.h"
#include "core/io/file_access.h"
#include "core/io/resource_loader.h"
#include "core/io/resource_saver.h"
#include "core/object/class_db.h"
#include "core/templates/hash_map.h"
#include "core/templates/hash_set.h"
#include "scene/main/node.h"
#include "scene/resources/packed_scene.h"

#include "writable_variant_codec.h"

#ifdef TOOLS_ENABLED
#include "editor/file_system/editor_file_system.h"
#include "editor/script/script_editor_plugin.h"
#endif

namespace {

static Error fail(const String &p_code, const String &p_message, String &r_code, String &r_message, Error p_error = ERR_INVALID_DATA) {
	r_code = p_code;
	r_message = p_message;
	return p_error;
}

static bool safe_target_path(const String &p_path, const String &p_extension) {
	if (!p_path.begins_with("res://") || !p_path.ends_with(p_extension) || p_path.contains("\\") || p_path.contains("/../") || p_path.contains("/./") || p_path.begins_with("res://addons/") || p_path.begins_with("res://.godot/")) {
		return false;
	}
	return !p_path.contains(".import/") && !p_path.contains("/imported/") && !p_path.contains("/generated/");
}

static String file_digest(const String &p_path) {
	const String digest = FileAccess::get_sha256(p_path);
	return digest.is_empty() ? String() : "sha256:" + digest;
}

static Error read_file(const String &p_path, PackedByteArray &r_bytes) {
	Error error = OK;
	Ref<FileAccess> file = FileAccess::open(p_path, FileAccess::READ, &error);
	if (error != OK || file.is_null()) {
		return error == OK ? ERR_CANT_OPEN : error;
	}
	const uint64_t length = file->get_length();
	ERR_FAIL_COND_V(length > ScopedPersistenceExecutor::MAX_SCRIPT_BYTES * 4, ERR_OUT_OF_MEMORY);
	r_bytes.resize(length);
	if (length > 0 && file->get_buffer(r_bytes.ptrw(), length) != length) {
		return ERR_FILE_CORRUPT;
	}
	return OK;
}

static Error write_file(const String &p_path, const PackedByteArray &p_bytes) {
	Error error = OK;
	Ref<FileAccess> file = FileAccess::open(p_path, FileAccess::WRITE, &error);
	if (error != OK || file.is_null()) {
		return error == OK ? ERR_CANT_OPEN : error;
	}
	if (!p_bytes.is_empty() && !file->store_buffer(p_bytes)) {
		return ERR_CANT_CREATE;
	}
	file->flush();
	return file->get_error();
}

static Error ensure_parent(const String &p_path) {
	const String absolute = ProjectSettings::get_singleton()->globalize_path(p_path.get_base_dir());
	return DirAccess::make_dir_recursive_absolute(absolute);
}

static bool target_is_symlink(const String &p_path) {
	const String absolute = ProjectSettings::get_singleton()->globalize_path(p_path);
	Ref<DirAccess> filesystem = DirAccess::create(DirAccess::ACCESS_FILESYSTEM);
	return filesystem.is_valid() && filesystem->is_link(absolute);
}

static bool has_unsaved_editor_buffer(const String &p_path) {
#ifdef TOOLS_ENABLED
	ScriptEditor *script_editor = ScriptEditor::get_singleton();
	if (script_editor) {
		const PackedStringArray unsaved = script_editor->get_unsaved_files();
		for (const String &path : unsaved) {
			if (path == p_path) {
				return true;
			}
		}
	}
#else
	(void)p_path;
#endif
	return false;
}

static void request_reload_barrier() {
#ifdef TOOLS_ENABLED
	if (ScriptEditor::get_singleton()) {
		ScriptEditor::get_singleton()->reload_open_files();
	}
	if (EditorFileSystem::get_singleton()) {
		EditorFileSystem::get_singleton()->scan_changes();
	}
#endif
}

static Error apply_script_edits(const PackedByteArray &p_preimage, const Array &p_edits, PackedByteArray &r_postimage) {
	if (p_preimage.size() > ScopedPersistenceExecutor::MAX_SCRIPT_BYTES) {
		return ERR_OUT_OF_MEMORY;
	}
	int64_t cursor = 0;
	for (int index = 0; index < p_edits.size(); index++) {
		const Dictionary edit = p_edits[index];
		const int64_t start = edit["start_byte"];
		const int64_t end = edit["end_byte"];
		if (start < cursor || end < start || end > p_preimage.size()) {
			return ERR_INVALID_DATA;
		}
		for (int64_t byte = cursor; byte < start; byte++) {
			r_postimage.push_back(p_preimage[byte]);
		}
		const CharString replacement = String(edit["replacement"]).utf8();
		for (int64_t byte = 0; byte < replacement.length(); byte++) {
			r_postimage.push_back((uint8_t)replacement[byte]);
		}
		cursor = end;
	}
	for (int64_t byte = cursor; byte < p_preimage.size(); byte++) {
		r_postimage.push_back(p_preimage[byte]);
	}
	if (r_postimage.size() > ScopedPersistenceExecutor::MAX_SCRIPT_BYTES) {
		return ERR_OUT_OF_MEMORY;
	}
	// Strict UTF-8 round-trip: invalid byte sequences are never persisted.
	const String decoded = String::utf8(reinterpret_cast<const char *>(r_postimage.ptr()), r_postimage.size());
	const CharString round_trip = decoded.utf8();
	if (round_trip.length() != r_postimage.size()) {
		return ERR_INVALID_DATA;
	}
	for (int index = 0; index < round_trip.length(); index++) {
		if ((uint8_t)round_trip[index] != r_postimage[index]) {
			return ERR_INVALID_DATA;
		}
	}
	return OK;
}

static String resolve_resource_path(const String &p_reference, const HashMap<String, String> &p_alias_paths, const Dictionary &p_resource_paths) {
	if (p_reference.begins_with("alias:")) {
		const String *path = p_alias_paths.getptr(p_reference);
		return path ? *path : String();
	}
	if (p_resource_paths.has(p_reference) && p_resource_paths[p_reference].get_type() == Variant::STRING) {
		return p_resource_paths[p_reference];
	}
	return String();
}

static Ref<Resource> instantiate_resource(const String &p_class) {
	Object *object = ClassDB::instantiate(p_class);
	Resource *resource = Object::cast_to<Resource>(object);
	if (!resource) {
		if (object) {
			memdelete(object);
		}
		return Ref<Resource>();
	}
	return Ref<Resource>(resource);
}

static Error apply_properties(const Ref<Resource> &p_resource, const String &p_class, const Array &p_properties, String &r_error_code, String &r_error_message) {
	for (int index = 0; index < p_properties.size(); index++) {
		const Dictionary property = p_properties[index];
		const String name = property["name"];
		if (!ScopedPersistenceExecutor::property_allowed(p_class, name)) {
			return fail("resource_property_not_allowed", "The stored Resource property is outside the closed allowlist.", r_error_code, r_error_message);
		}
		WritableVariantCodec::Result decoded;
		if (WritableVariantCodec::decode(property["value"], nullptr, nullptr, decoded, r_error_code, r_error_message) != OK) {
			return ERR_INVALID_DATA;
		}
		bool valid = false;
		const Variant current = p_resource->get(name, &valid);
		if (!valid) {
			return fail("resource_property_not_allowed", "The stored Resource property is unavailable on the selected class.", r_error_code, r_error_message);
		}
		PropertyInfo info(current.get_type(), name);
		if (WritableVariantCodec::validate_property_compatibility(info, decoded.value, r_error_code, r_error_message) != OK) {
			return fail("resource_property_type_mismatch", "The Resource property value is not type-compatible.", r_error_code, r_error_message);
		}
		p_resource->set(name, WritableVariantCodec::normalize_property_value(info, decoded.value));
	}
	return OK;
}

static Error stage_scene_postimage(ScopedPersistenceExecutor::FileRecord &r_record, String &r_error_code, String &r_error_message) {
	Node *root = Object::cast_to<Node>(ObjectDB::get_instance(r_record.scene_root_id));
	if (!root || root->get_scene_file_path() != r_record.target_path) {
		return fail("scene_not_open", "The scoped scene root no longer matches its saved target.", r_error_code, r_error_message, ERR_DOES_NOT_EXIST);
	}
	Ref<PackedScene> packed;
	packed.instantiate();
	if (packed->pack(root) != OK || ResourceSaver::save(packed, r_record.stage_path, ResourceSaver::FLAG_OMIT_EDITOR_PROPERTIES) != OK) {
		return fail("scene_serialize_failed", "The scoped scene postimage could not be serialized.", r_error_code, r_error_message, ERR_CANT_CREATE);
	}
	const String digest = file_digest(r_record.stage_path);
	if (digest.is_empty() || (!r_record.postimage_digest.is_empty() && r_record.postimage_digest != digest)) {
		return fail("scene_postimage_changed", "The scoped scene Redo did not reproduce the exact postimage.", r_error_code, r_error_message, ERR_FILE_CORRUPT);
	}
	r_record.postimage_digest = digest;
	return OK;
}

} // namespace

bool ScopedPersistenceExecutor::property_allowed(const String &p_class, const String &p_property) {
	if (p_class == "Gradient") {
		return p_property == "interpolation_mode" || p_property == "interpolation_color_space" || p_property == "offsets" || p_property == "colors";
	}
	if (p_class == "Curve") {
		return p_property == "min_domain" || p_property == "max_domain" || p_property == "min_value" || p_property == "max_value" || p_property == "bake_resolution";
	}
	if (p_class == "Curve2D") {
		return p_property == "bake_interval";
	}
	if (p_class == "Curve3D") {
		return p_property == "closed" || p_property == "bake_interval" || p_property == "up_vector_enabled";
	}
	if (p_class == "Animation") {
		return p_property == "length" || p_property == "loop_mode" || p_property == "step";
	}
	if (p_class == "CanvasItemMaterial") {
		return p_property == "blend_mode" || p_property == "light_mode" || p_property == "particles_animation" || p_property == "particles_anim_h_frames" || p_property == "particles_anim_v_frames" || p_property == "particles_anim_loop";
	}
	if (p_class == "StandardMaterial3D") {
		return p_property == "render_priority" || p_property == "transparency" || p_property == "blend_mode" || p_property == "cull_mode" || p_property == "shading_mode" || p_property == "vertex_color_use_as_albedo" || p_property == "albedo_color" || p_property == "albedo_texture" || p_property == "metallic" || p_property == "metallic_specular" || p_property == "roughness" || p_property == "emission_enabled" || p_property == "emission" || p_property == "emission_energy_multiplier" || p_property == "emission_texture";
	}
	return false;
}

Error ScopedPersistenceExecutor::stage(const CompoundChangeSetPlanner::Plan &p_plan, const Dictionary &p_resource_paths, const String &p_scene_path, ObjectID p_scene_root_id, Prepared &r_prepared, String &r_error_code, String &r_error_message) {
	r_prepared = Prepared();
	r_error_code.clear();
	r_error_message.clear();
	const String opaque_id = p_plan.change_set_id.trim_prefix("change-set:");
	if (opaque_id.length() != 32) {
		return fail("invalid_change_set", "The change-set identifier is invalid.", r_error_code, r_error_message);
	}
	r_prepared.change_set_id = p_plan.change_set_id;
	r_prepared.stage_directory = "res://.godot/codex/staging/" + opaque_id;
	r_prepared.escrow_directory = "res://.godot/codex/rollback/" + opaque_id;
	if (ensure_parent(r_prepared.stage_directory + "/placeholder") != OK || ensure_parent(r_prepared.escrow_directory + "/placeholder") != OK) {
		return fail("staging_unavailable", "Private staging directories could not be created.", r_error_code, r_error_message, ERR_CANT_CREATE);
	}
	const String stage_absolute = ProjectSettings::get_singleton()->globalize_path(r_prepared.stage_directory);
	const String escrow_absolute = ProjectSettings::get_singleton()->globalize_path(r_prepared.escrow_directory);
	DirAccess::make_dir_recursive_absolute(stage_absolute);
	DirAccess::make_dir_recursive_absolute(escrow_absolute);
#ifdef UNIX_ENABLED
	FileAccess::set_unix_permissions(stage_absolute, 0700);
	FileAccess::set_unix_permissions(escrow_absolute, 0700);
#endif

	HashSet<String> save_paths;
	for (int index = 0; index < p_plan.save_scope.size(); index++) {
		save_paths.insert(p_plan.save_scope[index]);
	}
	HashMap<String, String> alias_paths;
	HashMap<String, Ref<Resource>> resources;
	HashMap<String, PackedByteArray> scripts;
	HashMap<String, String> expected_hashes;
	for (int index = 0; index < p_plan.ordered_operations.size(); index++) {
		const Dictionary operation = p_plan.ordered_operations[index];
		const String kind = operation["kind"];
		if (kind == "create_resource") {
			const String path = operation["path"];
			if (!safe_target_path(path, ".tres") || !save_paths.has(path) || FileAccess::exists(path) || target_is_symlink(path)) {
				cleanup(r_prepared);
				return fail("resource_path_conflict", "The new Resource target is unsafe, exists, or is outside save_scope.", r_error_code, r_error_message);
			}
			Ref<Resource> resource = instantiate_resource(operation["resource_class"]);
			if (resource.is_null()) {
				cleanup(r_prepared);
				return fail("resource_class_not_allowed", "The allowlisted Resource class could not be instantiated.", r_error_code, r_error_message);
			}
			if (apply_properties(resource, operation["resource_class"], operation["properties"], r_error_code, r_error_message) != OK) {
				cleanup(r_prepared);
				return ERR_INVALID_DATA;
			}
			alias_paths.insert(operation["alias"], path);
			resources.insert(path, resource);
		} else if (kind == "update_resource") {
			const String path = operation.has("resolved_path") ? String(operation["resolved_path"]) : resolve_resource_path(operation["resource"], alias_paths, p_resource_paths);
			if (!safe_target_path(path, ".tres") || !save_paths.has(path) || target_is_symlink(path)) {
				cleanup(r_prepared);
				return fail("resource_path_conflict", "The Resource target is unsafe or outside save_scope.", r_error_code, r_error_message);
			}
			Ref<Resource> resource;
			if (const Ref<Resource> *known = resources.getptr(path)) {
				resource = *known;
			} else {
				if (!FileAccess::exists(path) || file_digest(path) != String(operation["expected_hash"])) {
					cleanup(r_prepared);
					return fail("stale_file_hash", "The Resource preimage hash changed.", r_error_code, r_error_message);
				}
				resource = ResourceLoader::load(path, "", ResourceFormatLoader::CACHE_MODE_IGNORE);
				if (resource.is_null() || !property_allowed(resource->get_class(), String(Dictionary(Array(operation["properties"])[0])["name"]))) {
					cleanup(r_prepared);
					return fail("resource_class_not_allowed", "The existing Resource class is outside the closed allowlist.", r_error_code, r_error_message);
				}
				resource = resource->duplicate(true);
				resources.insert(path, resource);
			}
			if (apply_properties(resource, resource->get_class(), operation["properties"], r_error_code, r_error_message) != OK) {
				cleanup(r_prepared);
				return ERR_INVALID_DATA;
			}
		} else if (kind == "update_gdscript") {
			const String path = operation["path"];
			if (!safe_target_path(path, ".gd") || !save_paths.has(path) || !FileAccess::exists(path) || target_is_symlink(path) || has_unsaved_editor_buffer(path) || file_digest(path) != String(operation["expected_hash"])) {
				cleanup(r_prepared);
				return fail("script_editor_conflict", "The GDScript target is unsafe, stale, outside save_scope, or has an unsaved editor buffer.", r_error_code, r_error_message);
			}
			PackedByteArray preimage;
			if (read_file(path, preimage) != OK) {
				cleanup(r_prepared);
				return fail("file_unreadable", "The GDScript preimage could not be read.", r_error_code, r_error_message, ERR_CANT_OPEN);
			}
			PackedByteArray postimage;
			if (apply_script_edits(preimage, operation["edits"], postimage) != OK) {
				cleanup(r_prepared);
				return fail("script_edit_invalid", "The bounded UTF-8 edit set is invalid.", r_error_code, r_error_message);
			}
			scripts.insert(path, postimage);
			expected_hashes.insert(path, operation["expected_hash"]);
		}
	}

	int64_t total_bytes = 0;
	int file_index = 0;
	for (int save_index = 0; save_index < p_plan.save_scope.size(); save_index++) {
		const String path = p_plan.save_scope[save_index];
		const bool scene = !p_scene_path.is_empty() && path == p_scene_path;
		if (!resources.has(path) && !scripts.has(path) && !scene) {
			cleanup(r_prepared);
			return fail("save_scope_mismatch", "Every save_scope path must have a planned postimage.", r_error_code, r_error_message);
		}
		if (scene && (!safe_target_path(path, ".tscn") || p_scene_root_id == ObjectID() || !FileAccess::exists(path) || target_is_symlink(path))) {
			cleanup(r_prepared);
			return fail("scene_path_conflict", "The saved open scene target is unsafe or stale.", r_error_code, r_error_message);
		}
		FileRecord record;
		record.target_path = path;
		record.scene = scene;
		record.scene_root_id = scene ? p_scene_root_id : ObjectID();
		record.existed = FileAccess::exists(path);
		record.preimage_digest = record.existed ? file_digest(path) : "missing";
		const String extension = path.get_extension();
		record.stage_path = r_prepared.stage_directory + "/" + itos(file_index) + "." + extension;
		record.escrow_path = r_prepared.escrow_directory + "/" + itos(file_index) + ".bin";
		if (record.existed) {
			PackedByteArray preimage;
			if (read_file(path, preimage) != OK || write_file(record.escrow_path, preimage) != OK) {
				cleanup(r_prepared);
				return fail("escrow_unavailable", "The private rollback escrow could not capture a preimage.", r_error_code, r_error_message, ERR_CANT_CREATE);
			}
		}
		if (scene) {
			// The final scene postimage is serialized by the one native action
			// after its in-memory steps and before the first project-file write.
		} else if (const Ref<Resource> *resource = resources.getptr(path)) {
			if (ResourceSaver::save(*resource, record.stage_path, ResourceSaver::FLAG_OMIT_EDITOR_PROPERTIES) != OK) {
				cleanup(r_prepared);
				return fail("resource_serialize_failed", "The Resource postimage could not be serialized.", r_error_code, r_error_message, ERR_CANT_CREATE);
			}
		} else if (const PackedByteArray *script = scripts.getptr(path)) {
			if (write_file(record.stage_path, *script) != OK) {
				cleanup(r_prepared);
				return fail("script_stage_failed", "The GDScript postimage could not be staged.", r_error_code, r_error_message, ERR_CANT_CREATE);
			}
		}
		total_bytes += scene ? 0 : FileAccess::get_size(record.stage_path);
		if (total_bytes > MAX_TOTAL_STAGED_BYTES) {
			cleanup(r_prepared);
			return fail("change_set_too_large", "The staged postimages exceed the 4 MiB aggregate limit.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
		}
		record.postimage_digest = scene ? String() : file_digest(record.stage_path);
		r_prepared.files.push_back(record);
		file_index++;
	}
	r_prepared.staged = true;
	return OK;
}

Error ScopedPersistenceExecutor::commit(Prepared &r_prepared, String &r_error_code, String &r_error_message) {
	if (!r_prepared.staged) {
		return fail("persistence_state_invalid", "The staged change set cannot enter this commit point.", r_error_code, r_error_message);
	}
	for (const FileRecord &record : r_prepared.files) {
		if (record.written) {
			return fail("persistence_state_invalid", "A committed postimage cannot be replayed without an exact restore.", r_error_code, r_error_message);
		}
		if (target_is_symlink(record.target_path) || (record.existed ? file_digest(record.target_path) != record.preimage_digest : FileAccess::exists(record.target_path))) {
			return fail("stale_file_hash", "A target changed after staging; no file was written.", r_error_code, r_error_message);
		}
	}
	int64_t total_bytes = 0;
	for (int index = 0; index < r_prepared.files.size(); index++) {
		FileRecord &record = r_prepared.files.write[index];
		if (record.scene && stage_scene_postimage(record, r_error_code, r_error_message) != OK) {
			return ERR_CANT_CREATE;
		}
		total_bytes += FileAccess::get_size(record.stage_path);
		if (total_bytes > MAX_TOTAL_STAGED_BYTES) {
			return fail("change_set_too_large", "The staged postimages exceed the 4 MiB aggregate limit.", r_error_code, r_error_message, ERR_OUT_OF_MEMORY);
		}
	}
	r_prepared.commit_point_entered = true;
	for (int index = 0; index < r_prepared.files.size(); index++) {
		FileRecord &record = r_prepared.files.write[index];
		record.written = true;
		PackedByteArray postimage;
		if (read_file(record.stage_path, postimage) != OK || ensure_parent(record.target_path) != OK || write_file(record.target_path, postimage) != OK || file_digest(record.target_path) != record.postimage_digest) {
			r_error_code = "persistence_failed";
			r_error_message = "A staged postimage could not be committed exactly.";
			String restore_code;
			String restore_message;
			restore(r_prepared, restore_code, restore_message);
			return ERR_CANT_CREATE;
		}
	}
	request_reload_barrier();
	return OK;
}

Error ScopedPersistenceExecutor::restore(Prepared &r_prepared, String &r_error_code, String &r_error_message) {
	for (const FileRecord &record : r_prepared.files) {
		if (record.written && file_digest(record.target_path) != record.postimage_digest) {
			return fail("rollback_blocked", "A committed file was externally modified; rollback will not overwrite it.", r_error_code, r_error_message, ERR_BUSY);
		}
	}
	for (int index = r_prepared.files.size() - 1; index >= 0; index--) {
		FileRecord &record = r_prepared.files.write[index];
		if (!record.written) {
			continue;
		}
		if (record.existed) {
			PackedByteArray preimage;
			if (read_file(record.escrow_path, preimage) != OK || write_file(record.target_path, preimage) != OK || file_digest(record.target_path) != record.preimage_digest) {
				return fail("rollback_in_doubt", "The exact file preimage could not be restored.", r_error_code, r_error_message, ERR_FILE_CORRUPT);
			}
		} else if (DirAccess::remove_absolute(ProjectSettings::get_singleton()->globalize_path(record.target_path)) != OK) {
			return fail("rollback_in_doubt", "A created file could not be removed.", r_error_code, r_error_message, ERR_CANT_CREATE);
		}
		record.written = false;
	}
	request_reload_barrier();
	return OK;
}

bool ScopedPersistenceExecutor::verify_postimages(const Prepared &p_prepared) {
	if (!p_prepared.staged || !p_prepared.commit_point_entered) {
		return false;
	}
	for (const FileRecord &record : p_prepared.files) {
		if (!record.written || target_is_symlink(record.target_path) || file_digest(record.target_path) != record.postimage_digest) {
			return false;
		}
	}
	return !p_prepared.files.is_empty();
}

bool ScopedPersistenceExecutor::verify_preimages(const Prepared &p_prepared) {
	if (!p_prepared.staged) {
		return false;
	}
	for (const FileRecord &record : p_prepared.files) {
		if (record.written || target_is_symlink(record.target_path)) {
			return false;
		}
		if (record.existed) {
			if (file_digest(record.target_path) != record.preimage_digest) {
				return false;
			}
		} else if (FileAccess::exists(record.target_path)) {
			return false;
		}
	}
	return !p_prepared.files.is_empty();
}

void ScopedPersistenceExecutor::cleanup(Prepared &r_prepared) {
	for (const FileRecord &record : r_prepared.files) {
		if (!record.stage_path.is_empty()) {
			DirAccess::remove_absolute(ProjectSettings::get_singleton()->globalize_path(record.stage_path));
		}
		if (!record.escrow_path.is_empty()) {
			DirAccess::remove_absolute(ProjectSettings::get_singleton()->globalize_path(record.escrow_path));
		}
	}
	if (!r_prepared.stage_directory.is_empty()) {
		DirAccess::remove_absolute(ProjectSettings::get_singleton()->globalize_path(r_prepared.stage_directory));
	}
	if (!r_prepared.escrow_directory.is_empty()) {
		DirAccess::remove_absolute(ProjectSettings::get_singleton()->globalize_path(r_prepared.escrow_directory));
	}
	r_prepared.files.clear();
	r_prepared.staged = false;
}
