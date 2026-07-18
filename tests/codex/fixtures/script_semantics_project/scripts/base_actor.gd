extends Node
class_name BaseActor

signal damaged(amount: int)

enum Mood {
	IDLE,
	HURT,
}

const DEFAULT_HEALTH: int = 100
@export var health: int = DEFAULT_HEALTH
var label = "base"

func take_damage(amount: int) -> int:
	var remaining: int = max(health - amount, 0)
	health = remaining
	damaged.emit(amount)
	return remaining

static func make_default() -> BaseActor:
	return BaseActor.new()
