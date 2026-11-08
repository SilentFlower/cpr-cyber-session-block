//! 通过公开宿主 HTTP 回调读取原生管理合同，不建立额外管理员凭据

use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::middleware::{MiddlewareHeader, http::Version},
    client::{HostClient, HttpBody, HttpFrame, HttpRequest},
};
use serde_json::{Value, from_slice, to_vec};

use crate::fault;

/// 调用既有管理接口，只消费成功信封，不传播上游正文或敏感字段
/// @param host 当前父调用的受管客户端
/// @param method 已确认的接口方法
/// @param uri 原生接口路径与查询参数
/// @param body 可选 JSON 正文
/// @return 成功数据；404 返回 None，其他失败保留明确分类
pub async fn request(
    host: &HostClient,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> Result<Option<Value>, PluginFault> {
    let headers = body
        .as_ref()
        .map(|_| {
            vec![MiddlewareHeader {
                name: "content-type".into(),
                value: b"application/json".to_vec(),
            }]
        })
        .unwrap_or_default();
    let body = match body {
        Some(value) => HttpBody::from_bytes(to_vec(&value).map_err(|_| unavailable())?),
        None => HttpBody::empty(),
    };
    let mut response = host
        .dispatch_http(HttpRequest {
            settings: Value::Null,
            method: method.into(),
            uri: uri.into(),
            version: Version::Http11,
            headers,
            timeout_ms: None,
            body,
        })
        .await
        .map_err(|_| unavailable())?;
    let mut bytes = Vec::new();
    while let Some(frame) = response.body.read().await.map_err(|_| unavailable())? {
        if let HttpFrame::Data(chunk) = frame {
            if bytes.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
                let _ = response.body.close().await;
                return Err(unavailable());
            }
            bytes.extend(chunk);
        }
    }
    if response.status == 404 {
        return Ok(None);
    }
    let value: Value = from_slice(&bytes).map_err(|_| unavailable())?;
    match response.status {
        200..=299 if value.get("code").and_then(Value::as_u64) == Some(200) => {
            Ok(Some(value.get("data").cloned().unwrap_or(Value::Null)))
        }
        409 => Err(fault(ErrorCode::Conflict, "宿主数据已变化，请重新读取")),
        400 => Err(fault(ErrorCode::InvalidInput, "宿主拒绝此操作")),
        _ => Err(unavailable()),
    }
}

fn unavailable() -> PluginFault {
    fault(ErrorCode::Uncertain, "无法确认宿主操作结果，请刷新后核对")
}
