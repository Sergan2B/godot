class_name ContextPlayer
extends ContextBaseActor

const PROFILE = preload("res://resources/shared_profile.tres")

@export var dynamic_method: StringName = &"take_damage"

func take_damage(amount: int) -> int:
	return super.take_damage(amount)

func apply_damage(amount: int) -> int:
	return take_damage(amount)

func dynamic_damage(target: Object, amount: int) -> Variant:
	return target.call(dynamic_method, amount)

func recover(amount: int) -> void:
	healed.emit(amount)
