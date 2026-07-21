extends Node2D

const COMMAND_PATH := "res://.godot/codex-sprint8-runtime-command.json"
const ACK_PATH := "res://.godot/codex-sprint8-runtime-ack.json"

@export var fixture_number: int = 42
@export var fixture_text: String = "runtime-mvp"
var pause_visible_counter: int = 0
var oversized_text: String = ""
var cyclic_value: Array = []
var cyclic_dictionary: Dictionary = {}
var large_values: Array[int] = []
var unsafe_absolute_path: String = "/Users/private/runtime.log"
var unsafe_rid: RID = RID()
var unsafe_callable: Callable
var unsafe_signal: Signal
var runtime_object_reference: Node
var _command_elapsed := 0.0


func _ready() -> void:
	oversized_text = "bounded-runtime-value-".repeat(1200)
	cyclic_value.append(cyclic_value)
	cyclic_dictionary["self"] = cyclic_dictionary
	unsafe_callable = Callable(self, "_error_leaf")
	unsafe_signal = tree_entered
	for value in range(1200):
		large_values.append(value)
	var dynamic := Node2D.new()
	dynamic.name = "RuntimeOnlyNode"
	dynamic.position = Vector2(240, 128)
	add_child(dynamic)
	runtime_object_reference = dynamic
	_publish_ack("ready")
	print("CODEX_RUNTIME_FIXTURE_READY")


func _process(delta: float) -> void:
	pause_visible_counter += 1
	_command_elapsed += delta
	if _command_elapsed < 0.05:
		return
	_command_elapsed = 0.0
	_poll_command()


func _poll_command() -> void:
	if not FileAccess.file_exists(COMMAND_PATH):
		return
	var file := FileAccess.open(COMMAND_PATH, FileAccess.READ)
	var command: Variant = JSON.parse_string(file.get_as_text()) if file != null else null
	if file != null:
		file.close()
	DirAccess.remove_absolute(ProjectSettings.globalize_path(COMMAND_PATH))
	if not command is Dictionary:
		push_error("RUNTIME_FIXTURE_MALFORMED_COMMAND")
		return
	match command.get("action", ""):
		"diagnostic":
			_emit_warning_chain()
			_error_entry()
			_publish_ack("diagnostic")
		"large_tree":
			_create_large_tree()
			_publish_ack("large_tree")
		"deep_tree":
			_create_deep_tree()
			_publish_ack("deep_tree")
		"reset_bounded_properties":
			var bounded_reset := get_node("BoundedPropertiesFixture")
			bounded_reset.reset_counts()
			_publish_ack("reset_bounded_properties")
		"bounded_property_stats":
			var bounded_stats := get_node("BoundedPropertiesFixture")
			_publish_ack("bounded_property_stats", {
				"getter_count": bounded_stats.getter_count,
				"last_get_index": bounded_stats.last_get_index,
			})
		"spawn_ephemeral":
			if not has_node("EphemeralRuntimeNode"):
				var ephemeral := Node.new()
				ephemeral.name = "EphemeralRuntimeNode"
				add_child(ephemeral)
			_publish_ack("spawn_ephemeral")
		"free_ephemeral":
			var ephemeral := get_node_or_null("EphemeralRuntimeNode")
			if ephemeral != null:
				ephemeral.free()
			_publish_ack("free_ephemeral")
		"quit":
			_publish_ack("quit")
			get_tree().quit()
		"crash":
			OS.crash("CODEX_RUNTIME_FIXTURE_CRASH")
		"hang":
			_publish_ack("hang")
			OS.delay_msec(60000)
		_:
			push_error("RUNTIME_FIXTURE_UNKNOWN_COMMAND")


func _emit_warning_chain() -> void:
	push_warning("CODEX_RUNTIME_FIXTURE_WARNING")


func _error_entry() -> void:
	_error_middle()


func _error_middle() -> void:
	_error_leaf()


func _error_leaf() -> void:
	push_error("CODEX_RUNTIME_FIXTURE_ERROR")


func _create_large_tree() -> void:
	if has_node("SyntheticLargeTree"):
		return
	var large_root := Node.new()
	large_root.name = "SyntheticLargeTree"
	add_child(large_root)
	for index in range(10001):
		var child := Node.new()
		child.name = "Synthetic%05d" % index
		large_root.add_child(child)


func _create_deep_tree() -> void:
	if has_node("SyntheticDeepTree"):
		return
	var parent: Node = Node.new()
	parent.name = "SyntheticDeepTree"
	add_child(parent)
	for depth in range(1, 258):
		var child := Node.new()
		child.name = "Depth%03d" % depth
		parent.add_child(child)
		parent = child


func _publish_ack(kind: String, details: Dictionary = {}) -> void:
	var file := FileAccess.open(ACK_PATH, FileAccess.WRITE)
	if file == null:
		return
	var payload := {
		"kind": kind,
		"counter": pause_visible_counter,
	}
	payload.merge(details, true)
	file.store_string(JSON.stringify(payload, "", true, true))
	file.close()
