extends Node

var getter_count: int = 0
var last_get_index: int = -1


func reset_counts() -> void:
	getter_count = 0
	last_get_index = -1


func _get_property_list() -> Array[Dictionary]:
	var properties: Array[Dictionary] = []
	for index in range(513):
		properties.append({
			"name": "bounded_%04d" % index,
			"type": TYPE_INT,
			"usage": PROPERTY_USAGE_EDITOR,
		})
	return properties


func _get(property: StringName) -> Variant:
	var name := String(property)
	if not name.begins_with("bounded_"):
		return null
	var index := name.trim_prefix("bounded_").to_int()
	getter_count += 1
	last_get_index = max(last_get_index, index)
	return index
