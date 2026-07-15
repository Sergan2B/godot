@tool
extends SceneTree

const UID_UNIQUE_LEAF := "uid://e"
const UID_FAN_IN_B := "uid://h"
const UID_BINARY_SCENE := "uid://r"


func _initialize() -> void:
	call_deferred("_run")


func _run() -> void:
	var arguments := _parse_arguments(OS.get_cmdline_user_args())
	var mode: String = arguments.get("mode", "")
	var exit_code := 0

	if mode == "materialize":
		exit_code = _materialize_binary_resources(arguments)
	elif mode == "probe":
		exit_code = await _probe(arguments)
	else:
		push_error("resource graph probe requires --mode materialize or --mode probe")
		exit_code = 2

	quit(exit_code)


func _parse_arguments(arguments: PackedStringArray) -> Dictionary:
	var parsed := {}
	var index := 0
	while index < arguments.size():
		var argument := arguments[index]
		if not argument.begins_with("--"):
			index += 1
			continue
		var key := argument.trim_prefix("--")
		var value := "true"
		if index + 1 < arguments.size() and not arguments[index + 1].begins_with("--"):
			value = arguments[index + 1]
			index += 1
		parsed[key] = value
		index += 1
	return parsed


func _materialize_binary_resources(arguments: Dictionary) -> int:
	var shared_path: String = arguments.get("shared-path", "res://resources/shared_leaf.tres")
	var shared := ResourceLoader.load(shared_path)
	if shared == null:
		push_error("cannot load shared fixture resource")
		return 3

	var unique_leaf := Resource.new()
	unique_leaf.resource_name = "unique_leaf"
	unique_leaf.set_meta("value", 2)
	var error := ResourceSaver.save(unique_leaf, "res://resources/unique_leaf.res")
	if error != OK:
		push_error("cannot save unique binary fixture resource")
		return error
	error = ResourceSaver.set_uid(
		"res://resources/unique_leaf.res", ResourceUID.text_to_id(UID_UNIQUE_LEAF)
	)
	if error != OK:
		push_error("cannot set unique binary fixture UID")
		return error

	var fan_in_b := Resource.new()
	fan_in_b.resource_name = "fan_in_b"
	fan_in_b.set_meta("dependency", shared)
	error = ResourceSaver.save(fan_in_b, "res://resources/fan_in_b.res")
	if error != OK:
		push_error("cannot save fan-in binary fixture resource")
		return error
	error = ResourceSaver.set_uid(
		"res://resources/fan_in_b.res", ResourceUID.text_to_id(UID_FAN_IN_B)
	)
	if error != OK:
		push_error("cannot set fan-in binary fixture UID")
		return error

	var root := Node.new()
	root.name = "BinaryScene"
	root.set_meta("shared", shared)
	var packed_scene := PackedScene.new()
	error = packed_scene.pack(root)
	root.free()
	if error != OK:
		push_error("cannot pack binary fixture scene")
		return error
	error = ResourceSaver.save(packed_scene, "res://scenes/binary_scene.scn")
	if error != OK:
		push_error("cannot save binary fixture scene")
		return error
	error = ResourceSaver.set_uid(
		"res://scenes/binary_scene.scn", ResourceUID.text_to_id(UID_BINARY_SCENE)
	)
	if error != OK:
		push_error("cannot set binary fixture scene UID")
		return error

	return OK


func _probe(arguments: Dictionary) -> int:
	if not arguments.has("request") or not arguments.has("output"):
		push_error("probe mode requires --request and --output")
		return 2

	var request := _read_json(arguments["request"])
	if request.is_empty() or not request.has("paths") or not request["paths"] is Array:
		push_error("invalid resource graph probe request")
		return 2

	var editor_filesystem := EditorInterface.get_resource_filesystem()
	if editor_filesystem == null:
		push_error("editor filesystem is unavailable")
		return 3

	editor_filesystem.scan()
	while editor_filesystem.is_scanning() or editor_filesystem.is_importing():
		await process_frame

	var observations: Array[Dictionary] = []
	for value in request["paths"]:
		if not value is String or not value.begins_with("res://"):
			push_error("probe request contains a non-resource path")
			return 2
		observations.append(_observe_resource(value, editor_filesystem))

	observations.sort_custom(func(left: Dictionary, right: Dictionary) -> bool:
		return left["path"] < right["path"]
	)
	var output := {
		"schema_version": 1,
		"phase": request.get("phase", "unknown"),
		"resources": observations,
	}
	return _write_json(arguments["output"], output)


func _observe_resource(path: String, editor_filesystem: EditorFileSystem) -> Dictionary:
	var exists := FileAccess.file_exists(path) or ResourceLoader.exists(path)
	var uid_value := ResourceLoader.get_resource_uid(path) if exists else ResourceUID.INVALID_ID
	var uid = null if uid_value == ResourceUID.INVALID_ID else ResourceUID.id_to_text(uid_value)
	var resource_type := editor_filesystem.get_file_type(path)
	if resource_type.is_empty() and exists:
		resource_type = ResourceLoader.get_resource_type(path)

	var has_import_sidecar := FileAccess.file_exists(path + ".import")
	var import_state := "not_imported"
	if has_import_sidecar:
		import_state = "valid" if _editor_import_is_valid(path, editor_filesystem) else "invalid"

	var dependencies: Array[Dictionary] = []
	if exists:
		for raw_dependency in ResourceLoader.get_dependencies(path):
			dependencies.append(_parse_dependency(raw_dependency))
	dependencies.sort_custom(func(left: Dictionary, right: Dictionary) -> bool:
		return JSON.stringify(left) < JSON.stringify(right)
	)

	return {
		"dependencies": dependencies,
		"exists": exists,
		"import_state": import_state,
		"imported": has_import_sidecar,
		"path": path,
		"type": resource_type,
		"uid": uid,
	}


func _editor_import_is_valid(path: String, editor_filesystem: EditorFileSystem) -> bool:
	var directory := editor_filesystem.get_filesystem_path(path.get_base_dir())
	if directory == null:
		return false
	var file_index := directory.find_file_index(path.get_file())
	if file_index < 0:
		return false
	return directory.get_file_import_is_valid(file_index)


func _parse_dependency(raw_dependency: String) -> Dictionary:
	var target_uid = null
	var fallback_path := raw_dependency
	if raw_dependency.contains("::"):
		var parts := raw_dependency.split("::", true, 2)
		target_uid = parts[0]
		fallback_path = parts[2]

	var resolved_path = null
	var resolution := "missing"
	if target_uid != null:
		var uid_value := ResourceUID.text_to_id(target_uid)
		if uid_value != ResourceUID.INVALID_ID and ResourceUID.has_id(uid_value):
			var candidate := ResourceUID.get_id_path(uid_value)
			if FileAccess.file_exists(candidate) or ResourceLoader.exists(candidate):
				resolved_path = candidate
				resolution = "resolved"
			else:
				resolution = "stale_uid"
		else:
			resolution = "stale_uid"
	elif FileAccess.file_exists(fallback_path) or ResourceLoader.exists(fallback_path):
		resolved_path = fallback_path
		resolution = "resolved"

	return {
		"fallback_path": fallback_path,
		"raw": raw_dependency,
		"resolution": resolution,
		"resolved_path": resolved_path,
		"uid": target_uid,
	}


func _read_json(path: String) -> Dictionary:
	var file := FileAccess.open(path, FileAccess.READ)
	if file == null:
		return {}
	var value = JSON.parse_string(file.get_as_text())
	return value if value is Dictionary else {}


func _write_json(path: String, value: Dictionary) -> int:
	var file := FileAccess.open(path, FileAccess.WRITE)
	if file == null:
		push_error("cannot open resource graph probe output")
		return 3
	file.store_string(JSON.stringify(value, "  ", true) + "\n")
	return OK
