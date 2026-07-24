use std::collections::{BTreeMap, BTreeSet, VecDeque};

use getrandom::fill as random_fill;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_REPORT_PAGE_BYTES: usize = 65_536;
pub const MAX_REPORT_PAGES: usize = 4;
pub const MAX_RETAINED_REPORT_BYTES: usize = 262_144;
pub const MAX_REPORTS: usize = 64;
pub const MAX_CHECKS: usize = 16;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationReportOutcome {
    Passed,
    Failed,
    Inconclusive,
    TimedOut,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationCheck {
    Intrinsic,
    Persistence,
    ReloadReparse,
    IndexConvergence,
    SemanticGraph,
    Diagnostics,
    Runtime,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckAuthority {
    Required,
    Optional,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    Pending,
    Passed,
    Failed,
    Inconclusive,
    TimedOut,
    Skipped,
    Stale,
    Truncated,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationPolicy {
    pub rollback: String,
    pub warnings: String,
    pub runtime: String,
}

impl ValidationPolicy {
    fn valid(&self) -> bool {
        matches!(
            self.rollback.as_str(),
            "never" | "on_required_failure" | "on_any_failure"
        ) && matches!(self.warnings.as_str(), "allow" | "fail_on_introduced")
            && matches!(
                self.runtime.as_str(),
                "skip" | "run_current_scene" | "run_project"
            )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRelation {
    pub source: String,
    pub predicate: String,
    pub target: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticSnapshot {
    pub entities: BTreeMap<String, Value>,
    pub relations: BTreeSet<SemanticRelation>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedSemanticDelta {
    pub affected_closure: BTreeSet<String>,
    pub added_entities: BTreeSet<String>,
    pub removed_entities: BTreeSet<String>,
    pub changed_entities: BTreeSet<String>,
    pub added_relations: BTreeSet<SemanticRelation>,
    pub removed_relations: BTreeSet<SemanticRelation>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticComparison {
    pub affected_closure: BTreeSet<String>,
    pub expected_added_entities: BTreeSet<String>,
    pub expected_removed_entities: BTreeSet<String>,
    pub expected_changed_entities: BTreeSet<String>,
    pub unexpected_added_entities: BTreeSet<String>,
    pub unexpected_removed_entities: BTreeSet<String>,
    pub unexpected_changed_entities: BTreeSet<String>,
    pub unexpected_added_relations: BTreeSet<SemanticRelation>,
    pub unexpected_removed_relations: BTreeSet<SemanticRelation>,
    pub matched: bool,
}

impl SemanticComparison {
    #[must_use]
    pub fn compare(
        baseline: &SemanticSnapshot,
        postimage: &SemanticSnapshot,
        expected: &ExpectedSemanticDelta,
    ) -> Self {
        let baseline_ids: BTreeSet<_> = baseline.entities.keys().cloned().collect();
        let postimage_ids: BTreeSet<_> = postimage.entities.keys().cloned().collect();
        let added: BTreeSet<_> = postimage_ids.difference(&baseline_ids).cloned().collect();
        let removed: BTreeSet<_> = baseline_ids.difference(&postimage_ids).cloned().collect();
        let changed: BTreeSet<_> = baseline_ids
            .intersection(&postimage_ids)
            .filter(|entity| baseline.entities.get(*entity) != postimage.entities.get(*entity))
            .cloned()
            .collect();
        let relation_added: BTreeSet<_> = postimage
            .relations
            .difference(&baseline.relations)
            .cloned()
            .collect();
        let relation_removed: BTreeSet<_> = baseline
            .relations
            .difference(&postimage.relations)
            .cloned()
            .collect();

        let unexpected_added_entities = added
            .difference(&expected.added_entities)
            .cloned()
            .collect();
        let unexpected_removed_entities = removed
            .difference(&expected.removed_entities)
            .cloned()
            .collect();
        let unexpected_changed_entities = changed
            .difference(&expected.changed_entities)
            .cloned()
            .collect();
        let unexpected_added_relations = relation_added
            .difference(&expected.added_relations)
            .cloned()
            .collect();
        let unexpected_removed_relations = relation_removed
            .difference(&expected.removed_relations)
            .cloned()
            .collect();
        let matched = added == expected.added_entities
            && removed == expected.removed_entities
            && changed == expected.changed_entities
            && relation_added == expected.added_relations
            && relation_removed == expected.removed_relations
            && added
                .iter()
                .chain(removed.iter())
                .chain(changed.iter())
                .all(|entity| expected.affected_closure.contains(entity));

        Self {
            affected_closure: expected.affected_closure.clone(),
            expected_added_entities: expected.added_entities.clone(),
            expected_removed_entities: expected.removed_entities.clone(),
            expected_changed_entities: expected.changed_entities.clone(),
            unexpected_added_entities,
            unexpected_removed_entities,
            unexpected_changed_entities,
            unexpected_added_relations,
            unexpected_removed_relations,
            matched,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticFingerprint {
    pub id: String,
    pub severity: DiagnosticSeverity,
    pub source: String,
    pub entity_id: Option<String>,
    pub message_digest: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticSummary {
    pub pre_existing: BTreeSet<DiagnosticFingerprint>,
    pub resolved: BTreeSet<DiagnosticFingerprint>,
    pub introduced: BTreeSet<DiagnosticFingerprint>,
    pub introduced_errors: usize,
    pub introduced_warnings: usize,
}

impl DiagnosticSummary {
    #[must_use]
    pub fn classify(
        baseline: &BTreeSet<DiagnosticFingerprint>,
        postimage: &BTreeSet<DiagnosticFingerprint>,
    ) -> Self {
        let pre_existing = baseline.intersection(postimage).cloned().collect();
        let resolved = baseline.difference(postimage).cloned().collect();
        let introduced: BTreeSet<_> = postimage.difference(baseline).cloned().collect();
        let introduced_errors = introduced
            .iter()
            .filter(|item| item.severity == DiagnosticSeverity::Error)
            .count();
        let introduced_warnings = introduced
            .iter()
            .filter(|item| item.severity == DiagnosticSeverity::Warning)
            .count();
        Self {
            pre_existing,
            resolved,
            introduced,
            introduced_errors,
            introduced_warnings,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckRecord {
    pub check: ValidationCheck,
    pub authority: CheckAuthority,
    pub outcome: CheckOutcome,
    pub started_at_ms: u64,
    pub completed_at_ms: Option<u64>,
    pub summary: String,
    pub evidence_digest: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationReport {
    pub report_id: String,
    pub report_digest: String,
    pub change_set_id: String,
    pub preview_digest: String,
    pub outcome: ValidationReportOutcome,
    pub policy: ValidationPolicy,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub checks: Vec<CheckRecord>,
    pub semantic: Option<SemanticComparison>,
    pub diagnostics: Option<DiagnosticSummary>,
    pub runtime_session_id: Option<String>,
    pub affected_closure_only: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReportPage {
    pub report_id: String,
    pub report_digest: String,
    pub page: usize,
    pub page_count: usize,
    pub content: String,
    pub content_bytes: usize,
}

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("validation policy is invalid")]
    InvalidPolicy,
    #[error("validation check is invalid")]
    InvalidCheck,
    #[error("validation is incomplete")]
    Incomplete,
    #[error("validation report exceeds the retained limit")]
    ReportTooLarge,
    #[error("validation report was not found")]
    NotFound,
    #[error("validation report page is invalid")]
    InvalidPage,
    #[error("validation identity generation failed")]
    Identity,
    #[error("validation serialization failed")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Clone, Debug)]
struct ActiveValidation {
    change_set_id: String,
    preview_digest: String,
    policy: ValidationPolicy,
    started_at_ms: u64,
    deadline_ms: u64,
    checks: BTreeMap<ValidationCheck, CheckRecord>,
    semantic: Option<SemanticComparison>,
    diagnostics: Option<DiagnosticSummary>,
    runtime_session_id: Option<String>,
}

#[derive(Default)]
pub struct ValidationCoordinator {
    active: BTreeMap<String, ActiveValidation>,
    reports: BTreeMap<String, (ValidationReport, Vec<u8>)>,
    order: VecDeque<String>,
}

fn random_id(prefix: &str) -> Result<String, ValidationError> {
    let mut bytes = [0_u8; 16];
    random_fill(&mut bytes).map_err(|_| ValidationError::Identity)?;
    let mut hex = String::with_capacity(32);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").map_err(|_| ValidationError::Identity)?;
    }
    Ok(format!("{prefix}:{hex}"))
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

impl ValidationCoordinator {
    pub fn begin(
        &mut self,
        change_set_id: &str,
        preview_digest: &str,
        policy: ValidationPolicy,
        started_at_ms: u64,
        deadline_ms: u64,
        checks: &[(ValidationCheck, CheckAuthority)],
    ) -> Result<(), ValidationError> {
        if !policy.valid()
            || checks.is_empty()
            || checks.len() > MAX_CHECKS
            || deadline_ms <= started_at_ms
            || self.active.contains_key(change_set_id)
        {
            return Err(ValidationError::InvalidPolicy);
        }
        let mut records = BTreeMap::new();
        for (check, authority) in checks {
            if records
                .insert(
                    *check,
                    CheckRecord {
                        check: *check,
                        authority: *authority,
                        outcome: CheckOutcome::Pending,
                        started_at_ms,
                        completed_at_ms: None,
                        summary: "pending".to_owned(),
                        evidence_digest: None,
                    },
                )
                .is_some()
            {
                return Err(ValidationError::InvalidCheck);
            }
        }
        self.active.insert(
            change_set_id.to_owned(),
            ActiveValidation {
                change_set_id: change_set_id.to_owned(),
                preview_digest: preview_digest.to_owned(),
                policy,
                started_at_ms,
                deadline_ms,
                checks: records,
                semantic: None,
                diagnostics: None,
                runtime_session_id: None,
            },
        );
        Ok(())
    }

    pub fn complete_check(
        &mut self,
        change_set_id: &str,
        check: ValidationCheck,
        outcome: CheckOutcome,
        summary: &str,
        evidence: Option<&Value>,
        completed_at_ms: u64,
    ) -> Result<(), ValidationError> {
        if matches!(outcome, CheckOutcome::Pending) || summary.len() > 512 {
            return Err(ValidationError::InvalidCheck);
        }
        let active = self
            .active
            .get_mut(change_set_id)
            .ok_or(ValidationError::NotFound)?;
        let record = active
            .checks
            .get_mut(&check)
            .ok_or(ValidationError::InvalidCheck)?;
        if record.outcome != CheckOutcome::Pending || completed_at_ms < record.started_at_ms {
            return Err(ValidationError::InvalidCheck);
        }
        record.outcome = outcome;
        record.completed_at_ms = Some(completed_at_ms);
        record.summary = summary.to_owned();
        record.evidence_digest = evidence
            .map(serde_json::to_vec)
            .transpose()?
            .map(|bytes| digest(&bytes));
        Ok(())
    }

    pub fn set_semantic(
        &mut self,
        change_set_id: &str,
        comparison: SemanticComparison,
    ) -> Result<(), ValidationError> {
        self.active
            .get_mut(change_set_id)
            .ok_or(ValidationError::NotFound)?
            .semantic = Some(comparison);
        Ok(())
    }

    pub fn set_diagnostics(
        &mut self,
        change_set_id: &str,
        diagnostics: DiagnosticSummary,
    ) -> Result<(), ValidationError> {
        self.active
            .get_mut(change_set_id)
            .ok_or(ValidationError::NotFound)?
            .diagnostics = Some(diagnostics);
        Ok(())
    }

    pub fn set_runtime_session(
        &mut self,
        change_set_id: &str,
        runtime_session_id: &str,
    ) -> Result<(), ValidationError> {
        if !runtime_session_id.starts_with("runtime-session:") {
            return Err(ValidationError::InvalidCheck);
        }
        self.active
            .get_mut(change_set_id)
            .ok_or(ValidationError::NotFound)?
            .runtime_session_id = Some(runtime_session_id.to_owned());
        Ok(())
    }

    pub fn finalize(
        &mut self,
        change_set_id: &str,
        completed_at_ms: u64,
    ) -> Result<ValidationReport, ValidationError> {
        let mut active = self
            .active
            .remove(change_set_id)
            .ok_or(ValidationError::NotFound)?;
        if completed_at_ms >= active.deadline_ms {
            for record in active.checks.values_mut() {
                if record.outcome == CheckOutcome::Pending {
                    record.outcome = CheckOutcome::TimedOut;
                    record.completed_at_ms = Some(completed_at_ms);
                    record.summary = "deadline elapsed".to_owned();
                }
            }
        } else if active
            .checks
            .values()
            .any(|record| record.outcome == CheckOutcome::Pending)
        {
            self.active.insert(change_set_id.to_owned(), active);
            return Err(ValidationError::Incomplete);
        }

        let diagnostics_fail = active.diagnostics.as_ref().is_some_and(|diagnostics| {
            diagnostics.introduced_errors > 0
                || (active.policy.warnings == "fail_on_introduced"
                    && diagnostics.introduced_warnings > 0)
        });
        let semantic_fail = active
            .semantic
            .as_ref()
            .is_some_and(|comparison| !comparison.matched);
        let required_failed = active.checks.values().any(|record| {
            record.authority == CheckAuthority::Required && record.outcome == CheckOutcome::Failed
        });
        let timed_out = active
            .checks
            .values()
            .any(|record| record.outcome == CheckOutcome::TimedOut);
        let required_inconclusive = active.checks.values().any(|record| {
            record.authority == CheckAuthority::Required
                && matches!(
                    record.outcome,
                    CheckOutcome::Inconclusive
                        | CheckOutcome::Skipped
                        | CheckOutcome::Stale
                        | CheckOutcome::Truncated
                )
        });
        let outcome = if required_failed || diagnostics_fail || semantic_fail {
            ValidationReportOutcome::Failed
        } else if timed_out {
            ValidationReportOutcome::TimedOut
        } else if required_inconclusive {
            ValidationReportOutcome::Inconclusive
        } else {
            ValidationReportOutcome::Passed
        };
        let report_id = random_id("validation-report")?;
        let mut report = ValidationReport {
            report_id,
            report_digest: String::new(),
            change_set_id: active.change_set_id,
            preview_digest: active.preview_digest,
            outcome,
            policy: active.policy,
            started_at_ms: active.started_at_ms,
            completed_at_ms,
            checks: active.checks.into_values().collect(),
            semantic: active.semantic,
            diagnostics: active.diagnostics,
            runtime_session_id: active.runtime_session_id,
            affected_closure_only: true,
        };
        let identity_bytes = serde_json::to_vec(&report)?;
        report.report_digest = digest(&identity_bytes);
        let encoded = serde_json::to_vec(&report)?;
        if encoded.len() > MAX_RETAINED_REPORT_BYTES {
            return Err(ValidationError::ReportTooLarge);
        }
        self.order.push_back(report.report_id.clone());
        self.reports
            .insert(report.report_id.clone(), (report.clone(), encoded));
        while self.order.len() > MAX_REPORTS {
            if let Some(expired) = self.order.pop_front() {
                self.reports.remove(&expired);
            }
        }
        Ok(report)
    }

    pub fn page(&self, report_id: &str, page: usize) -> Result<ReportPage, ValidationError> {
        let (report, encoded) = self
            .reports
            .get(report_id)
            .ok_or(ValidationError::NotFound)?;
        let mut ranges = Vec::new();
        let mut cursor = 0;
        while cursor < encoded.len() {
            let mut end = (cursor + MAX_REPORT_PAGE_BYTES).min(encoded.len());
            while end > cursor && std::str::from_utf8(&encoded[cursor..end]).is_err() {
                end -= 1;
            }
            if end == cursor {
                return Err(ValidationError::InvalidPage);
            }
            ranges.push((cursor, end));
            cursor = end;
        }
        let page_count = ranges.len();
        if page >= page_count || page_count > MAX_REPORT_PAGES {
            return Err(ValidationError::InvalidPage);
        }
        let (start, end) = ranges[page];
        let content = std::str::from_utf8(&encoded[start..end])
            .map_err(|_| ValidationError::InvalidPage)?
            .to_owned();
        Ok(ReportPage {
            report_id: report.report_id.clone(),
            report_digest: report.report_digest.clone(),
            page,
            page_count,
            content_bytes: content.len(),
            content,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> ValidationPolicy {
        ValidationPolicy {
            rollback: "on_required_failure".to_owned(),
            warnings: "fail_on_introduced".to_owned(),
            runtime: "skip".to_owned(),
        }
    }

    #[test]
    fn required_skipped_stale_and_truncated_are_inconclusive() {
        for check_outcome in [
            CheckOutcome::Skipped,
            CheckOutcome::Stale,
            CheckOutcome::Truncated,
            CheckOutcome::Inconclusive,
        ] {
            let mut coordinator = ValidationCoordinator::default();
            coordinator
                .begin(
                    "change-set:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    policy(),
                    10,
                    100,
                    &[(ValidationCheck::IndexConvergence, CheckAuthority::Required)],
                )
                .unwrap();
            coordinator
                .complete_check(
                    "change-set:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    ValidationCheck::IndexConvergence,
                    check_outcome,
                    "bounded result",
                    None,
                    20,
                )
                .unwrap();
            let report = coordinator
                .finalize("change-set:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 30)
                .unwrap();
            assert_eq!(report.outcome, ValidationReportOutcome::Inconclusive);
        }
    }

    #[test]
    fn semantic_comparison_reports_unexpected_deltas_only_inside_closure() {
        let baseline = SemanticSnapshot {
            entities: BTreeMap::from([
                ("node:a".to_owned(), Value::String("old".to_owned())),
                ("node:b".to_owned(), Value::Bool(true)),
            ]),
            relations: BTreeSet::new(),
        };
        let postimage = SemanticSnapshot {
            entities: BTreeMap::from([
                ("node:a".to_owned(), Value::String("new".to_owned())),
                ("node:b".to_owned(), Value::Bool(true)),
                ("node:c".to_owned(), Value::Bool(true)),
            ]),
            relations: BTreeSet::new(),
        };
        let expected = ExpectedSemanticDelta {
            affected_closure: BTreeSet::from(["node:a".to_owned()]),
            changed_entities: BTreeSet::from(["node:a".to_owned()]),
            ..ExpectedSemanticDelta::default()
        };
        let comparison = SemanticComparison::compare(&baseline, &postimage, &expected);
        assert!(!comparison.matched);
        assert_eq!(
            comparison.unexpected_added_entities,
            BTreeSet::from(["node:c".to_owned()])
        );
        assert_eq!(comparison.affected_closure, expected.affected_closure);
    }

    #[test]
    fn introduced_errors_override_passing_checks_and_report_pages_are_bounded() {
        let baseline = BTreeSet::new();
        let diagnostic = DiagnosticFingerprint {
            id: "diagnostic:intentional".to_owned(),
            severity: DiagnosticSeverity::Error,
            source: "gdscript".to_owned(),
            entity_id: Some("script:fixture".to_owned()),
            message_digest:
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
        };
        let summary = DiagnosticSummary::classify(&baseline, &BTreeSet::from([diagnostic.clone()]));
        assert_eq!(summary.introduced_errors, 1);
        assert!(!serde_json::to_string(&summary).unwrap().contains("stack"));

        let mut coordinator = ValidationCoordinator::default();
        let id = "change-set:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        coordinator
            .begin(
                id,
                "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                policy(),
                10,
                100,
                &[(ValidationCheck::Diagnostics, CheckAuthority::Required)],
            )
            .unwrap();
        coordinator
            .complete_check(
                id,
                ValidationCheck::Diagnostics,
                CheckOutcome::Passed,
                "capture complete",
                None,
                20,
            )
            .unwrap();
        coordinator.set_diagnostics(id, summary).unwrap();
        let report = coordinator.finalize(id, 30).unwrap();
        assert_eq!(report.outcome, ValidationReportOutcome::Failed);
        let page = coordinator.page(&report.report_id, 0).unwrap();
        assert!(page.content_bytes <= MAX_REPORT_PAGE_BYTES);
        assert!(page.page_count <= MAX_REPORT_PAGES);
        assert_eq!(page.report_digest, report.report_digest);
    }

    #[test]
    fn pending_required_check_times_out_honestly() {
        let mut coordinator = ValidationCoordinator::default();
        let id = "change-set:cccccccccccccccccccccccccccccccc";
        coordinator
            .begin(
                id,
                "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                policy(),
                10,
                100,
                &[(ValidationCheck::Runtime, CheckAuthority::Required)],
            )
            .unwrap();
        let report = coordinator.finalize(id, 100).unwrap();
        assert_eq!(report.outcome, ValidationReportOutcome::TimedOut);
        assert_eq!(report.checks[0].outcome, CheckOutcome::TimedOut);
    }
}
