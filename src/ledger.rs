//! 从原生请求详情核实流式故障，通用 unavailable 永远不是封禁证据

use form_urlencoded::Serializer;
use gateway_plugin_sdk::{ErrorCode, PluginFault, client::HostClient};
use serde::Deserialize;
use serde_json::{Value, from_value};

use crate::{fault, native, store::Pending};

/// 只读取原生记录中的请求归属、终态和上游错误码
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Detail {
    request_id: String,
    client_api_key_id: Option<String>,
    provider: Option<String>,
    error_code: Option<String>,
    logical_outcome: String,
    attempt_index: Option<u32>,
    account_id: Option<String>,
    attempts: Vec<Attempt>,
}

/// 尝试记录保留的真实上游错误码，允许在逻辑请求结束前停止已确认的重试
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Attempt {
    attempt_index: u32,
    provider: String,
    provider_error_code: Option<String>,
    account_id: Option<String>,
}

/// 原生记录可能晚于流式错误落库，未完成核实不能伪称策略命中
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pending,
    Hit,
    Clear,
}

/// 读取原请求精确详情，不依赖消息文本或其他请求的统计信息
/// @param host 当前宿主回调客户端
/// @param pending 原尝试冻结的最小归属
/// @return 已命中、已排除或等待原生记录完成
pub async fn lookup(host: &HostClient, pending: &Pending) -> Result<Verdict, PluginFault> {
    let query = Serializer::new(String::new())
        .append_pair("id", &pending.evidence.request_id)
        .finish();
    let Some(value) = native::request(
        host,
        "GET",
        &format!("/api/admin/usage/records/detail?{query}"),
        None,
    )
    .await?
    else {
        return Ok(Verdict::Pending);
    };
    evaluate(value, pending)
}

/// 校验原生请求归属后，只接受 OpenAI 的真实上游 cyber_policy
/// @param value 原生详情数据，忽略无关敏感字段
/// @param pending 触发读取的请求与尝试快照
/// @return 确定的核实状态，数据合同失配时返回错误
pub fn evaluate(value: Value, pending: &Pending) -> Result<Verdict, PluginFault> {
    let detail: Detail =
        from_value(value).map_err(|_| fault(ErrorCode::Fault, "原生请求记录格式无效"))?;
    let evidence = &pending.evidence;
    if detail.request_id != evidence.request_id
        || detail.client_api_key_id.as_deref() != Some(evidence.identity.client_key_id.as_str())
    {
        return Err(fault(ErrorCode::Fault, "原生请求记录归属不匹配"));
    }
    let cyber = |code: Option<&str>| {
        code.is_some_and(|code| code.trim().eq_ignore_ascii_case("cyber_policy"))
    };
    if detail.attempts.iter().any(|attempt| {
        Some(attempt.attempt_index) == evidence.attempt_index
            && attempt.account_id == evidence.account_id
            && attempt.provider == "openai"
            && cyber(attempt.provider_error_code.as_deref())
    }) {
        return Ok(Verdict::Hit);
    }
    if detail.logical_outcome == "running" {
        return Ok(Verdict::Pending);
    }
    if matches!(detail.logical_outcome.as_str(), "failed" | "incomplete")
        && detail.attempt_index == evidence.attempt_index
        && detail.account_id == evidence.account_id
        && detail.provider.as_deref() == Some("openai")
        && cyber(detail.error_code.as_deref())
    {
        return Ok(Verdict::Hit);
    }
    if matches!(
        detail.logical_outcome.as_str(),
        "succeeded" | "failed" | "incomplete" | "cancelled"
    ) {
        Ok(Verdict::Clear)
    } else {
        Err(fault(ErrorCode::Fault, "原生请求记录状态无效"))
    }
}
