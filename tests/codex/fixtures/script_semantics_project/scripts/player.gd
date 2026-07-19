extends "res://scripts/base_actor.gd"
class_name Player

signal healed(amount: int)

enum Weapon {
	SWORD,
	STAFF,
}

class Inventory:
	var slots: int = 4

	func add(item) -> bool:
		var accepted = item != null
		return accepted

const DAMAGE_PROFILE = preload("res://resources/damage_profile.tres")
@export var display_name: String = "Café 🚀"
var metadata = {}
var dynamic_path: String = "res://resources/damage_profile.tres"

func take_damage(amount: int) -> int:
	var remaining := super.take_damage(amount)
	return _apply_bonus(remaining)

func _apply_bonus(value: int) -> int:
	var adjust := func(delta: int) -> int:
		return value + delta
	return adjust.call(1)

func exact_load_uid() -> Resource:
	return load("uid://b3iupk70ub7mc")

func dynamic_actions(target: Object, method_name: StringName, path: String, node_path: NodePath) -> void:
	target.call(method_name, 1)
	load(path)
	get_node(node_path)
	var marker := $Marker
	print(marker)
