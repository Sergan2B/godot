# Sprint 2 live acceptance checklist

**Status:** AUTOMATED PASS on Windows x86_64 — optional external Codex UX gate pending

## Environment

- [x] Windows 11 `10.0.26200`, x86_64 host recorded
- [x] custom Godot commit/build command recorded in `SPRINT-2-EVIDENCE.md`
- [x] `godot-codex-mcp 0.1.0` recorded
- [x] canonical fixture path `tests/codex/fixtures/smoke_project` recorded
- [x] automated `sprint2_live_smoke.py` evidence attached

## Automated model-free gate

- [x] `tests/codex/evidence/sprint-2-live-smoke.json` reports `status: pass`
- [x] first value is `275.0`, second value is `310.0`, disk value is `240.0`
- [x] second `event_seq` and `scene_revision` are larger
- [x] snapshot IDs differ
- [x] editor and sidecar exit cleanly; session discovery/token/lock are removed

## External Codex gate

- [ ] start a new Codex task trusted for only the copied fixture project
- [ ] preserve the original prompt and the MCP tool-call list
- [ ] do not include NodePath, script path, property name/value, scene tree, or a
      screenshot in the prompt
- [ ] select `Player` (`CharacterBody2D`) and change `movement_speed` without save
- [ ] ask: `Что сейчас выбрано в открытом Godot и какие живые свойства этого узла важны?`
- [ ] repeat after a second unsaved value change

## Required assertions

- [x] scene identity and selected node identity are present
- [x] NodePath is exactly `Player`
- [x] type is exactly `CharacterBody2D`
- [x] owner path is exactly `.`
- [x] attached script is exactly `res://player.gd`
- [x] returned property value matches the Inspector and differs from disk
- [x] dirty state is explicit
- [x] `project_id`, `editor_session_id`, freshness, and scene revision are present
- [x] evidence source is `live_editor_property`/live editor snapshot
- [x] the second response contains the new value and a larger revision
- [x] exact canonical project binding prevents another project from affecting it

## Evidence to archive

- [x] normalized MCP observations for both requests in `sprint-2-live-smoke.json`
- [ ] pre/post editor snapshots or Inspector captures
- [ ] redacted protocol/tool-call trace
- [ ] short UX recording or screenshots
- [ ] this checklist updated to `PASS`, including operator and timestamp
