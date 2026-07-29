use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use godot_codex_bridge_client::{BridgeClient, run_bridge_sync};
use godot_codex_mcp_server::{GodotMcpServer, TransactionCoordinatorSlot};
use godot_codex_resource_indexer::{ProjectSessionState, ResourceIndexCoordinator};
use godot_codex_semantic_model::{NegotiatedBridgeMetadata, ReplicaFailure, SnapshotReplicator};
use godot_codex_surface_capture::{ClaimContext, ClaimedLease, FinalizeOutcome, LeaseStore};
use godot_codex_transactions::{TransactionCoordinator, run_transaction_events};
use rmcp::service::QuitReason;
use rmcp::{RoleServer, ServiceExt};
use tokio::sync::watch;

const BRIDGE_PROBE_INTERVAL: Duration = Duration::from_secs(1);
const SERVICE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

#[cfg(unix)]
struct ShutdownSignals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl ShutdownSignals {
    fn install() -> std::io::Result<Self> {
        Ok(Self {
            interrupt: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
            terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
        })
    }

    async fn receive(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Run {
        project_root: PathBuf,
        doctor_probe: bool,
    },
    Version,
}

async fn run_bridge_service(
    project_root: PathBuf,
    replicator: SnapshotReplicator,
    mut project_session: watch::Receiver<ProjectSessionState>,
) {
    let mut retry = Duration::from_millis(200);
    loop {
        let project_session_state = *project_session.borrow();
        match project_session_state {
            ProjectSessionState::Owner => {
                run_bridge_sync(project_root, replicator).await;
            }
            ProjectSessionState::Acquiring | ProjectSessionState::Busy => {
                let connection = tokio::select! {
                    result = BridgeClient::connect(&project_root) => Some(result),
                    changed = project_session.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        None
                    }
                };
                let Some(connection) = connection else {
                    continue;
                };
                match connection {
                    Ok(client) => {
                        let profile = client.negotiated_profile();
                        if let Some(metadata) = NegotiatedBridgeMetadata::new(
                            profile.protocol_version,
                            profile.capabilities,
                        ) {
                            replicator.mark_bridge_negotiated(metadata);
                            retry = Duration::from_millis(200);
                        } else {
                            replicator.mark_disconnected(ReplicaFailure::TransportDisconnected);
                        }
                    }
                    Err(error) => {
                        replicator.mark_disconnected(error.failure_class().replica_failure());
                    }
                }
                let wait = if replicator.observation().negotiated_bridge.is_some() {
                    BRIDGE_PROBE_INTERVAL
                } else {
                    retry
                };
                tokio::select! {
                    changed = project_session.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                    () = tokio::time::sleep(wait) => {
                        retry = (retry * 2).min(Duration::from_secs(5));
                    }
                }
            }
        }
    }
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

fn claim_surface_capture(project_root: &std::path::Path) -> Option<ClaimedLease> {
    let data_root = std::env::var_os("GODOT_CODEX_DATA_ROOT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)?;
    let store = match LeaseStore::open(&data_root) {
        Ok(store) => store,
        Err(error) => {
            eprintln!(
                "godot-codex-mcp: surface capture store unavailable; continuing without capture ({error})"
            );
            return None;
        }
    };
    match store.has_capture_state() {
        Ok(false) => return None,
        Ok(true) => {}
        Err(error) => {
            eprintln!(
                "godot-codex-mcp: surface capture state unreadable; continuing without capture ({error})"
            );
            return None;
        }
    }
    let context =
        match godot_codex_operations::verify_surface_capture_claim(project_root, &data_root) {
            Ok(context) => context,
            Err(_) => return None,
        };
    if store.data_root() != context.data_root() {
        eprintln!(
            "godot-codex-mcp: surface capture data-root binding differs; continuing without capture"
        );
        return None;
    }
    match store.claim(&ClaimContext {
        project_root: context.canonical_project_root().to_path_buf(),
        bindings: context.binding_digests(),
    }) {
        Ok(claim) => claim,
        Err(error) => {
            eprintln!(
                "godot-codex-mcp: surface capture lease not claimable; continuing without capture ({error})"
            );
            None
        }
    }
}

async fn serve_mcp(
    server: GodotMcpServer,
    mut capture: Option<ClaimedLease>,
) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    let mut shutdown_signals = match ShutdownSignals::install() {
        Ok(signals) => signals,
        Err(error) => {
            if let Some(capture) = capture.take() {
                let _ = capture.finalize(FinalizeOutcome::Failed);
            }
            return Err(error.into());
        }
    };

    let server_result = if let Some(capture) = capture.as_ref() {
        let transport = rmcp::transport::IntoTransport::<RoleServer, _, _>::into_transport(
            rmcp::transport::stdio(),
        );
        server
            .serve(capture.wrap_transport::<RoleServer, _>(transport))
            .await
    } else {
        server.serve(rmcp::transport::stdio()).await
    };
    let server = match server_result {
        Ok(server) => server,
        Err(error) => {
            if let Some(capture) = capture.take() {
                let _ = capture.finalize(FinalizeOutcome::Failed);
            }
            return Err(error.into());
        }
    };

    let cancellation_token = server.cancellation_token();
    let mut waiting = Box::pin(server.waiting());
    #[cfg(unix)]
    let (result, shutdown_requested) = tokio::select! {
        result = &mut waiting => (Some(result), false),
        () = shutdown_signals.receive() => {
            cancellation_token.cancel();
            let result = tokio::time::timeout(SERVICE_SHUTDOWN_TIMEOUT, &mut waiting)
                .await
                .ok();
            (result, true)
        }
    };
    #[cfg(not(unix))]
    let (result, shutdown_requested) = (Some(waiting.await), false);

    let Some(result) = result else {
        // The service task may still own the tapped transport. Leaving the
        // lease claimed is safer than publishing a journal while capture can
        // still be changing.
        return Err(std::io::Error::other(
            "MCP service cleanup exceeded the graceful shutdown deadline",
        )
        .into());
    };
    let result = classify_service_exit(result, shutdown_requested);
    let outcome = service_finalize_outcome(result.is_ok(), shutdown_requested);
    let finalized = capture.map(|capture| capture.finalize(outcome)).transpose();
    result?;
    finalized?;
    Ok(())
}

fn service_finalize_outcome(service_succeeded: bool, shutdown_requested: bool) -> FinalizeOutcome {
    match (service_succeeded, shutdown_requested) {
        (true, false) => FinalizeOutcome::Completed,
        (true, true) => FinalizeOutcome::Cancelled,
        (false, _) => FinalizeOutcome::Failed,
    }
}

fn classify_service_exit(
    result: Result<QuitReason, tokio::task::JoinError>,
    shutdown_requested: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match result {
        Ok(QuitReason::Closed) => Ok(()),
        Ok(QuitReason::Cancelled) if shutdown_requested => Ok(()),
        Ok(QuitReason::Cancelled) => {
            Err(std::io::Error::other("MCP service was cancelled unexpectedly").into())
        }
        Ok(QuitReason::JoinError(error)) | Err(error) => Err(error.into()),
        Ok(reason) => Err(std::io::Error::other(format!(
            "MCP service stopped unexpectedly: {reason:?}"
        ))
        .into()),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run());
    // Tokio's stdin adapter uses a blocking reader which cannot be cancelled
    // while the parent keeps the MCP pipe open. All service and application
    // cleanup has completed before this point; bound runtime teardown so a
    // handled SIGINT/SIGTERM cannot strand the sidecar process.
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    result
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
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
    let surface_capture = claim_surface_capture(&project_root);
    let replicator = SnapshotReplicator::new();
    let (resource_coordinator, resource_index_reader, scene_index_reader, script_index_reader) =
        ResourceIndexCoordinator::new_semantic(&project_root)?;
    let bridge_project_session = resource_coordinator.subscribe_project_session();
    let transaction_project_session = resource_coordinator.subscribe_project_session();
    // Offline authority and the exact project-bound store are opened before
    // any Bridge discovery/connect attempt can publish live state.
    let bridge_task = tokio::spawn(run_bridge_service(
        project_root.clone(),
        replicator.clone(),
        bridge_project_session,
    ));
    let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
    let resource_task = tokio::spawn(resource_coordinator.run(shutdown_receiver));
    let transaction_coordinator = TransactionCoordinatorSlot::acquiring();
    let transaction_task = tokio::spawn(run_transaction_service(
        project_root.clone(),
        transaction_coordinator.clone(),
        transaction_project_session,
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
    .with_product_startup_observation(product_startup);
    let server_result = serve_mcp(server, surface_capture).await;
    let _ = shutdown_sender.send(true);
    let _ = resource_task.await;
    let _ = transaction_task.await;
    bridge_task.abort();
    let _ = bridge_task.await;
    server_result
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

    #[test]
    fn normal_and_signal_requested_service_exits_are_distinguished() {
        assert!(classify_service_exit(Ok(QuitReason::Closed), false).is_ok());
        assert!(classify_service_exit(Ok(QuitReason::Cancelled), true).is_ok());
        assert!(classify_service_exit(Ok(QuitReason::Cancelled), false).is_err());
        assert_eq!(
            service_finalize_outcome(true, false),
            FinalizeOutcome::Completed
        );
        assert_eq!(
            service_finalize_outcome(true, true),
            FinalizeOutcome::Cancelled
        );
        assert_eq!(
            service_finalize_outcome(false, false),
            FinalizeOutcome::Failed
        );
        assert_eq!(
            service_finalize_outcome(false, true),
            FinalizeOutcome::Failed
        );
    }

    #[tokio::test]
    async fn service_join_errors_are_not_classified_as_completed() {
        let task = tokio::spawn(std::future::pending::<()>());
        task.abort();
        let error = task.await.unwrap_err();
        assert!(classify_service_exit(Ok(QuitReason::JoinError(error)), false).is_err());

        let task = tokio::spawn(std::future::pending::<()>());
        task.abort();
        let error = task.await.unwrap_err();
        assert!(classify_service_exit(Err(error), false).is_err());
    }
}
