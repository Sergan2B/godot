//! Deterministic, model-free operational workflows for the external Codex
//! beta package.
//!
//! `doctor` is strictly read-only and never opens the Bridge endpoint.
//! `setup` separates an immutable preview from a digest-bound apply step.

mod doctor;
mod launcher;
mod local_probe;
mod setup;
mod surface_capture;

pub use doctor::{
    BridgeProbeObservation, BridgeProbeStatus, DoctorCheck, DoctorCheckStatus, DoctorOptions,
    DoctorProbeObservations, DoctorReport, DoctorStatus, GodotProbeObservation,
    HostProbeObservation, McpProbeObservation, SurfaceSelection, observe_product_startup,
    run_doctor, run_doctor_with_probes,
};
pub use godot_codex_surface_capture::{ArmDisposition, LeaseState};
pub use setup::{
    GuidanceMode, PendingSetup, RemoveOptions, RepairOptions, SetupAction, SetupChange, SetupError,
    SetupMode, SetupOptions, SetupPreview, SetupProfile, SetupReport, apply_setup_plan,
    default_plan_store, prepare_setup, prepare_setup_for_consent, prepare_setup_remove,
    prepare_setup_remove_for_consent, prepare_setup_repair, prepare_setup_repair_for_consent,
};
pub use surface_capture::{
    SurfaceCaptureAbandonReport, SurfaceCaptureArmOptions, SurfaceCaptureArmReport,
    SurfaceCaptureCancelReport, SurfaceCaptureClaimContext, SurfaceCaptureConsumeReport,
    SurfaceCaptureError, SurfaceCaptureProjectContext, SurfaceCaptureStatusReport,
    SurfaceCaptureSurface, abandon_surface_capture, arm_surface_capture, cancel_surface_capture,
    consume_surface_capture, surface_capture_status, verify_surface_capture_claim,
    verify_surface_capture_project,
};

/// Semantic version displayed by the operations CLI and bound into setup
/// plans and receipts.
pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");
