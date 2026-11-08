//! 验证显式会话、Key 隔离与缓存字段不会造成误封

use cpr_cyber_session_block::identity::identify;
use gateway_plugin_sdk::call::middleware::MiddlewareHeader;

#[test]
fn same_session_under_different_keys_has_independent_blocks() {
    let body = br#"{"session_id":"session-private"}"#;
    assert_ne!(
        identify("key-a", &[], body).unwrap().session_hash,
        identify("key-b", &[], body).unwrap().session_hash
    );
}

#[test]
fn prompt_cache_key_and_shared_origin_do_not_identify_a_conversation() {
    let headers = vec![MiddlewareHeader {
        name: "x-session-affinity".into(),
        value: b"shared-affinity".to_vec(),
    }];
    assert!(
        identify(
            "key-a",
            &headers,
            br#"{"prompt_cache_key":"shared-cache","input":"session-private"}"#
        )
        .is_none()
    );
}

#[test]
fn turn_metadata_has_the_same_precedence_as_the_existing_session_plugin() {
    let headers = vec![MiddlewareHeader {
        name: "X-Codex-Turn-Metadata".into(),
        value: br#"{"session_id":"session-root"}"#.to_vec(),
    }];
    let actual = identify("key-a", &headers, br#"{"session_id":"other-session"}"#).unwrap();
    let expected = identify("key-a", &[], br#"{"session_id":"session-root"}"#).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn native_continuation_still_uses_the_explicit_session_identity() {
    let headers = vec![MiddlewareHeader {
        name: "session-id".into(),
        value: b"session-private".to_vec(),
    }];
    assert_eq!(
        identify(
            "key-a",
            &headers,
            br#"{"previous_response_id":"resp_private"}"#
        ),
        identify("key-a", &headers, b"{}")
    );
}

#[test]
fn session_alias_headers_share_one_identity() {
    let expected = identify(
        "key-a",
        &[MiddlewareHeader {
            name: "session-id".into(),
            value: b"session-private".to_vec(),
        }],
        b"{}",
    );
    for name in [
        "session_id",
        "conversation_id",
        "X-Session-Id",
        "X-OpenCode-Session",
        "X-Conversation-ID",
    ] {
        assert_eq!(
            identify(
                "key-a",
                &[MiddlewareHeader {
                    name: name.into(),
                    value: b"session-private".to_vec()
                }],
                b"{}"
            ),
            expected
        );
    }
}
