use std::path::PathBuf;

use godot_codex_bridge_client::BridgeClient;
use godot_codex_resource_indexer::{ResourceNormalizer, SceneNormalizer};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project_root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: scene_index_live <project-root> [--details]")?,
    );
    let details = std::env::args_os().any(|argument| argument == "--details");
    let mut client = BridgeClient::connect(&project_root).await?;
    let resource_snapshot = client.get_resource_snapshot().await?;
    let resource_generation = ResourceNormalizer::new(&project_root)?.normalize_full_snapshot(
        client.project_id(),
        1,
        &resource_snapshot,
    )?;
    let scene_snapshot = client.get_scene_snapshot().await?;
    let scene = SceneNormalizer.normalize_full_snapshot(&resource_generation, &scene_snapshot)?;

    let mut output = json!({
        "project_id": client.project_id(),
        "resource_revision": scene.resource_revision,
        "scene_graph_revision": scene.scene_graph_revision,
        "scene_count": scene.scenes.len(),
        "node_definition_count": scene.nodes.len(),
        "property_count": scene.properties.len(),
        "relation_count": scene.relations.len(),
        "connection_count": scene.connections.len(),
        "group_count": scene.groups.len(),
        "animation_count": scene.animations.len(),
        "validation_digest": scene.validation_digest,
    });
    if details {
        output["scene_domain"] = serde_json::to_value(scene)?;
    }
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
