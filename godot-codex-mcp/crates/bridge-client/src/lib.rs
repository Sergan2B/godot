mod change_set;
mod discovery;
mod protocol;
mod resource;
mod runtime;
mod scene;
mod script;
mod transaction;

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
pub use runtime::{
    RuntimeDiagnostic, RuntimeDiagnosticSeverity, RuntimeDiagnosticSource, RuntimeDomain,
    RuntimeEntity, RuntimeEvent, RuntimeEventType, RuntimeInvalidated, RuntimeInvalidationReason,
    RuntimeLimits, RuntimeNode, RuntimeNotification, RuntimeObjectResult, RuntimeOrigin,
    RuntimeProjectedValue, RuntimeProperty, RuntimeRevisionVector, RuntimeSnapshot,
    RuntimeSnapshotAccepted, RuntimeSnapshotEnd, RuntimeSourceHint, RuntimeStack,
    RuntimeStackFrame, RuntimeStackKind, RuntimeStackResult, RuntimeState, RuntimeStateEntity,
    RuntimeStateResult, RuntimeTarget, RuntimeViewportCapture, RuntimeVisibility,
};
pub use scene::{
    AnimationTrackObservation, AnimationTrackResolution, ConnectionObservation, NodeObservation,
    ProjectContextObservation, ProjectedVariant, PropertyObservation, SceneDeltaBatch,
    SceneDeltaOperation, SceneDeltaPoll, SceneDiagnostic, SceneDiagnosticCode, SceneIdentityScope,
    SceneObservation, SceneRevisionVector, SceneSnapshot, SceneSnapshotAccepted,
    SceneSnapshotBeginParams, SceneSnapshotChunk, SceneSnapshotEndParams, SceneSnapshotLimits,
    SceneSnapshotPayload, SceneSnapshotSink, SceneSnapshotTransfer, SubresourceObservation,
};
pub use script::{
    LanguageAdapterStatus, NormalizedScriptGraph, ScriptAdapterAvailability, ScriptAdapterProfile,
    ScriptCompleteness, ScriptConfidence, ScriptDeltaBatch, ScriptDeltaOperation, ScriptDeltaPoll,
    ScriptDiagnostic, ScriptDiagnosticAuthority, ScriptDiagnosticIdentity,
    ScriptDiagnosticSeverity, ScriptDocument, ScriptDocumentBundle, ScriptIdentityScope,
    ScriptLanguage, ScriptModifier, ScriptRelation, ScriptRelationAuthority,
    ScriptRelationEndpoint, ScriptRelationPredicate, ScriptRevisionVector, ScriptSnapshot,
    ScriptSnapshotAccepted, ScriptSnapshotBeginParams, ScriptSnapshotChunk,
    ScriptSnapshotEndParams, ScriptSnapshotLimits, ScriptSnapshotPayload, ScriptSnapshotSink,
    ScriptSnapshotTransfer, ScriptSymbol, ScriptSymbolKind, ScriptTypeState, ScriptVisibility,
    SourceRange, canonical_content_symbol_id, canonical_diagnostic_id, canonical_named_symbol_id,
    canonical_script_resource_id, normalize_script_graph, normalize_script_snapshot,
};
pub use transaction::{
    AffectedEntity, AffectedEntityRole, ApprovalBinding, ApprovalReceipt, ApprovalReceiptKind,
    ApprovalScope, DirtyEffect, OperationKind, PrepareResult, RevisionCoordinates, Risk,
    SafeTransactionError, SaveEffect, SignedApprovalReceipt, TransactionCoordinates,
    TransactionEvent, TransactionEventReason, TransactionLimits, TransactionOperation,
    TransactionOutcome, TransactionPathRef, TransactionPreview, TransactionResourceRef,
    TransactionState, TransactionStatus, TransactionUidRef, UndoEligibility, UndoEligibilityReason,
    WritableDictionaryEntry, WritableVariant,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NegotiatedBridgeProfile {
    pub protocol_version: String,
    pub capabilities: std::collections::BTreeSet<String>,
    pub resource_graph_available: bool,
    pub scene_graph_available: bool,
    pub script_graph_available: bool,
    pub live_editor_available: bool,
    pub runtime_available: bool,
    pub transaction_available: bool,
    pub change_set_available: bool,
    pub automatic_validation_available: bool,
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
            resource_graph_available: protocol::has_resource_profile(
                self.session.protocol_version(),
            ) && capabilities.contains("resource.uid_dependencies")
                && capabilities.contains("resource.incremental_index"),
            scene_graph_available: protocol::has_scene_profile(self.session.protocol_version())
                && capabilities.contains("scene.packed_state")
                && capabilities.contains("scene.incremental_index")
                && capabilities.contains("scene.project_context"),
            script_graph_available: protocol::has_script_profile(self.session.protocol_version())
                && capabilities.contains("script.gdscript_semantics")
                && capabilities.contains("script.incremental_index")
                && capabilities.contains("script.diagnostics")
                && capabilities.contains("script.csharp_discovery"),
            live_editor_available: protocol::has_live_editor_profile(
                self.session.protocol_version(),
            ) && capabilities.contains("editor.open_scenes")
                && capabilities.contains("editor.open_scripts")
                && capabilities.contains("editor.native_history")
                && capabilities.contains("editor.diagnostics")
                && capabilities.contains("editor.viewport_metadata"),
            runtime_available: protocol::has_runtime_profile(self.session.protocol_version())
                && capabilities.contains("runtime.debugger")
                && capabilities.contains("runtime.process_control")
                && capabilities.contains("runtime.remote_tree")
                && capabilities.contains("runtime.bounded_properties")
                && capabilities.contains("runtime.diagnostics")
                && capabilities.contains("runtime.viewport_capture"),
            transaction_available: protocol::has_transaction_profile(
                self.session.protocol_version(),
            ) && capabilities.contains("transaction.scene_v1"),
            change_set_available: protocol::has_change_set_profile(self.session.protocol_version())
                && capabilities.contains("transaction.change_set_v1"),
            automatic_validation_available: protocol::has_change_set_profile(
                self.session.protocol_version(),
            ) && capabilities.contains("validation.automatic_v1"),
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

    pub async fn next_runtime_notification(&mut self) -> Result<RuntimeNotification, BridgeError> {
        let value = self.session.receive_runtime_notification().await?;
        runtime::parse_runtime_notification(&self.session, value)
    }

    pub async fn next_transaction_notification(&mut self) -> Result<TransactionEvent, BridgeError> {
        transaction::next_event(&mut self.session).await
    }

    pub async fn prepare_transaction(
        &mut self,
        idempotency_key: &str,
        coordinates: RevisionCoordinates,
        operation: TransactionOperation,
    ) -> Result<transaction::PrepareResult, BridgeError> {
        transaction::prepare(&mut self.session, idempotency_key, coordinates, operation).await
    }

    pub async fn apply_transaction(
        &mut self,
        transaction_id: &str,
        preview_digest: &str,
        expected_scene_revision: u64,
        expected_operation_seq: u64,
        approval: ApprovalReceipt,
    ) -> Result<TransactionStatus, BridgeError> {
        transaction::apply(
            &mut self.session,
            transaction_id,
            preview_digest,
            expected_scene_revision,
            expected_operation_seq,
            approval,
        )
        .await
    }

    pub async fn get_transaction_status(
        &mut self,
        transaction_id: &str,
    ) -> Result<TransactionStatus, BridgeError> {
        transaction::status(&mut self.session, transaction_id).await
    }

    pub async fn undo_transaction(
        &mut self,
        transaction_id: &str,
        expected_transaction_seq: u64,
        expected_scene_revision: u64,
        expected_operation_seq: u64,
    ) -> Result<TransactionStatus, BridgeError> {
        transaction::undo(
            &mut self.session,
            transaction_id,
            expected_transaction_seq,
            expected_scene_revision,
            expected_operation_seq,
        )
        .await
    }

    pub async fn prepare_change_set(
        &mut self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, BridgeError> {
        change_set::prepare(&mut self.session, params).await
    }

    pub async fn apply_change_set(
        &mut self,
        change_set_id: &str,
        preview_digest: &str,
        expected_scene_revision: u64,
        expected_operation_seq: u64,
        approval: ApprovalReceipt,
    ) -> Result<serde_json::Value, BridgeError> {
        change_set::apply(
            &mut self.session,
            change_set_id,
            preview_digest,
            expected_scene_revision,
            expected_operation_seq,
            approval,
        )
        .await
    }

    pub async fn get_change_set_status(
        &mut self,
        change_set_id: &str,
    ) -> Result<serde_json::Value, BridgeError> {
        change_set::status(&mut self.session, change_set_id).await
    }

    pub async fn undo_change_set(
        &mut self,
        change_set_id: &str,
        expected_transaction_seq: u64,
    ) -> Result<serde_json::Value, BridgeError> {
        change_set::undo(&mut self.session, change_set_id, expected_transaction_seq).await
    }

    pub async fn complete_change_set_validation(
        &mut self,
        change_set_id: &str,
        report_id: &str,
        report_digest: &str,
        outcome: &str,
        expected_transaction_seq: u64,
        expected_postimage_digest: &str,
    ) -> Result<serde_json::Value, BridgeError> {
        change_set::validation_complete(
            &mut self.session,
            change_set_id,
            report_id,
            report_digest,
            outcome,
            expected_transaction_seq,
            expected_postimage_digest,
        )
        .await
    }

    pub async fn rollback_change_set(
        &mut self,
        change_set_id: &str,
        expected_transaction_seq: u64,
        expected_postimage_digest: &str,
    ) -> Result<serde_json::Value, BridgeError> {
        change_set::rollback(
            &mut self.session,
            change_set_id,
            expected_transaction_seq,
            expected_postimage_digest,
        )
        .await
    }

    pub fn issue_transaction_approval(
        &self,
        binding: &ApprovalBinding,
        issued_at_ms: u64,
    ) -> Result<SignedApprovalReceipt, BridgeError> {
        transaction::issue_approval(&self.session, binding, issued_at_ms)
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

    pub async fn stream_scene_snapshot<S: SceneSnapshotSink>(
        &mut self,
        sink: &mut S,
    ) -> Result<SceneSnapshotTransfer, BridgeError> {
        scene::stream_scene_snapshot(&mut self.session, sink).await
    }

    pub async fn get_scene_snapshot(&mut self) -> Result<SceneSnapshot, BridgeError> {
        scene::get_scene_snapshot(&mut self.session).await
    }

    pub async fn get_next_scene_delta(
        &mut self,
        after_scene_graph_revision: u64,
    ) -> Result<SceneDeltaPoll, BridgeError> {
        scene::get_next_scene_delta(&mut self.session, after_scene_graph_revision).await
    }

    pub async fn stream_script_snapshot<S: ScriptSnapshotSink>(
        &mut self,
        sink: &mut S,
    ) -> Result<ScriptSnapshotTransfer, BridgeError> {
        script::stream_script_snapshot(&mut self.session, sink).await
    }

    pub async fn get_script_snapshot(&mut self) -> Result<ScriptSnapshot, BridgeError> {
        script::get_script_snapshot(&mut self.session).await
    }

    pub async fn get_normalized_script_snapshot(
        &mut self,
    ) -> Result<NormalizedScriptGraph, BridgeError> {
        let snapshot = self.get_script_snapshot().await?;
        normalize_script_snapshot(&snapshot)
    }

    pub async fn get_next_script_delta(
        &mut self,
        after_script_graph_revision: u64,
    ) -> Result<ScriptDeltaPoll, BridgeError> {
        script::get_next_script_delta(&mut self.session, after_script_graph_revision).await
    }

    pub async fn run_runtime(
        &mut self,
        target: RuntimeTarget,
    ) -> Result<RuntimeStateResult, BridgeError> {
        runtime::run_runtime(&mut self.session, target).await
    }

    pub async fn stop_runtime(
        &mut self,
        runtime_session_id: &str,
        expected_runtime_event_seq: Option<u64>,
    ) -> Result<RuntimeStateResult, BridgeError> {
        runtime::stop_runtime(
            &mut self.session,
            runtime_session_id,
            expected_runtime_event_seq,
        )
        .await
    }

    pub async fn pause_runtime(
        &mut self,
        runtime_session_id: &str,
        expected_runtime_event_seq: Option<u64>,
    ) -> Result<RuntimeStateResult, BridgeError> {
        runtime::pause_runtime(
            &mut self.session,
            runtime_session_id,
            expected_runtime_event_seq,
        )
        .await
    }

    pub async fn continue_runtime(
        &mut self,
        runtime_session_id: &str,
        expected_runtime_event_seq: Option<u64>,
    ) -> Result<RuntimeStateResult, BridgeError> {
        runtime::continue_runtime(
            &mut self.session,
            runtime_session_id,
            expected_runtime_event_seq,
        )
        .await
    }

    pub async fn get_runtime_snapshot(
        &mut self,
        runtime_session_id: &str,
        expected_runtime_event_seq: Option<u64>,
        domains: Option<Vec<RuntimeDomain>>,
    ) -> Result<RuntimeSnapshot, BridgeError> {
        runtime::get_runtime_snapshot(
            &mut self.session,
            runtime_session_id,
            expected_runtime_event_seq,
            domains,
        )
        .await
    }

    pub async fn inspect_runtime_object(
        &mut self,
        runtime_session_id: &str,
        runtime_object_id: &str,
        expected_runtime_event_seq: Option<u64>,
    ) -> Result<RuntimeObjectResult, BridgeError> {
        runtime::inspect_runtime_object(
            &mut self.session,
            runtime_session_id,
            runtime_object_id,
            expected_runtime_event_seq,
        )
        .await
    }

    pub async fn get_runtime_stack(
        &mut self,
        runtime_session_id: &str,
        runtime_stack_id: &str,
        expected_runtime_event_seq: Option<u64>,
    ) -> Result<RuntimeStackResult, BridgeError> {
        runtime::get_runtime_stack(
            &mut self.session,
            runtime_session_id,
            runtime_stack_id,
            expected_runtime_event_seq,
        )
        .await
    }

    pub async fn capture_runtime_viewport(
        &mut self,
        runtime_session_id: &str,
        expected_runtime_event_seq: Option<u64>,
        max_width: u32,
        max_height: u32,
    ) -> Result<RuntimeViewportCapture, BridgeError> {
        runtime::capture_runtime_viewport(
            &mut self.session,
            runtime_session_id,
            expected_runtime_event_seq,
            max_width,
            max_height,
        )
        .await
    }
}

pub async fn run_bridge_sync(project_root: PathBuf, replicator: SnapshotReplicator) -> ! {
    let mut retry = Duration::from_millis(200);
    loop {
        let result = protocol::run_session(&project_root, &replicator).await;
        if let Err(error) = &result
            && std::env::var_os("GODOT_CODEX_DEBUG_ERRORS").is_some()
        {
            eprintln!("[godot-codex-bridge-sync] {error}");
        }
        let message = result.map_or_else(
            |error| error.safe_summary().to_owned(),
            |()| "bridge session ended".to_owned(),
        );
        replicator.mark_disconnected(message);
        tokio::time::sleep(retry).await;
        retry = (retry * 2).min(Duration::from_secs(5));
    }
}
