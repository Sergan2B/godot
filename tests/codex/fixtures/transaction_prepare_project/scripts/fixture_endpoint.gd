@tool
extends Node

signal pulse

@export var custom_value := 7
@export var secret_value := ""


func _on_pulse() -> void:
	pass


func _on_bound(_value: String) -> void:
	pass


func _on_tree_entered() -> void:
	pass
