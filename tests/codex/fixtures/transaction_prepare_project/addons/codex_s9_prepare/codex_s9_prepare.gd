@tool
extends EditorPlugin

const PHASE_PATH := "res://.godot/codex-s9-prepare-phase.json"
const VERIFY_PATH := "res://.godot/codex-s9-prepare-verify"
const CHANGE_PATH := "res://.godot/codex-s9-prepare-change"
const BOUNDED_PATH := "res://.godot/codex-s9-prepare-bounded"
const OVERSIZED_PATH := "res://.godot/codex-s9-prepare-oversized"
const DONE_PATH := "res://.godot/codex-s9-prepare-done"
const NATIVE_UNDO_PATH := "res://.godot/codex-s9-native-undo"
const NATIVE_REDO_PATH := "res://.godot/codex-s9-native-redo"
const ORACLE_SNAPSHOT_PATH := "res://.godot/codex-s9-oracle-private.json"

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
	if FileAccess.file_exists(NATIVE_UNDO_PATH):
		_remove(NATIVE_UNDO_PATH)
		var undo_manager := EditorInterface.get_editor_undo_redo()
		var undo_history := undo_manager.get_history_undo_redo(undo_manager.get_object_history_id(_player))
		undo_history.undo()
		undo_manager.version_changed.emit()
		_probe_seq += 1
		_write_phase()
	elif FileAccess.file_exists(NATIVE_REDO_PATH):
		_remove(NATIVE_REDO_PATH)
		var redo_manager := EditorInterface.get_editor_undo_redo()
		var redo_history := redo_manager.get_history_undo_redo(redo_manager.get_object_history_id(_player))
		redo_history.redo()
		redo_manager.version_changed.emit()
		_probe_seq += 1
		_write_phase()
	elif FileAccess.file_exists(VERIFY_PATH):
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
	_probe_seq += 1
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
	_write_oracle_snapshot()


func _write_oracle_snapshot() -> void:
	var undo_manager := EditorInterface.get_editor_undo_redo()
	var history_id := undo_manager.get_object_history_id(_player)
	var history := undo_manager.get_history_undo_redo(history_id)
	var selection: Array[String] = []
	var selected_object_ids: Array[String] = []
	for selected in EditorInterface.get_selection().get_selected_nodes():
		var selected_node := selected as Node
		if selected_node == null:
			continue
		selection.push_back(str(_root.get_path_to(selected_node)))
		selected_object_ids.push_back(str(selected_node.get_instance_id()))
	selection.sort()
	selected_object_ids.sort()
	var object_ids: Array[String] = []
	for item in _tree_shape(_root):
		var node := _root.get_node_or_null(NodePath(item["path"]))
		if node != null:
			object_ids.push_back(str(node.get_instance_id()))
	object_ids.sort()
	var file := FileAccess.open(ORACLE_SNAPSHOT_PATH, FileAccess.WRITE)
	if file == null:
		_fail("cannot write oracle snapshot")
		return
	file.store_string(JSON.stringify({
		"schema_version": "s9-transaction-private-snapshot/1.0",
		"probe_seq": _probe_seq,
		"tree": _tree_shape(_root),
		"connections": _connections(),
		"selection": selection,
		"property_hashes": {
			"Deletable.secret_value": _sha256_text(str(_root.get_node("Deletable").get("secret_value"))) if _root.has_node("Deletable") else "",
			"Deletable/Descendant.secret_value": _sha256_text(str(_root.get_node("Deletable/Descendant").get("secret_value"))) if _root.has_node("Deletable/Descendant") else "",
		},
		"native_probe": {
			"history_id": history_id,
			"history_action": history.get_current_action() if history != null else -1,
			"history_count": history.get_history_count() if history != null else 0,
			"history_version": history.get_version() if history != null else 0,
			"object_ids": object_ids,
			"selected_object_ids": selected_object_ids,
		},
	}, "", true, true))
	file.close()


func _sha256_text(value: String) -> String:
	var context := HashingContext.new()
	context.start(HashingContext.HASH_SHA256)
	context.update(value.to_utf8_buffer())
	return context.finish().hex_encode()


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
			"replacement_value": _property_or_null(current, &"replacement_value"),
			"transform": _transform_evidence(current),
		})
		for child in current.get_children():
			pending.push_back(child)
	return result


func _property_or_null(node: Node, property: StringName) -> Variant:
	for candidate in node.get_property_list():
		if candidate["name"] == property:
			return node.get(property)
	return null


func _transform_evidence(node: Node) -> Dictionary:
	if node is Node2D:
		var node_2d := node as Node2D
		return {
			"kind": "2d",
			"local": _transform_2d_values(node_2d.transform),
			"global": _transform_2d_values(node_2d.global_transform),
		}
	if node is Node3D:
		var node_3d := node as Node3D
		return {
			"kind": "3d",
			"local": _transform_3d_values(node_3d.transform),
			"global": _transform_3d_values(node_3d.global_transform),
		}
	return {}


func _transform_2d_values(value: Transform2D) -> Array[float]:
	return [value.x.x, value.x.y, value.y.x, value.y.y, value.origin.x, value.origin.y]


func _transform_3d_values(value: Transform3D) -> Array[float]:
	return [
		value.basis.x.x, value.basis.x.y, value.basis.x.z,
		value.basis.y.x, value.basis.y.y, value.basis.y.z,
		value.basis.z.x, value.basis.z.y, value.basis.z.z,
		value.origin.x, value.origin.y, value.origin.z,
	]


func _connections() -> Array[Dictionary]:
	var result: Array[Dictionary] = []
	for emitter_name in [&"Emitter", &"ReparentTarget", &"Deletable"]:
		var emitter := _root.find_child(emitter_name, true, false)
		if emitter == null:
			continue
		var signal_names: Array[StringName] = []
		signal_names.push_back(&"pulse" if emitter_name == &"Emitter" else &"tree_entered")
		for signal_name in signal_names:
			for connection in emitter.get_signal_connection_list(signal_name):
				var callable: Callable = connection["callable"]
				var receiver := callable.get_object() as Node
				result.push_back({
					"emitter": str(_root.get_path_to(emitter)),
					"signal": str(signal_name),
					"receiver": str(_root.get_path_to(receiver)) if receiver != null else "",
					"method": str(callable.get_method()),
					"flags": connection["flags"],
					"unbinds": callable.get_unbound_arguments_count(),
					"binds": callable.get_bound_arguments(),
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
	for path in [PHASE_PATH, VERIFY_PATH, CHANGE_PATH, BOUNDED_PATH, OVERSIZED_PATH, DONE_PATH, NATIVE_UNDO_PATH, NATIVE_REDO_PATH, ORACLE_SNAPSHOT_PATH]:
		_remove(path)


func _remove(path: String) -> void:
	if FileAccess.file_exists(path):
		DirAccess.remove_absolute(ProjectSettings.globalize_path(path))
