//! 验证真实错误码匹配与通用故障、正文内容的区分

use cpr_cyber_session_block::policy::{Config, fault_hit};
use gateway_plugin_sdk::{ErrorCode, PluginFault};
use serde_json::json;

#[test]
fn ordinary_errors_and_error_messages_do_not_trigger_a_ban() {
    for details in [
        json!({"provider":{"code":null}}),
        json!({"provider":{"code":"server_error","message":"cyber_policy"}}),
        json!({"provider":{"code":"fixture_unavailable"}}),
    ] {
        let mut error = PluginFault::new(ErrorCode::Upstream, "cyber_policy");
        error.details = Some(details);
        assert!(!fault_hit(&error));
    }
}

#[test]
fn structured_upstream_code_is_used_even_without_http_status() {
    let mut error = PluginFault::new(ErrorCode::Upstream, "OpenAI upstream failure");
    error.details = Some(json!({"provider":{"code":"cyber_policy"}}));
    assert!(fault_hit(&error));
}

#[test]
fn local_policy_errors_are_not_presented_as_new_upstream_hits() {
    let mut error = PluginFault::new(ErrorCode::Rejected, "local rejection");
    error.details = Some(json!({"code":"cyber_policy","provider":{"code":null}}));
    assert!(!fault_hit(&error));
}

#[test]
fn invalid_block_duration_is_rejected() {
    assert!(Config { block_seconds: 0 }.validate().is_err());
    assert!(
        Config {
            block_seconds: 2_592_001
        }
        .validate()
        .is_err()
    );
}
