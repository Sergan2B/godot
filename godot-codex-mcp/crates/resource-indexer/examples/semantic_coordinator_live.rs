use std::path::PathBuf;
use std::time::Duration;

use godot_codex_resource_indexer::{
    ResourceIndexCoordinator, ResourceIndexStatus, SceneIndexStatus,
};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project_root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: semantic_coordinator_live <project-root>")?,
    );
    let (coordinator, resource_reader, scene_reader) =
        ResourceIndexCoordinator::new_semantic(project_root)?;
    let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(coordinator.run(shutdown_receiver));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if matches!(
            resource_reader.status(),
            ResourceIndexStatus::Current { .. }
        ) && matches!(scene_reader.status(), SceneIndexStatus::Current { .. })
        {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = shutdown_sender.send(true);
            let _ = task.await;
            return Err(format!(
                "coordinator timeout: resource={:?}, scene={:?}",
                resource_reader.status(),
                scene_reader.status()
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let resource = resource_reader.pin_current()?;
    let scene = scene_reader.pin_current()?;
    let same_generation = resource.generation().generation_id == scene.generation().generation_id
        && resource.generation().index_revision == scene.generation().index_revision;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "resource_status": format!("{:?}", resource_reader.status()),
            "scene_status": format!("{:?}", scene_reader.status()),
            "same_generation": same_generation,
            "generation_id": scene.generation().generation_id,
            "index_revision": scene.generation().index_revision,
            "resource_revision": scene.generation().checkpoint.resource_revision,
            "scene_graph_revision": scene.generation().scene.scene_graph_revision,
            "scene_count": scene.generation().scene.scenes.len(),
        }))?
    );
    let _ = shutdown_sender.send(true);
    task.await?;
    if !same_generation {
        return Err("resource and scene readers pinned different generations".into());
    }
    Ok(())
}
