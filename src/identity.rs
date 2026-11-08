//! 从显式会话字段派生带 Key 隔离的摘要，不使用缓存键或请求内容推测身份

use gateway_plugin_sdk::call::middleware::MiddlewareHeader;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// 只保存用于封禁的脱敏会话身份
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Identity {
    pub session_hash: String,
    pub client_key_id: String,
}

/// 只解析明确身份字段，避免保留对话其他元数据
#[derive(Default, Deserialize)]
struct Metadata {
    session_id: Option<String>,
    #[serde(rename = "x-codex-turn-metadata", alias = "turn_metadata")]
    turn_metadata: Option<String>,
}

/// 请求身份视图，未声明字段由反序列化直接跳过
#[derive(Default, Deserialize)]
struct Envelope {
    session_id: Option<String>,
    client_metadata: Option<Metadata>,
    context: Option<Metadata>,
    #[serde(alias = "turnMetadata", alias = "x-codex-turn-metadata")]
    turn_metadata: Option<String>,
}

fn text(value: Option<&str>) -> Option<&str> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 1024)
}

fn header<'a>(headers: &'a [MiddlewareHeader], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .and_then(|header| std::str::from_utf8(&header.value).ok())
        .and_then(|value| text(Some(value)))
}

fn turn_session(value: Option<&str>) -> Option<String> {
    let value: Value = serde_json::from_str(value?).ok()?;
    text(value.get("session_id").and_then(Value::as_str)).map(str::to_owned)
}

/// 读取与现有会话插件一致的身份优先级，附加识别明确的会话请求头
/// @param client_key_id 宿主认证后的 Key ID，不是 Key 原文
/// @param headers 当前模型请求的有效请求头
/// @param body 模型请求正文，只反序列化身份字段
/// @return 缺少可靠身份时返回 None
pub fn identify(
    client_key_id: &str,
    headers: &[MiddlewareHeader],
    body: &[u8],
) -> Option<Identity> {
    if client_key_id.is_empty() || client_key_id.len() > 128 {
        return None;
    }
    let body: Envelope = serde_json::from_slice(body).ok()?;
    let client = body.client_metadata.as_ref();
    let context = body.context.as_ref();
    let session = turn_session(header(headers, "x-codex-turn-metadata"))
        .or_else(|| text(client.and_then(|value| value.session_id.as_deref())).map(str::to_owned))
        .or_else(|| turn_session(body.turn_metadata.as_deref()))
        .or_else(|| turn_session(client.and_then(|value| value.turn_metadata.as_deref())))
        .or_else(|| turn_session(context.and_then(|value| value.turn_metadata.as_deref())))
        .or_else(|| text(body.session_id.as_deref()).map(str::to_owned))
        .or_else(|| header(headers, "session-id").map(str::to_owned))
        .or_else(|| text(context.and_then(|value| value.session_id.as_deref())).map(str::to_owned))
        .or_else(|| {
            [
                "session_id",
                "conversation_id",
                "x-session-id",
                "x-opencode-session",
                "x-conversation-id",
            ]
            .iter()
            .find_map(|name| header(headers, name).map(str::to_owned))
        })?;
    let mut hash = Sha256::new();
    hash.update(b"cpr-cyber-session-block-v1\0");
    hash.update(client_key_id.as_bytes());
    hash.update(b"\0");
    hash.update(session.as_bytes());
    Some(Identity {
        session_hash: format!("{:x}", hash.finalize()),
        client_key_id: client_key_id.into(),
    })
}
