//! Product-level contracts shared by the Godot Codex MCP server and operations
//! tooling.
//!
//! This crate deliberately has no Godot, transport, filesystem, network, or
//! host-runtime dependency. It owns only deterministic compatibility,
//! diagnostics, connection-health, and public registry projections.

mod compatibility;
mod diagnostics;
mod health;
mod registry;

pub use compatibility::{
    Architecture, BridgeCapabilityProfile, CompatibilityEvaluation, CompatibilityMatrix,
    CompatibilityObservation, CompatibilityReason, CompatibilityStatus, GodotCoordinate,
    MatrixError, OperatingSystem, PackageCoordinate, ProductCompatibilityBasis,
    ProductCompatibilityObservation, ProtocolContract, ProtocolVersion, RegistryBinding,
    SchemaContract, SurfaceBundleError, SurfaceCompatibilityBundle, SurfaceKind,
    SurfaceQualification, SurfaceRule, TargetCoordinate, embedded_compatibility_matrix,
};
pub use diagnostics::{
    Component, Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticSpec, RemediationId,
    all_diagnostic_specs, diagnostic_spec,
};
pub use health::{
    BridgeCondition, BridgeProjection, CacheCondition, CacheRevisions, ComponentCondition,
    ConfigurationCondition, ConnectionComponents, ConnectionEvidence, ConnectionHealth,
    ConnectionLimits, ConnectionObservation, ConnectionOmittedCounts, ConnectionStatus,
    PackageCondition, ProductStartupObservation, RecoveryCondition, StaticCacheProjection,
    configuration_diagnostic, package_diagnostic,
};
pub use registry::{
    FIXED_RESOURCE_URIS, FULL_BETA_TOOLS, READ_ONLY_TOOLS, REGISTRY_PROFILE_JSON,
    RESOURCE_TEMPLATE_URIS, RegistryProfile, canonical_registry_profile, registry_profile_digest,
};

/// Version of the closed External Codex Beta compatibility document.
pub const COMPATIBILITY_MATRIX_SCHEMA: &str = "godot-codex-compatibility-matrix/1.0";

/// Version of independently distributed host-surface compatibility bundles.
pub const SURFACE_COMPATIBILITY_BUNDLE_SCHEMA: &str =
    "godot-codex-surface-compatibility-bundle/1.0";

/// Version of the bounded connection-status projection.
pub const CONNECTION_STATUS_SCHEMA: &str = "godot-connection-status/1.1";

/// Serialization ceiling for the product-core connection projection.
pub const CONNECTION_STATUS_MAX_BYTES: usize = 4_096;

/// Canonical matrix JSON embedded into every product consumer.
pub const COMPATIBILITY_MATRIX_JSON: &str =
    include_str!("../../../product/compatibility-matrix.v1.json");

/// Exact host-artifact coordinates packaged with this product.
pub const HOST_COORDINATE_PROFILE_JSON: &str =
    include_str!("../../../product/host-coordinate-profile.v1.json");

/// Exact MCP instructions bytes packaged with and served by the sidecar.
pub const SERVER_INSTRUCTIONS_TEXT: &str =
    include_str!("../../../product/server-instructions.v1.txt");

/// Closed JSON Schema for [`COMPATIBILITY_MATRIX_JSON`].
pub const COMPATIBILITY_MATRIX_JSON_SCHEMA: &str =
    include_str!("../../../schemas/godot_codex/compatibility-matrix.schema.json");

/// Closed JSON Schema for independently distributed surface bundles.
pub const SURFACE_COMPATIBILITY_BUNDLE_JSON_SCHEMA: &str =
    include_str!("../../../schemas/godot_codex/surface-compatibility-bundle.schema.json");
