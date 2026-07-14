@tool
extends EditorPlugin

const PHASE_PATH := "res://.godot/codex-sprint2-phase.json"
const NEXT_PATH := "res://.godot/codex-sprint2-next"
const DONE_PATH := "res://.godot/codex-sprint2-done"
const FIRST_VALUE := 275.0
const SECOND_VALUE := 310.0

var _player: Node
var _phase := 0


func _enter_tree() -> void:
	if OS.get_environment("CODEX_SPRINT2_AUTOMATION") != "1":
		return
	_cleanup_markers()
	set_process(true)
	_prepare_phase_one.call_deferred()


func _exit_tree() -> void:
	set_process(false)


func _process(_delta: float) -> void:
	if _phase == 1 and FileAccess.file_exists(NEXT_PATH):
		_remove(NEXT_PATH)
		_apply_value(SECOND_VALUE, 2)
	elif _phase == 2 and FileAccess.file_exists(DONE_PATH):
		_cleanup_markers()
		get_tree().quit()


func _prepare_phase_one() -> void:
	# EditorPlugin enters the tree before the saved editor layout finishes
	# reopening scenes. Applying the smoke edit during that window lets the
	# later layout restore replace the edited root and selection. Wait for the
	# editor main loop to settle, then resolve the live root that will remain.
	await get_tree().create_timer(1.0).timeout
	for frame in range(5):
		await get_tree().process_frame
	var scene_root := EditorInterface.get_edited_scene_root()
	if scene_root == null:
		EditorInterface.open_scene_from_path("res://main.tscn")
		for attempt in range(200):
			await get_tree().create_timer(0.05).timeout
			scene_root = EditorInterface.get_edited_scene_root()
			if scene_root != null:
				break
	if scene_root == null:
		_fail("main scene did not open")
		return
	_player = scene_root.get_node_or_null("Player")
	if _player == null or not (_player is CharacterBody2D):
		_fail("Player CharacterBody2D is missing")
		return
	EditorInterface.get_selection().clear()
	EditorInterface.get_selection().add_node(_player)
	_apply_value(FIRST_VALUE, 1)


func _apply_value(value: float, phase: int) -> void:
	var previous: Variant = _player.get("movement_speed")
	var undo_redo := EditorInterface.get_editor_undo_redo()
	undo_redo.create_action("Codex Sprint 2 smoke phase %d" % phase)
	undo_redo.add_do_property(_player, "movement_speed", value)
	undo_redo.add_undo_property(_player, "movement_speed", previous)
	undo_redo.commit_action()
	EditorInterface.mark_scene_as_unsaved()
	_phase = phase
	_write_phase({
		"phase": phase,
		"node_path": "Player",
		"script_path": "res://player.gd",
		"value": value,
	})


func _write_phase(payload: Dictionary) -> void:
	var file := FileAccess.open(PHASE_PATH, FileAccess.WRITE)
	if file == null:
		_fail("cannot write phase marker")
		return
	file.store_string(JSON.stringify(payload, "", true, true))
	file.close()


func _fail(message: String) -> void:
	if message != "cannot write phase marker":
		_write_phase({"error": message})
	push_error("Codex Sprint 2 smoke automation failed: %s" % message)
	set_process(false)


func _cleanup_markers() -> void:
	_remove(PHASE_PATH)
	_remove(NEXT_PATH)
	_remove(DONE_PATH)


func _remove(path: String) -> void:
	if FileAccess.file_exists(path):
		DirAccess.remove_absolute(ProjectSettings.globalize_path(path))
