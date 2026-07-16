mod discovery;
mod protocol;
mod resource;

use std::path::PathBuf;
use std::time::Duration;

pub use discovery::{BridgeEndpoint, Discovery, project_id_for_root};
use godot_codex_semantic_model::SnapshotReplicator;
pub use protocol::BridgeError;
pub use resource::{
    DependencyObservation, DependencyResolution, PathResourceRef, ResourceDeltaBatch,
    ResourceDeltaOperation, ResourceDeltaPoll, ResourceDiagnostic, ResourceDiagnosticCode,
    ResourceImportState, ResourceObservation, ResourceRef, ResourceRevisionVector,
    ResourceSnapshot, ResourceSnapshotAccepted, ResourceSnapshotBeginParams, ResourceSnapshotChunk,
    ResourceSnapshotEndParams, ResourceSnapshotLimits, ResourceSnapshotPayload,
    ResourceSnapshotSink, ResourceSnapshotTransfer, ResourceSourceKind, ResourceValidity,
    ResourceWithDependencies, RpcContext, UidResourceRef,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NegotiatedBridgeProfile {
    pub protocol_version: String,
    pub capabilities: std::collections::BTreeSet<String>,
    pub resource_graph_available: bool,
}

/// Backward-compatible short name for callers that adopted the Stage 3 spike API.
pub type NegotiatedProfile = NegotiatedBridgeProfile;

#[cfg(any(unix, windows))]
pub struct BridgeClient {
    session: protocol::Session,
}

#[cfg(any(unix, windows))]
impl BridgeClient {
    pub async fn connect(project_root: impl AsRef<std::path::Path>) -> Result<Self, BridgeError> {
        Ok(Self {
            session: protocol::Session::connect(project_root.as_ref()).await?,
        })
    }

    pub fn negotiated_profile(&self) -> NegotiatedBridgeProfile {
        let capabilities = self.session.capabilities().clone();
        NegotiatedBridgeProfile {
            protocol_version: self.session.protocol_version().to_owned(),
            resource_graph_available: self.session.protocol_version() == "1.2"
                && capabilities.contains("resource.uid_dependencies")
                && capabilities.contains("resource.incremental_index"),
            capabilities,
        }
    }

    /// Returns the authenticated project binding for this Bridge session.
    #[must_use]
    pub fn project_id(&self) -> &str {
        self.session.project_id()
    }

    /// Returns the authenticated editor lifetime for this Bridge session.
    #[must_use]
    pub fn editor_session_id(&self) -> &str {
        self.session.editor_session_id()
    }

    pub async fn stream_resource_snapshot<S: ResourceSnapshotSink>(
        &mut self,
        sink: &mut S,
    ) -> Result<ResourceSnapshotTransfer, BridgeError> {
        resource::stream_resource_snapshot(&mut self.session, sink).await
    }

    pub async fn get_resource_snapshot(&mut self) -> Result<ResourceSnapshot, BridgeError> {
        resource::get_resource_snapshot(&mut self.session).await
    }

    pub async fn get_next_resource_delta(
        &mut self,
        after_resource_revision: u64,
    ) -> Result<ResourceDeltaPoll, BridgeError> {
        resource::get_next_resource_delta(&mut self.session, after_resource_revision).await
    }
}

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
