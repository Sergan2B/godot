mod discovery;
mod protocol;

use std::path::PathBuf;
use std::time::Duration;

pub use discovery::{BridgeEndpoint, Discovery, project_id_for_root};
use godot_codex_semantic_model::SnapshotReplicator;
pub use protocol::BridgeError;

pub async fn run_bridge_sync(project_root: PathBuf, replicator: SnapshotReplicator) -> ! {
    let mut retry = Duration::from_millis(200);
    loop {
        let result = protocol::run_session(&project_root, &replicator).await;
        let message = result.map_or_else(
            |error| error.safe_summary().to_owned(),
            |()| "bridge session ended".to_owned(),
        );
        replicator.mark_disconnected(message);
        tokio::time::sleep(retry).await;
        retry = (retry * 2).min(Duration::from_secs(5));
    }
}
