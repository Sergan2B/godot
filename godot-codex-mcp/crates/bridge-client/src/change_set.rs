use std::time::Duration;

use serde_json::{Value, json};

use crate::protocol::{BridgeError, Session};
use crate::transaction::{ApprovalReceipt, MAX_SAFE_INTEGER};

const REQUEST_DEADLINE: Duration = Duration::from_secs(30);
const MAX_RESULT_BYTES: usize = 65_536;

fn ensure_available(session: &Session) -> Result<(), BridgeError> {
    if session.protocol_version() != "1.8"
        || !session.capabilities().contains("transaction.change_set_v1")
    {
        return Err(BridgeError::CapabilityUnavailable {
            capability: "transaction.change_set_v1",
            negotiated_version: session.protocol_version().to_owned(),
        });
    }
    Ok(())
}

fn valid_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|suffix| {
        suffix.len() == 64
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn bounded_result(response: Value) -> Result<Value, BridgeError> {
    let result = response
        .get("result")
        .cloned()
        .ok_or_else(|| BridgeError::Invalid("change-set result is missing".to_owned()))?;
    if !result.is_object() || serde_json::to_vec(&result)?.len() > MAX_RESULT_BYTES {
        return Err(BridgeError::Invalid(
            "change-set result exceeds its safe limit".to_owned(),
        ));
    }
    Ok(result)
}

pub(crate) async fn prepare(session: &mut Session, params: Value) -> Result<Value, BridgeError> {
    ensure_available(session)?;
    if !params.is_object() || serde_json::to_vec(&params)?.len() > MAX_RESULT_BYTES {
        return Err(BridgeError::Invalid(
            "change-set prepare parameters are invalid".to_owned(),
        ));
    }
    let response = session
        .request_with_deadline_and_timeout(
            "transaction.prepare_change_set",
            params,
            REQUEST_DEADLINE,
            REQUEST_DEADLINE,
        )
        .await?;
    bounded_result(response)
}

pub(crate) async fn apply(
    session: &mut Session,
    change_set_id: &str,
    preview_digest: &str,
    expected_scene_revision: u64,
    expected_operation_seq: u64,
    approval: ApprovalReceipt,
) -> Result<Value, BridgeError> {
    ensure_available(session)?;
    if !valid_id(change_set_id, "change-set:")
        || !valid_digest(preview_digest)
        || expected_scene_revision > MAX_SAFE_INTEGER
        || expected_operation_seq > MAX_SAFE_INTEGER
    {
        return Err(BridgeError::Invalid(
            "change-set apply coordinates are invalid".to_owned(),
        ));
    }
    let response = session
        .request_with_deadline_and_timeout(
            "transaction.apply",
            json!({
                "change_set_id": change_set_id,
                "preview_digest": preview_digest,
                "expected_scene_revision": expected_scene_revision,
                "expected_operation_seq": expected_operation_seq,
                "approval": approval,
            }),
            REQUEST_DEADLINE,
            REQUEST_DEADLINE,
        )
        .await?;
    bounded_result(response)
}

pub(crate) async fn status(
    session: &mut Session,
    change_set_id: &str,
) -> Result<Value, BridgeError> {
    ensure_available(session)?;
    if !valid_id(change_set_id, "change-set:") {
        return Err(BridgeError::Invalid(
            "change-set identifier is invalid".to_owned(),
        ));
    }
    bounded_result(
        session
            .request(
                "transaction.status",
                json!({"change_set_id": change_set_id}),
            )
            .await?,
    )
}

pub(crate) async fn undo(
    session: &mut Session,
    change_set_id: &str,
    expected_transaction_seq: u64,
) -> Result<Value, BridgeError> {
    ensure_available(session)?;
    if !valid_id(change_set_id, "change-set:")
        || expected_transaction_seq == 0
        || expected_transaction_seq > MAX_SAFE_INTEGER
    {
        return Err(BridgeError::Invalid(
            "change-set Undo coordinates are invalid".to_owned(),
        ));
    }
    bounded_result(
        session
            .request_with_deadline_and_timeout(
                "transaction.undo",
                json!({
                    "change_set_id": change_set_id,
                    "expected_transaction_seq": expected_transaction_seq,
                }),
                REQUEST_DEADLINE,
                REQUEST_DEADLINE,
            )
            .await?,
    )
}

pub(crate) async fn validation_complete(
    session: &mut Session,
    change_set_id: &str,
    report_id: &str,
    report_digest: &str,
    outcome: &str,
    expected_transaction_seq: u64,
    expected_postimage_digest: &str,
) -> Result<Value, BridgeError> {
    ensure_available(session)?;
    if !session.capabilities().contains("validation.automatic_v1")
        || !valid_id(change_set_id, "change-set:")
        || !valid_id(report_id, "validation-report:")
        || !valid_digest(report_digest)
        || !matches!(outcome, "passed" | "failed" | "inconclusive" | "timed_out")
        || expected_transaction_seq == 0
        || expected_transaction_seq > MAX_SAFE_INTEGER
        || !valid_digest(expected_postimage_digest)
    {
        return Err(BridgeError::Invalid(
            "validation completion binding is invalid".to_owned(),
        ));
    }
    bounded_result(
        session
            .request_with_deadline_and_timeout(
                "transaction.validation_complete",
                json!({
                    "change_set_id": change_set_id,
                    "validation_report_id": report_id,
                    "report_digest": report_digest,
                    "outcome": outcome,
                    "expected_transaction_seq": expected_transaction_seq,
                    "expected_postimage_digest": expected_postimage_digest,
                }),
                REQUEST_DEADLINE,
                REQUEST_DEADLINE,
            )
            .await?,
    )
}

pub(crate) async fn rollback(
    session: &mut Session,
    change_set_id: &str,
    expected_transaction_seq: u64,
    expected_postimage_digest: &str,
) -> Result<Value, BridgeError> {
    ensure_available(session)?;
    if !valid_id(change_set_id, "change-set:")
        || expected_transaction_seq == 0
        || expected_transaction_seq > MAX_SAFE_INTEGER
        || !valid_digest(expected_postimage_digest)
    {
        return Err(BridgeError::Invalid(
            "rollback coordinates are invalid".to_owned(),
        ));
    }
    bounded_result(
        session
            .request_with_deadline_and_timeout(
                "transaction.rollback",
                json!({
                    "change_set_id": change_set_id,
                    "expected_transaction_seq": expected_transaction_seq,
                    "expected_postimage_digest": expected_postimage_digest,
                }),
                REQUEST_DEADLINE,
                REQUEST_DEADLINE,
            )
            .await?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_and_digests_are_closed() {
        assert!(valid_id(
            "change-set:0123456789abcdef0123456789abcdef",
            "change-set:"
        ));
        assert!(!valid_id(
            "transaction:0123456789abcdef0123456789abcdef",
            "change-set:"
        ));
        assert!(valid_digest(&format!("sha256:{}", "a".repeat(64))));
        assert!(!valid_digest(&format!("sha256:{}", "A".repeat(64))));
    }
}
