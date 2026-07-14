extends CharacterBody2D

@export var display_name: String = "Smoke Player"
@export var movement_speed: float = 240.0


func _ready() -> void:
	print("Codex smoke fixture ready: %s" % display_name)
