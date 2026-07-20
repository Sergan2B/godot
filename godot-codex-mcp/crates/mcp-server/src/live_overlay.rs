use std::collections::{BTreeMap, BTreeSet};

use godot_codex_semantic_model::SemanticSnapshot;
use serde_json::{Value, json};
use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum LiveOverlayError {
    #[error("live editor snapshot belongs to another project")]
    ProjectMismatch,
}

#[derive(Clone, Debug)]
pub(crate) struct SceneOverlay {
    pub live_scene: Value,
    pub nodes: Vec<Value>,
    pub coverage_complete: bool,
    pub suppressed_disk_nodes: usize,
    pub live_only_nodes: usize,
    pub conflicts: Vec<Value>,
}

#[derive(Clone, Debug)]
pub(crate) struct NodeOverlay {
    pub live_scene: Value,
    pub live_node: Value,
    pub properties: Vec<Value>,
    pub conflicts: Vec<Value>,
    pub properties_complete: bool,
}

pub(crate) struct LiveOverlay<'a> {
    snapshot: &'a SemanticSnapshot,
}

impl<'a> LiveOverlay<'a> {
    pub(crate) fn bind(
        snapshot: &'a SemanticSnapshot,
        project_id: &str,
    ) -> Result<Self, LiveOverlayError> {
        if snapshot.project_id != project_id {
            return Err(LiveOverlayError::ProjectMismatch);
        }
        Ok(Self { snapshot })
    }

    pub(crate) fn snapshot(&self) -> &'a SemanticSnapshot {
        self.snapshot
    }

    pub(crate) fn scene_for_path(&self, path: &str) -> Option<&'a Value> {
        self.snapshot
            .scenes
            .values()
            .find(|scene| scene.get("path").and_then(Value::as_str) == Some(path))
    }

    pub(crate) fn compose_scene_nodes(
        &self,
        scene_path: &str,
        disk_nodes: Vec<Value>,
    ) -> Option<SceneOverlay> {
        let live_scene = self.scene_for_path(scene_path)?.clone();
        let live_scene_id = live_scene.get("entity_id")?.as_str()?;
        let coverage_complete = live_scene.get("coverage").and_then(Value::as_str)
            == Some("complete")
            && live_scene.get("nodes_truncated").and_then(Value::as_bool) != Some(true);
        let mut live_by_path: BTreeMap<String, Value> = self
            .snapshot
            .nodes
            .values()
            .filter(|node| node.get("scene_id").and_then(Value::as_str) == Some(live_scene_id))
            .filter_map(|node| {
                node.get("node_path")
                    .and_then(Value::as_str)
                    .map(|path| (path.to_owned(), node.clone()))
            })
            .collect();
        let mut composed = Vec::new();
        let mut conflicts = Vec::new();
        let mut suppressed_disk_nodes = 0;
        for disk_node in disk_nodes {
            let Some(path) = disk_node
                .get("node_path")
                .and_then(Value::as_str)
                .map(str::to_owned)
            else {
                if !coverage_complete {
                    composed.push(disk_node);
                }
                continue;
            };
            if let Some(live_node) = live_by_path.remove(&path) {
                composed.push(compose_node_value(disk_node, &live_node, &mut conflicts));
            } else if coverage_complete {
                suppressed_disk_nodes += 1;
            } else {
                composed.push(with_disk_source(disk_node, "live_node_not_observed"));
            }
        }
        let live_only_nodes = live_by_path.len();
        composed.extend(live_by_path.into_values().map(live_only_node_value));
        composed.sort_by(|left, right| {
            left.get("node_path")
                .and_then(Value::as_str)
                .cmp(&right.get("node_path").and_then(Value::as_str))
        });
        Some(SceneOverlay {
            live_scene,
            nodes: composed,
            coverage_complete,
            suppressed_disk_nodes,
            live_only_nodes,
            conflicts,
        })
    }

    pub(crate) fn compose_node_properties(
        &self,
        scene_path: &str,
        node_path: &str,
        disk_properties: Vec<Value>,
    ) -> Option<NodeOverlay> {
        let live_scene = self.scene_for_path(scene_path)?.clone();
        let live_scene_id = live_scene.get("entity_id")?.as_str()?;
        let live_node = self
            .snapshot
            .nodes
            .values()
            .find(|node| {
                node.get("scene_id").and_then(Value::as_str) == Some(live_scene_id)
                    && node.get("node_path").and_then(Value::as_str) == Some(node_path)
            })?
            .clone();
        let live_properties = live_node.get("properties").and_then(Value::as_array)?;
        let properties_complete = live_node
            .get("properties_truncated")
            .and_then(Value::as_bool)
            != Some(true);
        let mut disk_by_name: BTreeMap<String, Value> = disk_properties
            .into_iter()
            .filter_map(|property| {
                let name = property
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned)?;
                Some((name, property))
            })
            .collect();
        let mut composed = Vec::new();
        let mut conflicts = Vec::new();
        let mut observed_names = BTreeSet::new();
        for (property_index, live_property) in live_properties.iter().enumerate() {
            let Some(name) = live_property.get("name").and_then(Value::as_str) else {
                continue;
            };
            observed_names.insert(name.to_owned());
            let disk_property = disk_by_name.remove(name);
            if let Some(disk) = &disk_property {
                let disk_value = disk.pointer("/value/value");
                let editor_value = live_property.get("value");
                if disk_value != editor_value {
                    conflicts.push(json!({
                        "kind": "disk_live_conflict",
                        "field": format!("property:{name}"),
                        "disk_value": disk_value,
                        "editor_value": editor_value,
                        "resolution": "editor_wins",
                    }));
                }
            }
            composed.push(json!({
                "property_id": disk_property.as_ref().and_then(|value| value.get("property_id")).cloned().unwrap_or_else(|| json!(format!("live:{}:{property_index}", live_node.get("entity_id").and_then(Value::as_str).unwrap_or("node")))),
                "name": name,
                "value": {
                    "type": live_property.get("variant_type"),
                    "value": live_property.get("value"),
                    "truncated": live_property.pointer("/value/truncated").and_then(Value::as_bool).unwrap_or(false),
                },
                "origin": "live_editor",
                "authority": "editor_inspector",
                "freshness": "current",
                "scene_revision": live_property.get("scene_revision"),
                "disk_comparison": disk_property,
                "evidence": [
                    {"source": "live_editor_property", "freshness": "current"},
                    {"source": "persistent_segment_index", "retained_for_comparison": true}
                ],
            }));
        }
        // The Inspector exposes an editor-property projection, not an absence
        // oracle for every serialized property class. Unobserved disk facts
        // therefore remain visible even when the bounded projection completed.
        composed.extend(
            disk_by_name
                .into_values()
                .map(|property| with_disk_source(property, "not_covered_by_live_inspector")),
        );
        composed.sort_by(|left, right| {
            left.get("name")
                .and_then(Value::as_str)
                .cmp(&right.get("name").and_then(Value::as_str))
        });
        Some(NodeOverlay {
            live_scene,
            live_node,
            properties: composed,
            conflicts,
            properties_complete,
        })
    }

    pub(crate) fn dirty_scripts(&self) -> Vec<Value> {
        let mut scripts = self
            .snapshot
            .script_tabs
            .values()
            .filter(|script| script.get("dirty").and_then(Value::as_bool) == Some(true))
            .map(|script| {
                json!({
                    "live_script_id": script.get("entity_id"),
                    "path": script.get("path"),
                    "editor_content_sha256": script.get("editor_content_sha256"),
                    "disk_content_sha256": script.get("disk_content_sha256"),
                    "reason": "unsaved_source_not_parsed",
                })
            })
            .collect::<Vec<_>>();
        scripts.sort_by(|left, right| {
            left.get("path")
                .and_then(Value::as_str)
                .cmp(&right.get("path").and_then(Value::as_str))
        });
        scripts
    }
}

fn compose_node_value(mut disk: Value, live: &Value, conflicts: &mut Vec<Value>) -> Value {
    let disk_comparison = disk.clone();
    if let Some(object) = disk.as_object_mut() {
        object.insert("disk_comparison".to_owned(), disk_comparison.clone());
        object.insert("live_node_id".to_owned(), live["entity_id"].clone());
        object.insert("source".to_owned(), json!("live_editor"));
        object.insert("freshness".to_owned(), json!("current"));
        object.insert("selected".to_owned(), live["selected"].clone());
        object.insert("primary".to_owned(), live["primary"].clone());
        if let Some(definition) = object.get_mut("definition").and_then(Value::as_object_mut) {
            for field in ["name", "godot_type"] {
                let disk_value = definition.get(field);
                let editor_value = live.get(field);
                if disk_value != editor_value {
                    conflicts.push(json!({
                        "kind": "disk_live_conflict",
                        "node_path": live.get("node_path"),
                        "field": field,
                        "disk_value": disk_value,
                        "editor_value": editor_value,
                        "resolution": "editor_wins",
                    }));
                }
                if let Some(value) = editor_value {
                    definition.insert(field.to_owned(), value.clone());
                }
            }
        }
    }
    disk
}

fn live_only_node_value(live: Value) -> Value {
    json!({
        "node_id": live.get("entity_id"),
        "live_node_id": live.get("entity_id"),
        "node_path": live.get("node_path"),
        "definition": {
            "node_id": live.get("entity_id"),
            "node_path": live.get("node_path"),
            "name": live.get("name"),
            "godot_type": live.get("godot_type"),
            "identity_scope": "editor_session",
            "authority": "live_editor_scene_tree",
        },
        "owner_path": live.get("owner_path"),
        "script_path": live.get("script_path"),
        "selected": live.get("selected"),
        "primary": live.get("primary"),
        "source": "live_editor",
        "freshness": "current",
        "disk_comparison": Value::Null,
    })
}

fn with_disk_source(mut value: Value, reason: &str) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.insert("source".to_owned(), json!("persistent_segment_index"));
        object.insert("freshness".to_owned(), json!("current_on_disk"));
        object.insert("live_coverage".to_owned(), json!(reason));
    }
    value
}

pub(crate) fn overlay_metadata(snapshot: &SemanticSnapshot, live_scene: Option<&Value>) -> Value {
    json!({
        "editor_session_id": snapshot.editor_session_id,
        "snapshot_id": snapshot.snapshot_id,
        "revision_vector": snapshot.revisions,
        "scene": live_scene,
        "source": "live_editor_snapshot",
        "freshness": "current",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use godot_codex_semantic_model::RevisionVector;

    fn snapshot(complete: bool) -> SemanticSnapshot {
        let scene_id = "scene:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        SemanticSnapshot {
            snapshot_id: "snapshot:test".to_owned(),
            project_id: "project:test".to_owned(),
            editor_session_id: "editor:test".to_owned(),
            base_event_seq: 9,
            revisions: RevisionVector {
                editor_session_id: "editor:test".to_owned(),
                event_seq: 9,
                project_revision: 4,
                operation_seq: 2,
                scene_revisions: [(scene_id.to_owned(), 3)].into_iter().collect(),
            },
            editor_state: json!({"kind": "editor_state"}),
            inspector_state: None,
            script_state: None,
            history_state: None,
            diagnostic_state: None,
            viewport_state: None,
            scenes: [(
                scene_id.to_owned(),
                json!({
                    "kind": "scene",
                    "entity_id": scene_id,
                    "path": "res://main.tscn",
                    "coverage": if complete { "complete" } else { "partial" },
                    "nodes_truncated": !complete,
                    "dirty": true,
                    "scene_revision": 3,
                }),
            )]
            .into_iter()
            .collect(),
            nodes: [
                (
                    "node:root".to_owned(),
                    json!({
                        "kind": "node", "entity_id": "node:root", "scene_id": scene_id,
                        "node_path": ".", "name": "LiveRoot", "godot_type": "Node2D",
                        "selected": true, "primary": true,
                        "properties": [{"name": "speed", "variant_type": "float", "value": {"type": "float", "value": 12.0}, "scene_revision": 3}],
                        "properties_truncated": false,
                    }),
                ),
                (
                    "node:new".to_owned(),
                    json!({
                        "kind": "node", "entity_id": "node:new", "scene_id": scene_id,
                        "node_path": "NewChild", "name": "NewChild", "godot_type": "Node",
                        "selected": false, "primary": false,
                    }),
                ),
            ]
            .into_iter()
            .collect(),
            script_tabs: BTreeMap::new(),
            histories: BTreeMap::new(),
            diagnostics: BTreeMap::new(),
            current_scene_id: Some(scene_id.to_owned()),
            selected_node_ids: vec!["node:root".to_owned()],
            truncated: !complete,
            limits_applied: json!({}),
        }
    }

    #[test]
    fn complete_scene_suppresses_missing_disk_nodes_and_retains_comparisons() {
        let snapshot = snapshot(true);
        let overlay = LiveOverlay::bind(&snapshot, "project:test")
            .unwrap()
            .compose_scene_nodes(
                "res://main.tscn",
                vec![
                    json!({"node_id": "disk:root", "node_path": ".", "definition": {"name": "DiskRoot", "godot_type": "Node"}}),
                    json!({"node_id": "disk:gone", "node_path": "Gone", "definition": {"name": "Gone", "godot_type": "Node"}}),
                ],
            )
            .unwrap();
        assert!(overlay.coverage_complete);
        assert_eq!(overlay.suppressed_disk_nodes, 1);
        assert_eq!(overlay.live_only_nodes, 1);
        assert_eq!(overlay.nodes.len(), 2);
        assert_eq!(overlay.nodes[0]["definition"]["name"], "LiveRoot");
        assert_eq!(
            overlay.nodes[0]["disk_comparison"]["definition"]["name"],
            "DiskRoot"
        );
    }

    #[test]
    fn partial_scene_never_creates_an_absence_tombstone() {
        let snapshot = snapshot(false);
        let overlay = LiveOverlay::bind(&snapshot, "project:test")
            .unwrap()
            .compose_scene_nodes(
                "res://main.tscn",
                vec![json!({"node_id": "disk:unknown", "node_path": "Unknown"})],
            )
            .unwrap();
        assert!(!overlay.coverage_complete);
        assert_eq!(overlay.suppressed_disk_nodes, 0);
        assert!(
            overlay
                .nodes
                .iter()
                .any(|node| node["node_id"] == "disk:unknown")
        );
    }

    #[test]
    fn live_property_wins_while_disk_evidence_is_retained() {
        let snapshot = snapshot(true);
        let overlay = LiveOverlay::bind(&snapshot, "project:test")
            .unwrap()
            .compose_node_properties(
                "res://main.tscn",
                ".",
                vec![json!({
                    "property_id": "property:disk",
                    "name": "speed",
                    "value": {"type": "float", "value": {"type": "float", "value": 8.0}, "truncated": false},
                    "authority": "packed_scene_state"
                })],
            )
            .unwrap();
        assert_eq!(overlay.properties[0]["value"]["value"]["value"], 12.0);
        assert_eq!(
            overlay.properties[0]["disk_comparison"]["value"]["value"]["value"],
            8.0
        );
        assert_eq!(overlay.conflicts[0]["resolution"], "editor_wins");
    }

    #[test]
    fn overlay_never_crosses_a_project_boundary() {
        let snapshot = snapshot(true);
        assert_eq!(
            LiveOverlay::bind(&snapshot, "project:other").err(),
            Some(LiveOverlayError::ProjectMismatch)
        );
    }
}
