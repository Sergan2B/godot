use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use godot_codex_bridge_client::run_bridge_sync;
use godot_codex_mcp_server::{GodotMcpServer, TransactionCoordinatorSlot};
use godot_codex_resource_indexer::{ProjectSessionState, ResourceIndexCoordinator};
use godot_codex_semantic_model::SnapshotReplicator;
use godot_codex_transactions::{TransactionCoordinator, run_transaction_events};
use rmcp::ServiceExt;
use tokio::sync::watch;

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Run {
        project_root: PathBuf,
        doctor_probe: bool,
    },
    Version,
}

async fn run_transaction_service(
    project_root: PathBuf,
    slot: TransactionCoordinatorSlot,
    mut project_session: watch::Receiver<ProjectSessionState>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut retry = Duration::from_millis(200);
    loop {
        if *shutdown.borrow() {
            slot.publish_unavailable();
            return;
        }
        let project_session_state = *project_session.borrow();
        match project_session_state {
            ProjectSessionState::Busy => {
                slot.publish_project_session_busy();
                tokio::select! {
                    changed = project_session.changed() => {
                        if changed.is_err() {
                            slot.publish_unavailable();
                            return;
                        }
                    }
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            slot.publish_unavailable();
                            return;
                        }
                    }
                }
            }
            ProjectSessionState::Acquiring => {
                slot.publish_acquiring();
                tokio::select! {
                    changed = project_session.changed() => {
                        if changed.is_err() {
                            slot.publish_unavailable();
                            return;
                        }
                    }
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            slot.publish_unavailable();
                            return;
                        }
                    }
                }
            }
            ProjectSessionState::Owner => {
                slot.publish_acquiring();
                match TransactionCoordinator::open(&project_root) {
                    Ok(coordinator) => {
                        if coordinator.journal_recovered_corruption() {
                            eprintln!(
                                "godot-codex-mcp: quarantined an invalid transaction journal; recovery index rebuilt"
                            );
                        }
                        slot.publish_ready(coordinator.clone());
                        retry = Duration::from_millis(200);
                        let (event_shutdown_sender, event_shutdown) = watch::channel(false);
                        let events = run_transaction_events(
                            project_root.clone(),
                            coordinator,
                            event_shutdown,
                        );
                        tokio::pin!(events);
                        tokio::select! {
                            () = &mut events => {
                                slot.publish_unavailable();
                            }
                            changed = project_session.changed() => {
                                let _ = event_shutdown_sender.send(true);
                                events.await;
                                slot.publish_unavailable();
                                if changed.is_err() {
                                    return;
                                }
                            }
                            changed = shutdown.changed() => {
                                let _ = event_shutdown_sender.send(true);
                                events.await;
                                slot.publish_unavailable();
                                if changed.is_err() || *shutdown.borrow() {
                                    return;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        if error.is_project_session_busy() {
                            slot.publish_project_session_busy();
                        } else {
                            slot.publish_unavailable();
                            eprintln!(
                                "godot-codex-mcp: transaction coordinator unavailable; diagnostic tools remain active"
                            );
                        }
                        tokio::select! {
                            changed = project_session.changed() => {
                                if changed.is_err() {
                                    slot.publish_unavailable();
                                    return;
                                }
                            }
                            changed = shutdown.changed() => {
                                if changed.is_err() || *shutdown.borrow() {
                                    slot.publish_unavailable();
                                    return;
                                }
                            }
                            () = tokio::time::sleep(retry) => {}
                        }
                        retry = (retry * 2).min(Duration::from_secs(5));
                    }
                }
            }
        }
    }
}

fn command_from_args(arguments: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut arguments = arguments.into_iter();
    let mut project_root = None;
    let mut doctor_probe = false;
    while let Some(argument) = arguments.next() {
        if argument == "--version" || argument == "-V" {
            if project_root.is_some() || doctor_probe || arguments.next().is_some() {
                return Err("--version cannot be combined with other arguments".to_owned());
            }
            return Ok(Command::Version);
        } else if argument == "--project-root" {
            if project_root.is_some() {
                return Err("--project-root may be specified only once".to_owned());
            }
            let value = arguments
                .next()
                .ok_or_else(|| "--project-root requires a path".to_owned())?;
            project_root = Some(PathBuf::from(value));
        } else if argument == "--doctor-probe" {
            if doctor_probe {
                return Err("--doctor-probe may be specified only once".to_owned());
            }
            doctor_probe = true;
        } else {
            return Err(format!("unknown argument: {}", argument.to_string_lossy()));
        }
    }
    project_root
        .map(|project_root| Command::Run {
            project_root,
            doctor_probe,
        })
        .ok_or_else(|| "usage: godot-codex-mcp --project-root <path>".to_owned())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let command = command_from_args(std::env::args_os().skip(1)).map_err(std::io::Error::other)?;
    let Command::Run {
        project_root,
        doctor_probe,
    } = command
    else {
        println!("godot-codex-mcp {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    };
    let product_startup = godot_codex_operations::observe_product_startup(&project_root, None);
    if doctor_probe {
        let server = GodotMcpServer::with_all_indexes_and_project_root(
            SnapshotReplicator::new(),
            godot_codex_resource_indexer::ResourceIndexReader::new(),
            godot_codex_resource_indexer::SceneIndexReader::new(),
            godot_codex_resource_indexer::ScriptIndexReader::new(),
            project_root,
        )
        .with_product_startup_observation(product_startup)
        .serve(rmcp::transport::stdio())
        .await?;
        server.waiting().await?;
        return Ok(());
    }
    let replicator = SnapshotReplicator::new();
    let (resource_coordinator, resource_index_reader, scene_index_reader, script_index_reader) =
        ResourceIndexCoordinator::new_semantic(&project_root)?;
    let project_session = resource_coordinator.subscribe_project_session();
    // Offline authority and the exact project-bound store are opened before
    // any Bridge discovery/connect attempt can publish live state.
    let bridge_task = tokio::spawn(run_bridge_sync(project_root.clone(), replicator.clone()));
    let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
    let resource_task = tokio::spawn(resource_coordinator.run(shutdown_receiver));
    let transaction_coordinator = TransactionCoordinatorSlot::acquiring();
    let transaction_task = tokio::spawn(run_transaction_service(
        project_root.clone(),
        transaction_coordinator.clone(),
        project_session,
        shutdown_sender.subscribe(),
    ));
    let server = GodotMcpServer::with_all_indexes_and_project_slot(
        replicator,
        resource_index_reader,
        scene_index_reader,
        script_index_reader,
        project_root,
        transaction_coordinator,
    )
    .with_product_startup_observation(product_startup)
    .serve(rmcp::transport::stdio())
    .await?;
    server.waiting().await?;
    let _ = shutdown_sender.send(true);
    let _ = resource_task.await;
    let _ = transaction_task.await;
    bridge_task.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_project_root_is_rejected() {
        assert!(command_from_args(Vec::new()).is_err());
    }

    #[test]
    fn doctor_probe_is_explicit_and_project_bound() {
        assert_eq!(
            command_from_args([
                OsString::from("--doctor-probe"),
                OsString::from("--project-root"),
                OsString::from("/tmp/project"),
            ])
            .unwrap(),
            Command::Run {
                project_root: PathBuf::from("/tmp/project"),
                doctor_probe: true,
            }
        );
        assert!(
            command_from_args([
                OsString::from("--doctor-probe"),
                OsString::from("--doctor-probe"),
                OsString::from("--project-root"),
                OsString::from("/tmp/project"),
            ])
            .is_err()
        );
    }

    #[test]
    fn version_is_standalone_and_deterministic() {
        assert_eq!(
            command_from_args([OsString::from("--version")]).unwrap(),
            Command::Version
        );
        assert!(
            command_from_args([
                OsString::from("--version"),
                OsString::from("--doctor-probe"),
            ])
            .is_err()
        );
        assert!(
            command_from_args([
                OsString::from("--version"),
                OsString::from("--project-root"),
                OsString::from("."),
            ])
            .is_err()
        );
    }
}
