use std::path::PathBuf;
use std::time::Duration;

use godot_codex_bridge_client::{BridgeClient, BridgeError, ScriptDeltaPoll};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let project_root = PathBuf::from(
        arguments.next().ok_or(
            "usage: script_graph_live <project-root> [--poll-delta|--wait-delta] [--after=<revision>] [--signal-ready] [--details]",
        )?,
    );
    let flags: Vec<_> = arguments.collect();
    let poll_delta = flags.iter().any(|argument| argument == "--poll-delta");
    let wait_delta = flags.iter().any(|argument| argument == "--wait-delta");
    let signal_ready = flags.iter().any(|argument| argument == "--signal-ready");
    let details = flags.iter().any(|argument| argument == "--details");
    let requested_after = flags
        .iter()
        .find_map(|argument| argument.to_str()?.strip_prefix("--after="))
        .map(str::parse::<u64>)
        .transpose()?;

    let mut client = BridgeClient::connect(&project_root).await?;
    let profile = client.negotiated_profile();
    let snapshot_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let snapshot = loop {
        match client.get_script_snapshot().await {
            Ok(snapshot) => break snapshot,
            Err(BridgeError::Rpc {
                code, retryable, ..
            }) if code == "script_catalog_building"
                && retryable
                && tokio::time::Instant::now() < snapshot_deadline =>
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error.into()),
        }
    };
    let normalized = godot_codex_bridge_client::normalize_script_snapshot(&snapshot)?;
    let mut document_paths: Vec<_> = normalized
        .documents
        .iter()
        .map(|document| document.path.clone())
        .collect();
    document_paths.sort();
    let mut output = json!({
        "protocol_version": profile.protocol_version,
        "resource_graph_available": profile.resource_graph_available,
        "scene_graph_available": profile.scene_graph_available,
        "script_graph_available": profile.script_graph_available,
        "snapshot": {
            "resource_revision": normalized.resource_revision,
            "scene_graph_revision": normalized.scene_graph_revision,
            "script_graph_revision": normalized.script_graph_revision,
            "document_count": normalized.documents.len(),
            "symbol_count": normalized.symbols.len(),
            "relation_count": normalized.relations.len(),
            "diagnostic_count": normalized.diagnostics.len(),
            "adapter_status_count": normalized.adapter_statuses.len(),
            "semantic_digest": normalized.semantic_digest,
            "transport_checksum": snapshot.end.checksum,
            "document_paths": document_paths,
        }
    });
    if details {
        output["snapshot"]["documents"] = serde_json::to_value(&normalized.documents)?;
        output["snapshot"]["symbols"] = serde_json::to_value(&normalized.symbols)?;
        output["snapshot"]["relations"] = serde_json::to_value(&normalized.relations)?;
        output["snapshot"]["diagnostics"] = serde_json::to_value(&normalized.diagnostics)?;
        output["snapshot"]["adapter_statuses"] =
            serde_json::to_value(&normalized.adapter_statuses)?;
    }

    if signal_ready {
        std::fs::write(
            project_root.join(".godot/codex-script-live-client-ready"),
            b"ready\n",
        )?;
    }

    if poll_delta || wait_delta {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let after_revision = requested_after.unwrap_or(snapshot.accepted.script_graph_revision);
        output["delta"] = loop {
            let delta = client.get_next_script_delta(after_revision).await?;
            if wait_delta
                && matches!(delta, ScriptDeltaPoll::Current { .. })
                && tokio::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            break match delta {
                ScriptDeltaPoll::Current {
                    current_script_graph_revision,
                } => json!({
                    "status": "current",
                    "current_script_graph_revision": current_script_graph_revision,
                }),
                ScriptDeltaPoll::Batch {
                    current_script_graph_revision,
                    batch,
                } => json!({
                    "status": "batch",
                    "current_script_graph_revision": current_script_graph_revision,
                    "previous_script_graph_revision": batch.previous_script_graph_revision,
                    "script_graph_revision": batch.script_graph_revision,
                    "operation_count": batch.operations.len(),
                    "checksum": batch.checksum,
                }),
                ScriptDeltaPoll::Gap {
                    requested_after_script_graph_revision,
                    oldest_available_script_graph_revision,
                    current_script_graph_revision,
                } => json!({
                    "status": "gap",
                    "requested_after_script_graph_revision": requested_after_script_graph_revision,
                    "oldest_available_script_graph_revision": oldest_available_script_graph_revision,
                    "current_script_graph_revision": current_script_graph_revision,
                }),
            };
        };
    }

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
