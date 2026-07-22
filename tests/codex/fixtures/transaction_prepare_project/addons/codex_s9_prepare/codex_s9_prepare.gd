@tool
extends EditorPlugin

const PHASE_PATH := "res://.godot/codex-s9-prepare-phase.json"
const VERIFY_PATH := "res://.godot/codex-s9-prepare-verify"
const CHANGE_PATH := "res://.godot/codex-s9-prepare-change"
const BOUNDED_PATH := "res://.godot/codex-s9-prepare-bounded"
const OVERSIZED_PATH := "res://.godot/codex-s9-prepare-oversized"
const DONE_PATH := "res://.godot/codex-s9-prepare-done"

var _phase := 0
var _probe_seq := 0
var _root: Node
var _player: Node2D


func _enter_tree() -> void:
	if OS.get_environment("CODEX_S9_PREPARE_AUTOMATION") != "1":
		return
	_cleanup_markers()
	set_process(true)
	_prepare.call_deferred()


func _exit_tree() -> void:
	set_process(false)


func _process(_delta: float) -> void:
	if _phase == 1 and FileAccess.file_exists(VERIFY_PATH):
		_remove(VERIFY_PATH)
		_probe_seq += 1
		_write_phase()
	elif _phase == 1 and FileAccess.file_exists(CHANGE_PATH):
		_remove(CHANGE_PATH)
		_commit_position(Vector2(9, 13), 2)
	elif _phase == 2 and FileAccess.file_exists(BOUNDED_PATH):
		_remove(BOUNDED_PATH)
		_add_runtime_children(900, "Bounded")
		_phase = 3
		_write_phase()
	elif _phase == 3 and FileAccess.file_exists(OVERSIZED_PATH):
		_remove(OVERSIZED_PATH)
		_add_runtime_children(101, "Oversized")
		_phase = 4
		_write_phase()
	elif FileAccess.file_exists(DONE_PATH):
		_cleanup_markers()
		get_tree().quit()


func _prepare() -> void:
	await get_tree().create_timer(1.0).timeout
	for frame in range(5):
		await get_tree().process_frame
	_root = EditorInterface.get_edited_scene_root()
	if _root == null:
		EditorInterface.open_scene_from_path("res://main.tscn")
		for attempt in range(200):
			await get_tree().create_timer(0.05).timeout
			_root = EditorInterface.get_edited_scene_root()
			if _root != null:
				break
	if _root == null:
		_fail("main scene did not open")
		return
	_player = _root.get_node_or_null("Player") as Node2D
	if _player == null:
		_fail("Player Node2D is missing")
		return
	EditorInterface.get_selection().clear()
	EditorInterface.get_selection().add_node(_player)
	_phase = 1
	_write_phase()


func _commit_position(value: Vector2, phase: int) -> void:
	var undo_redo := EditorInterface.get_editor_undo_redo()
	undo_redo.create_action("Codex Sprint 9 fixture phase %d" % phase)
	undo_redo.add_do_property(_player, "position", value)
	undo_redo.add_undo_property(_player, "position", _player.position)
	undo_redo.commit_action()
	EditorInterface.mark_scene_as_unsaved()
	_phase = phase
	_write_phase()


func _add_runtime_children(count: int, prefix: String) -> void:
	var start := _root.get_child_count()
	for index in range(count):
		var node := Node.new()
		node.name = "%s%04d" % [prefix, start + index]
		_root.add_child(node)


func _write_phase() -> void:
	var file := FileAccess.open(PHASE_PATH, FileAccess.WRITE)
	if file == null:
		_fail("cannot write phase marker")
		return
	file.store_string(JSON.stringify({
		"phase": _phase,
		"probe_seq": _probe_seq,
		"position": [_player.position.x, _player.position.y],
		"node_count": _count_nodes(_root),
		"tree_shape": _tree_shape(_root),
		"connections": _connections(),
	}, "", true, true))
	file.close()


func _count_nodes(node: Node) -> int:
	var count := 1
	for child in node.get_children():
		count += _count_nodes(child)
	return count


func _tree_shape(node: Node) -> Array[Dictionary]:
	var result: Array[Dictionary] = []
	var pending: Array[Node] = [node]
	while not pending.is_empty():
		var current := pending.pop_front()
		var script := current.get_script() as Script
		var owner: Node = current.owner
		result.push_back({
			"path": str(node.get_path_to(current)),
			"type": current.get_class(),
			"index": current.get_index(),
			"owner": str(node.get_path_to(owner)) if owner != null else "",
			"script": script.resource_path if script != null else "",
			"custom_value": _property_or_null(current, &"custom_value"),
		})
		for child in current.get_children():
			pending.push_back(child)
	return result


func _property_or_null(node: Node, property: StringName) -> Variant:
	for candidate in node.get_property_list():
		if candidate["name"] == property:
			return node.get(property)
	return null


func _connections() -> Array[Dictionary]:
	var result: Array[Dictionary] = []
	var emitter := _root.get_node("Emitter")
	for signal_name in [&"pulse", &"tree_entered"]:
		for connection in emitter.get_signal_connection_list(signal_name):
			var callable: Callable = connection["callable"]
			var receiver := callable.get_object() as Node
			result.push_back({
				"signal": str(signal_name),
				"receiver": str(_root.get_path_to(receiver)) if receiver != null else "",
				"method": str(callable.get_method()),
				"flags": connection["flags"],
			})
	return result


func _fail(message: String) -> void:
	if message != "cannot write phase marker":
		var file := FileAccess.open(PHASE_PATH, FileAccess.WRITE)
		if file != null:
			file.store_string(JSON.stringify({"error": message}))
			file.close()
	push_error("Codex Sprint 9 prepare fixture failed: %s" % message)
	set_process(false)


func _cleanup_markers() -> void:
	for path in [PHASE_PATH, VERIFY_PATH, CHANGE_PATH, BOUNDED_PATH, OVERSIZED_PATH, DONE_PATH]:
		_remove(path)


func _remove(path: String) -> void:
	if FileAccess.file_exists(path):
		DirAccess.remove_absolute(ProjectSettings.globalize_path(path))
