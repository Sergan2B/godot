@tool
extends EditorPlugin

const READY_PATH := "res://.godot/codex-sprint8-editor-ready.json"
const COMMAND_PATH := "res://.godot/codex-sprint8-editor-command.json"


func _enter_tree() -> void:
	if OS.get_environment("CODEX_SPRINT8_AUTOMATION") != "1":
		return
	_remove(READY_PATH)
	_remove(COMMAND_PATH)
	_prepare.call_deferred()
	_watch_commands.call_deferred()


func _prepare() -> void:
	await get_tree().create_timer(0.5).timeout
	EditorInterface.open_scene_from_path("res://main.tscn")
	for attempt in range(200):
		await get_tree().create_timer(0.05).timeout
		var root := EditorInterface.get_edited_scene_root()
		if root != null and root.scene_file_path == "res://main.tscn":
			var file := FileAccess.open(READY_PATH, FileAccess.WRITE)
			if file != null:
				file.store_string(JSON.stringify({"status": "ready", "scene": root.scene_file_path}, "", true, true))
				file.close()
			return
	push_error("Codex Sprint 8 fixture could not open main scene")


func _watch_commands() -> void:
	while is_inside_tree():
		await get_tree().create_timer(0.05).timeout
		if not FileAccess.file_exists(COMMAND_PATH):
			continue
		var file := FileAccess.open(COMMAND_PATH, FileAccess.READ)
		var command: Variant = JSON.parse_string(file.get_as_text()) if file != null else null
		if file != null:
			file.close()
		_remove(COMMAND_PATH)
		if not command is Dictionary:
			continue
		match command.get("action", ""):
			"manual_run":
				EditorInterface.play_main_scene()
			"manual_stop":
				EditorInterface.stop_playing_scene()
			"done":
				_remove(READY_PATH)
				get_tree().quit()


func _remove(path: String) -> void:
	if FileAccess.file_exists(path):
		DirAccess.remove_absolute(ProjectSettings.globalize_path(path))
