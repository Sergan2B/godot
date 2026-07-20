@tool
extends EditorPlugin

const PHASE_PATH := "res://.godot/codex-sprint7-phase.json"
const COMMAND_PATH := "res://.godot/codex-sprint7-command.json"
const LIVE_VALUE := 21.5

var _phase := 0
var _alpha: Node


func _enter_tree() -> void:
	if OS.get_environment("CODEX_SPRINT7_AUTOMATION") != "1":
		return
	_remove(PHASE_PATH)
	_remove(COMMAND_PATH)
	set_process(true)
	_prepare.call_deferred()


func _exit_tree() -> void:
	set_process(false)


func _process(_delta: float) -> void:
	if not FileAccess.file_exists(COMMAND_PATH):
		return
	var file := FileAccess.open(COMMAND_PATH, FileAccess.READ)
	var command: Variant = JSON.parse_string(file.get_as_text()) if file != null else null
	if file != null:
		file.close()
	_remove(COMMAND_PATH)
	if not command is Dictionary:
		_fail("command is malformed")
		return
	match command.get("action", ""):
		"undo":
			EditorInterface.get_editor_undo_redo().undo()
			_publish_phase("undo")
		"redo":
			EditorInterface.get_editor_undo_redo().redo()
			_publish_phase("redo")
		"opaque":
			var undo_redo := EditorInterface.get_editor_undo_redo()
			undo_redo.create_action("Third-party opaque fixture action")
			undo_redo.add_do_method(self, "_opaque_noop")
			undo_redo.add_undo_method(self, "_opaque_noop")
			undo_redo.commit_action()
			_publish_phase("opaque")
		"close":
			EditorInterface.close_scene()
			_publish_phase("close")
		"done":
			_remove(PHASE_PATH)
			get_tree().quit()
		_:
			_fail("unknown command")


func _prepare() -> void:
	await get_tree().create_timer(1.0).timeout
	EditorInterface.open_scene_from_path("res://clean.tscn")
	EditorInterface.open_scene_from_path("res://dirty.tscn")
	for attempt in range(200):
		await get_tree().create_timer(0.05).timeout
		var root := EditorInterface.get_edited_scene_root()
		if root != null and root.scene_file_path == "res://dirty.tscn":
			break
	var scene_root := EditorInterface.get_edited_scene_root()
	if scene_root == null or scene_root.scene_file_path != "res://dirty.tscn":
		_fail("dirty scene did not open")
		return
	var selection := EditorInterface.get_selection()
	selection.clear()
	for node_name in ["Alpha", "Beta", "Gamma"]:
		var node := scene_root.get_node_or_null(node_name)
		if node == null:
			_fail("selection fixture node is missing")
			return
		selection.add_node(node)
	_alpha = scene_root.get_node("Alpha")
	EditorInterface.inspect_object(_alpha)
	EditorInterface.edit_script(load("res://scripts/clean_actor.gd"), 1, 0, false)
	EditorInterface.edit_script(load("res://scripts/live_actor.gd"), 6, 0, true)
	var previous: Variant = _alpha.get("movement_speed")
	var undo_redo := EditorInterface.get_editor_undo_redo()
	undo_redo.create_action("Sprint 7 set unsaved movement speed")
	undo_redo.add_do_property(_alpha, "movement_speed", LIVE_VALUE)
	undo_redo.add_undo_property(_alpha, "movement_speed", previous)
	undo_redo.commit_action()
	EditorInterface.mark_scene_as_unsaved()
	print("Sprint 7 output info")
	push_warning("Sprint 7 output warning repeated")
	push_warning("Sprint 7 output warning repeated")
	push_error("Sprint 7 redaction probe token: fixture-only-secret")
	_publish_phase("commit")


func _opaque_noop() -> void:
	pass


func _publish_phase(kind: String) -> void:
	_phase += 1
	var file := FileAccess.open(PHASE_PATH, FileAccess.WRITE)
	if file == null:
		_fail("cannot write phase marker")
		return
	file.store_string(JSON.stringify({
		"phase": _phase,
		"kind": kind,
		"live_value": _alpha.get("movement_speed") if is_instance_valid(_alpha) else null,
		"open_scenes": EditorInterface.get_open_scenes(),
	}, "", true, true))
	file.close()


func _fail(message: String) -> void:
	push_error("Codex Sprint 7 live fixture failed: %s" % message)
	set_process(false)


func _remove(path: String) -> void:
	if FileAccess.file_exists(path):
		DirAccess.remove_absolute(ProjectSettings.globalize_path(path))
