//! 核实归属、尝试和真实错误字段，防止通用故障或无关请求导致误封

use cpr_cyber_session_block::{
    identity::Identity,
    ledger::{Verdict, evaluate},
    store::{Evidence, Pending, migrate},
};
use gateway_plugin_sdk::call::host::{
    StateMigrationChange, StateMigrationRecord, StateMigrationRequest,
};
use serde_json::{Value, json};

fn pending() -> Pending {
    Pending {
        evidence: Evidence {
            identity: Identity {
                session_hash: "a".repeat(64),
                client_key_id: "key-test".into(),
            },
            request_id: "request-one".into(),
            account_id: Some("account-test".into()),
            attempt_index: Some(1),
            model: None,
            transport: "http_sse".into(),
        },
        observed_at_ms: 1000,
        block_seconds: 60,
    }
}

fn detail() -> Value {
    json!({"requestId":"request-one","clientApiKeyId":"key-test","provider":"openai","errorCode":"cyber_policy","logicalOutcome":"incomplete","accountId":"account-test","attemptIndex":1,"attempts":[]})
}

#[test]
fn final_raw_code_confirms_a_block_without_parsing_messages() {
    assert_eq!(evaluate(detail(), &pending()).unwrap(), Verdict::Hit);
    let mut value = detail();
    value["errorCode"] = Value::Null;
    value["message"] = json!("cyber_policy");
    assert_eq!(evaluate(value, &pending()).unwrap(), Verdict::Clear);
}

#[test]
fn other_keys_requests_providers_and_attempts_cannot_confirm_a_block() {
    for field in ["clientApiKeyId", "requestId"] {
        let mut value = detail();
        value[field] = json!("other");
        assert!(evaluate(value, &pending()).is_err());
    }
    for (field, other) in [
        ("provider", json!("other")),
        ("attemptIndex", json!(2)),
        ("accountId", json!("other")),
        ("logicalOutcome", json!("succeeded")),
    ] {
        let mut value = detail();
        value[field] = other;
        assert_eq!(evaluate(value, &pending()).unwrap(), Verdict::Clear);
    }
}

#[test]
fn running_request_waits_until_a_matching_attempt_has_real_evidence() {
    let mut value = detail();
    value["logicalOutcome"] = json!("running");
    assert_eq!(
        evaluate(value.clone(), &pending()).unwrap(),
        Verdict::Pending
    );
    value["attempts"] = json!([{"attemptIndex":1,"accountId":"account-test","provider":"openai","providerErrorCode":"cyber_policy"}]);
    assert_eq!(evaluate(value, &pending()).unwrap(), Verdict::Hit);
}

#[test]
fn state_upgrade_preserves_history_and_adds_an_empty_pending_list() {
    let result = migrate(StateMigrationRequest {
        namespace: "blocks".into(),
        from_schema_version: 1,
        to_schema_version: 2,
        records: vec![StateMigrationRecord {
            key: "bucket/00".into(),
            value: json!({"records":[]}),
            version: 3,
        }],
    })
    .unwrap();
    assert!(
        matches!(&result.changes[0], StateMigrationChange::Replace { key,value } if key=="bucket/00" && value==&json!({"records":[],"pending":[]}))
    );
}
