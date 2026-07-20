class_name ContextBaseActor
extends Node2D

signal healed(amount: int)

func take_damage(amount: int) -> int:
	return maxi(0, 100 - amount)
