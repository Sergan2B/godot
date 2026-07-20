@tool
extends Node2D

@export var display_name: String = "disk"
@export var movement_speed: float = 8.0
@export var fixture_resource: Resource


func fixture_method() -> String:
	return display_name
