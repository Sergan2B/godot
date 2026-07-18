use std::ffi::OsString;
use std::path::PathBuf;

use godot_codex_bridge_client::run_bridge_sync;
use godot_codex_mcp_server::GodotMcpServer;
use godot_codex_resource_indexer::ResourceIndexCoordinator;
use godot_codex_semantic_model::SnapshotReplicator;
use rmcp::ServiceExt;

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Run { project_root: PathBuf },
    Version,
}

fn command_from_args(arguments: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut arguments = arguments.into_iter();
    let mut project_root = None;
    while let Some(argument) = arguments.next() {
        if argument == "--version" || argument == "-V" {
            if project_root.is_some() || arguments.next().is_some() {
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
        } else {
            return Err(format!("unknown argument: {}", argument.to_string_lossy()));
        }
    }
    project_root
        .map(|project_root| Command::Run { project_root })
        .ok_or_else(|| "usage: godot-codex-mcp --project-root <path>".to_owned())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let command = command_from_args(std::env::args_os().skip(1)).map_err(std::io::Error::other)?;
    let Command::Run { project_root } = command else {
        println!("godot-codex-mcp {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    };
    let replicator = SnapshotReplicator::new();
    let bridge_task = tokio::spawn(run_bridge_sync(project_root.clone(), replicator.clone()));
    let (resource_coordinator, resource_index_reader, scene_index_reader, script_index_reader) =
        ResourceIndexCoordinator::new_semantic(&project_root)?;
    let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
    let resource_task = tokio::spawn(resource_coordinator.run(shutdown_receiver));
    let server = GodotMcpServer::with_all_indexes(
        replicator,
        resource_index_reader,
        scene_index_reader,
        script_index_reader,
    )
    .serve(rmcp::transport::stdio())
    .await?;
    server.waiting().await?;
    let _ = shutdown_sender.send(true);
    let _ = resource_task.await;
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
    fn version_is_standalone_and_deterministic() {
        assert_eq!(
            command_from_args([OsString::from("--version")]).unwrap(),
            Command::Version
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
