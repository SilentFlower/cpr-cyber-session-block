//! 验证封禁时间边界、重复命中及历史与有效封禁的关系

use cpr_cyber_session_block::{
    identity::Identity,
    store::{Bucket, Evidence},
};

fn evidence(request: &str) -> Evidence {
    Evidence {
        identity: Identity {
            session_hash: "a".repeat(64),
            client_key_id: "key-test".into(),
        },
        request_id: request.into(),
        account_id: Some("account-test".into()),
        model: Some("test-model".into()),
        attempt_index: Some(1),
        transport: "http_sse".into(),
    }
}

#[test]
fn concurrent_hits_do_not_extend_the_original_expiration() {
    let mut bucket = Bucket::default();
    let first = bucket.register(&evidence("request-one"), 60, 1000);
    let second = bucket.register(&evidence("request-two"), 3600, 2000);
    assert_eq!(
        (second.id, second.expires_at_ms, bucket.records.len()),
        (first.id, 61_000, 1)
    );
}

#[test]
fn expiration_allows_a_later_real_hit_to_create_new_history() {
    let mut bucket = Bucket::default();
    let first = bucket.register(&evidence("request-one"), 1, 1000);
    assert!(bucket.active(&first.session_hash, 2000).is_none());
    let second = bucket.register(&evidence("request-two"), 1, 2000);
    assert_ne!(first.id, second.id);
    assert_eq!(bucket.records.len(), 2);
}

#[test]
fn manual_release_keeps_history_but_stops_blocking() {
    let mut bucket = Bucket::default();
    let record = bucket.register(&evidence("request-one"), 60, 1000);
    bucket.records[0].released_at_ms = Some(2000);
    assert!(bucket.active(&record.session_hash, 2000).is_none());
    assert_eq!(bucket.records[0].status(2000), "released");
    let duplicate = bucket.register(&evidence("request-one"), 3600, 3000);
    assert_eq!(duplicate.id, record.id);
    assert_eq!(duplicate.status(3000), "released");
    assert_eq!(bucket.records.len(), 1);
}
