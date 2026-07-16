use std::path::PathBuf;
use std::time::Duration;

use godot_codex_bridge_client::{BridgeClient, BridgeError, ResourceDeltaPoll};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let project_root = PathBuf::from(
        arguments
            .next()
            .ok_or(
                "usage: resource_graph_live <project-root> [--poll-delta|--poll-deltas=N] [--signal-ready] [--details]",
            )?,
    );
    let flags: Vec<_> = arguments.collect();
    let poll_delta_count = flags
        .iter()
        .find_map(|argument| {
            argument
                .to_str()
                .and_then(|value| value.strip_prefix("--poll-deltas="))
                .and_then(|value| value.parse::<usize>().ok())
        })
        .unwrap_or_else(|| usize::from(flags.iter().any(|argument| argument == "--poll-delta")));
    let signal_ready = flags.iter().any(|argument| argument == "--signal-ready");
    let details = flags.iter().any(|argument| argument == "--details");
    let mut client = BridgeClient::connect(&project_root).await?;
    let profile = client.negotiated_profile();
    let snapshot_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let snapshot = loop {
        match client.get_resource_snapshot().await {
            Ok(snapshot) => break snapshot,
            Err(BridgeError::Rpc {
                code, retryable, ..
            }) if code == "resource_catalog_building"
                && retryable
                && tokio::time::Instant::now() < snapshot_deadline =>
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error.into()),
        }
    };
    let mut resource_paths: Vec<_> = snapshot
        .payload
        .resources
        .iter()
        .map(|resource| resource.path.clone())
        .collect();
    resource_paths.sort();
    let mut output = json!({
        "protocol_version": profile.protocol_version,
        "resource_graph_available": profile.resource_graph_available,
        "snapshot": {
            "resource_revision": snapshot.accepted.resource_revision,
            "resource_count": snapshot.payload.resources.len(),
            "dependency_count": snapshot.payload.dependencies.len(),
            "diagnostic_count": snapshot.payload.diagnostics.len(),
            "checksum": snapshot.end.checksum,
            "resource_paths": resource_paths,
        }
    });
    if details {
        output["snapshot"]["resources"] = serde_json::to_value(&snapshot.payload.resources)?;
        output["snapshot"]["dependencies"] = serde_json::to_value(&snapshot.payload.dependencies)?;
        output["snapshot"]["diagnostics"] = serde_json::to_value(&snapshot.payload.diagnostics)?;
    }

    if signal_ready {
        std::fs::write(
            project_root.join(".godot/codex-resource-live-client-ready"),
            b"ready\n",
        )?;
    }

    if poll_delta_count > 0 {
        let mut after = snapshot.accepted.resource_revision;
        let mut deltas = Vec::with_capacity(poll_delta_count);
        for delta_index in 0..poll_delta_count {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
            let delta = loop {
                match client.get_next_resource_delta(after).await? {
                    ResourceDeltaPoll::Current { .. } if tokio::time::Instant::now() < deadline => {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    ResourceDeltaPoll::Current { .. } => {
                        return Err("delta polling timed out".into());
                    }
                    ResourceDeltaPoll::Batch {
                        current_resource_revision,
                        batch,
                    } => {
                        after = batch.resource_revision;
                        let mut value = json!({
                            "status": "batch",
                            "previous_resource_revision": batch.previous_resource_revision,
                            "resource_revision": batch.resource_revision,
                            "current_resource_revision": current_resource_revision,
                            "operation_count": batch.operations.len(),
                            "checksum": batch.checksum,
                        });
                        if details {
                            value["operations"] = serde_json::to_value(&batch.operations)?;
                        }
                        break value;
                    }
                    ResourceDeltaPoll::Gap {
                        requested_after_resource_revision,
                        oldest_available_resource_revision,
                        current_resource_revision,
                    } => {
                        if delta_index + 1 != poll_delta_count {
                            return Err("resource gap terminated a multi-delta run".into());
                        }
                        break json!({
                            "status": "gap",
                            "requested_after_resource_revision": requested_after_resource_revision,
                            "oldest_available_resource_revision": oldest_available_resource_revision,
                            "current_resource_revision": current_resource_revision,
                        });
                    }
                }
            };
            deltas.push(delta);
            if signal_ready {
                std::fs::write(
                    project_root.join(format!(
                        ".godot/codex-resource-live-delta-{}-ready",
                        delta_index + 1
                    )),
                    b"ready\n",
                )?;
            }
        }
        if deltas.len() == 1 {
            output["delta"] = deltas.pop().expect("one delta");
        } else {
            output["deltas"] = serde_json::to_value(deltas)?;
        }
    }

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
