@tool
extends SceneTree

const MUTATE_MARKER_PREFIX := "res://.godot/codex-resource-live-mutate"
const DONE_MARKER := "res://.godot/codex-resource-live-done"
const LIVE_RESOURCE := "res://resources/live_delta.tres"


func _initialize() -> void:
	call_deferred("_run")


func _run() -> void:
	var arguments := OS.get_cmdline_user_args()
	var external_mutation := arguments.has("--external-mutation")
	var mutation_count := 1
	for argument in arguments:
		if argument.begins_with("--mutation-count="):
			mutation_count = maxi(1, argument.trim_prefix("--mutation-count=").to_int())
	var editor_filesystem := EditorInterface.get_resource_filesystem()
	if editor_filesystem == null:
		push_error("resource graph live driver cannot access EditorFileSystem")
		quit(2)
		return

	while editor_filesystem.is_scanning() or editor_filesystem.is_importing():
		await process_frame
	for mutation_index in mutation_count:
		var marker := MUTATE_MARKER_PREFIX
		if mutation_count > 1:
			marker += "-%d" % (mutation_index + 1)
		while not FileAccess.file_exists(marker):
			await process_frame

		if not external_mutation and mutation_index == 0:
			var resource := Resource.new()
			resource.resource_name = "codex_resource_live_delta"
			resource.set_meta("revision", 2)
			var error := ResourceSaver.save(resource, LIVE_RESOURCE)
			if error != OK:
				push_error("resource graph live driver could not save the mutation")
				quit(error)
				return
		if external_mutation:
			editor_filesystem.scan()
		else:
			editor_filesystem.scan_sources()
		while editor_filesystem.is_scanning() or editor_filesystem.is_importing():
			await process_frame

	while not FileAccess.file_exists(DONE_MARKER):
		await process_frame
	quit(0)
