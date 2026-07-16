//! End-to-end benchmark, correctness, fault, and decision runner.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use godot_codex_index_store::{IndexGeneration, ResourceQuery, ResourceSelector, StoreError};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::dataset::{
    SYNTHETIC_SEED, file_sha256, oracle_generations, renamed_batch, renamed_generation,
    synthetic_generation, validate_synthetic_shape,
};
use crate::evidence::{
    BackendEvidence, CombinedBackendEvidence, CombinedStorageSpikeEvidence, DatasetEvidence,
    DependencyEvidence, HostEvidence, StorageSpikeEvidence, percentile,
};
use crate::{BackendKind, FaultInjection, FaultMode, FaultPoint, SpikeStore, open_store};

/// Full decision-run configuration.
#[derive(Clone, Debug)]
pub struct RunConfig {
    /// Repository root containing canonical Stage 1 fixtures.
    pub repo_root: PathBuf,
    /// Canonical evidence output.
    pub output: PathBuf,
    /// Synthetic reference-medium resources.
    pub resources: usize,
    /// Synthetic reference-medium edges.
    pub edges: usize,
    /// Fresh full-build samples.
    pub full_build_iterations: usize,
    /// Sequential rename samples.
    pub rename_iterations: usize,
    /// Untimed reverse-query warmups.
    pub query_warmup: usize,
    /// Timed reverse-query samples.
    pub query_iterations: usize,
    /// Run one 100k/500k stress correctness build per backend.
    pub include_stress: bool,
    /// Build feature-specific release binaries and dependency trees.
    pub measure_packaging: bool,
}

impl RunConfig {
    /// Frozen D-05 decision profile.
    #[must_use]
    pub fn decision(repo_root: PathBuf, output: PathBuf) -> Self {
        Self {
            repo_root,
            output,
            resources: 10_000,
            edges: 50_000,
            full_build_iterations: 5,
            rename_iterations: 50,
            query_warmup: 1_000,
            query_iterations: 10_000,
            include_stress: true,
            measure_packaging: true,
        }
    }

    /// Fast local development profile that does not qualify D-05 evidence.
    #[must_use]
    pub fn quick(repo_root: PathBuf, output: PathBuf) -> Self {
        Self {
            repo_root,
            output,
            resources: 500,
            edges: 2_000,
            full_build_iterations: 2,
            rename_iterations: 5,
            query_warmup: 100,
            query_iterations: 1_000,
            include_stress: false,
            measure_packaging: false,
        }
    }
}

/// Runs all available candidate backends and writes canonical evidence.
pub fn run(config: &RunConfig) -> Result<StorageSpikeEvidence, StoreError> {
    let oracle_path = config
        .repo_root
        .join("tests/codex/fixtures/resource_graph_oracle/golden-resource-graph.json");
    let oracle = oracle_generations(&oracle_path)?;
    #[cfg(feature = "sqlite")]
    let kinds = vec![BackendKind::Sqlite, BackendKind::Segment];
    #[cfg(not(feature = "sqlite"))]
    let kinds = vec![BackendKind::Segment];

    let package = if config.measure_packaging {
        package_metrics(&config.repo_root)?
    } else {
        BTreeMap::new()
    };
    let mut backends = Vec::new();
    for kind in kinds {
        let packaging = package.get(kind.as_str()).cloned().unwrap_or_default();
        backends.push(run_backend(kind, config, &oracle, packaging));
    }
    assign_scores(&mut backends);
    let (chosen_backend, decision_reason) = choose_backend(&backends);
    let evidence = StorageSpikeEvidence {
        schema_version: 2,
        decision: "D-05".to_owned(),
        profile: if config.include_stress && config.measure_packaging {
            "decision"
        } else {
            "quick"
        }
        .to_owned(),
        seed: SYNTHETIC_SEED.to_owned(),
        git_commit: command_output(&config.repo_root, "git", &["rev-parse", "HEAD"])
            .unwrap_or_else(|| "unknown".to_owned()),
        git_dirty: relevant_git_dirty(&config.repo_root)?,
        source_tree_sha256: source_tree_sha256(&config.repo_root)?,
        rustc: command_output(&config.repo_root, "rustc", &["--version", "--verbose"])
            .unwrap_or_else(|| "unknown".to_owned()),
        os: std::env::consts::OS.to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        host: HostEvidence {
            runner: std::env::var("RUNNER_NAME").unwrap_or_else(|_| "local".to_owned()),
            logical_cpus: thread::available_parallelism().map_or(1, usize::from),
        },
        oracle_sha256: file_sha256(&oracle_path)?,
        dataset: DatasetEvidence {
            resources: config.resources,
            edges: config.edges,
            full_build_iterations: config.full_build_iterations,
            rename_iterations: config.rename_iterations,
            query_warmup: config.query_warmup,
            query_iterations: config.query_iterations,
            stress_resources: usize::from(config.include_stress) * 100_000,
            stress_edges: usize::from(config.include_stress) * 500_000,
            fan_in_targets: vec![0, 1, 10, 100, 1_000],
        },
        backends,
        chosen_backend,
        decision_reason,
    };
    write_evidence(&config.output, &evidence)?;
    Ok(evidence)
}

/// Merges Linux, macOS, and Windows raw runs into the canonical D-05 artifact.
pub fn merge_platform_evidence(
    output: &Path,
    inputs: &[PathBuf],
) -> Result<CombinedStorageSpikeEvidence, StoreError> {
    let mut platform_runs = inputs
        .iter()
        .map(|path| {
            let bytes = fs::read(path).map_err(io_error)?;
            serde_json::from_slice::<StorageSpikeEvidence>(&bytes)
                .map_err(|error| StoreError::CorruptStore(error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    platform_runs.sort_by(|left, right| left.os.cmp(&right.os));
    let present: BTreeSet<_> = platform_runs.iter().map(|run| run.os.as_str()).collect();
    let required: BTreeSet<_> = ["linux", "macos", "windows"].into_iter().collect();
    let same_oracle = platform_runs.first().is_some_and(|first| {
        platform_runs
            .iter()
            .all(|run| run.oracle_sha256 == first.oracle_sha256)
    });
    let same_source = platform_runs.first().is_some_and(|first| {
        platform_runs
            .iter()
            .all(|run| run.source_tree_sha256 == first.source_tree_sha256)
    });
    let same_commit = platform_runs.first().is_some_and(|first| {
        platform_runs
            .iter()
            .all(|run| run.git_commit == first.git_commit)
    });
    let clean_sources = platform_runs.iter().all(|run| !run.git_dirty);
    let valid_profiles = platform_runs.iter().all(decision_profile_valid);
    let coordinates_complete = platform_runs.len() == 3
        && present == required
        && same_oracle
        && same_source
        && same_commit
        && clean_sources
        && valid_profiles;
    let backend_summaries = if coordinates_complete {
        cross_platform_summaries(&platform_runs)
    } else {
        Vec::new()
    };
    let candidates: Vec<_> = backend_summaries
        .iter()
        .filter(|summary| summary.qualified_all_platforms)
        .collect();
    let cross_platform_complete = coordinates_complete && !candidates.is_empty();
    let (chosen_backend, decision_reason) = if !coordinates_complete {
        (
            "blocked".to_owned(),
            "Linux/macOS/Windows coordinates, clean source digest, seed, or canonical oracle digest are incomplete or inconsistent."
                .to_owned(),
        )
    } else {
        match candidates.as_slice() {
            [] => (
                "blocked".to_owned(),
                "No backend passed every disqualifying gate on Linux, macOS, and Windows."
                    .to_owned(),
            ),
            [summary] => (
                summary.backend.as_str().to_owned(),
                format!(
                    "Only {} passed every D-05 gate on Linux, macOS, and Windows.",
                    summary.backend.as_str()
                ),
            ),
            [left, right] => {
                let difference = (left.median_weighted_score - right.median_weighted_score).abs();
                let intervals_overlap = left.weighted_score_ci95_low
                    <= right.weighted_score_ci95_high
                    && right.weighted_score_ci95_low <= left.weighted_score_ci95_high;
                if difference < 0.05 || intervals_overlap {
                    (
                        "sqlite".to_owned(),
                        "Both backends qualified cross-platform and their median weighted scores differed by less than five points or bootstrap intervals overlapped; the D-05 tie-break selects bundled SQLite."
                            .to_owned(),
                    )
                } else if left.median_weighted_score > right.median_weighted_score {
                    (
                        left.backend.as_str().to_owned(),
                        format!(
                            "Both backends qualified cross-platform; {} had the higher median weighted score ({:.4}).",
                            left.backend.as_str(),
                            left.median_weighted_score
                        ),
                    )
                } else {
                    (
                        right.backend.as_str().to_owned(),
                        format!(
                            "Both backends qualified cross-platform; {} had the higher median weighted score ({:.4}).",
                            right.backend.as_str(),
                            right.median_weighted_score
                        ),
                    )
                }
            }
            _ => unreachable!("two backend kinds are defined"),
        }
    };
    let combined = CombinedStorageSpikeEvidence {
        schema_version: 2,
        decision: "D-05".to_owned(),
        platform_runs,
        backend_summaries,
        cross_platform_complete,
        chosen_backend,
        decision_reason,
    };
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let bytes = serde_json::to_vec_pretty(&combined)
        .map_err(|error| StoreError::StorageIo(error.to_string()))?;
    fs::write(output, bytes).map_err(io_error)?;
    Ok(combined)
}

fn decision_profile_valid(run: &StorageSpikeEvidence) -> bool {
    let expected_gates: BTreeSet<_> = [
        "cached_query_p95_lte_300ms",
        "canonical_oracle",
        "concurrent_read_activation",
        "corrupt_cache_isolation",
        "direct_reverse_parity",
        "graceful_cancellation",
        "process_crash_recovery",
        "process_locking_reopen",
        "rename_p95_lte_2s",
        "single_binary_packaging",
        "stress_large_reopen",
        "v1_v2_migration",
    ]
    .into_iter()
    .collect();
    let expected_faults: BTreeSet<_> = [
        "graceful_cancel.capture",
        "graceful_cancel.staging",
        "graceful_cancel.pre_commit",
        "graceful_cancel.post_commit",
        "hard_kill.capture",
        "hard_kill.staging",
        "hard_kill.pre_commit",
        "hard_kill.post_commit",
        "corruption.metadata",
        "corruption.resource",
        "corruption.reverse",
        "corruption.project_binding",
        "corruption.incompatible_schema",
    ]
    .into_iter()
    .collect();
    let dataset = &run.dataset;
    if run.schema_version != 2
        || run.decision != "D-05"
        || run.profile != "decision"
        || run.seed != SYNTHETIC_SEED
        || run.git_commit == "unknown"
        || !run.source_tree_sha256.starts_with("sha256:")
        || !run.oracle_sha256.starts_with("sha256:")
        || !run.rustc.starts_with("rustc 1.94.1 ")
        || run.host.logical_cpus == 0
        || dataset.resources != 10_000
        || dataset.edges != 50_000
        || dataset.full_build_iterations != 5
        || dataset.rename_iterations != 50
        || dataset.query_warmup != 1_000
        || dataset.query_iterations != 10_000
        || dataset.stress_resources != 100_000
        || dataset.stress_edges != 500_000
        || dataset.fan_in_targets != [0, 1, 10, 100, 1_000]
        || run.backends.len() != 2
    {
        return false;
    }
    let backend_kinds: BTreeSet<_> = run
        .backends
        .iter()
        .map(|backend| backend.backend.as_str())
        .collect();
    if backend_kinds != BTreeSet::from(["sqlite", "segment"]) {
        return false;
    }
    for backend in &run.backends {
        let gate_names: BTreeSet<_> = backend.gates.keys().map(String::as_str).collect();
        let fault_names: BTreeSet<_> = backend.fault_matrix.keys().map(String::as_str).collect();
        if gate_names != expected_gates
            || fault_names != expected_faults
            || backend.config != backend_config(backend.backend)
            || backend.full_build_ns.len() != dataset.full_build_iterations
            || backend.rename_ns.len() != dataset.rename_iterations
            || backend.reverse_query_ns.len() != dataset.query_iterations
            || backend.full_build_ns.contains(&0)
            || backend.rename_ns.contains(&0)
            || backend.reverse_query_ns.contains(&0)
            || backend.reverse_query_p50_ns != percentile(&backend.reverse_query_ns, 50)
            || backend.reverse_query_p95_ns != percentile(&backend.reverse_query_ns, 95)
            || backend.artifact_bytes == 0
            || backend.rename_changed_blocks == 0
            || backend.rename_normalized_bytes == 0
            || !approximately_equal(
                backend.rename_write_amplification,
                backend.rename_changed_blocks as f64 * 4096.0
                    / backend.rename_normalized_bytes as f64,
            )
            || backend.binary_bytes == 0
            || backend.dependency_count != backend.dependencies.len()
            || backend.dependencies.is_empty()
            || backend.qualified != backend.gates.values().all(|passed| *passed)
            || backend.gates["graceful_cancellation"]
                != matrix_group_passed(&backend.fault_matrix, "graceful_cancel.")
            || backend.gates["process_crash_recovery"]
                != matrix_group_passed(&backend.fault_matrix, "hard_kill.")
            || backend.gates["corrupt_cache_isolation"]
                != matrix_group_passed(&backend.fault_matrix, "corruption.")
            || backend.gates["cached_query_p95_lte_300ms"]
                != (backend.reverse_query_p95_ns <= 300_000_000)
            || backend.gates["rename_p95_lte_2s"]
                != (percentile(&backend.rename_ns, 95) <= 2_000_000_000)
            || (backend.qualified && !backend.errors.is_empty())
        {
            return false;
        }
    }
    let mut recomputed = run.backends.clone();
    assign_scores(&mut recomputed);
    if recomputed
        .iter()
        .zip(&run.backends)
        .any(|(actual, recorded)| {
            !approximately_equal(actual.weighted_score, recorded.weighted_score)
                || !approximately_equal(
                    actual.weighted_score_ci95_low,
                    recorded.weighted_score_ci95_low,
                )
                || !approximately_equal(
                    actual.weighted_score_ci95_high,
                    recorded.weighted_score_ci95_high,
                )
        })
    {
        return false;
    }
    let (chosen, reason) = choose_backend(&recomputed);
    run.chosen_backend == chosen && run.decision_reason == reason
}

fn cross_platform_summaries(runs: &[StorageSpikeEvidence]) -> Vec<CombinedBackendEvidence> {
    let kinds = [BackendKind::Sqlite, BackendKind::Segment];
    let mut bootstrap_scores = [Vec::with_capacity(1_000), Vec::with_capacity(1_000)];
    let mut random_state = 0x5d05_d05d_05d0_5d05_u64;
    for _ in 0..1_000 {
        let mut platform_scores = [Vec::with_capacity(3), Vec::with_capacity(3)];
        for run in runs {
            let sampled: Vec<_> = run
                .backends
                .iter()
                .map(|backend| sampled_backend_metrics(backend, &mut random_state))
                .collect();
            for (backend_index, backend) in run.backends.iter().enumerate() {
                let slot = backend_slot(backend.backend);
                let score = if backend.qualified {
                    normalized_score(
                        &sampled,
                        backend_index,
                        &[0.30, 0.25, 0.20, 0.15, 0.05, 0.05],
                    )
                } else {
                    0.0
                };
                platform_scores[slot].push(score);
            }
        }
        for slot in 0..kinds.len() {
            platform_scores[slot].sort_by(f64::total_cmp);
            bootstrap_scores[slot].push(platform_scores[slot][1]);
        }
    }

    kinds
        .into_iter()
        .enumerate()
        .map(|(slot, kind)| {
            let mut scores: Vec<_> = runs
                .iter()
                .map(|run| {
                    run.backends
                        .iter()
                        .find(|backend| backend.backend == kind)
                        .expect("decision profile has both backends")
                        .weighted_score
                })
                .collect();
            scores.sort_by(f64::total_cmp);
            bootstrap_scores[slot].sort_by(f64::total_cmp);
            CombinedBackendEvidence {
                backend: kind,
                qualified_all_platforms: runs.iter().all(|run| {
                    run.backends
                        .iter()
                        .find(|backend| backend.backend == kind)
                        .is_some_and(|backend| backend.qualified)
                }),
                median_weighted_score: scores[1],
                weighted_score_ci95_low: bootstrap_scores[slot][24],
                weighted_score_ci95_high: bootstrap_scores[slot][974],
            }
        })
        .collect()
}

fn sampled_backend_metrics(backend: &BackendEvidence, state: &mut u64) -> [f64; 6] {
    [
        bootstrap_percentile(&backend.rename_ns, 50, state) as f64,
        bootstrap_percentile(&backend.reverse_query_ns, 95, state) as f64,
        bootstrap_percentile(&backend.full_build_ns, 50, state) as f64,
        backend.rename_write_amplification,
        backend.artifact_bytes as f64,
        (backend.binary_bytes + backend.dependency_count as u64 * 1024) as f64,
    ]
}

const fn backend_slot(kind: BackendKind) -> usize {
    match kind {
        BackendKind::Sqlite => 0,
        BackendKind::Segment => 1,
    }
}

fn approximately_equal(left: f64, right: f64) -> bool {
    (left - right).abs() <= 1.0e-12
}

fn run_backend(
    kind: BackendKind,
    config: &RunConfig,
    oracle: &[IndexGeneration],
    packaging: PackageMetrics,
) -> BackendEvidence {
    let mut gates = BTreeMap::new();
    let mut fault_matrix = BTreeMap::new();
    let mut errors = Vec::new();
    gate(&mut gates, &mut errors, "canonical_oracle", || {
        oracle_gate(kind, oracle)
    });
    gate(&mut gates, &mut errors, "direct_reverse_parity", || {
        parity_gate(kind, &oracle[0])
    });
    for point in [
        FaultPoint::Capture,
        FaultPoint::Staging,
        FaultPoint::PreCommit,
        FaultPoint::PostCommit,
    ] {
        gate(
            &mut fault_matrix,
            &mut errors,
            &format!("graceful_cancel.{}", fault_name(point)),
            || cancellation_case(kind, point),
        );
        gate(
            &mut fault_matrix,
            &mut errors,
            &format!("hard_kill.{}", fault_name(point)),
            || crash_case(kind, point),
        );
    }
    gates.insert(
        "graceful_cancellation".to_owned(),
        matrix_group_passed(&fault_matrix, "graceful_cancel."),
    );
    gates.insert(
        "process_crash_recovery".to_owned(),
        matrix_group_passed(&fault_matrix, "hard_kill."),
    );
    gate(&mut gates, &mut errors, "process_locking_reopen", || {
        locking_gate(kind)
    });
    gate(
        &mut gates,
        &mut errors,
        "concurrent_read_activation",
        || concurrent_read_gate(kind),
    );
    for corruption in [
        "metadata",
        "resource",
        "reverse",
        "project_binding",
        "incompatible_schema",
    ] {
        gate(
            &mut fault_matrix,
            &mut errors,
            &format!("corruption.{corruption}"),
            || corruption_case(kind, corruption),
        );
    }
    gates.insert(
        "corrupt_cache_isolation".to_owned(),
        matrix_group_passed(&fault_matrix, "corruption."),
    );
    gate(&mut gates, &mut errors, "v1_v2_migration", || {
        migration_gate(kind)
    });
    if config.include_stress {
        gate(&mut gates, &mut errors, "stress_large_reopen", || {
            stress_gate(kind)
        });
    }
    gates.insert(
        "single_binary_packaging".to_owned(),
        packaging.binary_bytes > 0
            && !packaging.dependencies.is_empty()
            && packaging.portable_single_binary,
    );

    let benchmark = benchmark(kind, config).unwrap_or_else(|error| {
        errors.push(format!("benchmark: {error}"));
        BenchmarkResult::default()
    });
    let reverse_query_p50_ns = percentile(&benchmark.reverse_query_ns, 50);
    let reverse_query_p95_ns = percentile(&benchmark.reverse_query_ns, 95);
    let rename_p95 = percentile(&benchmark.rename_ns, 95);
    gates.insert(
        "cached_query_p95_lte_300ms".to_owned(),
        reverse_query_p95_ns > 0 && reverse_query_p95_ns <= 300_000_000,
    );
    gates.insert(
        "rename_p95_lte_2s".to_owned(),
        rename_p95 > 0 && rename_p95 <= 2_000_000_000,
    );
    let qualified = gates.values().all(|passed| *passed);
    BackendEvidence {
        backend: kind,
        config: backend_config(kind),
        qualified,
        gates,
        fault_matrix,
        full_build_ns: benchmark.full_build_ns,
        rename_ns: benchmark.rename_ns,
        reverse_query_ns: benchmark.reverse_query_ns,
        reverse_query_p50_ns,
        reverse_query_p95_ns,
        artifact_bytes: benchmark.artifact_bytes,
        rename_changed_blocks: benchmark.rename_changed_blocks,
        rename_normalized_bytes: benchmark.rename_normalized_bytes,
        rename_write_amplification: benchmark.rename_changed_blocks as f64 * 4096.0
            / benchmark.rename_normalized_bytes.max(1) as f64,
        binary_bytes: packaging.binary_bytes,
        dependency_count: packaging.dependencies.len(),
        dependencies: packaging.dependencies,
        weighted_score: 0.0,
        weighted_score_ci95_low: 0.0,
        weighted_score_ci95_high: 0.0,
        errors,
    }
}

#[derive(Default)]
struct BenchmarkResult {
    full_build_ns: Vec<u64>,
    rename_ns: Vec<u64>,
    reverse_query_ns: Vec<u64>,
    artifact_bytes: u64,
    rename_changed_blocks: u64,
    rename_normalized_bytes: u64,
}

fn benchmark(kind: BackendKind, config: &RunConfig) -> Result<BenchmarkResult, StoreError> {
    let base = synthetic_generation(config.resources, config.edges, 1);
    validate_synthetic_shape(&base)?;
    let mut full_build_ns = Vec::with_capacity(config.full_build_iterations);
    for _ in 0..config.full_build_iterations {
        let temp = TempDir::new().map_err(io_error)?;
        let start = Instant::now();
        let mut store = open_store(kind, temp.path(), &base.project_id)?;
        store.activate(&base, None)?;
        full_build_ns.push(elapsed_ns(start));
        if store.active_generation()?.validation_digest != base.validation_digest {
            return Err(StoreError::ValidationFailed(
                "benchmark full-build digest mismatch".to_owned(),
            ));
        }
    }

    let temp = TempDir::new().map_err(io_error)?;
    let mut store = open_store(kind, temp.path(), &base.project_id)?;
    store.activate(&base, None)?;
    let blocks_before = block_map(temp.path())?;
    let first_rename = renamed_generation(&base, 2);
    let rename_normalized_bytes = u64::try_from(
        serde_json::to_vec(&renamed_batch(&base, 2))
            .map_err(|error| StoreError::StorageIo(error.to_string()))?
            .len(),
    )
    .map_err(|_| StoreError::ValidationFailed("normalized rename is too large".to_owned()))?;
    let start = Instant::now();
    store.activate(&first_rename, None)?;
    let mut rename_ns = vec![elapsed_ns(start)];
    let blocks_after = block_map(temp.path())?;
    let rename_changed_blocks = changed_blocks(&blocks_before, &blocks_after);
    let mut current = first_rename;
    for iteration in 1..config.rename_iterations {
        let next = renamed_generation(&current, (iteration + 2) as u64);
        let start = Instant::now();
        store.activate(&next, None)?;
        rename_ns.push(elapsed_ns(start));
        current = next;
    }

    let selector =
        ResourceSelector::EntityId("godot:resource:uid:v1:synthetic-00000005".to_owned());
    let query = ResourceQuery {
        selector,
        limit: 200,
        offset: 0,
    };
    for _ in 0..config.query_warmup {
        store.reverse(&query)?;
    }
    let mut reverse_query_ns = Vec::with_capacity(config.query_iterations);
    for _ in 0..config.query_iterations {
        let start = Instant::now();
        let result = store.reverse(&query)?;
        reverse_query_ns.push(elapsed_ns(start));
        if result.generation_id != current.generation_id {
            return Err(StoreError::ValidationFailed(
                "query escaped active generation".to_owned(),
            ));
        }
        if result.edges.len() != 200 {
            return Err(StoreError::ValidationFailed(format!(
                "high-fan-in query returned {} edges instead of 200",
                result.edges.len()
            )));
        }
    }
    Ok(BenchmarkResult {
        full_build_ns,
        rename_ns,
        reverse_query_ns,
        artifact_bytes: tree_bytes(temp.path())?,
        rename_changed_blocks,
        rename_normalized_bytes,
    })
}

fn oracle_gate(kind: BackendKind, oracle: &[IndexGeneration]) -> Result<(), StoreError> {
    let temp = TempDir::new().map_err(io_error)?;
    let mut store = open_store(kind, temp.path(), &oracle[0].project_id)?;
    for generation in oracle {
        store.activate(generation, None)?;
        let actual = store.active_generation()?;
        if actual != *generation {
            return Err(StoreError::ValidationFailed(format!(
                "oracle phase {} mismatch",
                generation.generation_id
            )));
        }
    }
    drop(store);
    let reopened = read_generation(kind, temp.path(), &oracle[0].project_id)?;
    if reopened != *oracle.last().expect("oracle is non-empty") {
        return Err(StoreError::ValidationFailed(
            "compatible reopen mismatch".to_owned(),
        ));
    }
    Ok(())
}

fn parity_gate(kind: BackendKind, generation: &IndexGeneration) -> Result<(), StoreError> {
    let temp = TempDir::new().map_err(io_error)?;
    let mut store = open_store(kind, temp.path(), &generation.project_id)?;
    store.activate(generation, None)?;
    for edge in &generation.dependencies {
        let direct = store.direct(&ResourceQuery {
            selector: ResourceSelector::EntityId(edge.source_entity_id.clone()),
            limit: 200,
            offset: 0,
        })?;
        if !direct
            .edges
            .iter()
            .any(|candidate| candidate.edge_id == edge.edge_id)
        {
            return Err(StoreError::ValidationFailed(format!(
                "direct edge {} missing",
                edge.edge_id
            )));
        }
        if let Some(target) = &edge.target_entity_id {
            let reverse = store.reverse(&ResourceQuery {
                selector: ResourceSelector::EntityId(target.clone()),
                limit: 200,
                offset: 0,
            })?;
            if !reverse
                .edges
                .iter()
                .any(|candidate| candidate.edge_id == edge.edge_id)
            {
                return Err(StoreError::ValidationFailed(format!(
                    "reverse edge {} missing",
                    edge.edge_id
                )));
            }
        }
    }
    Ok(())
}

fn cancellation_case(kind: BackendKind, point: FaultPoint) -> Result<(), StoreError> {
    let temp = TempDir::new().map_err(io_error)?;
    let base = synthetic_generation(64, 192, 1);
    let next = renamed_generation(&base, 2);
    let mut store = open_store(kind, temp.path(), &base.project_id)?;
    store.activate(&base, None)?;
    let result = store.activate(
        &next,
        Some(FaultInjection {
            point,
            mode: FaultMode::Cancel,
        }),
    );
    if result != Err(StoreError::Cancelled) {
        return Err(StoreError::ValidationFailed(format!(
            "{point:?} did not cancel"
        )));
    }
    let expected = if point == FaultPoint::PostCommit {
        &next
    } else {
        &base
    };
    drop(store);
    let mut reopened = open_store(kind, temp.path(), &base.project_id)?;
    if reopened.active_generation()?.generation_id != expected.generation_id {
        return Err(StoreError::ValidationFailed(format!(
            "{point:?} exposed wrong generation"
        )));
    }
    if point == FaultPoint::PostCommit {
        let metadata = reopened.activate(&next, None)?;
        if metadata.active_generation_id.as_deref() != Some(next.generation_id.as_str()) {
            return Err(StoreError::ValidationFailed(
                "post-commit cancellation retry was not idempotent".to_owned(),
            ));
        }
    }
    Ok(())
}

fn crash_case(kind: BackendKind, point: FaultPoint) -> Result<(), StoreError> {
    let executable = std::env::current_exe().map_err(io_error)?;
    let temp = TempDir::new().map_err(io_error)?;
    let base = synthetic_generation(64, 192, 1);
    let next = renamed_generation(&base, 2);
    {
        let mut store = open_store(kind, temp.path(), &base.project_id)?;
        store.activate(&base, None)?;
    }
    let ready = temp
        .path()
        .join(format!("fault-ready-{}", fault_name(point)));
    let mut child = Command::new(&executable)
        .args([
            "worker-fault",
            kind.as_str(),
            temp.path().to_string_lossy().as_ref(),
            fault_name(point),
        ])
        .env("CODEX_STORAGE_SPIKE_FAULT_READY", &ready)
        .spawn()
        .map_err(io_error)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready.exists() && Instant::now() < deadline {
        if let Some(status) = child.try_wait().map_err(io_error)? {
            return Err(StoreError::ValidationFailed(format!(
                "{point:?} crash worker exited before kill: {status}"
            )));
        }
        thread::sleep(Duration::from_millis(5));
    }
    if !ready.exists() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(StoreError::ValidationFailed(format!(
            "{point:?} crash worker did not reach the fault boundary"
        )));
    }
    child.kill().map_err(io_error)?;
    let status = child.wait().map_err(io_error)?;
    if status.success() {
        return Err(StoreError::ValidationFailed(format!(
            "{point:?} crash worker was not killed"
        )));
    }
    let expected = if point == FaultPoint::PostCommit {
        &next
    } else {
        &base
    };
    let mut reopened = open_store(kind, temp.path(), &base.project_id)?;
    if reopened.active_generation()?.generation_id != expected.generation_id {
        return Err(StoreError::ValidationFailed(format!(
            "{point:?} crash recovery selected wrong generation"
        )));
    }
    if point == FaultPoint::PostCommit {
        let metadata = reopened.activate(&next, None)?;
        if metadata.active_generation_id.as_deref() != Some(next.generation_id.as_str()) {
            return Err(StoreError::ValidationFailed(
                "post-commit crash retry was not idempotent".to_owned(),
            ));
        }
    }
    Ok(())
}

fn locking_gate(kind: BackendKind) -> Result<(), StoreError> {
    let temp = TempDir::new().map_err(io_error)?;
    let base = synthetic_generation(16, 32, 1);
    let mut owner = open_store(kind, temp.path(), &base.project_id)?;
    owner.activate(&base, None)?;
    let executable = std::env::current_exe().map_err(io_error)?;
    let status = Command::new(executable)
        .args([
            "worker-open",
            kind.as_str(),
            temp.path().to_string_lossy().as_ref(),
            &base.project_id,
        ])
        .status()
        .map_err(io_error)?;
    if status.code() != Some(75) {
        return Err(StoreError::ValidationFailed(
            "second process writer was not rejected".to_owned(),
        ));
    }
    drop(owner);
    let reopened = open_store(kind, temp.path(), &base.project_id)?;
    if reopened.active_generation()?.generation_id != base.generation_id {
        return Err(StoreError::ValidationFailed(
            "writer reopen lost active generation".to_owned(),
        ));
    }
    Ok(())
}

fn concurrent_read_gate(kind: BackendKind) -> Result<(), StoreError> {
    let temp = TempDir::new().map_err(io_error)?;
    let base = synthetic_generation(1_000, 5_000, 1);
    let next = renamed_generation(&base, 2);
    let mut store = open_store(kind, temp.path(), &base.project_id)?;
    store.activate(&base, None)?;
    let running = Arc::new(AtomicBool::new(true));
    let observations = Arc::new(Mutex::new(Vec::new()));
    let mut readers = Vec::new();
    for _ in 0..4 {
        let running = Arc::clone(&running);
        let observations = Arc::clone(&observations);
        let root = temp.path().to_path_buf();
        let project = base.project_id.clone();
        readers.push(thread::spawn(move || {
            while running.load(Ordering::Acquire) {
                let result = read_generation(kind, &root, &project)
                    .map(|generation| generation.generation_id)
                    .map_err(|error| error.to_string());
                observations.lock().expect("observations lock").push(result);
                thread::yield_now();
            }
        }));
    }
    thread::sleep(Duration::from_millis(10));
    store.activate(&next, None)?;
    thread::sleep(Duration::from_millis(20));
    running.store(false, Ordering::Release);
    for reader in readers {
        reader
            .join()
            .map_err(|_| StoreError::ValidationFailed("concurrent reader panicked".to_owned()))?;
    }
    let observations = observations.lock().expect("observations lock");
    if observations.is_empty() {
        return Err(StoreError::ValidationFailed(
            "concurrent readers produced no observations".to_owned(),
        ));
    }
    let allowed: BTreeSet<_> = [base.generation_id, next.generation_id]
        .into_iter()
        .collect();
    for observation in observations.iter() {
        let generation = observation.as_ref().map_err(|error| {
            StoreError::ValidationFailed(format!("concurrent reader error: {error}"))
        })?;
        if !allowed.contains(generation) {
            return Err(StoreError::ValidationFailed(
                "concurrent reader observed partial generation".to_owned(),
            ));
        }
    }
    Ok(())
}

fn migration_gate(kind: BackendKind) -> Result<(), StoreError> {
    let temp = TempDir::new().map_err(io_error)?;
    let base = synthetic_generation(128, 512, 1);
    let mut store: Box<dyn SpikeStore> = match kind {
        #[cfg(feature = "sqlite")]
        BackendKind::Sqlite => Box::new(crate::sqlite::SqliteStore::open_with_version(
            temp.path(),
            &base.project_id,
            1,
        )?),
        #[cfg(not(feature = "sqlite"))]
        BackendKind::Sqlite => return Err(StoreError::NotReady),
        BackendKind::Segment => Box::new(crate::segment::SegmentStore::open_with_version(
            temp.path(),
            &base.project_id,
            1,
        )?),
    };
    store.activate(&base, None)?;
    let before = store.active_generation()?;
    store.migrate_v2()?;
    let after = store.active_generation()?;
    if store.physical_version()? != 2
        || before.resources != after.resources
        || before.source_documents != after.source_documents
        || before.dependencies != after.dependencies
        || before.diagnostics != after.diagnostics
        || before.tombstones != after.tombstones
    {
        return Err(StoreError::ValidationFailed(
            "v1 to v2 changed logical graph".to_owned(),
        ));
    }
    let revision = after.index_revision;
    store.migrate_v2()?;
    if store.active_generation()?.index_revision != revision {
        return Err(StoreError::ValidationFailed(
            "v2 migration was not idempotent".to_owned(),
        ));
    }
    drop(store);
    let reopened = read_generation(kind, temp.path(), &base.project_id)?;
    if reopened.validation_digest != after.validation_digest {
        return Err(StoreError::ValidationFailed(
            "migrated graph changed after reopen".to_owned(),
        ));
    }
    Ok(())
}

fn corruption_case(kind: BackendKind, corruption: &str) -> Result<(), StoreError> {
    let temp = TempDir::new().map_err(io_error)?;
    fs::create_dir_all(temp.path().join("resources")).map_err(io_error)?;
    fs::write(
        temp.path().join("project.godot"),
        b"[application]\nconfig/name=\"D05 sentinel\"\n",
    )
    .map_err(io_error)?;
    fs::write(
        temp.path().join("resources/sentinel.tres"),
        b"[gd_resource format=3]\n",
    )
    .map_err(io_error)?;
    let project_before = project_source_digest(temp.path())?;
    let base = synthetic_generation(64, 192, 1);
    {
        let mut store = open_store(kind, temp.path(), &base.project_id)?;
        store.activate(&base, None)?;
    }
    corrupt(kind, temp.path(), corruption)?;
    let error = read_generation(kind, temp.path(), &base.project_id)
        .expect_err("corrupt or incompatible cache must not be current");
    let expected_error = match corruption {
        "project_binding" => error == StoreError::ProjectMismatch,
        "incompatible_schema" => error == StoreError::IncompatibleSchema,
        _ => matches!(error, StoreError::CorruptStore(_)),
    };
    if !expected_error {
        return Err(StoreError::ValidationFailed(format!(
            "{corruption} returned unexpected error: {error}"
        )));
    }
    quarantine(kind, temp.path())?;
    let replacement = open_store(kind, temp.path(), &base.project_id)?;
    if replacement.active_generation() != Err(StoreError::NotReady) {
        return Err(StoreError::ValidationFailed(
            "replacement store was not empty after quarantine".to_owned(),
        ));
    }
    if project_source_digest(temp.path())? != project_before {
        return Err(StoreError::ValidationFailed(
            "cache quarantine changed project source bytes".to_owned(),
        ));
    }
    Ok(())
}

fn project_source_digest(root: &Path) -> Result<String, StoreError> {
    let mut files = Vec::new();
    visit_files(root, root, &mut |relative, path| {
        if !relative.starts_with(".godot") {
            files.push((relative.to_path_buf(), fs::read(path).map_err(io_error)?));
        }
        Ok(())
    })?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut digest = Sha256::new();
    for (path, bytes) in files {
        let path = path.to_string_lossy();
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path.as_bytes());
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn stress_gate(kind: BackendKind) -> Result<(), StoreError> {
    let temp = TempDir::new().map_err(io_error)?;
    let generation = synthetic_generation(100_000, 500_000, 1);
    validate_synthetic_shape(&generation)?;
    {
        let mut store = open_store(kind, temp.path(), &generation.project_id)?;
        store.activate(&generation, None)?;
    }
    let reopened = read_generation(kind, temp.path(), &generation.project_id)?;
    if reopened.validation_digest != generation.validation_digest {
        return Err(StoreError::ValidationFailed(
            "stress reopen digest mismatch".to_owned(),
        ));
    }
    Ok(())
}

/// Worker entry used to make abrupt process termination observable.
pub fn worker_fault(kind: BackendKind, root: &Path, point: FaultPoint) -> Result<(), StoreError> {
    let project = "project-d05-synthetic";
    let mut store = open_store(kind, root, project)?;
    let active = store.active_generation()?;
    let next = renamed_generation(&active, active.index_revision + 1);
    store.activate(
        &next,
        Some(FaultInjection {
            point,
            mode: FaultMode::Crash,
        }),
    )?;
    Err(StoreError::ValidationFailed(
        "crash failpoint returned".to_owned(),
    ))
}

/// Worker entry used by the process-lock gate.
pub fn worker_open(kind: BackendKind, root: &Path, project_id: &str) -> i32 {
    match open_store(kind, root, project_id) {
        Err(StoreError::StoreBusy) => 75,
        Ok(_) => 0,
        Err(_) => 1,
    }
}

fn read_generation(
    kind: BackendKind,
    root: &Path,
    project_id: &str,
) -> Result<IndexGeneration, StoreError> {
    match kind {
        #[cfg(feature = "sqlite")]
        BackendKind::Sqlite => crate::sqlite::SqliteStore::read_generation(root, project_id),
        #[cfg(not(feature = "sqlite"))]
        BackendKind::Sqlite => Err(StoreError::NotReady),
        BackendKind::Segment => crate::segment::SegmentStore::read_generation(root, project_id),
    }
}

#[cfg(feature = "sqlite")]
fn corrupt_sqlite(root: &Path, corruption: &str) -> Result<(), StoreError> {
    let path = root.join(".godot/codex/index.sqlite");
    let connection = rusqlite::Connection::open(path)
        .map_err(|error| StoreError::StorageIo(error.to_string()))?;
    let sql = match corruption {
        "metadata" => "UPDATE generations SET header_json = '{' WHERE state = 'active'",
        "resource" => {
            "UPDATE resources SET record_json = '{' WHERE rowid = (SELECT MIN(rowid) FROM resources)"
        }
        "reverse" => {
            "UPDATE dependency_edges SET record_json = '{}' WHERE rowid = (SELECT MIN(rowid) FROM dependency_edges)"
        }
        "project_binding" => "UPDATE store_metadata SET project_id = 'wrong-project' WHERE id = 1",
        "incompatible_schema" => "UPDATE store_metadata SET physical_version = 999 WHERE id = 1",
        _ => {
            return Err(StoreError::ValidationFailed(
                "unknown corruption".to_owned(),
            ));
        }
    };
    connection
        .execute(sql, [])
        .map_err(|error| StoreError::StorageIo(error.to_string()))?;
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|error| StoreError::StorageIo(error.to_string()))
}

fn corrupt(kind: BackendKind, root: &Path, corruption: &str) -> Result<(), StoreError> {
    match kind {
        #[cfg(feature = "sqlite")]
        BackendKind::Sqlite => corrupt_sqlite(root, corruption),
        #[cfg(not(feature = "sqlite"))]
        BackendKind::Sqlite => Err(StoreError::NotReady),
        BackendKind::Segment => {
            let index = root.join(".godot/codex/index");
            if corruption == "project_binding" {
                return rewrite_segment_manifest_field(
                    &index,
                    "project_id",
                    serde_json::Value::String("wrong-project".to_owned()),
                );
            }
            if corruption == "incompatible_schema" {
                return rewrite_segment_manifest_field(
                    &index,
                    "physical_version",
                    serde_json::Value::from(999),
                );
            }
            if corruption == "metadata" {
                let path = latest_file(&index.join("generations"))?;
                return flip_byte(&path);
            }
            let manifest_path = latest_file(&index.join("generations"))?;
            let manifest: serde_json::Value =
                serde_json::from_slice(&fs::read(manifest_path).map_err(io_error)?)
                    .map_err(|error| StoreError::CorruptStore(error.to_string()))?;
            let key = if corruption == "reverse" {
                "reverse_edges"
            } else {
                "resources"
            };
            let digest = manifest
                .get(key)
                .and_then(serde_json::Value::as_object)
                .and_then(|map| map.values().next())
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| StoreError::CorruptStore("segment digest missing".to_owned()))?;
            let extension = if corruption == "reverse" {
                "idx"
            } else {
                "seg"
            };
            flip_byte(&index.join("segments").join(format!("{digest}.{extension}")))
        }
    }
}

fn rewrite_segment_manifest_field(
    index: &Path,
    field: &str,
    value: serde_json::Value,
) -> Result<(), StoreError> {
    let manifest_path = latest_file(&index.join("generations"))?;
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).map_err(io_error)?)
            .map_err(|error| StoreError::CorruptStore(error.to_string()))?;
    manifest
        .as_object_mut()
        .ok_or_else(|| StoreError::CorruptStore("segment manifest is not an object".to_owned()))?
        .insert(field.to_owned(), value);
    let manifest_bytes =
        serde_json::to_vec(&manifest).map_err(|error| StoreError::StorageIo(error.to_string()))?;
    fs::write(&manifest_path, &manifest_bytes).map_err(io_error)?;

    let marker_path = latest_file(&index.join("commits"))?;
    let mut marker: serde_json::Value =
        serde_json::from_slice(&fs::read(&marker_path).map_err(io_error)?)
            .map_err(|error| StoreError::CorruptStore(error.to_string()))?;
    marker
        .as_object_mut()
        .ok_or_else(|| StoreError::CorruptStore("activation marker is not an object".to_owned()))?
        .insert(
            "manifest_digest".to_owned(),
            serde_json::Value::String(format!("{:x}", Sha256::digest(&manifest_bytes))),
        );
    fs::write(
        marker_path,
        serde_json::to_vec(&marker).map_err(|error| StoreError::StorageIo(error.to_string()))?,
    )
    .map_err(io_error)
}

fn quarantine(kind: BackendKind, root: &Path) -> Result<(), StoreError> {
    let codex = root.join(".godot/codex");
    match kind {
        BackendKind::Sqlite => {
            for suffix in ["", "-wal", "-shm"] {
                let source = codex.join(format!("index.sqlite{suffix}"));
                if source.exists() {
                    fs::rename(
                        &source,
                        codex.join(format!("index.sqlite.quarantine{suffix}")),
                    )
                    .map_err(io_error)?;
                }
            }
        }
        BackendKind::Segment => {
            fs::rename(codex.join("index"), codex.join("index.quarantine")).map_err(io_error)?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
struct PackageMetrics {
    binary_bytes: u64,
    dependencies: Vec<DependencyEvidence>,
    portable_single_binary: bool,
}

fn package_metrics(repo_root: &Path) -> Result<BTreeMap<String, PackageMetrics>, StoreError> {
    let manifest = repo_root.join("tests/codex/storage_spike/Cargo.toml");
    let rustc = command_output(repo_root, "rustc", &["--version", "--verbose"])
        .ok_or_else(|| StoreError::StorageIo("rustc host details unavailable".to_owned()))?;
    let host = rustc
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or_else(|| StoreError::StorageIo("rustc host triple missing".to_owned()))?;
    let package_root = TempDir::new().map_err(io_error)?;
    let mut result = BTreeMap::new();
    for feature in ["segment", "sqlite"] {
        if feature == "sqlite" && !cfg!(feature = "sqlite") {
            continue;
        }
        let target = package_root.path().join(feature);
        let status = Command::new("cargo")
            .args([
                "build",
                "--locked",
                "--release",
                "--no-default-features",
                "--features",
                feature,
                "--manifest-path",
                manifest.to_string_lossy().as_ref(),
                "--target-dir",
                target.to_string_lossy().as_ref(),
            ])
            .status()
            .map_err(io_error)?;
        if !status.success() {
            return Err(StoreError::StorageIo(format!(
                "feature-specific {feature} build failed"
            )));
        }
        let executable = target.join("release").join(if cfg!(windows) {
            "codex-storage-spike.exe"
        } else {
            "codex-storage-spike"
        });
        let bytes = fs::metadata(executable).map_err(io_error)?.len();
        let output = Command::new("cargo")
            .args([
                "metadata",
                "--format-version",
                "1",
                "--filter-platform",
                host,
                "--locked",
                "--no-default-features",
                "--features",
                feature,
                "--manifest-path",
                manifest.to_string_lossy().as_ref(),
            ])
            .output()
            .map_err(io_error)?;
        if !output.status.success() {
            return Err(StoreError::StorageIo(format!(
                "feature-specific {feature} cargo metadata failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| StoreError::StorageIo(error.to_string()))?;
        let resolved: BTreeSet<_> = metadata
            .pointer("/resolve/nodes")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| StoreError::StorageIo("cargo resolve nodes missing".to_owned()))?
            .iter()
            .filter_map(|node| node.get("id").and_then(serde_json::Value::as_str))
            .collect();
        let package_names: BTreeMap<_, _> = metadata
            .get("packages")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| StoreError::StorageIo("cargo packages missing".to_owned()))?
            .iter()
            .filter_map(|package| {
                Some((package.get("id")?.as_str()?, package.get("name")?.as_str()?))
            })
            .collect();
        let sqlite_bundled = metadata
            .pointer("/resolve/nodes")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .find(|node| {
                node.get("id")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|id| package_names.get(id))
                    .is_some_and(|name| *name == "libsqlite3-sys")
            })
            .and_then(|node| node.get("features"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|features| {
                features
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .any(|enabled| enabled == "bundled")
            });
        let mut dependencies = metadata
            .get("packages")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| StoreError::StorageIo("cargo packages missing".to_owned()))?
            .iter()
            .filter(|package| {
                package
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| resolved.contains(id))
            })
            .map(|package| DependencyEvidence {
                name: package
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
                version: package
                    .get("version")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
                source: package
                    .get("source")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("path")
                    .to_owned(),
                license: package
                    .get("license")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| {
                        package
                            .get("license_file")
                            .and_then(serde_json::Value::as_str)
                            .map(|path| {
                                let name = Path::new(path)
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or("declared");
                                format!("file:{name}")
                            })
                    }),
            })
            .collect::<Vec<_>>();
        dependencies.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.version.cmp(&right.version))
                .then_with(|| left.source.cmp(&right.source))
        });
        result.insert(
            feature.to_owned(),
            PackageMetrics {
                binary_bytes: bytes,
                dependencies,
                portable_single_binary: if feature == "sqlite" {
                    sqlite_bundled
                } else {
                    !resolved.iter().any(|id| {
                        package_names
                            .get(id)
                            .is_some_and(|name| name.contains("sqlite"))
                    })
                },
            },
        );
    }
    Ok(result)
}

fn backend_config(kind: BackendKind) -> BTreeMap<String, String> {
    match kind {
        BackendKind::Sqlite => BTreeMap::from([
            (
                "artifact".to_owned(),
                ".godot/codex/index.sqlite".to_owned(),
            ),
            ("foreign_keys".to_owned(), "ON".to_owned()),
            ("journal_mode".to_owned(), "WAL".to_owned()),
            ("lock".to_owned(), ".godot/codex/index.lock".to_owned()),
            ("physical_version".to_owned(), "2".to_owned()),
            ("sqlite_linkage".to_owned(), "bundled".to_owned()),
            ("synchronous".to_owned(), "FULL".to_owned()),
        ]),
        BackendKind::Segment => BTreeMap::from([
            ("artifact".to_owned(), ".godot/codex/index/".to_owned()),
            ("activation".to_owned(), "unique_commit_marker".to_owned()),
            ("content_address".to_owned(), "sha256".to_owned()),
            ("lock".to_owned(), ".godot/codex/index.lock".to_owned()),
            ("physical_version".to_owned(), "2".to_owned()),
            (
                "record_encoding".to_owned(),
                "length_prefixed_json".to_owned(),
            ),
            ("shards".to_owned(), "256".to_owned()),
        ]),
    }
}

fn assign_scores(backends: &mut [BackendEvidence]) {
    if backends.is_empty() {
        return;
    }
    let metrics: Vec<[f64; 6]> = backends
        .iter()
        .map(|backend| {
            [
                percentile(&backend.rename_ns, 50) as f64,
                backend.reverse_query_p95_ns as f64,
                percentile(&backend.full_build_ns, 50) as f64,
                backend.rename_write_amplification,
                backend.artifact_bytes as f64,
                (backend.binary_bytes + backend.dependency_count as u64 * 1024) as f64,
            ]
        })
        .collect();
    let weights = [0.30, 0.25, 0.20, 0.15, 0.05, 0.05];
    for (backend_index, backend) in backends.iter_mut().enumerate() {
        if !backend.qualified {
            backend.weighted_score = 0.0;
            continue;
        }
        backend.weighted_score = normalized_score(&metrics, backend_index, &weights);
    }
    if backends.len() < 2 {
        for backend in backends {
            backend.weighted_score_ci95_low = backend.weighted_score;
            backend.weighted_score_ci95_high = backend.weighted_score;
        }
        return;
    }

    let mut bootstrap_scores = vec![Vec::with_capacity(1_000); backends.len()];
    let mut random_state = 0xd05d_05d0_5d05_d05d_u64;
    for _ in 0..1_000 {
        let sampled: Vec<[f64; 6]> = backends
            .iter()
            .map(|backend| {
                [
                    bootstrap_percentile(&backend.rename_ns, 50, &mut random_state) as f64,
                    bootstrap_percentile(&backend.reverse_query_ns, 95, &mut random_state) as f64,
                    bootstrap_percentile(&backend.full_build_ns, 50, &mut random_state) as f64,
                    backend.rename_write_amplification,
                    backend.artifact_bytes as f64,
                    (backend.binary_bytes + backend.dependency_count as u64 * 1024) as f64,
                ]
            })
            .collect();
        for (backend_index, backend) in backends.iter().enumerate() {
            let score = if backend.qualified {
                normalized_score(&sampled, backend_index, &weights)
            } else {
                0.0
            };
            bootstrap_scores[backend_index].push(score);
        }
    }
    for (backend, mut scores) in backends.iter_mut().zip(bootstrap_scores) {
        scores.sort_by(f64::total_cmp);
        backend.weighted_score_ci95_low = scores[24];
        backend.weighted_score_ci95_high = scores[974];
    }
}

fn normalized_score(metrics: &[[f64; 6]], backend_index: usize, weights: &[f64; 6]) -> f64 {
    (0..weights.len())
        .map(|metric| {
            let best = metrics
                .iter()
                .map(|values| values[metric])
                .filter(|value| *value > 0.0)
                .fold(f64::INFINITY, f64::min);
            let own = metrics[backend_index][metric];
            if own > 0.0 && best.is_finite() {
                weights[metric] * best / own
            } else {
                0.0
            }
        })
        .sum()
}

fn bootstrap_percentile(samples: &[u64], target: usize, state: &mut u64) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut resampled = Vec::with_capacity(samples.len());
    for _ in 0..samples.len() {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        let index = (*state as usize) % samples.len();
        resampled.push(samples[index]);
    }
    percentile(&resampled, target)
}

fn choose_backend(backends: &[BackendEvidence]) -> (String, String) {
    let qualified: Vec<_> = backends
        .iter()
        .filter(|backend| backend.qualified)
        .collect();
    match qualified.as_slice() {
        [] => (
            "blocked".to_owned(),
            "No backend passed every correctness, recovery, portability, and SLO gate.".to_owned(),
        ),
        [only] => (
            only.backend.as_str().to_owned(),
            format!(
                "Only {} passed every disqualifying gate.",
                only.backend.as_str()
            ),
        ),
        _ => {
            let sqlite = qualified
                .iter()
                .find(|backend| backend.backend == BackendKind::Sqlite);
            let segment = qualified
                .iter()
                .find(|backend| backend.backend == BackendKind::Segment);
            let best = qualified
                .iter()
                .max_by(|left, right| left.weighted_score.total_cmp(&right.weighted_score))
                .expect("qualified is non-empty");
            let tie = sqlite.zip(segment).is_some_and(|(sqlite, segment)| {
                let intervals_overlap = sqlite.weighted_score_ci95_low
                    <= segment.weighted_score_ci95_high
                    && segment.weighted_score_ci95_low <= sqlite.weighted_score_ci95_high;
                (sqlite.weighted_score - segment.weighted_score).abs() < 0.05 || intervals_overlap
            });
            if tie {
                return (
                    "sqlite".to_owned(),
                    "Both backends qualified and the weighted difference was below five points or bootstrap intervals overlapped; D-05 tie-break selects bundled SQLite to minimize custom transaction/recovery surface."
                        .to_owned(),
                );
            }
            (
                best.backend.as_str().to_owned(),
                format!(
                    "Both backends qualified; {} had the higher weighted score ({:.4}).",
                    best.backend.as_str(),
                    best.weighted_score
                ),
            )
        }
    }
}

fn gate<F>(gates: &mut BTreeMap<String, bool>, errors: &mut Vec<String>, name: &str, operation: F)
where
    F: FnOnce() -> Result<(), StoreError>,
{
    match operation() {
        Ok(()) => {
            gates.insert(name.to_owned(), true);
        }
        Err(error) => {
            gates.insert(name.to_owned(), false);
            errors.push(format!("{name}: {error}"));
        }
    }
}

fn matrix_group_passed(matrix: &BTreeMap<String, bool>, prefix: &str) -> bool {
    let selected: Vec<_> = matrix
        .iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(_, passed)| *passed)
        .collect();
    !selected.is_empty() && selected.into_iter().all(|passed| passed)
}

fn block_map(root: &Path) -> Result<BTreeMap<PathBuf, Vec<[u8; 32]>>, StoreError> {
    let mut result = BTreeMap::new();
    visit_files(root, root, &mut |relative, path| {
        if relative.ends_with("index.lock") {
            return Ok(());
        }
        let bytes = fs::read(path).map_err(io_error)?;
        result.insert(
            relative.to_path_buf(),
            bytes
                .chunks(4096)
                .map(|chunk| Sha256::digest(chunk).into())
                .collect(),
        );
        Ok(())
    })?;
    Ok(result)
}

fn changed_blocks(
    before: &BTreeMap<PathBuf, Vec<[u8; 32]>>,
    after: &BTreeMap<PathBuf, Vec<[u8; 32]>>,
) -> u64 {
    let paths: BTreeSet<_> = before.keys().chain(after.keys()).collect();
    paths
        .into_iter()
        .map(|path| {
            let left = before.get(path).map(Vec::as_slice).unwrap_or_default();
            let right = after.get(path).map(Vec::as_slice).unwrap_or_default();
            (0..left.len().max(right.len()))
                .filter(|index| left.get(*index) != right.get(*index))
                .count() as u64
        })
        .sum()
}

fn tree_bytes(root: &Path) -> Result<u64, StoreError> {
    let mut bytes = 0_u64;
    visit_files(root, root, &mut |_, path| {
        bytes = bytes.saturating_add(fs::metadata(path).map_err(io_error)?.len());
        Ok(())
    })?;
    Ok(bytes)
}

fn visit_files<F>(root: &Path, current: &Path, operation: &mut F) -> Result<(), StoreError>
where
    F: FnMut(&Path, &Path) -> Result<(), StoreError>,
{
    for entry in fs::read_dir(current).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let path = entry.path();
        if entry.file_type().map_err(io_error)?.is_dir() {
            visit_files(root, &path, operation)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| StoreError::StorageIo(error.to_string()))?;
            operation(relative, &path)?;
        }
    }
    Ok(())
}

fn latest_file(directory: &Path) -> Result<PathBuf, StoreError> {
    let mut files: Vec<_> = fs::read_dir(directory)
        .map_err(io_error)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .map(|entry| entry.path())
        .collect();
    files.sort();
    files.pop().ok_or(StoreError::NotReady)
}

fn flip_byte(path: &Path) -> Result<(), StoreError> {
    let mut bytes = fs::read(path).map_err(io_error)?;
    let index = bytes.len() / 2;
    let byte = bytes
        .get_mut(index)
        .ok_or_else(|| StoreError::CorruptStore("empty corruption target".to_owned()))?;
    *byte ^= 0x5a;
    fs::write(path, bytes).map_err(io_error)
}

fn write_evidence(path: &Path, evidence: &StorageSpikeEvidence) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let bytes = serde_json::to_vec_pretty(evidence)
        .map_err(|error| StoreError::StorageIo(error.to_string()))?;
    let temp = path.with_extension("tmp");
    fs::write(&temp, bytes).map_err(io_error)?;
    fs::rename(temp, path).map_err(io_error)
}

fn command_output(root: &Path, command: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(command)
        .args(arguments)
        .current_dir(root)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

const SOURCE_SCOPES: [&str; 5] = [
    "godot-codex-mcp/Cargo.toml",
    "godot-codex-mcp/Cargo.lock",
    "godot-codex-mcp/crates/index-store",
    "tests/codex/storage_spike",
    "tests/codex/fixtures/resource_graph_oracle/golden-resource-graph.json",
];

fn relevant_git_dirty(root: &Path) -> Result<bool, StoreError> {
    let output = Command::new("git")
        .arg("status")
        .arg("--porcelain")
        .arg("--")
        .args(SOURCE_SCOPES)
        .current_dir(root)
        .output()
        .map_err(io_error)?;
    if !output.status.success() {
        return Err(StoreError::StorageIo(
            "git status failed for source evidence".to_owned(),
        ));
    }
    Ok(!output.stdout.is_empty())
}

fn source_tree_sha256(root: &Path) -> Result<String, StoreError> {
    let output = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
        ])
        .args(SOURCE_SCOPES)
        .current_dir(root)
        .output()
        .map_err(io_error)?;
    if !output.status.success() {
        return Err(StoreError::StorageIo(
            "git ls-files failed for source evidence".to_owned(),
        ));
    }
    let mut paths: Vec<_> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            String::from_utf8(path.to_vec())
                .map_err(|error| StoreError::StorageIo(error.to_string()))
        })
        .collect::<Result<_, _>>()?;
    paths.sort();
    let mut digest = Sha256::new();
    for relative in paths {
        let bytes = fs::read(root.join(&relative)).map_err(io_error)?;
        digest.update((relative.len() as u64).to_be_bytes());
        digest.update(relative.as_bytes());
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn fault_name(point: FaultPoint) -> &'static str {
    match point {
        FaultPoint::Capture => "capture",
        FaultPoint::Staging => "staging",
        FaultPoint::PreCommit => "pre_commit",
        FaultPoint::PostCommit => "post_commit",
    }
}

fn elapsed_ns(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn io_error(error: std::io::Error) -> StoreError {
    StoreError::StorageIo(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision_backend(kind: BackendKind) -> BackendEvidence {
        let gates = [
            "cached_query_p95_lte_300ms",
            "canonical_oracle",
            "concurrent_read_activation",
            "corrupt_cache_isolation",
            "direct_reverse_parity",
            "graceful_cancellation",
            "process_crash_recovery",
            "process_locking_reopen",
            "rename_p95_lte_2s",
            "single_binary_packaging",
            "stress_large_reopen",
            "v1_v2_migration",
        ]
        .into_iter()
        .map(|name| (name.to_owned(), true))
        .collect();
        let fault_matrix = [
            "graceful_cancel.capture",
            "graceful_cancel.staging",
            "graceful_cancel.pre_commit",
            "graceful_cancel.post_commit",
            "hard_kill.capture",
            "hard_kill.staging",
            "hard_kill.pre_commit",
            "hard_kill.post_commit",
            "corruption.metadata",
            "corruption.resource",
            "corruption.reverse",
            "corruption.project_binding",
            "corruption.incompatible_schema",
        ]
        .into_iter()
        .map(|name| (name.to_owned(), true))
        .collect();
        BackendEvidence {
            backend: kind,
            config: backend_config(kind),
            qualified: true,
            gates,
            fault_matrix,
            full_build_ns: vec![100; 5],
            rename_ns: vec![100; 50],
            reverse_query_ns: vec![100; 10_000],
            reverse_query_p50_ns: 100,
            reverse_query_p95_ns: 100,
            artifact_bytes: 100,
            rename_changed_blocks: 1,
            rename_normalized_bytes: 4_096,
            rename_write_amplification: 1.0,
            binary_bytes: 100,
            dependency_count: 1,
            dependencies: vec![DependencyEvidence {
                name: "fixture".to_owned(),
                version: "1.0.0".to_owned(),
                source: "path".to_owned(),
                license: Some("MIT".to_owned()),
            }],
            weighted_score: 0.0,
            weighted_score_ci95_low: 0.0,
            weighted_score_ci95_high: 0.0,
            errors: Vec::new(),
        }
    }

    fn decision_run(os: &str) -> StorageSpikeEvidence {
        let mut backends = vec![
            decision_backend(BackendKind::Sqlite),
            decision_backend(BackendKind::Segment),
        ];
        assign_scores(&mut backends);
        let (chosen_backend, decision_reason) = choose_backend(&backends);
        StorageSpikeEvidence {
            schema_version: 2,
            decision: "D-05".to_owned(),
            profile: "decision".to_owned(),
            seed: SYNTHETIC_SEED.to_owned(),
            git_commit: "fixture-commit".to_owned(),
            git_dirty: false,
            source_tree_sha256: "sha256:fixture-source".to_owned(),
            rustc: "rustc 1.94.1 (fixture)".to_owned(),
            os: os.to_owned(),
            architecture: "fixture".to_owned(),
            host: HostEvidence {
                runner: "fixture".to_owned(),
                logical_cpus: 1,
            },
            oracle_sha256: "sha256:fixture-oracle".to_owned(),
            dataset: DatasetEvidence {
                resources: 10_000,
                edges: 50_000,
                full_build_iterations: 5,
                rename_iterations: 50,
                query_warmup: 1_000,
                query_iterations: 10_000,
                stress_resources: 100_000,
                stress_edges: 500_000,
                fan_in_targets: vec![0, 1, 10, 100, 1_000],
            },
            backends,
            chosen_backend,
            decision_reason,
        }
    }

    #[test]
    fn decision_profile_rejects_quick_or_tampered_evidence() {
        let valid = decision_run("macos");
        assert!(decision_profile_valid(&valid));

        let mut quick = valid.clone();
        quick.profile = "quick".to_owned();
        assert!(!decision_profile_valid(&quick));

        let mut tampered = valid;
        tampered.backends[0].weighted_score = 0.123;
        assert!(!decision_profile_valid(&tampered));
    }

    #[test]
    fn merge_requires_three_valid_profiles_and_applies_sqlite_tie_break() {
        let temp = TempDir::new().expect("temp");
        let mut inputs = Vec::new();
        for os in ["linux", "macos", "windows"] {
            let path = temp.path().join(format!("{os}.json"));
            fs::write(
                &path,
                serde_json::to_vec(&decision_run(os)).expect("serialize evidence"),
            )
            .expect("write evidence");
            inputs.push(path);
        }
        let combined = merge_platform_evidence(&temp.path().join("combined.json"), &inputs)
            .expect("merge evidence");
        assert!(combined.cross_platform_complete);
        assert_eq!(combined.chosen_backend, "sqlite");
        assert_eq!(combined.backend_summaries.len(), 2);

        let quick_path = &inputs[0];
        let mut quick = decision_run("linux");
        quick.profile = "quick".to_owned();
        fs::write(
            quick_path,
            serde_json::to_vec(&quick).expect("serialize quick evidence"),
        )
        .expect("write quick evidence");
        let blocked = merge_platform_evidence(&temp.path().join("blocked.json"), &inputs)
            .expect("merge blocked evidence");
        assert!(!blocked.cross_platform_complete);
        assert_eq!(blocked.chosen_backend, "blocked");
    }

    #[test]
    fn corruption_matrix_distinguishes_binding_and_schema_failures() {
        for kind in [BackendKind::Sqlite, BackendKind::Segment] {
            for corruption in [
                "metadata",
                "resource",
                "reverse",
                "project_binding",
                "incompatible_schema",
            ] {
                corruption_case(kind, corruption)
                    .unwrap_or_else(|error| panic!("{kind:?} {corruption} matrix failed: {error}"));
            }
        }
    }
}
