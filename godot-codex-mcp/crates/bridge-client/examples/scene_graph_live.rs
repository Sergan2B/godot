use std::path::PathBuf;
use std::time::Duration;

use godot_codex_bridge_client::{BridgeClient, BridgeError, SceneDeltaPoll};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let project_root = PathBuf::from(
        arguments
            .next()
            .ok_or(
                "usage: scene_graph_live <project-root> [--poll-delta|--wait-delta] [--signal-ready] [--details]",
            )?,
    );
    let flags: Vec<_> = arguments.collect();
    let poll_delta = flags.iter().any(|argument| argument == "--poll-delta");
    let wait_delta = flags.iter().any(|argument| argument == "--wait-delta");
    let signal_ready = flags.iter().any(|argument| argument == "--signal-ready");
    let details = flags.iter().any(|argument| argument == "--details");

    let mut client = BridgeClient::connect(&project_root).await?;
    let profile = client.negotiated_profile();
    let snapshot_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let snapshot = loop {
        match client.get_scene_snapshot().await {
            Ok(snapshot) => break snapshot,
            Err(BridgeError::Rpc {
                code, retryable, ..
            }) if code == "scene_catalog_building"
                && retryable
                && tokio::time::Instant::now() < snapshot_deadline =>
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error.into()),
        }
    };

    let mut scene_paths: Vec<_> = snapshot
        .payload
        .scenes
        .iter()
        .map(|scene| scene.path.clone())
        .collect();
    scene_paths.sort();
    let node_count = snapshot
        .payload
        .scenes
        .iter()
        .map(|scene| scene.nodes.len())
        .sum::<usize>();
    let property_count = snapshot
        .payload
        .scenes
        .iter()
        .flat_map(|scene| &scene.nodes)
        .map(|node| node.properties.len())
        .sum::<usize>();
    let connection_count = snapshot
        .payload
        .scenes
        .iter()
        .map(|scene| scene.connections.len())
        .sum::<usize>();
    let subresource_count = snapshot
        .payload
        .scenes
        .iter()
        .map(|scene| scene.subresources.len())
        .sum::<usize>();
    let animation_track_count = snapshot
        .payload
        .scenes
        .iter()
        .map(|scene| scene.animation_tracks.len())
        .sum::<usize>();
    let mut output = json!({
        "protocol_version": profile.protocol_version,
        "resource_graph_available": profile.resource_graph_available,
        "scene_graph_available": profile.scene_graph_available,
        "snapshot": {
            "resource_revision": snapshot.accepted.resource_revision,
            "scene_graph_revision": snapshot.accepted.scene_graph_revision,
            "scene_count": snapshot.payload.scenes.len(),
            "node_count": node_count,
            "property_count": property_count,
            "connection_count": connection_count,
            "subresource_count": subresource_count,
            "animation_track_count": animation_track_count,
            "project_context_count": snapshot.payload.project_context.len(),
            "diagnostic_count": snapshot.payload.diagnostics.len(),
            "checksum": snapshot.end.checksum,
            "scene_paths": scene_paths,
        }
    });
    if details {
        output["snapshot"]["scenes"] = serde_json::to_value(&snapshot.payload.scenes)?;
        output["snapshot"]["project_context"] =
            serde_json::to_value(&snapshot.payload.project_context)?;
        output["snapshot"]["diagnostics"] = serde_json::to_value(&snapshot.payload.diagnostics)?;
    }

    if signal_ready {
        std::fs::write(
            project_root.join(".godot/codex-scene-live-client-ready"),
            b"ready\n",
        )?;
    }

    if poll_delta || wait_delta {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        output["delta"] = loop {
            let delta = client
                .get_next_scene_delta(snapshot.accepted.scene_graph_revision)
                .await?;
            if wait_delta
                && matches!(delta, SceneDeltaPoll::Current { .. })
                && tokio::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            break match delta {
                SceneDeltaPoll::Current {
                    current_scene_graph_revision,
                } => json!({
                    "status": "current",
                    "current_scene_graph_revision": current_scene_graph_revision,
                }),
                SceneDeltaPoll::Batch {
                    current_scene_graph_revision,
                    batch,
                } => json!({
                    "status": "batch",
                    "current_scene_graph_revision": current_scene_graph_revision,
                    "previous_scene_graph_revision": batch.previous_scene_graph_revision,
                    "scene_graph_revision": batch.scene_graph_revision,
                    "operation_count": batch.operations.len(),
                    "checksum": batch.checksum,
                }),
                SceneDeltaPoll::Gap {
                    requested_after_scene_graph_revision,
                    oldest_available_scene_graph_revision,
                    current_scene_graph_revision,
                } => json!({
                    "status": "gap",
                    "requested_after_scene_graph_revision": requested_after_scene_graph_revision,
                    "oldest_available_scene_graph_revision": oldest_available_scene_graph_revision,
                    "current_scene_graph_revision": current_scene_graph_revision,
                }),
            };
        };
    }

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
