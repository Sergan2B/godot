use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::validation::{CheckOutcome, ValidationReportOutcome};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackPolicy {
    Never,
    OnRequiredFailure,
    OnAnyFailure,
}

impl RollbackPolicy {
    #[must_use]
    pub fn should_rollback(self, report: ValidationReportOutcome, optional_failure: bool) -> bool {
        match self {
            Self::Never => false,
            Self::OnRequiredFailure => matches!(
                report,
                ValidationReportOutcome::Failed
                    | ValidationReportOutcome::Inconclusive
                    | ValidationReportOutcome::TimedOut
            ),
            Self::OnAnyFailure => report != ValidationReportOutcome::Passed || optional_failure,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RollbackEvidence {
    pub change_set_id: String,
    pub expected_native_action_tag_digest: String,
    pub newest_native_action_tag_digest: Option<String>,
    pub expected_postimage_hashes: BTreeMap<String, String>,
    pub observed_postimage_hashes: BTreeMap<String, String>,
    pub native_history_known: bool,
    pub persistence_state_known: bool,
    pub validation_report_digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "decision", deny_unknown_fields)]
pub enum RollbackDecision {
    Allowed {
        evidence_digest: String,
    },
    Blocked {
        reason: String,
        conflicting_targets: BTreeSet<String>,
    },
    InDoubt {
        reason: String,
    },
}

impl RollbackEvidence {
    #[must_use]
    pub fn evaluate(&self) -> RollbackDecision {
        if !self.native_history_known || !self.persistence_state_known {
            return RollbackDecision::InDoubt {
                reason: "reconciliation_required".to_owned(),
            };
        }
        if self.newest_native_action_tag_digest.as_deref()
            != Some(self.expected_native_action_tag_digest.as_str())
        {
            return RollbackDecision::Blocked {
                reason: "intervening_native_action".to_owned(),
                conflicting_targets: BTreeSet::new(),
            };
        }
        let conflicts = self
            .expected_postimage_hashes
            .iter()
            .filter(|(target, expected)| {
                self.observed_postimage_hashes.get(*target) != Some(*expected)
            })
            .map(|(target, _)| target.clone())
            .collect::<BTreeSet<_>>();
        if !conflicts.is_empty()
            || self.observed_postimage_hashes.len() != self.expected_postimage_hashes.len()
        {
            return RollbackDecision::Blocked {
                reason: "postimage_hash_mismatch".to_owned(),
                conflicting_targets: conflicts,
            };
        }
        let encoded = serde_json::to_vec(self).expect("bounded rollback evidence is serializable");
        use sha2::{Digest, Sha256};
        RollbackDecision::Allowed {
            evidence_digest: format!("sha256:{:x}", Sha256::digest(encoded)),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryDisposition {
    ReconcileStatus,
    ForbidReplay,
    RetryWithNewApproval,
    Terminal,
}

impl RecoveryDisposition {
    #[must_use]
    pub fn at_boundary(
        mutation_started: bool,
        commit_point_entered: bool,
        state_known: bool,
    ) -> Self {
        if commit_point_entered {
            return Self::ForbidReplay;
        }
        if mutation_started && !state_known {
            return Self::ReconcileStatus;
        }
        if !mutation_started {
            return Self::RetryWithNewApproval;
        }
        Self::Terminal
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeValidationState {
    Running,
    Completed,
    Crashed,
    Disconnected,
    TimedOut,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeValidationObservation {
    pub runtime_session_id: String,
    pub state: RuntimeValidationState,
    pub introduced_error_count: usize,
    pub stack_trace_count: usize,
}

impl RuntimeValidationObservation {
    #[must_use]
    pub fn check_outcome(&self) -> CheckOutcome {
        match self.state {
            RuntimeValidationState::Completed if self.introduced_error_count == 0 => {
                CheckOutcome::Passed
            }
            RuntimeValidationState::Completed | RuntimeValidationState::Crashed => {
                CheckOutcome::Failed
            }
            RuntimeValidationState::TimedOut => CheckOutcome::TimedOut,
            RuntimeValidationState::Disconnected | RuntimeValidationState::Running => {
                CheckOutcome::Inconclusive
            }
        }
    }
}

#[derive(Default)]
pub struct RuntimeValidationGate {
    seen_sessions: BTreeSet<String>,
}

impl RuntimeValidationGate {
    pub fn begin(&mut self, runtime_session_id: &str) -> bool {
        runtime_session_id.starts_with("runtime-session:")
            && self.seen_sessions.insert(runtime_session_id.to_owned())
    }

    #[must_use]
    pub fn has_seen(&self, runtime_session_id: &str) -> bool {
        self.seen_sessions.contains(runtime_session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence() -> RollbackEvidence {
        RollbackEvidence {
            change_set_id: "change-set:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            expected_native_action_tag_digest:
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            newest_native_action_tag_digest: Some(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_owned(),
            ),
            expected_postimage_hashes: BTreeMap::from([(
                "target:script".to_owned(),
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    .to_owned(),
            )]),
            observed_postimage_hashes: BTreeMap::from([(
                "target:script".to_owned(),
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    .to_owned(),
            )]),
            native_history_known: true,
            persistence_state_known: true,
            validation_report_digest:
                "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_owned(),
        }
    }

    #[test]
    fn rollback_requires_newest_action_and_every_exact_postimage_hash() {
        assert!(matches!(
            evidence().evaluate(),
            RollbackDecision::Allowed { .. }
        ));
        let mut intervening = evidence();
        intervening.newest_native_action_tag_digest = Some(
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd".to_owned(),
        );
        assert_eq!(
            intervening.evaluate(),
            RollbackDecision::Blocked {
                reason: "intervening_native_action".to_owned(),
                conflicting_targets: BTreeSet::new(),
            }
        );
        let mut external = evidence();
        external.observed_postimage_hashes.insert(
            "target:script".to_owned(),
            "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_owned(),
        );
        assert!(matches!(
            external.evaluate(),
            RollbackDecision::Blocked { reason, .. } if reason == "postimage_hash_mismatch"
        ));
        let mut unknown = evidence();
        unknown.native_history_known = false;
        assert!(matches!(
            unknown.evaluate(),
            RollbackDecision::InDoubt { .. }
        ));
    }

    #[test]
    fn response_loss_never_replays_after_commit_point() {
        assert_eq!(
            RecoveryDisposition::at_boundary(false, false, true),
            RecoveryDisposition::RetryWithNewApproval
        );
        assert_eq!(
            RecoveryDisposition::at_boundary(true, false, false),
            RecoveryDisposition::ReconcileStatus
        );
        assert_eq!(
            RecoveryDisposition::at_boundary(true, true, false),
            RecoveryDisposition::ForbidReplay
        );
    }

    #[test]
    fn runtime_validation_requires_a_fresh_session_and_maps_terminal_states() {
        let mut gate = RuntimeValidationGate::default();
        let first = "runtime-session:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let second = "runtime-session:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        assert!(gate.begin(first));
        assert!(!gate.begin(first));
        assert!(gate.begin(second));
        assert!(gate.has_seen(first));
        assert_eq!(
            RuntimeValidationObservation {
                runtime_session_id: second.to_owned(),
                state: RuntimeValidationState::Crashed,
                introduced_error_count: 1,
                stack_trace_count: 1,
            }
            .check_outcome(),
            CheckOutcome::Failed
        );
        assert_eq!(
            RuntimeValidationObservation {
                runtime_session_id: second.to_owned(),
                state: RuntimeValidationState::TimedOut,
                introduced_error_count: 0,
                stack_trace_count: 0,
            }
            .check_outcome(),
            CheckOutcome::TimedOut
        );
    }

    #[test]
    fn rollback_policy_defaults_to_required_failure_semantics() {
        assert!(
            RollbackPolicy::OnRequiredFailure
                .should_rollback(ValidationReportOutcome::Inconclusive, false)
        );
        assert!(
            !RollbackPolicy::OnRequiredFailure
                .should_rollback(ValidationReportOutcome::Passed, true)
        );
        assert!(
            RollbackPolicy::OnAnyFailure.should_rollback(ValidationReportOutcome::Passed, true)
        );
        assert!(!RollbackPolicy::Never.should_rollback(ValidationReportOutcome::Failed, true));
    }
}
