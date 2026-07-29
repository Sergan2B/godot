//! Private, one-shot Sprint 11 surface capture support.
//!
//! Capture state is deliberately separate from project-controlled files. Every
//! lease and artifact lives below the canonical, owner-private
//! `GODOT_CODEX_DATA_ROOT`; an absent capture root is a read-only no-op for the
//! normal MCP sidecar startup path.

mod journal;
mod lease;
mod observation;
mod transport;

pub use journal::{
    CaptureDirection, CaptureEvent, CaptureEventClass, CaptureJournal, CaptureRecorder,
    ProjectionError, SemanticProjection, SemanticProjectionAllowlist,
};
pub use lease::{
    ArmDisposition, ArmRequest, ArmedLease, BindingDigests, ClaimContext, ClaimedLease,
    FinalizeOutcome, LeaseError, LeaseState, LeaseStatus, LeaseStore, MetadataBinding, Surface,
    project_identity_for_path,
};
pub use observation::ToolObservation;
pub use transport::TappedTransport;

/// Schema written for capture leases.
pub const LEASE_SCHEMA_VERSION: &str = "sprint11-surface-capture-lease/1.1";

/// Schema written for the bounded, content-minimized protocol journal.
pub const JOURNAL_SCHEMA_VERSION: &str = "sprint11-surface-capture-journal/1.1";
