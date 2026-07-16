//! Machine-readable evidence model populated by the spike runner.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::BackendKind;

/// One package in a feature-specific dependency and license manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyEvidence {
    /// Cargo package name.
    pub name: String,
    /// Cargo package version.
    pub version: String,
    /// Registry/git source or `path` for local crates.
    pub source: String,
    /// SPDX expression or package-provided license text, when declared.
    pub license: Option<String>,
}

/// Non-sensitive host coordinates needed to interpret one platform run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostEvidence {
    /// GitHub runner label/name, or `local` outside CI.
    pub runner: String,
    /// Logical parallelism visible to the process.
    pub logical_cpus: usize,
}

/// One backend's raw and summarized result.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendEvidence {
    /// Candidate name.
    pub backend: BackendKind,
    /// Frozen physical settings used by this candidate.
    pub config: BTreeMap<String, String>,
    /// Whether every correctness/recovery gate passed.
    pub qualified: bool,
    /// Gate names and pass/fail state.
    pub gates: BTreeMap<String, bool>,
    /// Individual cancellation, hard-kill, and corruption matrix coordinates.
    pub fault_matrix: BTreeMap<String, bool>,
    /// Full-build duration samples.
    pub full_build_ns: Vec<u64>,
    /// Incremental UID-preserving rename samples.
    pub rename_ns: Vec<u64>,
    /// Warm reverse-query samples.
    pub reverse_query_ns: Vec<u64>,
    /// Summarized query p50.
    pub reverse_query_p50_ns: u64,
    /// Summarized query p95.
    pub reverse_query_p95_ns: u64,
    /// Durable artifact bytes after the benchmark.
    pub artifact_bytes: u64,
    /// Changed 4 KiB blocks for the measured rename.
    pub rename_changed_blocks: u64,
    /// Canonical JSON bytes in the normalized logical rename batch.
    pub rename_normalized_bytes: u64,
    /// Durable changed bytes divided by normalized logical operation bytes.
    pub rename_write_amplification: f64,
    /// Feature-specific stripped/unstripped release executable size.
    pub binary_bytes: u64,
    /// Unique packages in the feature-specific Cargo dependency tree.
    pub dependency_count: usize,
    /// Feature-specific package, source, and license manifest.
    pub dependencies: Vec<DependencyEvidence>,
    /// Weighted decision score after correctness gates.
    pub weighted_score: f64,
    /// Deterministic bootstrap 95% lower bound for the weighted score.
    pub weighted_score_ci95_low: f64,
    /// Deterministic bootstrap 95% upper bound for the weighted score.
    pub weighted_score_ci95_high: f64,
    /// Errors captured without suppressing a gate.
    pub errors: Vec<String>,
}

/// Canonical D-05 evidence document.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageSpikeEvidence {
    /// Evidence schema.
    pub schema_version: u32,
    /// Decision identifier.
    pub decision: String,
    /// `decision` for the frozen D-05 profile or `quick` for development runs.
    pub profile: String,
    /// Frozen synthetic seed.
    pub seed: String,
    /// Source revision when available.
    pub git_commit: String,
    /// Whether relevant source paths differed from the recorded commit.
    pub git_dirty: bool,
    /// Deterministic digest of every relevant tracked or untracked source file.
    pub source_tree_sha256: String,
    /// Rust compiler details.
    pub rustc: String,
    /// Target operating system.
    pub os: String,
    /// Target architecture.
    pub architecture: String,
    /// Non-sensitive runner coordinates.
    pub host: HostEvidence,
    /// Canonical oracle fixture digest.
    pub oracle_sha256: String,
    /// Resource/edge counts used for local performance evidence.
    pub dataset: DatasetEvidence,
    /// Per-backend results.
    pub backends: Vec<BackendEvidence>,
    /// Chosen backend or `blocked`.
    pub chosen_backend: String,
    /// Human-readable deterministic decision reason.
    pub decision_reason: String,
}

/// Cross-platform D-05 aggregate produced only after all required OS runs.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CombinedStorageSpikeEvidence {
    /// Aggregate schema.
    pub schema_version: u32,
    /// Decision identifier.
    pub decision: String,
    /// Required platform runs in stable OS order.
    pub platform_runs: Vec<StorageSpikeEvidence>,
    /// Cross-platform median score and bootstrap interval per candidate.
    pub backend_summaries: Vec<CombinedBackendEvidence>,
    /// True only when Linux, macOS, and Windows all supplied qualified candidates.
    pub cross_platform_complete: bool,
    /// Cross-platform selected backend or `blocked`.
    pub chosen_backend: String,
    /// Deterministic merge explanation.
    pub decision_reason: String,
}

/// One backend's three-OS decision summary.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CombinedBackendEvidence {
    /// Candidate name.
    pub backend: BackendKind,
    /// Whether every disqualifying gate passed on every required OS.
    pub qualified_all_platforms: bool,
    /// Median of the same-host normalized scores across three operating systems.
    pub median_weighted_score: f64,
    /// Deterministic cross-platform bootstrap lower bound.
    pub weighted_score_ci95_low: f64,
    /// Deterministic cross-platform bootstrap upper bound.
    pub weighted_score_ci95_high: f64,
}

/// Dataset coordinates bound into evidence.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetEvidence {
    /// Resource records.
    pub resources: usize,
    /// Direct edges.
    pub edges: usize,
    /// Full-build iterations.
    pub full_build_iterations: usize,
    /// Rename iterations.
    pub rename_iterations: usize,
    /// Query warmup count.
    pub query_warmup: usize,
    /// Timed query count.
    pub query_iterations: usize,
    /// Stress dataset resources, or zero when the profile skips stress.
    pub stress_resources: usize,
    /// Stress dataset edges, or zero when the profile skips stress.
    pub stress_edges: usize,
    /// Exact fan-in values reserved by the deterministic generator.
    pub fan_in_targets: Vec<usize>,
}

/// Returns a nearest-rank percentile over raw nanosecond samples.
#[must_use]
pub fn percentile(samples: &[u64], percentile: usize) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = (percentile * sorted.len()).div_ceil(100).saturating_sub(1);
    sorted[rank.min(sorted.len() - 1)]
}
