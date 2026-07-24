mod journal;
mod recovery;
mod validation;

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use godot_codex_bridge_client::{
    AffectedEntityRole, ApprovalBinding, ApprovalScope, BridgeClient, BridgeError, OperationKind,
    PrepareResult, RevisionCoordinates, Risk, SafeTransactionError, TransactionEvent,
    TransactionLimits, TransactionOperation, TransactionPreview, TransactionState,
    TransactionStatus, UndoEligibility,
};
pub use journal::{
    JOURNAL_SCHEMA, JournalError, MAX_JOURNAL_BYTES, MAX_JOURNAL_RECORDS, TransactionJournalRecord,
};
use journal::{JournalState, JournalStore};
pub use recovery::{
    RecoveryDisposition, RollbackDecision, RollbackEvidence, RollbackPolicy, RuntimeValidationGate,
    RuntimeValidationObservation, RuntimeValidationState,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
pub use validation::{
    CheckAuthority, CheckOutcome, DiagnosticFingerprint, DiagnosticSeverity, DiagnosticSummary,
    ExpectedSemanticDelta, MAX_REPORT_PAGE_BYTES, MAX_REPORT_PAGES, MAX_RETAINED_REPORT_BYTES,
    ReportPage, SemanticComparison, SemanticRelation, SemanticSnapshot, ValidationCheck,
    ValidationCoordinator, ValidationError, ValidationPolicy, ValidationReport,
    ValidationReportOutcome,
};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

pub type BridgeFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Debug)]
pub struct PrepareCommand {
    pub project_id: String,
    pub editor_session_id: String,
    pub idempotency_key: String,
    pub coordinates: RevisionCoordinates,
    pub operation: TransactionOperation,
}

#[derive(Clone, Debug)]
pub struct PrepareTransactionContext {
    pub project_id: String,
    pub editor_session_id: String,
    pub idempotency_key: String,
    pub coordinates: RevisionCoordinates,
}

impl PrepareTransactionContext {
    fn with_operation(self, operation: TransactionOperation) -> PrepareCommand {
        PrepareCommand {
            project_id: self.project_id,
            editor_session_id: self.editor_session_id,
            idempotency_key: self.idempotency_key,
            coordinates: self.coordinates,
            operation,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PrepareCreateNodeCommand {
    pub context: PrepareTransactionContext,
    pub parent_node_id: String,
    pub godot_type: String,
    pub name: String,
    pub insertion_index: Option<u32>,
}

impl From<PrepareCreateNodeCommand> for PrepareCommand {
    fn from(command: PrepareCreateNodeCommand) -> Self {
        command
            .context
            .with_operation(TransactionOperation::CreateNode {
                parent_node_id: command.parent_node_id,
                godot_type: command.godot_type,
                name: command.name,
                insertion_index: command.insertion_index,
            })
    }
}

#[derive(Clone, Debug)]
pub struct PrepareDeleteNodeCommand {
    pub context: PrepareTransactionContext,
    pub node_id: String,
}

impl From<PrepareDeleteNodeCommand> for PrepareCommand {
    fn from(command: PrepareDeleteNodeCommand) -> Self {
        command
            .context
            .with_operation(TransactionOperation::DeleteNode {
                node_id: command.node_id,
            })
    }
}

#[derive(Clone, Debug)]
pub struct PrepareReparentNodeCommand {
    pub context: PrepareTransactionContext,
    pub node_id: String,
    pub new_parent_node_id: String,
    pub insertion_index: u32,
    pub keep_global_transform: bool,
}

impl From<PrepareReparentNodeCommand> for PrepareCommand {
    fn from(command: PrepareReparentNodeCommand) -> Self {
        command
            .context
            .with_operation(TransactionOperation::ReparentNode {
                node_id: command.node_id,
                new_parent_node_id: command.new_parent_node_id,
                insertion_index: command.insertion_index,
                keep_global_transform: command.keep_global_transform,
            })
    }
}

#[derive(Clone, Debug)]
pub struct PrepareSetPropertyCommand {
    pub context: PrepareTransactionContext,
    pub node_id: String,
    pub property: String,
    pub value: godot_codex_bridge_client::WritableVariant,
}

impl From<PrepareSetPropertyCommand> for PrepareCommand {
    fn from(command: PrepareSetPropertyCommand) -> Self {
        command
            .context
            .with_operation(TransactionOperation::SetProperty {
                node_id: command.node_id,
                property: command.property,
                value: command.value,
            })
    }
}

#[derive(Clone, Debug)]
pub struct PrepareAttachScriptCommand {
    pub context: PrepareTransactionContext,
    pub node_id: String,
    pub script_ref: godot_codex_bridge_client::TransactionResourceRef,
}

impl From<PrepareAttachScriptCommand> for PrepareCommand {
    fn from(command: PrepareAttachScriptCommand) -> Self {
        command
            .context
            .with_operation(TransactionOperation::AttachScript {
                node_id: command.node_id,
                script_ref: command.script_ref,
            })
    }
}

#[derive(Clone, Debug)]
pub struct PrepareDetachScriptCommand {
    pub context: PrepareTransactionContext,
    pub node_id: String,
}

impl From<PrepareDetachScriptCommand> for PrepareCommand {
    fn from(command: PrepareDetachScriptCommand) -> Self {
        command
            .context
            .with_operation(TransactionOperation::DetachScript {
                node_id: command.node_id,
            })
    }
}

#[derive(Clone, Debug)]
pub struct PrepareConnectSignalCommand {
    pub context: PrepareTransactionContext,
    pub emitter_node_id: String,
    pub signal: String,
    pub receiver_node_id: String,
    pub method: String,
    pub flags: u8,
    pub unbinds: u16,
    pub binds: Vec<godot_codex_bridge_client::WritableVariant>,
}

impl From<PrepareConnectSignalCommand> for PrepareCommand {
    fn from(command: PrepareConnectSignalCommand) -> Self {
        command
            .context
            .with_operation(TransactionOperation::ConnectSignal {
                emitter_node_id: command.emitter_node_id,
                signal: command.signal,
                receiver_node_id: command.receiver_node_id,
                method: command.method,
                flags: command.flags,
                unbinds: command.unbinds,
                binds: command.binds,
            })
    }
}

#[derive(Clone, Debug)]
pub struct PrepareDisconnectSignalCommand {
    pub context: PrepareTransactionContext,
    pub emitter_node_id: String,
    pub signal: String,
    pub receiver_node_id: String,
    pub method: String,
    pub flags: u8,
    pub unbinds: u16,
    pub binds: Vec<godot_codex_bridge_client::WritableVariant>,
}

impl From<PrepareDisconnectSignalCommand> for PrepareCommand {
    fn from(command: PrepareDisconnectSignalCommand) -> Self {
        command
            .context
            .with_operation(TransactionOperation::DisconnectSignal {
                emitter_node_id: command.emitter_node_id,
                signal: command.signal,
                receiver_node_id: command.receiver_node_id,
                method: command.method,
                flags: command.flags,
                unbinds: command.unbinds,
                binds: command.binds,
            })
    }
}

#[derive(Clone, Debug)]
pub struct ApplyCommand {
    pub transaction_id: String,
    pub preview_digest: String,
    pub expected_scene_revision: u64,
    pub expected_operation_seq: u64,
}

#[derive(Clone, Debug)]
pub struct UndoCommand {
    pub transaction_id: String,
    pub expected_transaction_seq: u64,
    pub expected_scene_revision: u64,
    pub expected_operation_seq: u64,
}

#[derive(Clone, Debug)]
pub struct ObservedPrepare {
    pub project_id: String,
    pub editor_session_id: String,
    pub result: PrepareResult,
}

#[derive(Clone, Debug)]
pub struct ObservedStatus {
    pub project_id: String,
    pub editor_session_id: String,
    pub result: TransactionStatus,
}

#[derive(Clone, Debug)]
pub struct BridgeApplyOutcome {
    pub observed: ObservedStatus,
    pub receipt_hash: String,
}

#[derive(Debug)]
pub struct BridgeApplyFailure {
    pub error: BridgeError,
    pub may_have_committed: bool,
}

pub trait TransactionBridge: Send + Sync {
    fn prepare<'a>(
        &'a self,
        command: &'a PrepareCommand,
    ) -> BridgeFuture<'a, Result<ObservedPrepare, BridgeError>>;

    fn status<'a>(
        &'a self,
        transaction_id: &'a str,
    ) -> BridgeFuture<'a, Result<ObservedStatus, BridgeError>>;

    fn apply_approved<'a>(
        &'a self,
        command: &'a ApplyCommand,
        binding: &'a ApprovalBinding,
        issued_at_ms: u64,
    ) -> BridgeFuture<'a, Result<BridgeApplyOutcome, BridgeApplyFailure>>;

    fn undo<'a>(
        &'a self,
        command: &'a UndoCommand,
    ) -> BridgeFuture<'a, Result<ObservedStatus, BridgeError>>;
}

#[derive(Clone, Debug)]
pub struct LiveTransactionBridge {
    project_root: PathBuf,
}

impl LiveTransactionBridge {
    #[must_use]
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }
}

impl TransactionBridge for LiveTransactionBridge {
    fn prepare<'a>(
        &'a self,
        command: &'a PrepareCommand,
    ) -> BridgeFuture<'a, Result<ObservedPrepare, BridgeError>> {
        Box::pin(async move {
            let mut client = BridgeClient::connect(&self.project_root).await?;
            require_expected_session(&client, &command.project_id, &command.editor_session_id)?;
            let result = client
                .prepare_transaction(
                    &command.idempotency_key,
                    command.coordinates.clone(),
                    command.operation.clone(),
                )
                .await?;
            Ok(ObservedPrepare {
                project_id: client.project_id().to_owned(),
                editor_session_id: client.editor_session_id().to_owned(),
                result,
            })
        })
    }

    fn status<'a>(
        &'a self,
        transaction_id: &'a str,
    ) -> BridgeFuture<'a, Result<ObservedStatus, BridgeError>> {
        Box::pin(async move {
            let mut client = BridgeClient::connect(&self.project_root).await?;
            let result = client.get_transaction_status(transaction_id).await?;
            Ok(ObservedStatus {
                project_id: client.project_id().to_owned(),
                editor_session_id: client.editor_session_id().to_owned(),
                result,
            })
        })
    }

    fn apply_approved<'a>(
        &'a self,
        command: &'a ApplyCommand,
        binding: &'a ApprovalBinding,
        issued_at_ms: u64,
    ) -> BridgeFuture<'a, Result<BridgeApplyOutcome, BridgeApplyFailure>> {
        Box::pin(async move {
            let mut client = BridgeClient::connect(&self.project_root)
                .await
                .map_err(|error| BridgeApplyFailure {
                    error,
                    may_have_committed: false,
                })?;
            require_expected_session(&client, &binding.project_id, &binding.editor_session_id)
                .map_err(|error| BridgeApplyFailure {
                    error,
                    may_have_committed: false,
                })?;
            let signed = client
                .issue_transaction_approval(binding, issued_at_ms)
                .map_err(|error| BridgeApplyFailure {
                    error,
                    may_have_committed: false,
                })?;
            let result = client
                .apply_transaction(
                    &command.transaction_id,
                    &command.preview_digest,
                    command.expected_scene_revision,
                    command.expected_operation_seq,
                    signed.receipt,
                )
                .await
                .map_err(|error| BridgeApplyFailure {
                    error,
                    may_have_committed: true,
                })?;
            Ok(BridgeApplyOutcome {
                observed: ObservedStatus {
                    project_id: client.project_id().to_owned(),
                    editor_session_id: client.editor_session_id().to_owned(),
                    result,
                },
                receipt_hash: signed.receipt_hash,
            })
        })
    }

    fn undo<'a>(
        &'a self,
        command: &'a UndoCommand,
    ) -> BridgeFuture<'a, Result<ObservedStatus, BridgeError>> {
        Box::pin(async move {
            let mut client = BridgeClient::connect(&self.project_root).await?;
            let result = client
                .undo_transaction(
                    &command.transaction_id,
                    command.expected_transaction_seq,
                    command.expected_scene_revision,
                    command.expected_operation_seq,
                )
                .await?;
            Ok(ObservedStatus {
                project_id: client.project_id().to_owned(),
                editor_session_id: client.editor_session_id().to_owned(),
                result,
            })
        })
    }
}

fn require_expected_session(
    client: &BridgeClient,
    project_id: &str,
    editor_session_id: &str,
) -> Result<(), BridgeError> {
    if client.project_id() != project_id || client.editor_session_id() != editor_session_id {
        return Err(BridgeError::Invalid(
            "transaction session binding is stale".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApprovalDecision {
    Accept,
    Decline,
    Cancel,
    Timeout,
    Invalid,
    Unsupported,
}

#[derive(Clone, Debug, Serialize)]
pub struct ApprovalPrompt {
    pub transaction_id: String,
    pub preview_digest: String,
    pub risk: Risk,
    pub scope: ApprovalScope,
    pub preview: TransactionPreview,
    pub expires_at_ms: u64,
}

pub trait ApprovalProvider: Send + Sync {
    fn request<'a>(&'a self, prompt: ApprovalPrompt) -> BridgeFuture<'a, ApprovalDecision>;
}

pub trait TransactionClock: Send + Sync {
    fn now_ms(&self) -> u64;
}

#[derive(Debug)]
pub struct SystemClock;

impl TransactionClock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
            })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PreparedTransactionView {
    pub transaction_id: String,
    pub editor_session_id: String,
    pub scene_id: String,
    pub state: TransactionState,
    pub transaction_seq: u64,
    pub scene_revision: u64,
    pub operation_seq: u64,
    pub operation_kind: OperationKind,
    pub risk: Risk,
    pub scope: ApprovalScope,
    pub affected_entities: Vec<godot_codex_bridge_client::AffectedEntity>,
    pub preview: TransactionPreview,
    pub preview_digest: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub limits_applied: TransactionLimits,
}

#[derive(Clone, Debug, Serialize)]
pub struct TransactionStatusView {
    pub transaction_id: String,
    pub editor_session_id: String,
    pub scene_id: String,
    pub state: TransactionState,
    pub transaction_seq: u64,
    pub operation_kind: OperationKind,
    pub risk: Risk,
    pub scope: ApprovalScope,
    pub preview_digest: String,
    pub current_scene_revision: u64,
    pub current_operation_seq: u64,
    pub outcome: Option<godot_codex_bridge_client::TransactionOutcome>,
    pub error: Option<SafeTransactionError>,
    pub undo_eligibility: UndoEligibility,
    pub committed_entities: Option<Vec<godot_codex_bridge_client::AffectedEntity>>,
    pub updated_at_ms: u64,
    pub limits_applied: TransactionLimits,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct TransactionError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub transaction_id: Option<String>,
}

impl std::fmt::Display for TransactionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TransactionError {}

impl TransactionError {
    fn new(code: &str, message: &str, retryable: bool) -> Self {
        Self {
            code: code.to_owned(),
            message: message.to_owned(),
            retryable,
            transaction_id: None,
        }
    }

    fn for_transaction(mut self, transaction_id: &str) -> Self {
        self.transaction_id = Some(transaction_id.to_owned());
        self
    }

    fn journal(_error: &JournalError) -> Self {
        Self::new(
            "transaction_coordinator_unavailable",
            "The transaction recovery journal is unavailable.",
            true,
        )
    }

    fn bridge(error: &BridgeError) -> Self {
        match error {
            BridgeError::Rpc {
                code,
                message,
                retryable,
                ..
            } => {
                let code = if valid_error_code(code) {
                    code.as_str()
                } else {
                    "transaction_bridge_error"
                };
                let message = if valid_safe_error_message(message) {
                    message.as_str()
                } else {
                    "The Bridge rejected the transaction request."
                };
                Self::new(code, message, *retryable)
            }
            BridgeError::CapabilityUnavailable { .. } => Self::new(
                "transaction_coordinator_unavailable",
                "The Bridge transaction capability is unavailable.",
                true,
            ),
            _ => Self::new(
                "transaction_coordinator_unavailable",
                error.safe_summary(),
                true,
            ),
        }
    }
}

fn valid_error_code(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn valid_safe_error_message(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 256
        || value.chars().any(char::is_control)
        || value.contains("/Users/")
        || value.contains("/home/")
        || value.contains('\\')
        || value.starts_with('/')
    {
        return false;
    }
    let bytes = value.as_bytes();
    !(bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/')
}

struct CoordinatorState {
    journal: JournalState,
    previews: BTreeMap<String, PrepareResult>,
}

pub struct TransactionCoordinator {
    project_id: String,
    bridge: Arc<dyn TransactionBridge>,
    clock: Arc<dyn TransactionClock>,
    journal_store: JournalStore,
    journal_recovered_corruption: bool,
    state: tokio::sync::Mutex<CoordinatorState>,
    operation_gate: tokio::sync::Mutex<()>,
    pending_approvals: std::sync::Mutex<BTreeSet<String>>,
}

impl std::fmt::Debug for TransactionCoordinator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TransactionCoordinator")
            .field("project_id", &self.project_id)
            .finish_non_exhaustive()
    }
}

impl TransactionCoordinator {
    pub fn open(project_root: &Path) -> Result<Arc<Self>, TransactionError> {
        let canonical_root = std::fs::canonicalize(project_root).map_err(|_| {
            TransactionError::new(
                "transaction_coordinator_unavailable",
                "The project root is unavailable.",
                true,
            )
        })?;
        let project_id =
            godot_codex_bridge_client::project_id_for_path(&canonical_root).map_err(|_| {
                TransactionError::new(
                    "transaction_coordinator_unavailable",
                    "The project root encoding is unsupported.",
                    false,
                )
            })?;
        Self::open_with(
            &canonical_root,
            project_id,
            Arc::new(LiveTransactionBridge::new(canonical_root.clone())),
            Arc::new(SystemClock),
        )
    }

    pub fn open_with(
        project_root: &Path,
        project_id: String,
        bridge: Arc<dyn TransactionBridge>,
        clock: Arc<dyn TransactionClock>,
    ) -> Result<Arc<Self>, TransactionError> {
        let (journal_store, journal) = JournalStore::open(project_root, &project_id)
            .map_err(|error| TransactionError::journal(&error))?;
        let journal_recovered_corruption = journal_store.recovered_corruption();
        Ok(Arc::new(Self {
            project_id,
            bridge,
            clock,
            journal_store,
            journal_recovered_corruption,
            state: tokio::sync::Mutex::new(CoordinatorState {
                journal,
                previews: BTreeMap::new(),
            }),
            operation_gate: tokio::sync::Mutex::new(()),
            pending_approvals: std::sync::Mutex::new(BTreeSet::new()),
        }))
    }

    #[must_use]
    pub fn journal_recovered_corruption(&self) -> bool {
        self.journal_recovered_corruption
    }

    #[must_use]
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub async fn prepare(
        &self,
        command: PrepareCommand,
    ) -> Result<PreparedTransactionView, TransactionError> {
        let _gate = self.operation_gate.lock().await;
        validate_prepare_command(&command)?;
        if command.project_id != self.project_id {
            return Err(TransactionError::new(
                "wrong_project",
                "The transaction project binding does not match this MCP server.",
                false,
            ));
        }
        command
            .operation
            .validate()
            .map_err(|error| TransactionError::bridge(&error))?;
        let idempotency_fingerprint = domain_digest(
            b"godot-codex/idempotency-key/v1\0",
            command.idempotency_key.as_bytes(),
        );
        let operation_json = serde_json::to_vec(&command.operation).map_err(|_| {
            TransactionError::new(
                "transaction_too_large",
                "The transaction operation could not be encoded safely.",
                false,
            )
        })?;
        let operation_digest = domain_digest(b"godot-codex/operation/v1\0", &operation_json);
        {
            let state = self.state.lock().await;
            if let Some(existing) = state
                .journal
                .records
                .values()
                .find(|record| record.idempotency_fingerprint == idempotency_fingerprint)
            {
                if existing.operation_digest != operation_digest
                    || existing.scene_id != command.coordinates.scene_id
                    || existing.history_id != command.coordinates.history_id
                    || existing.scene_revision != command.coordinates.scene_revision
                    || existing.operation_seq != command.coordinates.operation_seq
                {
                    return Err(TransactionError::new(
                        "idempotency_conflict",
                        "The idempotency key is already bound to a different transaction.",
                        false,
                    ));
                }
                if let Some(preview) = state.previews.get(&existing.transaction_id) {
                    return Ok(prepared_view(&command.editor_session_id, preview));
                }
            }
        }

        let observed = self
            .bridge
            .prepare(&command)
            .await
            .map_err(|error| TransactionError::bridge(&error))?;
        validate_observed_prepare(&command, &observed)?;
        let result = observed.result;
        let record = TransactionJournalRecord {
            transaction_id: result.coordinates.transaction_id.clone(),
            idempotency_fingerprint,
            operation_digest,
            editor_session_id: observed.editor_session_id.clone(),
            scene_id: result.coordinates.scene_id.clone(),
            history_id: result.coordinates.history_id.clone(),
            state: result.state,
            transaction_seq: result.coordinates.transaction_seq,
            scene_revision: result.coordinates.scene_revision,
            operation_seq: result.coordinates.operation_seq,
            operation_kind: result.operation_kind,
            risk: result.risk,
            scope: result.scope,
            affected_entities: result.affected_entities.clone(),
            preview_digest: result.preview_digest.clone(),
            created_at_ms: result.created_at_ms,
            updated_at_ms: result.created_at_ms,
            expires_at_ms: result.expires_at_ms,
            apply_dispatched: false,
            undo_eligible: false,
            safe_error: None,
            receipt_hash: None,
        };
        let mut state = self.state.lock().await;
        if let Some(existing) = state
            .journal
            .records
            .values()
            .find(|entry| entry.idempotency_fingerprint == record.idempotency_fingerprint)
            && (existing.transaction_id != record.transaction_id
                || existing.preview_digest != record.preview_digest)
        {
            return Err(TransactionError::new(
                "idempotency_conflict",
                "Bridge returned a different immutable transaction for the idempotency key.",
                false,
            ));
        }
        prune_previews(&mut state);
        state
            .journal
            .records
            .insert(record.transaction_id.clone(), record);
        state
            .previews
            .insert(result.coordinates.transaction_id.clone(), result.clone());
        self.journal_store
            .persist(&mut state.journal)
            .map_err(|error| TransactionError::journal(&error))?;
        Ok(prepared_view(&observed.editor_session_id, &result))
    }

    pub async fn status(
        &self,
        transaction_id: &str,
    ) -> Result<TransactionStatusView, TransactionError> {
        if !valid_identifier(transaction_id, "transaction:", 32) {
            return Err(TransactionError::new(
                "invalid_transaction_request",
                "The transaction identifier is malformed.",
                false,
            ));
        }
        {
            let state = self.state.lock().await;
            let record = state.journal.records.get(transaction_id).ok_or_else(|| {
                TransactionError::new(
                    "transaction_not_found",
                    "The transaction is not present in the recovery index.",
                    false,
                )
                .for_transaction(transaction_id)
            })?;
            if matches!(
                record.state,
                TransactionState::Rejected | TransactionState::Expired
            ) {
                return Ok(local_terminal_status_view(record));
            }
        }
        let observed =
            self.bridge.status(transaction_id).await.map_err(|error| {
                TransactionError::bridge(&error).for_transaction(transaction_id)
            })?;
        self.accept_status(observed).await
    }

    pub async fn apply(
        &self,
        command: ApplyCommand,
        approval_provider: &dyn ApprovalProvider,
    ) -> Result<TransactionStatusView, TransactionError> {
        {
            let mut pending = self
                .pending_approvals
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !pending.insert(command.transaction_id.clone()) {
                return Err(TransactionError::new(
                    "transaction_busy",
                    "An approval interaction is already active for this transaction.",
                    true,
                )
                .for_transaction(&command.transaction_id));
            }
        }
        let _pending_guard = PendingApprovalGuard {
            pending: &self.pending_approvals,
            transaction_id: command.transaction_id.clone(),
        };
        self.apply_inner(&command, approval_provider).await
    }

    async fn apply_inner(
        &self,
        command: &ApplyCommand,
        approval_provider: &dyn ApprovalProvider,
    ) -> Result<TransactionStatusView, TransactionError> {
        let _gate = self.operation_gate.lock().await;
        let (record, preview, status_only) = {
            let mut state = self.state.lock().await;
            let now = self.clock.now_ms();
            let status_only = {
                let record = state
                    .journal
                    .records
                    .get_mut(&command.transaction_id)
                    .ok_or_else(|| {
                        TransactionError::new(
                            "transaction_not_found",
                            "The transaction is not present in the recovery index.",
                            false,
                        )
                        .for_transaction(&command.transaction_id)
                    })?;
                if now > record.expires_at_ms
                    && matches!(
                        record.state,
                        TransactionState::Previewed | TransactionState::AwaitingApproval
                    )
                {
                    record.state = TransactionState::Expired;
                    record.updated_at_ms = now;
                    state.previews.remove(&command.transaction_id);
                    self.journal_store
                        .persist(&mut state.journal)
                        .map_err(|error| TransactionError::journal(&error))?;
                    return Err(TransactionError::new(
                        "transaction_expired",
                        "The prepared transaction has expired.",
                        false,
                    )
                    .for_transaction(&command.transaction_id));
                }
                validate_apply_command(record, command)?;
                let status_only = record.apply_dispatched || record.state.forbids_apply_replay();
                if !status_only
                    && !matches!(
                        record.state,
                        TransactionState::Previewed | TransactionState::AwaitingApproval
                    )
                {
                    let (code, message) = match record.state {
                        TransactionState::Expired => (
                            "transaction_expired",
                            "The prepared transaction has expired.",
                        ),
                        TransactionState::Conflicted => (
                            "transaction_conflicted",
                            "The prepared transaction was invalidated by an editor change.",
                        ),
                        TransactionState::Rejected => (
                            "approval_declined",
                            "The transaction approval was previously declined.",
                        ),
                        _ => (
                            "transaction_in_doubt",
                            "Apply cannot be replayed; query transaction status.",
                        ),
                    };
                    return Err(TransactionError::new(code, message, false)
                        .for_transaction(&command.transaction_id));
                }
                status_only
            };
            let record = state
                .journal
                .records
                .get(&command.transaction_id)
                .expect("record validated above")
                .clone();
            let preview = if status_only {
                None
            } else {
                state.previews.get(&command.transaction_id).cloned()
            };
            (record, preview, status_only)
        };
        if status_only {
            let observed = self
                .bridge
                .status(&command.transaction_id)
                .await
                .map_err(|error| {
                    TransactionError::bridge(&error).for_transaction(&command.transaction_id)
                })?;
            let reconciled = self.accept_status(observed).await?;
            if matches!(
                reconciled.state,
                TransactionState::Previewed | TransactionState::AwaitingApproval
            ) {
                return Err(TransactionError::new(
                    "approval_required",
                    "Bridge proved that the earlier apply did not reach the commit point; request a new approval.",
                    true,
                )
                .for_transaction(&command.transaction_id));
            }
            let (code, message) = if reconciled.state == TransactionState::InDoubt {
                (
                    "transaction_in_doubt",
                    "Apply outcome is unknown; use transaction status and do not replay apply.",
                )
            } else {
                (
                    "transaction_apply_replay_forbidden",
                    "This transaction has crossed the apply boundary; use transaction status instead.",
                )
            };
            return Err(TransactionError::new(code, message, false)
                .for_transaction(&command.transaction_id));
        }
        let preview = preview.ok_or_else(|| {
            TransactionError::new(
                "approval_required",
                "Repeat the same prepare call before requesting approval after reconnect.",
                false,
            )
            .for_transaction(&command.transaction_id)
        })?;

        let observed = self
            .bridge
            .status(&command.transaction_id)
            .await
            .map_err(|error| TransactionError::bridge(&error))?;
        let preflight = self.accept_status(observed).await?;
        validate_preflight_state(&preflight)?;

        {
            let mut state = self.state.lock().await;
            let current = state
                .journal
                .records
                .get_mut(&command.transaction_id)
                .ok_or_else(|| {
                    TransactionError::new(
                        "transaction_not_found",
                        "The transaction disappeared before approval.",
                        false,
                    )
                })?;
            validate_apply_command(current, command)?;
            current.state = TransactionState::AwaitingApproval;
            current.updated_at_ms = self.clock.now_ms();
            current.safe_error = None;
            self.journal_store
                .persist(&mut state.journal)
                .map_err(|error| TransactionError::journal(&error))?;
        }

        let decision = approval_provider
            .request(ApprovalPrompt {
                transaction_id: command.transaction_id.clone(),
                preview_digest: command.preview_digest.clone(),
                risk: record.risk,
                scope: record.scope,
                preview: preview.preview.clone(),
                expires_at_ms: record.expires_at_ms,
            })
            .await;
        match decision {
            ApprovalDecision::Accept => {}
            ApprovalDecision::Decline => {
                self.record_approval_result(
                    &command.transaction_id,
                    TransactionState::Rejected,
                    "approval_declined",
                    "The transaction approval was declined.",
                )
                .await?;
                return Err(TransactionError::new(
                    "approval_declined",
                    "The transaction approval was declined.",
                    false,
                )
                .for_transaction(&command.transaction_id));
            }
            ApprovalDecision::Cancel => {
                self.record_approval_result(
                    &command.transaction_id,
                    TransactionState::AwaitingApproval,
                    "approval_cancelled",
                    "The transaction approval was cancelled.",
                )
                .await?;
                return Err(TransactionError::new(
                    "approval_cancelled",
                    "The transaction approval was cancelled.",
                    true,
                )
                .for_transaction(&command.transaction_id));
            }
            ApprovalDecision::Timeout => {
                self.record_approval_result(
                    &command.transaction_id,
                    TransactionState::AwaitingApproval,
                    "approval_timeout",
                    "The transaction approval timed out.",
                )
                .await?;
                return Err(TransactionError::new(
                    "approval_timeout",
                    "The transaction approval timed out.",
                    true,
                )
                .for_transaction(&command.transaction_id));
            }
            ApprovalDecision::Invalid => {
                return Err(TransactionError::new(
                    "approval_invalid",
                    "The approval response did not contain confirm=true.",
                    true,
                )
                .for_transaction(&command.transaction_id));
            }
            ApprovalDecision::Unsupported => {
                return Err(TransactionError::new(
                    "approval_host_unsupported",
                    "The MCP host does not support form elicitation.",
                    false,
                )
                .for_transaction(&command.transaction_id));
            }
        }

        {
            let mut state = self.state.lock().await;
            let now = self.clock.now_ms();
            let current = state
                .journal
                .records
                .get_mut(&command.transaction_id)
                .ok_or_else(|| {
                    TransactionError::new(
                        "transaction_not_found",
                        "The transaction disappeared after approval.",
                        false,
                    )
                    .for_transaction(&command.transaction_id)
                })?;
            if now > current.expires_at_ms {
                current.state = TransactionState::Expired;
                current.updated_at_ms = now;
                current.safe_error = Some(SafeTransactionError {
                    code: "transaction_expired".to_owned(),
                    message: "The prepared transaction expired before apply.".to_owned(),
                    retryable: false,
                });
                state.previews.remove(&command.transaction_id);
                self.journal_store
                    .persist(&mut state.journal)
                    .map_err(|error| TransactionError::journal(&error))?;
                return Err(TransactionError::new(
                    "transaction_expired",
                    "The prepared transaction expired before apply.",
                    false,
                )
                .for_transaction(&command.transaction_id));
            }
        }

        let observed = self
            .bridge
            .status(&command.transaction_id)
            .await
            .map_err(|error| TransactionError::bridge(&error))?;
        let last_preflight = self.accept_status(observed).await?;
        validate_preflight_state(&last_preflight)?;

        let binding = {
            let mut state = self.state.lock().await;
            let current = state
                .journal
                .records
                .get_mut(&command.transaction_id)
                .ok_or_else(|| {
                    TransactionError::new(
                        "transaction_not_found",
                        "The transaction disappeared before apply.",
                        false,
                    )
                })?;
            validate_apply_command(current, command)?;
            current.state = TransactionState::Applying;
            current.apply_dispatched = true;
            current.updated_at_ms = self.clock.now_ms();
            current.safe_error = None;
            let binding = ApprovalBinding {
                project_id: self.project_id.clone(),
                editor_session_id: current.editor_session_id.clone(),
                scene_id: current.scene_id.clone(),
                transaction_id: current.transaction_id.clone(),
                preview_digest: current.preview_digest.clone(),
                scope: current.scope,
                risk: current.risk,
                scene_revision: current.scene_revision,
                operation_seq: current.operation_seq,
            };
            self.journal_store
                .persist(&mut state.journal)
                .map_err(|error| TransactionError::journal(&error))?;
            binding
        };
        let issued_at_ms = self.clock.now_ms();
        match self
            .bridge
            .apply_approved(command, &binding, issued_at_ms)
            .await
        {
            Ok(outcome) => {
                let mut state = self.state.lock().await;
                if let Some(record) = state.journal.records.get_mut(&command.transaction_id) {
                    record.receipt_hash = Some(outcome.receipt_hash);
                }
                update_record_from_status(
                    &mut state.journal,
                    &outcome.observed.editor_session_id,
                    &outcome.observed.result,
                );
                self.journal_store
                    .persist(&mut state.journal)
                    .map_err(|error| TransactionError::journal(&error))?;
                Ok(status_view(
                    &outcome.observed.editor_session_id,
                    &outcome.observed.result,
                ))
            }
            Err(failure) if !failure.may_have_committed => {
                let mut state = self.state.lock().await;
                if let Some(record) = state.journal.records.get_mut(&command.transaction_id) {
                    record.state = TransactionState::AwaitingApproval;
                    record.apply_dispatched = false;
                    record.updated_at_ms = self.clock.now_ms();
                }
                self.journal_store
                    .persist(&mut state.journal)
                    .map_err(|error| TransactionError::journal(&error))?;
                Err(TransactionError::bridge(&failure.error)
                    .for_transaction(&command.transaction_id))
            }
            Err(failure) => {
                if matches!(failure.error, BridgeError::Rpc { .. })
                    && let Ok(observed) = self.bridge.status(&command.transaction_id).await
                {
                    let view = self.accept_status(observed).await?;
                    if !matches!(
                        view.state,
                        TransactionState::Previewed | TransactionState::AwaitingApproval
                    ) {
                        return Ok(view);
                    }
                    let mut state = self.state.lock().await;
                    if let Some(record) = state.journal.records.get_mut(&command.transaction_id) {
                        record.apply_dispatched = false;
                    }
                    self.journal_store
                        .persist(&mut state.journal)
                        .map_err(|error| TransactionError::journal(&error))?;
                    return Err(TransactionError::bridge(&failure.error)
                        .for_transaction(&command.transaction_id));
                }
                let mut state = self.state.lock().await;
                if let Some(record) = state.journal.records.get_mut(&command.transaction_id) {
                    record.state = TransactionState::InDoubt;
                    record.updated_at_ms = self.clock.now_ms();
                    record.safe_error = Some(SafeTransactionError {
                        code: "transaction_in_doubt".to_owned(),
                        message: "Apply outcome is unknown; status reconciliation is required."
                            .to_owned(),
                        retryable: false,
                    });
                }
                state.previews.remove(&command.transaction_id);
                self.journal_store
                    .persist(&mut state.journal)
                    .map_err(|error| TransactionError::journal(&error))?;
                Err(TransactionError::new(
                    "transaction_in_doubt",
                    "Apply outcome is unknown; query transaction status without replay.",
                    false,
                )
                .for_transaction(&command.transaction_id))
            }
        }
    }

    async fn record_approval_result(
        &self,
        transaction_id: &str,
        state_value: TransactionState,
        code: &str,
        message: &str,
    ) -> Result<(), TransactionError> {
        let mut state = self.state.lock().await;
        if let Some(record) = state.journal.records.get_mut(transaction_id) {
            record.state = state_value;
            record.updated_at_ms = self.clock.now_ms();
            record.safe_error = Some(SafeTransactionError {
                code: code.to_owned(),
                message: message.to_owned(),
                retryable: state_value == TransactionState::AwaitingApproval,
            });
        }
        if state_value != TransactionState::AwaitingApproval {
            state.previews.remove(transaction_id);
        }
        self.journal_store
            .persist(&mut state.journal)
            .map_err(|error| TransactionError::journal(&error))
    }

    pub async fn undo(
        &self,
        command: UndoCommand,
    ) -> Result<TransactionStatusView, TransactionError> {
        let _gate = self.operation_gate.lock().await;
        if !valid_identifier(&command.transaction_id, "transaction:", 32)
            || command.expected_transaction_seq == 0
            || command.expected_transaction_seq > MAX_SAFE_INTEGER
            || command.expected_scene_revision > MAX_SAFE_INTEGER
            || command.expected_operation_seq > MAX_SAFE_INTEGER
        {
            return Err(TransactionError::new(
                "invalid_transaction_request",
                "The Undo coordinates are malformed.",
                false,
            )
            .for_transaction(&command.transaction_id));
        }
        {
            let state = self.state.lock().await;
            if !state.journal.records.contains_key(&command.transaction_id) {
                return Err(TransactionError::new(
                    "transaction_not_found",
                    "The transaction is not present in the recovery index.",
                    false,
                )
                .for_transaction(&command.transaction_id));
            }
        }
        let observed = self
            .bridge
            .status(&command.transaction_id)
            .await
            .map_err(|error| TransactionError::bridge(&error))?;
        let current = self.accept_status(observed).await?;
        if current.state == TransactionState::Undone {
            return Ok(current);
        }
        if current.state != TransactionState::Committed
            || !current.undo_eligibility.eligible
            || current.transaction_seq != command.expected_transaction_seq
            || current.current_scene_revision != command.expected_scene_revision
            || current.current_operation_seq != command.expected_operation_seq
        {
            return Err(TransactionError::new(
                "transaction_not_undoable",
                "The transaction is not the newest eligible native history action.",
                false,
            )
            .for_transaction(&command.transaction_id));
        }
        match self.bridge.undo(&command).await {
            Ok(observed) => self.accept_status(observed).await,
            Err(error) => {
                if let Ok(observed) = self.bridge.status(&command.transaction_id).await {
                    return self.accept_status(observed).await;
                }
                Err(TransactionError::bridge(&error).for_transaction(&command.transaction_id))
            }
        }
    }

    async fn accept_status(
        &self,
        observed: ObservedStatus,
    ) -> Result<TransactionStatusView, TransactionError> {
        if observed.project_id != self.project_id {
            return Err(TransactionError::new(
                "wrong_project",
                "Bridge returned a transaction from a different project.",
                false,
            ));
        }
        let mut state = self.state.lock().await;
        let record = state
            .journal
            .records
            .get(&observed.result.coordinates.transaction_id)
            .ok_or_else(|| {
                TransactionError::new(
                    "transaction_not_found",
                    "Bridge returned a transaction absent from the recovery index.",
                    false,
                )
                .for_transaction(&observed.result.coordinates.transaction_id)
            })?;
        validate_observed_status(record, &observed)?;
        let view = status_view(&observed.editor_session_id, &observed.result);
        update_record_from_status(
            &mut state.journal,
            &observed.editor_session_id,
            &observed.result,
        );
        if !matches!(
            observed.result.state,
            TransactionState::Previewed | TransactionState::AwaitingApproval
        ) {
            state
                .previews
                .remove(&observed.result.coordinates.transaction_id);
        }
        self.journal_store
            .persist(&mut state.journal)
            .map_err(|error| TransactionError::journal(&error))?;
        Ok(view)
    }

    pub async fn observe_event(&self, event: TransactionEvent) -> Result<(), TransactionError> {
        let transaction_id = event.coordinates.transaction_id.clone();
        let needs_status = {
            let state = self.state.lock().await;
            let Some(record) = state.journal.records.get(&transaction_id) else {
                return Ok(());
            };
            event.revisions.editor_session_id != record.editor_session_id
                || event.coordinates.scene_id != record.scene_id
                || event.coordinates.history_id != record.history_id
                || event.coordinates.transaction_seq != record.transaction_seq.saturating_add(1)
        };
        if needs_status {
            if let Ok(observed) = self.bridge.status(&transaction_id).await {
                self.accept_status(observed).await?;
            }
            return Ok(());
        }
        {
            let mut state = self.state.lock().await;
            if let Some(record) = state.journal.records.get_mut(&transaction_id) {
                record.state = event.state;
                record.transaction_seq = event.coordinates.transaction_seq;
                record.scene_revision = event.coordinates.scene_revision;
                record.operation_seq = event.coordinates.operation_seq;
                record.updated_at_ms = event.timestamp_ms;
                record.undo_eligible = false;
            }
            if !matches!(
                event.state,
                TransactionState::Previewed | TransactionState::AwaitingApproval
            ) {
                state.previews.remove(&transaction_id);
            }
            self.journal_store
                .persist(&mut state.journal)
                .map_err(|error| TransactionError::journal(&error))?;
        }
        if matches!(
            event.state,
            TransactionState::Committed
                | TransactionState::Undone
                | TransactionState::Conflicted
                | TransactionState::Expired
                | TransactionState::Rejected
                | TransactionState::Failed
                | TransactionState::FailedRolledBack
                | TransactionState::InDoubt
        ) && let Ok(observed) = self.bridge.status(&transaction_id).await
        {
            self.accept_status(observed).await?;
        }
        Ok(())
    }

    pub async fn reconcile_editor_session(
        &self,
        current_editor_session_id: &str,
    ) -> Result<(), TransactionError> {
        let mut state = self.state.lock().await;
        let now = self.clock.now_ms();
        let mut changed = false;
        let mut remove_previews = Vec::new();
        let mut remove_records = Vec::new();
        for record in state.journal.records.values_mut() {
            if record.editor_session_id != current_editor_session_id {
                if matches!(
                    record.state,
                    TransactionState::Preparing
                        | TransactionState::Previewed
                        | TransactionState::AwaitingApproval
                ) {
                    record.state = TransactionState::Expired;
                    record.updated_at_ms = now;
                    record.apply_dispatched = false;
                    record.safe_error = Some(SafeTransactionError {
                        code: "transaction_expired".to_owned(),
                        message: "The editor session changed after transaction preparation."
                            .to_owned(),
                        retryable: false,
                    });
                    remove_previews.push(record.transaction_id.clone());
                    changed = true;
                } else if matches!(
                    record.state,
                    TransactionState::Committed | TransactionState::Undone
                ) {
                    remove_records.push(record.transaction_id.clone());
                    changed = true;
                } else if record.state == TransactionState::InDoubt {
                    record.undo_eligible = false;
                    record.safe_error = Some(SafeTransactionError {
                        code: "transaction_in_doubt".to_owned(),
                        message: "The editor session changed before reconciliation.".to_owned(),
                        retryable: false,
                    });
                    changed = true;
                }
            }
        }
        for transaction_id in remove_previews {
            state.previews.remove(&transaction_id);
        }
        for transaction_id in remove_records {
            state.previews.remove(&transaction_id);
            state.journal.records.remove(&transaction_id);
        }
        if changed {
            self.journal_store
                .persist(&mut state.journal)
                .map_err(|error| TransactionError::journal(&error))?;
        }
        Ok(())
    }

    pub async fn reconcile_retained(&self) -> Result<(), TransactionError> {
        let transaction_ids: Vec<String> = {
            let state = self.state.lock().await;
            state
                .journal
                .records
                .values()
                .filter(|record| {
                    record.state.is_protected_from_gc()
                        || matches!(
                            record.state,
                            TransactionState::Committed | TransactionState::Undone
                        )
                })
                .map(|record| record.transaction_id.clone())
                .collect()
        };
        for transaction_id in transaction_ids {
            match self.bridge.status(&transaction_id).await {
                Ok(observed) => {
                    self.accept_status(observed).await?;
                }
                Err(BridgeError::Rpc { code, .. }) if code == "transaction_not_found" => {
                    self.retire_missing_transaction(&transaction_id).await?;
                }
                Err(_) => {}
            }
        }
        Ok(())
    }

    async fn retire_missing_transaction(
        &self,
        transaction_id: &str,
    ) -> Result<(), TransactionError> {
        let mut state = self.state.lock().await;
        let now = self.clock.now_ms();
        let remove = state
            .journal
            .records
            .get(transaction_id)
            .is_some_and(|record| {
                matches!(
                    record.state,
                    TransactionState::Committed | TransactionState::Undone
                )
            });
        if remove {
            state.journal.records.remove(transaction_id);
            state.previews.remove(transaction_id);
        } else if let Some(record) = state.journal.records.get_mut(transaction_id)
            && matches!(
                record.state,
                TransactionState::Preparing
                    | TransactionState::Previewed
                    | TransactionState::AwaitingApproval
            )
        {
            record.state = TransactionState::Expired;
            record.updated_at_ms = now;
            record.apply_dispatched = false;
            record.safe_error = Some(SafeTransactionError {
                code: "transaction_expired".to_owned(),
                message: "Bridge no longer retains the prepared transaction.".to_owned(),
                retryable: false,
            });
            state.previews.remove(transaction_id);
        } else {
            return Ok(());
        }
        self.journal_store
            .persist(&mut state.journal)
            .map_err(|error| TransactionError::journal(&error))
    }
}

struct PendingApprovalGuard<'a> {
    pending: &'a std::sync::Mutex<BTreeSet<String>>,
    transaction_id: String,
}

impl Drop for PendingApprovalGuard<'_> {
    fn drop(&mut self) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.transaction_id);
    }
}

fn prune_previews(state: &mut CoordinatorState) {
    state.previews.retain(|transaction_id, _| {
        state
            .journal
            .records
            .get(transaction_id)
            .is_some_and(|record| {
                matches!(
                    record.state,
                    TransactionState::Previewed | TransactionState::AwaitingApproval
                )
            })
    });
}

fn validate_prepare_command(command: &PrepareCommand) -> Result<(), TransactionError> {
    if !valid_identifier(&command.project_id, "project:sha256:", 64)
        || !valid_identifier(&command.editor_session_id, "editor:", 32)
        || !valid_identifier(&command.coordinates.scene_id, "scene:", 32)
        || !valid_identifier(&command.coordinates.history_id, "history:", 32)
        || !valid_identifier(&command.idempotency_key, "idempotency:", 32)
        || command.coordinates.scene_revision > MAX_SAFE_INTEGER
        || command.coordinates.operation_seq > MAX_SAFE_INTEGER
    {
        return Err(TransactionError::new(
            "invalid_transaction_request",
            "The transaction coordinates are malformed.",
            false,
        ));
    }
    Ok(())
}

fn valid_identifier(value: &str, prefix: &str, hexadecimal_length: usize) -> bool {
    value.strip_prefix(prefix).is_some_and(|tail| {
        tail.len() == hexadecimal_length
            && tail
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn validate_observed_prepare(
    command: &PrepareCommand,
    observed: &ObservedPrepare,
) -> Result<(), TransactionError> {
    let result = &observed.result;
    if observed.project_id != command.project_id
        || observed.editor_session_id != command.editor_session_id
        || result.coordinates.scene_id != command.coordinates.scene_id
        || result.coordinates.history_id != command.coordinates.history_id
        || result.coordinates.scene_revision != command.coordinates.scene_revision
        || result.coordinates.operation_seq != command.coordinates.operation_seq
        || result.operation_kind != command.operation.kind()
        || !valid_risk(result.operation_kind, result.risk)
        || result.scope != expected_scope(result.operation_kind)
        || result
            .affected_entities
            .iter()
            .any(|entity| !allowed_role(result.operation_kind, entity.role))
    {
        return Err(TransactionError::new(
            "stale_editor_state",
            "Bridge returned a preview for different editor coordinates.",
            true,
        ));
    }
    Ok(())
}

fn validate_observed_status(
    record: &TransactionJournalRecord,
    observed: &ObservedStatus,
) -> Result<(), TransactionError> {
    let status = &observed.result;
    let terminal_transition_is_valid = match record.state {
        TransactionState::Committed => {
            matches!(
                status.state,
                TransactionState::Committed | TransactionState::Undone
            )
        }
        TransactionState::Undone => {
            matches!(
                status.state,
                TransactionState::Undone | TransactionState::Committed
            )
        }
        TransactionState::Expired
        | TransactionState::Rejected
        | TransactionState::Conflicted
        | TransactionState::Failed
        | TransactionState::FailedRolledBack => status.state == record.state,
        _ => true,
    };
    if observed.editor_session_id != record.editor_session_id
        || status.coordinates.scene_id != record.scene_id
        || status.coordinates.history_id != record.history_id
        || status.coordinates.transaction_seq < record.transaction_seq
        || status.operation_kind != record.operation_kind
        || status.risk != record.risk
        || status.scope != record.scope
        || status.preview_digest != record.preview_digest
        || !terminal_transition_is_valid
    {
        return Err(TransactionError::new(
            "transaction_reconciliation_failed",
            "Bridge status does not match the retained transaction binding.",
            false,
        )
        .for_transaction(&record.transaction_id));
    }
    Ok(())
}

fn valid_risk(kind: OperationKind, risk: Risk) -> bool {
    match kind {
        OperationKind::DeleteNode
        | OperationKind::ReparentNode
        | OperationKind::SetProperty
        | OperationKind::DetachScript
        | OperationKind::DisconnectSignal => risk == Risk::Destructive,
        OperationKind::AttachScript => matches!(risk, Risk::Write | Risk::Destructive),
        OperationKind::CreateNode | OperationKind::ConnectSignal => risk == Risk::Write,
    }
}

fn expected_scope(kind: OperationKind) -> ApprovalScope {
    match kind {
        OperationKind::CreateNode => ApprovalScope::SceneNodeCreate,
        OperationKind::DeleteNode => ApprovalScope::SceneNodeDelete,
        OperationKind::ReparentNode => ApprovalScope::SceneNodeReparent,
        OperationKind::SetProperty => ApprovalScope::ScenePropertySet,
        OperationKind::AttachScript => ApprovalScope::SceneScriptAttach,
        OperationKind::DetachScript => ApprovalScope::SceneScriptDetach,
        OperationKind::ConnectSignal => ApprovalScope::SceneSignalConnect,
        OperationKind::DisconnectSignal => ApprovalScope::SceneSignalDisconnect,
    }
}

fn allowed_role(kind: OperationKind, role: AffectedEntityRole) -> bool {
    match kind {
        OperationKind::CreateNode => {
            matches!(
                role,
                AffectedEntityRole::Parent | AffectedEntityRole::Created
            )
        }
        OperationKind::DeleteNode
        | OperationKind::SetProperty
        | OperationKind::AttachScript
        | OperationKind::DetachScript => role == AffectedEntityRole::Target,
        OperationKind::ReparentNode => {
            matches!(
                role,
                AffectedEntityRole::Target | AffectedEntityRole::NewParent
            )
        }
        OperationKind::ConnectSignal | OperationKind::DisconnectSignal => {
            matches!(
                role,
                AffectedEntityRole::Emitter | AffectedEntityRole::Receiver
            )
        }
    }
}

fn validate_apply_command(
    record: &TransactionJournalRecord,
    command: &ApplyCommand,
) -> Result<(), TransactionError> {
    if record.preview_digest != command.preview_digest {
        return Err(TransactionError::new(
            "preview_mismatch",
            "The preview digest does not match the immutable transaction.",
            false,
        )
        .for_transaction(&command.transaction_id));
    }
    if record.scene_revision != command.expected_scene_revision {
        return Err(TransactionError::new(
            "stale_scene_revision",
            "The expected scene revision is stale.",
            true,
        )
        .for_transaction(&command.transaction_id));
    }
    if record.operation_seq != command.expected_operation_seq {
        return Err(TransactionError::new(
            "stale_editor_state",
            "The expected editor operation sequence is stale.",
            true,
        )
        .for_transaction(&command.transaction_id));
    }
    Ok(())
}

fn validate_preflight_state(status: &TransactionStatusView) -> Result<(), TransactionError> {
    if matches!(
        status.state,
        TransactionState::Previewed | TransactionState::AwaitingApproval
    ) {
        return Ok(());
    }
    let (code, message) = match status.state {
        TransactionState::Expired => (
            "transaction_expired",
            "The prepared transaction has expired.",
        ),
        TransactionState::Conflicted => (
            "transaction_conflicted",
            "The prepared transaction was invalidated by an editor change.",
        ),
        TransactionState::Rejected => (
            "approval_declined",
            "The transaction approval was previously declined.",
        ),
        TransactionState::InDoubt => (
            "transaction_in_doubt",
            "Apply outcome is unknown; use transaction status and do not replay apply.",
        ),
        _ => (
            "transaction_apply_replay_forbidden",
            "This transaction is no longer at an applicable preview state.",
        ),
    };
    Err(TransactionError::new(code, message, false).for_transaction(&status.transaction_id))
}

fn update_record_from_status(
    journal: &mut JournalState,
    editor_session_id: &str,
    status: &TransactionStatus,
) {
    if let Some(record) = journal.records.get_mut(&status.coordinates.transaction_id) {
        record.editor_session_id = editor_session_id.to_owned();
        record.state = status.state;
        record.transaction_seq = status.coordinates.transaction_seq;
        record.scene_revision = status.current_scene_revision;
        record.operation_seq = status.current_operation_seq;
        record.updated_at_ms = status.updated_at_ms;
        record.undo_eligible = status.undo_eligibility.eligible;
        record.safe_error = status.error.clone();
        if !matches!(
            status.state,
            TransactionState::Applying
                | TransactionState::Applied
                | TransactionState::Validating
                | TransactionState::InDoubt
        ) {
            record.apply_dispatched = status.state == TransactionState::Committed;
        }
    } else {
        journal.records.insert(
            status.coordinates.transaction_id.clone(),
            TransactionJournalRecord {
                transaction_id: status.coordinates.transaction_id.clone(),
                idempotency_fingerprint: domain_digest(
                    b"godot-codex/recovered-idempotency/v1\0",
                    status.coordinates.transaction_id.as_bytes(),
                ),
                operation_digest: domain_digest(
                    b"godot-codex/recovered-operation/v1\0",
                    status.preview_digest.as_bytes(),
                ),
                editor_session_id: editor_session_id.to_owned(),
                scene_id: status.coordinates.scene_id.clone(),
                history_id: status.coordinates.history_id.clone(),
                state: status.state,
                transaction_seq: status.coordinates.transaction_seq,
                scene_revision: status.current_scene_revision,
                operation_seq: status.current_operation_seq,
                operation_kind: status.operation_kind,
                risk: status.risk,
                scope: status.scope,
                affected_entities: status.committed_entities.clone().unwrap_or_default(),
                preview_digest: status.preview_digest.clone(),
                created_at_ms: status.updated_at_ms,
                updated_at_ms: status.updated_at_ms,
                expires_at_ms: status.updated_at_ms,
                apply_dispatched: matches!(
                    status.state,
                    TransactionState::Applying
                        | TransactionState::Applied
                        | TransactionState::Validating
                        | TransactionState::Committed
                        | TransactionState::InDoubt
                ),
                undo_eligible: status.undo_eligibility.eligible,
                safe_error: status.error.clone(),
                receipt_hash: None,
            },
        );
    }
}

fn prepared_view(editor_session_id: &str, result: &PrepareResult) -> PreparedTransactionView {
    PreparedTransactionView {
        transaction_id: result.coordinates.transaction_id.clone(),
        editor_session_id: editor_session_id.to_owned(),
        scene_id: result.coordinates.scene_id.clone(),
        state: result.state,
        transaction_seq: result.coordinates.transaction_seq,
        scene_revision: result.coordinates.scene_revision,
        operation_seq: result.coordinates.operation_seq,
        operation_kind: result.operation_kind,
        risk: result.risk,
        scope: result.scope,
        affected_entities: result.affected_entities.clone(),
        preview: result.preview.clone(),
        preview_digest: result.preview_digest.clone(),
        created_at_ms: result.created_at_ms,
        expires_at_ms: result.expires_at_ms,
        limits_applied: result.limits_applied.clone(),
    }
}

fn status_view(editor_session_id: &str, result: &TransactionStatus) -> TransactionStatusView {
    TransactionStatusView {
        transaction_id: result.coordinates.transaction_id.clone(),
        editor_session_id: editor_session_id.to_owned(),
        scene_id: result.coordinates.scene_id.clone(),
        state: result.state,
        transaction_seq: result.coordinates.transaction_seq,
        operation_kind: result.operation_kind,
        risk: result.risk,
        scope: result.scope,
        preview_digest: result.preview_digest.clone(),
        current_scene_revision: result.current_scene_revision,
        current_operation_seq: result.current_operation_seq,
        outcome: result.outcome,
        error: result.error.clone(),
        undo_eligibility: result.undo_eligibility.clone(),
        committed_entities: result.committed_entities.clone(),
        updated_at_ms: result.updated_at_ms,
        limits_applied: result.limits_applied.clone(),
        truncated: result.truncated,
    }
}

fn local_terminal_status_view(record: &TransactionJournalRecord) -> TransactionStatusView {
    TransactionStatusView {
        transaction_id: record.transaction_id.clone(),
        editor_session_id: record.editor_session_id.clone(),
        scene_id: record.scene_id.clone(),
        state: record.state,
        transaction_seq: record.transaction_seq,
        operation_kind: record.operation_kind,
        risk: record.risk,
        scope: record.scope,
        preview_digest: record.preview_digest.clone(),
        current_scene_revision: record.scene_revision,
        current_operation_seq: record.operation_seq,
        outcome: Some(godot_codex_bridge_client::TransactionOutcome::None),
        error: record.safe_error.clone(),
        undo_eligibility: UndoEligibility {
            eligible: false,
            reason: godot_codex_bridge_client::UndoEligibilityReason::NotCommitted,
        },
        committed_entities: None,
        updated_at_ms: record.updated_at_ms,
        limits_applied: fixed_transaction_limits(),
        truncated: false,
    }
}

fn fixed_transaction_limits() -> TransactionLimits {
    TransactionLimits {
        operations: 1,
        prepared_records: 64,
        applying_per_history: 1,
        prepared_ttl_ms: 300_000,
        approval_timeout_ms: 120_000,
        receipt_ttl_ms: 30_000,
        clock_skew_ms: 2_000,
        approval_message_bytes: 8_192,
        preview_bytes: 65_536,
        operation_bytes: 65_536,
        variant_depth: 8,
        container_items: 1_000,
        string_characters: 16_384,
        structural_nodes: 1_000,
        journal_records: 1_024,
        journal_bytes: 8_388_608,
        status_bytes: 65_536,
        request_deadline_ms: 5_000,
    }
}

fn domain_digest(domain: &[u8], value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(value);
    format!("sha256:{:x}", hasher.finalize())
}

pub async fn run_transaction_events(
    project_root: PathBuf,
    coordinator: Arc<TransactionCoordinator>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut retry = std::time::Duration::from_millis(200);
    loop {
        if *shutdown.borrow() {
            return;
        }
        match BridgeClient::connect(&project_root).await {
            Ok(mut client)
                if client.negotiated_profile().transaction_available
                    && coordinator
                        .reconcile_editor_session(client.editor_session_id())
                        .await
                        .is_ok() =>
            {
                let _ = coordinator.reconcile_retained().await;
                retry = std::time::Duration::from_millis(200);
                loop {
                    tokio::select! {
                        changed = shutdown.changed() => {
                            if changed.is_err() || *shutdown.borrow() {
                                return;
                            }
                        }
                        event = client.next_transaction_notification() => {
                            match event {
                                Ok(event) => {
                                    let _ = coordinator.observe_event(event).await;
                                }
                                Err(_) => break,
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            () = tokio::time::sleep(retry) => {}
        }
        retry = (retry * 2).min(std::time::Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use godot_codex_bridge_client::{
        AffectedEntity, AffectedEntityRole, DirtyEffect, SaveEffect, TransactionCoordinates,
        TransactionOutcome, UndoEligibilityReason,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Debug)]
    struct FixedClock(u64);

    impl TransactionClock for FixedClock {
        fn now_ms(&self) -> u64 {
            self.0
        }
    }

    struct FakeBridge {
        project_id: String,
        editor_session_id: String,
        prepare: PrepareResult,
        status: std::sync::Mutex<TransactionStatus>,
        prepare_calls: AtomicUsize,
        status_calls: AtomicUsize,
        status_missing: AtomicBool,
        apply_calls: AtomicUsize,
        undo_calls: AtomicUsize,
        reject_before_commit: AtomicBool,
        lose_apply_response: bool,
    }

    impl TransactionBridge for FakeBridge {
        fn prepare<'a>(
            &'a self,
            _command: &'a PrepareCommand,
        ) -> BridgeFuture<'a, Result<ObservedPrepare, BridgeError>> {
            Box::pin(async move {
                self.prepare_calls.fetch_add(1, Ordering::SeqCst);
                Ok(ObservedPrepare {
                    project_id: self.project_id.clone(),
                    editor_session_id: self.editor_session_id.clone(),
                    result: self.prepare.clone(),
                })
            })
        }

        fn status<'a>(
            &'a self,
            _transaction_id: &'a str,
        ) -> BridgeFuture<'a, Result<ObservedStatus, BridgeError>> {
            Box::pin(async move {
                self.status_calls.fetch_add(1, Ordering::SeqCst);
                if self.status_missing.load(Ordering::SeqCst) {
                    return Err(BridgeError::Rpc {
                        code: "transaction_not_found".to_owned(),
                        message: "The transaction is not retained.".to_owned(),
                        retryable: false,
                        data: serde_json::json!({}),
                    });
                }
                Ok(ObservedStatus {
                    project_id: self.project_id.clone(),
                    editor_session_id: self.editor_session_id.clone(),
                    result: self.status.lock().unwrap().clone(),
                })
            })
        }

        fn apply_approved<'a>(
            &'a self,
            _command: &'a ApplyCommand,
            _binding: &'a ApprovalBinding,
            _issued_at_ms: u64,
        ) -> BridgeFuture<'a, Result<BridgeApplyOutcome, BridgeApplyFailure>> {
            Box::pin(async move {
                self.apply_calls.fetch_add(1, Ordering::SeqCst);
                if self.reject_before_commit.load(Ordering::SeqCst) {
                    return Err(BridgeApplyFailure {
                        error: BridgeError::Rpc {
                            code: "transaction_precommit_failed".to_owned(),
                            message: "Bridge proved that the commit point was not reached."
                                .to_owned(),
                            retryable: true,
                            data: serde_json::json!({}),
                        },
                        may_have_committed: false,
                    });
                }
                if self.lose_apply_response {
                    let mut status = self.status.lock().unwrap().clone();
                    status.state = TransactionState::InDoubt;
                    status.outcome = Some(TransactionOutcome::Unknown);
                    status.error = Some(SafeTransactionError {
                        code: "transaction_in_doubt".to_owned(),
                        message: "Apply outcome requires reconciliation.".to_owned(),
                        retryable: false,
                    });
                    *self.status.lock().unwrap() = status;
                    return Err(BridgeApplyFailure {
                        error: BridgeError::Timeout,
                        may_have_committed: true,
                    });
                }
                let mut status = self.status.lock().unwrap().clone();
                status.state = TransactionState::Committed;
                status.outcome = Some(TransactionOutcome::Committed);
                status.undo_eligibility = UndoEligibility {
                    eligible: true,
                    reason: UndoEligibilityReason::Eligible,
                };
                *self.status.lock().unwrap() = status.clone();
                Ok(BridgeApplyOutcome {
                    observed: ObservedStatus {
                        project_id: self.project_id.clone(),
                        editor_session_id: self.editor_session_id.clone(),
                        result: status,
                    },
                    receipt_hash: format!("sha256:{}", "b".repeat(64)),
                })
            })
        }

        fn undo<'a>(
            &'a self,
            _command: &'a UndoCommand,
        ) -> BridgeFuture<'a, Result<ObservedStatus, BridgeError>> {
            Box::pin(async move {
                self.undo_calls.fetch_add(1, Ordering::SeqCst);
                let mut status = self.status.lock().unwrap().clone();
                status.state = TransactionState::Undone;
                status.outcome = Some(TransactionOutcome::Undone);
                status.undo_eligibility = UndoEligibility {
                    eligible: false,
                    reason: UndoEligibilityReason::NotCommitted,
                };
                *self.status.lock().unwrap() = status.clone();
                Ok(ObservedStatus {
                    project_id: self.project_id.clone(),
                    editor_session_id: self.editor_session_id.clone(),
                    result: status,
                })
            })
        }
    }

    struct Accept;

    impl ApprovalProvider for Accept {
        fn request<'a>(&'a self, _prompt: ApprovalPrompt) -> BridgeFuture<'a, ApprovalDecision> {
            Box::pin(async { ApprovalDecision::Accept })
        }
    }

    struct Decline;

    impl ApprovalProvider for Decline {
        fn request<'a>(&'a self, _prompt: ApprovalPrompt) -> BridgeFuture<'a, ApprovalDecision> {
            Box::pin(async { ApprovalDecision::Decline })
        }
    }

    struct BlockingApproval {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    impl ApprovalProvider for BlockingApproval {
        fn request<'a>(&'a self, _prompt: ApprovalPrompt) -> BridgeFuture<'a, ApprovalDecision> {
            Box::pin(async move {
                self.entered.notify_one();
                self.release.notified().await;
                ApprovalDecision::Decline
            })
        }
    }

    fn limits() -> TransactionLimits {
        TransactionLimits {
            operations: 1,
            prepared_records: 64,
            applying_per_history: 1,
            prepared_ttl_ms: 300_000,
            approval_timeout_ms: 120_000,
            receipt_ttl_ms: 30_000,
            clock_skew_ms: 2_000,
            approval_message_bytes: 8_192,
            preview_bytes: 65_536,
            operation_bytes: 65_536,
            variant_depth: 8,
            container_items: 1_000,
            string_characters: 16_384,
            structural_nodes: 1_000,
            journal_records: 1_024,
            journal_bytes: 8_388_608,
            status_bytes: 65_536,
            request_deadline_ms: 5_000,
        }
    }

    fn fixture(
        lose_apply_response: bool,
    ) -> (
        tempfile::TempDir,
        Arc<TransactionCoordinator>,
        Arc<FakeBridge>,
        PrepareCommand,
    ) {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("project.godot"), b"[application]\n").unwrap();
        let project_id = format!("project:sha256:{}", "1".repeat(64));
        let editor_session_id = format!("editor:{}", "2".repeat(32));
        let transaction_id = format!("transaction:{}", "3".repeat(32));
        let scene_id = format!("scene:{}", "4".repeat(32));
        let history_id = format!("history:{}", "5".repeat(32));
        let node_id = format!("node:{}", "6".repeat(32));
        let preview_payload_json = r#"{"summary":"Create Node2D"}"#.to_owned();
        let preview_digest = format!(
            "sha256:{:x}",
            Sha256::digest(preview_payload_json.as_bytes())
        );
        let coordinates = TransactionCoordinates {
            transaction_id,
            scene_id: scene_id.clone(),
            history_id: history_id.clone(),
            scene_revision: 7,
            operation_seq: 8,
            transaction_seq: 1,
        };
        let prepare = PrepareResult {
            schema_version: "transaction/1.0".to_owned(),
            coordinates: coordinates.clone(),
            state: TransactionState::Previewed,
            operation_kind: OperationKind::CreateNode,
            risk: Risk::Write,
            scope: ApprovalScope::SceneNodeCreate,
            affected_entities: vec![AffectedEntity {
                node_id: node_id.clone(),
                role: AffectedEntityRole::Parent,
            }],
            preview: TransactionPreview {
                operation_kind: OperationKind::CreateNode,
                summary: "Create Node2D".to_owned(),
                dirty_effect: DirtyEffect::MarksSceneDirty,
                save_effect: SaveEffect::NotSaved,
                preconditions: vec!["Scene is open".to_owned()],
                truncated: false,
            },
            preview_payload_json,
            preview_digest: preview_digest.clone(),
            created_at_ms: 1_000,
            expires_at_ms: 301_000,
            limits_applied: limits(),
        };
        let status = TransactionStatus {
            schema_version: "transaction/1.0".to_owned(),
            coordinates,
            state: TransactionState::Previewed,
            operation_kind: OperationKind::CreateNode,
            risk: Risk::Write,
            scope: ApprovalScope::SceneNodeCreate,
            preview_digest,
            current_scene_revision: 7,
            current_operation_seq: 8,
            outcome: Some(TransactionOutcome::None),
            error: None,
            undo_eligibility: UndoEligibility {
                eligible: false,
                reason: UndoEligibilityReason::NotCommitted,
            },
            committed_entities: None,
            updated_at_ms: 1_000,
            limits_applied: limits(),
            truncated: false,
        };
        let bridge = Arc::new(FakeBridge {
            project_id: project_id.clone(),
            editor_session_id: editor_session_id.clone(),
            prepare,
            status: std::sync::Mutex::new(status),
            prepare_calls: AtomicUsize::new(0),
            status_calls: AtomicUsize::new(0),
            status_missing: AtomicBool::new(false),
            apply_calls: AtomicUsize::new(0),
            undo_calls: AtomicUsize::new(0),
            reject_before_commit: AtomicBool::new(false),
            lose_apply_response,
        });
        let coordinator = TransactionCoordinator::open_with(
            project.path(),
            project_id.clone(),
            bridge.clone(),
            Arc::new(FixedClock(2_000)),
        )
        .unwrap();
        let command = PrepareCommand {
            project_id,
            editor_session_id,
            idempotency_key: format!("idempotency:{}", "7".repeat(32)),
            coordinates: RevisionCoordinates {
                scene_id,
                history_id,
                scene_revision: 7,
                operation_seq: 8,
            },
            operation: TransactionOperation::CreateNode {
                parent_node_id: node_id,
                godot_type: "Node2D".to_owned(),
                name: "Created".to_owned(),
                insertion_index: None,
            },
        };
        (project, coordinator, bridge, command)
    }

    #[tokio::test]
    async fn prepare_apply_status_and_undo_are_coordinated() {
        let (_project, coordinator, bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command).await.unwrap();
        let committed = coordinator
            .apply(
                ApplyCommand {
                    transaction_id: prepared.transaction_id.clone(),
                    preview_digest: prepared.preview_digest,
                    expected_scene_revision: 7,
                    expected_operation_seq: 8,
                },
                &Accept,
            )
            .await
            .unwrap();
        assert_eq!(committed.state, TransactionState::Committed);
        assert_eq!(bridge.apply_calls.load(Ordering::SeqCst), 1);
        let undone = coordinator
            .undo(UndoCommand {
                transaction_id: committed.transaction_id,
                expected_transaction_seq: committed.transaction_seq,
                expected_scene_revision: committed.current_scene_revision,
                expected_operation_seq: committed.current_operation_seq,
            })
            .await
            .unwrap();
        assert_eq!(undone.state, TransactionState::Undone);
    }

    #[tokio::test]
    async fn response_loss_latches_in_doubt_and_forbids_replay() {
        let (_project, coordinator, bridge, command) = fixture(true);
        let prepared = coordinator.prepare(command).await.unwrap();
        let apply = ApplyCommand {
            transaction_id: prepared.transaction_id.clone(),
            preview_digest: prepared.preview_digest,
            expected_scene_revision: 7,
            expected_operation_seq: 8,
        };
        let error = coordinator.apply(apply.clone(), &Accept).await.unwrap_err();
        assert_eq!(error.code, "transaction_in_doubt");
        let second = coordinator.apply(apply, &Accept).await.unwrap_err();
        assert_eq!(second.code, "transaction_in_doubt");
        let status = coordinator.status(&prepared.transaction_id).await.unwrap();
        assert_eq!(status.state, TransactionState::InDoubt);
        assert_eq!(bridge.apply_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn proven_precommit_failure_can_retry_only_with_a_new_approval() {
        let (_project, coordinator, bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command).await.unwrap();
        let apply = ApplyCommand {
            transaction_id: prepared.transaction_id,
            preview_digest: prepared.preview_digest,
            expected_scene_revision: 7,
            expected_operation_seq: 8,
        };
        bridge.reject_before_commit.store(true, Ordering::SeqCst);
        let first = coordinator.apply(apply.clone(), &Accept).await.unwrap_err();
        assert_eq!(first.code, "transaction_precommit_failed");
        bridge.reject_before_commit.store(false, Ordering::SeqCst);
        let committed = coordinator.apply(apply, &Accept).await.unwrap();
        assert_eq!(committed.state, TransactionState::Committed);
        assert_eq!(bridge.apply_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn committed_apply_is_not_replayed_through_the_apply_endpoint() {
        let (_project, coordinator, bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command).await.unwrap();
        let apply = ApplyCommand {
            transaction_id: prepared.transaction_id,
            preview_digest: prepared.preview_digest,
            expected_scene_revision: 7,
            expected_operation_seq: 8,
        };
        coordinator.apply(apply.clone(), &Accept).await.unwrap();
        let repeated = coordinator.apply(apply, &Accept).await.unwrap_err();
        assert_eq!(repeated.code, "transaction_apply_replay_forbidden");
        assert_eq!(bridge.apply_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_apply_is_rejected_before_a_second_approval_or_dispatch() {
        let (_project, coordinator, bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command).await.unwrap();
        let apply = ApplyCommand {
            transaction_id: prepared.transaction_id,
            preview_digest: prepared.preview_digest,
            expected_scene_revision: 7,
            expected_operation_seq: 8,
        };
        let approval = Arc::new(BlockingApproval {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let first = tokio::spawn({
            let coordinator = coordinator.clone();
            let approval = approval.clone();
            let apply = apply.clone();
            async move { coordinator.apply(apply, approval.as_ref()).await }
        });
        approval.entered.notified().await;
        let duplicate = coordinator.apply(apply, &Accept).await.unwrap_err();
        assert_eq!(duplicate.code, "transaction_busy");
        assert_eq!(bridge.apply_calls.load(Ordering::SeqCst), 0);
        approval.release.notify_one();
        let first_error = first.await.unwrap().unwrap_err();
        assert_eq!(first_error.code, "approval_declined");
        assert_eq!(bridge.apply_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cancelled_approval_releases_the_pending_latch() {
        let (_project, coordinator, bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command).await.unwrap();
        let apply = ApplyCommand {
            transaction_id: prepared.transaction_id,
            preview_digest: prepared.preview_digest,
            expected_scene_revision: 7,
            expected_operation_seq: 8,
        };
        let approval = Arc::new(BlockingApproval {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let first = tokio::spawn({
            let coordinator = coordinator.clone();
            let approval = approval.clone();
            let apply = apply.clone();
            async move { coordinator.apply(apply, approval.as_ref()).await }
        });
        approval.entered.notified().await;
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        let committed = coordinator.apply(apply, &Accept).await.unwrap();
        assert_eq!(committed.state, TransactionState::Committed);
        assert_eq!(bridge.apply_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn declined_approval_remains_terminal_without_bridge_mutation() {
        let (_project, coordinator, bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command).await.unwrap();
        let error = coordinator
            .apply(
                ApplyCommand {
                    transaction_id: prepared.transaction_id.clone(),
                    preview_digest: prepared.preview_digest,
                    expected_scene_revision: 7,
                    expected_operation_seq: 8,
                },
                &Decline,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "approval_declined");
        let status_calls = bridge.status_calls.load(Ordering::SeqCst);
        let status = coordinator.status(&prepared.transaction_id).await.unwrap();
        assert_eq!(status.state, TransactionState::Rejected);
        assert_eq!(status.error.unwrap().code, "approval_declined");
        assert_eq!(bridge.status_calls.load(Ordering::SeqCst), status_calls);
        assert_eq!(bridge.apply_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn native_undo_and_redo_events_advance_the_retained_state() {
        let (_project, coordinator, bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command).await.unwrap();
        coordinator
            .apply(
                ApplyCommand {
                    transaction_id: prepared.transaction_id.clone(),
                    preview_digest: prepared.preview_digest,
                    expected_scene_revision: 7,
                    expected_operation_seq: 8,
                },
                &Accept,
            )
            .await
            .unwrap();

        let event = |transaction_seq, state: &str, reason: &str| {
            serde_json::from_value::<TransactionEvent>(serde_json::json!({
                "coordinates": {
                    "transaction_id": prepared.transaction_id.clone(),
                    "scene_id": prepared.scene_id.clone(),
                    "history_id": format!("history:{}", "5".repeat(32)),
                    "scene_revision": 7,
                    "operation_seq": 8,
                    "transaction_seq": transaction_seq
                },
                "previous_state": null,
                "state": state,
                "reason": reason,
                "revisions": {
                    "editor_session_id": prepared.editor_session_id.clone(),
                    "event_seq": transaction_seq,
                    "project_revision": 1,
                    "operation_seq": 8,
                    "scene_revisions": {}
                },
                "timestamp_ms": 2_100 + transaction_seq
            }))
            .unwrap()
        };

        {
            let mut status = bridge.status.lock().unwrap();
            status.state = TransactionState::Undone;
            status.coordinates.transaction_seq = 2;
            status.outcome = Some(TransactionOutcome::Undone);
            status.undo_eligibility = UndoEligibility {
                eligible: false,
                reason: UndoEligibilityReason::NotCommitted,
            };
        }
        coordinator
            .observe_event(event(2, "undone", "native_undo_observed"))
            .await
            .unwrap();
        assert_eq!(
            coordinator
                .status(&prepared.transaction_id)
                .await
                .unwrap()
                .state,
            TransactionState::Undone
        );

        {
            let mut status = bridge.status.lock().unwrap();
            status.state = TransactionState::Committed;
            status.coordinates.transaction_seq = 3;
            status.outcome = Some(TransactionOutcome::Committed);
            status.undo_eligibility = UndoEligibility {
                eligible: true,
                reason: UndoEligibilityReason::Eligible,
            };
        }
        coordinator
            .observe_event(event(3, "committed", "native_redo_observed"))
            .await
            .unwrap();
        assert_eq!(
            coordinator
                .status(&prepared.transaction_id)
                .await
                .unwrap()
                .state,
            TransactionState::Committed
        );
    }

    #[tokio::test]
    async fn unrelated_newer_native_action_makes_targeted_undo_fail_closed() {
        let (_project, coordinator, bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command).await.unwrap();
        let committed = coordinator
            .apply(
                ApplyCommand {
                    transaction_id: prepared.transaction_id,
                    preview_digest: prepared.preview_digest,
                    expected_scene_revision: 7,
                    expected_operation_seq: 8,
                },
                &Accept,
            )
            .await
            .unwrap();
        {
            let mut status = bridge.status.lock().unwrap();
            status.undo_eligibility = UndoEligibility {
                eligible: false,
                reason: UndoEligibilityReason::NotNewestAction,
            };
        }
        let error = coordinator
            .undo(UndoCommand {
                transaction_id: committed.transaction_id,
                expected_transaction_seq: committed.transaction_seq,
                expected_scene_revision: committed.current_scene_revision,
                expected_operation_seq: committed.current_operation_seq,
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "transaction_not_undoable");
        assert_eq!(bridge.undo_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn sidecar_restart_requires_idempotent_reprepare_and_journal_is_redacted() {
        let (project, coordinator, bridge, command) = fixture(false);
        let first = coordinator.prepare(command.clone()).await.unwrap();
        drop(coordinator);
        let journal = project
            .path()
            .join(".godot/codex/transactions/journal-v1.json");
        let serialized = std::fs::read_to_string(&journal).unwrap();
        assert!(!serialized.contains("Created"));
        assert!(!serialized.contains("Node2D"));
        assert!(!serialized.contains("idempotency:"));
        let reopened = TransactionCoordinator::open_with(
            project.path(),
            command.project_id.clone(),
            bridge.clone(),
            Arc::new(FixedClock(2_000)),
        )
        .unwrap();
        let second = reopened.prepare(command).await.unwrap();
        assert_eq!(first.transaction_id, second.transaction_id);
        assert_eq!(first.preview_digest, second.preview_digest);
        assert_eq!(bridge.prepare_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn restart_after_bridge_response_before_journal_ack_reconciles_without_apply_replay() {
        let (project, coordinator, bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command.clone()).await.unwrap();
        {
            let mut state = coordinator.state.lock().await;
            let record = state
                .journal
                .records
                .get_mut(&prepared.transaction_id)
                .unwrap();
            record.state = TransactionState::Applying;
            record.apply_dispatched = true;
            record.updated_at_ms = 2_001;
            coordinator
                .journal_store
                .persist(&mut state.journal)
                .unwrap();
        }
        {
            let mut status = bridge.status.lock().unwrap();
            status.state = TransactionState::Committed;
            status.coordinates.transaction_seq = 2;
            status.current_scene_revision = 8;
            status.current_operation_seq = 9;
            status.outcome = Some(TransactionOutcome::Committed);
            status.undo_eligibility = UndoEligibility {
                eligible: true,
                reason: UndoEligibilityReason::Eligible,
            };
            status.updated_at_ms = 2_002;
        }
        drop(coordinator);
        let reopened = TransactionCoordinator::open_with(
            project.path(),
            command.project_id,
            bridge.clone(),
            Arc::new(FixedClock(2_003)),
        )
        .unwrap();
        let recovered = reopened.status(&prepared.transaction_id).await.unwrap();
        assert_eq!(recovered.state, TransactionState::Committed);
        let replay = reopened
            .apply(
                ApplyCommand {
                    transaction_id: prepared.transaction_id,
                    preview_digest: prepared.preview_digest,
                    expected_scene_revision: recovered.current_scene_revision,
                    expected_operation_seq: recovered.current_operation_seq,
                },
                &Accept,
            )
            .await
            .unwrap_err();
        assert_eq!(replay.code, "transaction_apply_replay_forbidden");
        assert_eq!(bridge.apply_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn quarantined_journal_cannot_resurrect_bridge_only_transaction() {
        let (project, coordinator, bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command.clone()).await.unwrap();
        drop(coordinator);
        let journal = project
            .path()
            .join(".godot/codex/transactions/journal-v1.json");
        std::fs::write(&journal, b"{\"invalid\":").unwrap();
        let reopened = TransactionCoordinator::open_with(
            project.path(),
            command.project_id,
            bridge.clone(),
            Arc::new(FixedClock(2_000)),
        )
        .unwrap();
        assert!(reopened.journal_recovered_corruption());
        let error = reopened.status(&prepared.transaction_id).await.unwrap_err();
        assert_eq!(error.code, "transaction_not_found");
        assert_eq!(bridge.status_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn idempotency_key_cannot_be_rebound_to_another_operation() {
        let (_project, coordinator, bridge, command) = fixture(false);
        coordinator.prepare(command.clone()).await.unwrap();
        let mut conflicting = command;
        conflicting.operation = TransactionOperation::CreateNode {
            parent_node_id: format!("node:{}", "6".repeat(32)),
            godot_type: "Node2D".to_owned(),
            name: "Different".to_owned(),
            insertion_index: None,
        };
        let error = coordinator.prepare(conflicting).await.unwrap_err();
        assert_eq!(error.code, "idempotency_conflict");
        assert_eq!(bridge.prepare_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn apply_rejects_tampered_digest_and_stale_revision_before_approval() {
        let (_project, coordinator, bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command).await.unwrap();
        let tampered = coordinator
            .apply(
                ApplyCommand {
                    transaction_id: prepared.transaction_id.clone(),
                    preview_digest: format!("sha256:{}", "f".repeat(64)),
                    expected_scene_revision: 7,
                    expected_operation_seq: 8,
                },
                &Accept,
            )
            .await
            .unwrap_err();
        assert_eq!(tampered.code, "preview_mismatch");
        let stale = coordinator
            .apply(
                ApplyCommand {
                    transaction_id: prepared.transaction_id,
                    preview_digest: prepared.preview_digest,
                    expected_scene_revision: 6,
                    expected_operation_seq: 8,
                },
                &Accept,
            )
            .await
            .unwrap_err();
        assert_eq!(stale.code, "stale_scene_revision");
        assert_eq!(bridge.status_calls.load(Ordering::SeqCst), 0);
        assert_eq!(bridge.apply_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn prepare_rejects_wrong_project_before_bridge_dispatch() {
        let (_project, coordinator, bridge, mut command) = fixture(false);
        command.project_id = format!("project:sha256:{}", "9".repeat(64));
        let error = coordinator.prepare(command).await.unwrap_err();
        assert_eq!(error.code, "wrong_project");
        assert_eq!(bridge.prepare_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn bridge_conflict_during_preflight_never_reaches_approval_or_apply() {
        let (_project, coordinator, bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command).await.unwrap();
        {
            let mut status = bridge.status.lock().unwrap();
            status.state = TransactionState::Conflicted;
            status.error = Some(SafeTransactionError {
                code: "transaction_conflicted".to_owned(),
                message: "The scene changed before apply.".to_owned(),
                retryable: false,
            });
        }
        let error = coordinator
            .apply(
                ApplyCommand {
                    transaction_id: prepared.transaction_id,
                    preview_digest: prepared.preview_digest,
                    expected_scene_revision: 7,
                    expected_operation_seq: 8,
                },
                &Accept,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "transaction_conflicted");
        assert_eq!(bridge.apply_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn event_gap_reconciles_through_status_instead_of_trusting_the_event() {
        let (_project, coordinator, bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command).await.unwrap();
        let status_calls_before = bridge.status_calls.load(Ordering::SeqCst);
        let event: TransactionEvent = serde_json::from_value(serde_json::json!({
            "coordinates": {
                "transaction_id": prepared.transaction_id,
                "scene_id": prepared.scene_id,
                "history_id": format!("history:{}", "5".repeat(32)),
                "scene_revision": 9,
                "operation_seq": 10,
                "transaction_seq": 3
            },
            "previous_state": "previewed",
            "state": "committed",
            "reason": "native_action_committed",
            "revisions": {
                "editor_session_id": prepared.editor_session_id,
                "event_seq": 3,
                "project_revision": 1,
                "operation_seq": 10,
                "scene_revisions": {}
            },
            "timestamp_ms": 2_100
        }))
        .unwrap();
        coordinator.observe_event(event).await.unwrap();
        assert_eq!(
            bridge.status_calls.load(Ordering::SeqCst),
            status_calls_before + 1
        );
        let reconciled = coordinator.status(&prepared.transaction_id).await.unwrap();
        assert_eq!(reconciled.transaction_seq, 1);
        assert_eq!(reconciled.state, TransactionState::Previewed);
    }

    #[tokio::test]
    async fn editor_session_replacement_expires_prepared_transactions() {
        let (_project, coordinator, bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command).await.unwrap();
        coordinator
            .reconcile_editor_session(&format!("editor:{}", "f".repeat(32)))
            .await
            .unwrap();
        let error = coordinator
            .apply(
                ApplyCommand {
                    transaction_id: prepared.transaction_id,
                    preview_digest: prepared.preview_digest,
                    expected_scene_revision: prepared.scene_revision,
                    expected_operation_seq: prepared.operation_seq,
                },
                &Accept,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "transaction_expired");
        assert_eq!(bridge.apply_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn editor_session_replacement_drops_native_history_dependent_terminal_records() {
        let (project, coordinator, _bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command).await.unwrap();
        coordinator
            .apply(
                ApplyCommand {
                    transaction_id: prepared.transaction_id.clone(),
                    preview_digest: prepared.preview_digest,
                    expected_scene_revision: 7,
                    expected_operation_seq: 8,
                },
                &Accept,
            )
            .await
            .unwrap();
        coordinator
            .reconcile_editor_session(&format!("editor:{}", "f".repeat(32)))
            .await
            .unwrap();
        let journal: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                project
                    .path()
                    .join(".godot/codex/transactions/journal-v1.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(
            journal["records"]
                .as_array()
                .unwrap()
                .iter()
                .all(|record| record["transaction_id"] != prepared.transaction_id)
        );
    }

    #[tokio::test]
    async fn startup_reconciliation_expires_missing_previews_and_drops_unconfirmed_history() {
        let (_project, coordinator, bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command).await.unwrap();
        bridge.status_missing.store(true, Ordering::SeqCst);
        coordinator.reconcile_retained().await.unwrap();
        let expired = coordinator.status(&prepared.transaction_id).await.unwrap();
        assert_eq!(expired.state, TransactionState::Expired);

        let (project, coordinator, bridge, command) = fixture(false);
        let prepared = coordinator.prepare(command).await.unwrap();
        coordinator
            .apply(
                ApplyCommand {
                    transaction_id: prepared.transaction_id.clone(),
                    preview_digest: prepared.preview_digest,
                    expected_scene_revision: 7,
                    expected_operation_seq: 8,
                },
                &Accept,
            )
            .await
            .unwrap();
        bridge.status_missing.store(true, Ordering::SeqCst);
        coordinator.reconcile_retained().await.unwrap();
        let journal: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                project
                    .path()
                    .join(".godot/codex/transactions/journal-v1.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(
            journal["records"]
                .as_array()
                .unwrap()
                .iter()
                .all(|record| record["transaction_id"] != prepared.transaction_id)
        );
    }

    #[test]
    fn bridge_errors_are_redacted_before_reaching_mcp_or_the_journal() {
        let error = TransactionError::bridge(&BridgeError::Rpc {
            code: "../unsafe".to_owned(),
            message: "Failed at /Users/private/project/main.tscn".to_owned(),
            retryable: false,
            data: serde_json::json!({"nonce": "secret"}),
        });
        assert_eq!(error.code, "transaction_bridge_error");
        assert_eq!(
            error.message,
            "The Bridge rejected the transaction request."
        );
        assert!(!format!("{} {}", error.code, error.message).contains("/Users/"));
    }

    #[test]
    fn operation_specific_prepare_commands_close_all_eight_operation_kinds() {
        let context = PrepareTransactionContext {
            project_id: format!("project:sha256:{}", "1".repeat(64)),
            editor_session_id: format!("editor:{}", "2".repeat(32)),
            idempotency_key: format!("idempotency:{}", "3".repeat(32)),
            coordinates: RevisionCoordinates {
                scene_id: format!("scene:{}", "4".repeat(32)),
                history_id: format!("history:{}", "5".repeat(32)),
                scene_revision: 1,
                operation_seq: 1,
            },
        };
        let node = format!("node:{}", "6".repeat(32));
        let other = format!("node:{}", "7".repeat(32));
        let operations: Vec<PrepareCommand> = vec![
            PrepareCreateNodeCommand {
                context: context.clone(),
                parent_node_id: node.clone(),
                godot_type: "Node2D".to_owned(),
                name: "Created".to_owned(),
                insertion_index: None,
            }
            .into(),
            PrepareDeleteNodeCommand {
                context: context.clone(),
                node_id: node.clone(),
            }
            .into(),
            PrepareReparentNodeCommand {
                context: context.clone(),
                node_id: node.clone(),
                new_parent_node_id: other.clone(),
                insertion_index: 0,
                keep_global_transform: true,
            }
            .into(),
            PrepareSetPropertyCommand {
                context: context.clone(),
                node_id: node.clone(),
                property: "visible".to_owned(),
                value: godot_codex_bridge_client::WritableVariant::Bool(true),
            }
            .into(),
            PrepareAttachScriptCommand {
                context: context.clone(),
                node_id: node.clone(),
                script_ref: godot_codex_bridge_client::TransactionResourceRef::Path(
                    godot_codex_bridge_client::TransactionPathRef {
                        uid_missing: true,
                        path: "res://script.gd".to_owned(),
                    },
                ),
            }
            .into(),
            PrepareDetachScriptCommand {
                context: context.clone(),
                node_id: node.clone(),
            }
            .into(),
            PrepareConnectSignalCommand {
                context: context.clone(),
                emitter_node_id: node.clone(),
                signal: "ready".to_owned(),
                receiver_node_id: other.clone(),
                method: "_on_ready".to_owned(),
                flags: 2,
                unbinds: 0,
                binds: Vec::new(),
            }
            .into(),
            PrepareDisconnectSignalCommand {
                context,
                emitter_node_id: node,
                signal: "ready".to_owned(),
                receiver_node_id: other,
                method: "_on_ready".to_owned(),
                flags: 2,
                unbinds: 0,
                binds: Vec::new(),
            }
            .into(),
        ];
        assert_eq!(
            operations
                .iter()
                .map(|command| command.operation.kind())
                .collect::<Vec<_>>(),
            vec![
                OperationKind::CreateNode,
                OperationKind::DeleteNode,
                OperationKind::ReparentNode,
                OperationKind::SetProperty,
                OperationKind::AttachScript,
                OperationKind::DetachScript,
                OperationKind::ConnectSignal,
                OperationKind::DisconnectSignal,
            ]
        );
    }
}
