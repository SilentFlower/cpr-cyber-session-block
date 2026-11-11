//! 为隔离扩展页提供分页记录、精确解除和当前实例参数管理

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use cpr_cyber_session_block::{fault, store::Record};
use form_urlencoded::parse;
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        data::AccountFactsQuery,
        management::{
            ManagementPage, ManagementRegistration, ManagementRequest, ManagementResource,
            ManagementResponse, ManagementRoute,
        },
        middleware::MiddlewareHeader,
    },
    client::{HostClient, TypedCall, TypedReply},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, from_slice, json, to_value, to_vec};
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

use crate::{
    app::{App, wall_ms},
    routing,
};

/// 只解除页面读取的精确记录，不能按会话摘要批量解除
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleaseInput {
    session_hash: String,
    record_id: String,
}

/// 补充当前状态和北京时间展示，保留原始时间点及旧版 UTC 字段
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    #[serde(flatten)]
    record: Record,
    status: &'static str,
    #[serde(flatten)]
    routing: routing::Routing,
    account_name: Option<String>,
    account_email: Option<String>,
    created_time_utc: String,
    expires_time_utc: String,
    released_time_utc: Option<String>,
    created_time_display: String,
    expires_time_display: String,
    released_time_display: Option<String>,
}

/// 声明最小管理路由和随包发布的资源，由宿主提供页面标题
/// @return 与清单资源一致的注册合同
pub(crate) fn registration() -> ManagementRegistration {
    ManagementRegistration {
        routes: [
            ("GET", "history"),
            ("POST", "release"),
            ("GET", "settings"),
            ("POST", "settings"),
        ]
        .into_iter()
        .map(|(method, path)| ManagementRoute {
            method: method.into(),
            path: path.into(),
            request_content_types: if method == "POST" {
                vec!["application/json".into()]
            } else {
                Vec::new()
            },
            response_content_types: vec!["application/json".into()],
        })
        .collect(),
        resources: ["web/index.html", "web/app.js", "web/app.css"]
            .into_iter()
            .map(|path| ManagementResource {
                path: path.into(),
                public: false,
            })
            .collect(),
        pages: vec![ManagementPage {
            id: "history".into(),
            title: "会话封禁记录".into(),
            description: Some("查看 cyber_policy 命中、封禁状态与本地拦截记录".into()),
            entry: "web/index.html".into(),
            icon: None,
        }],
        callbacks: Vec::new(),
    }
}

/// 处理管理员已认证的原始插件请求，不创建第二套管理身份
/// @param app 当前插件实例
/// @param call 已由宿主路由验证的方法、路径和正文
/// @return 原始 JSON 响应及安全错误
pub(crate) async fn handle(
    app: &App,
    call: TypedCall<ManagementRequest>,
) -> Result<TypedReply<ManagementResponse>, PluginFault> {
    let result = route(app, &call).await;
    let (status, value) = match result {
        Ok(value) => (200, value),
        Err(error) => (
            match error.code {
                ErrorCode::InvalidInput => 400,
                ErrorCode::Conflict => 409,
                ErrorCode::Rejected => 404,
                _ => 503,
            },
            json!({"error":{"code":error.code,"message":error.message}}),
        ),
    };
    Ok(TypedReply::new(ManagementResponse {
        status,
        content_type: "application/json".into(),
        headers: vec![MiddlewareHeader {
            name: "cache-control".into(),
            value: b"no-store".to_vec(),
        }],
    })
    .with_payload(to_vec(&value).map_err(|_| fault(ErrorCode::Fault, "管理响应编码失败"))?))
}

async fn route(app: &App, call: &TypedCall<ManagementRequest>) -> Result<Value, PluginFault> {
    match (call.request.method.as_str(), call.request.path.as_str()) {
        ("GET", "history") => history(app, call).await,
        ("GET", "settings") if call.request.query.is_empty() => {
            to_value(app.settings.view(&call.host).await?)
                .map_err(|_| fault(ErrorCode::Fault, "设置响应编码失败"))
        }
        ("POST", "settings") if call.request.query.is_empty() => {
            app.settings
                .save(
                    &call.host,
                    from_slice(&call.payload).map_err(|_| invalid())?,
                )
                .await
        }
        ("POST", "release") if call.request.query.is_empty() => {
            let input: ReleaseInput = from_slice(&call.payload).map_err(|_| invalid())?;
            if input.record_id.len() > 64 {
                return Err(invalid());
            }
            let record = app
                .repository
                .release(&call.host, &input.session_hash, &input.record_id, wall_ms())
                .await?;
            Ok(json!({"released":true,"recordId":record.id}))
        }
        _ => Err(invalid()),
    }
}

async fn history(app: &App, call: &TypedCall<ManagementRequest>) -> Result<Value, PluginFault> {
    let mut pairs = BTreeMap::new();
    for (key, value) in parse(call.request.query.as_bytes()).into_owned() {
        if !["search", "status", "page", "limit", "asOf"].contains(&key.as_str())
            || pairs.insert(key, value).is_some()
        {
            return Err(invalid());
        }
    }
    let search = pairs.get("search").map(|value| value.trim()).unwrap_or("");
    let status = pairs.get("status").map(String::as_str).unwrap_or("all");
    let page = number(&pairs, "page", 1)?;
    let limit = number(&pairs, "limit", 20)?;
    let now = wall_ms();
    let as_of = number(&pairs, "asOf", now)?;
    if search.len() > 256
        || page == 0
        || !(1..=100).contains(&limit)
        || as_of > now
        || !["all", "active", "expired", "released"].contains(&status)
    {
        return Err(invalid());
    }
    let reconciled = app.reconcile(&call.host, None, None).await?;
    let mut records = app.repository.records(&call.host).await?;
    records.retain(|record| record.created_at_ms <= as_of);
    let active_count = records
        .iter()
        .filter(|record| record.status(now) == "active")
        .count();
    let retained_count = records.len();
    records.retain(|record| {
        (status == "all" || record.status(now) == status)
            && (search.is_empty()
                || record.request_id.contains(search)
                || record.session_hash.contains(search)
                || record.client_key_id.contains(search))
    });
    records.sort_by(|left, right| {
        right
            .created_at_ms
            .cmp(&left.created_at_ms)
            .then_with(|| right.id.cmp(&left.id))
    });
    let total = records.len();
    let pages = total.div_ceil(limit as usize).max(1);
    let page = page.min(pages as u64);
    let offset =
        usize::try_from(page.saturating_sub(1).saturating_mul(limit)).map_err(|_| invalid())?;
    let records: Vec<Record> = records
        .into_iter()
        .skip(offset)
        .take(limit as usize)
        .collect();
    let requests: Vec<_> = records
        .iter()
        .map(|r| (r.request_id.clone(), r.client_key_id.clone()))
        .collect();
    let (profiles, groups) = tokio::join!(
        account_profiles(&call.host, &records),
        routing::load(&call.host, &requests)
    );
    let records: Vec<Entry> = records
        .into_iter()
        .map(|record| Entry {
            routing: groups.get(&record.request_id).cloned().unwrap_or_default(),
            status: record.status(now),
            account_name: record
                .account_id
                .as_ref()
                .and_then(|id| profiles.get(id))
                .map(|(name, _)| name.clone()),
            account_email: record
                .account_id
                .as_ref()
                .and_then(|id| profiles.get(id))
                .and_then(|(_, email)| email.clone()),
            created_time_utc: utc(record.created_at_ms),
            expires_time_utc: utc(record.expires_at_ms),
            released_time_utc: record.released_at_ms.map(utc),
            created_time_display: beijing(record.created_at_ms),
            expires_time_display: beijing(record.expires_at_ms),
            released_time_display: record.released_at_ms.map(beijing),
            record,
        })
        .collect();
    Ok(
        json!({"records":records,"pagination":{"page":page,"pageSize":limit,"total":total},"asOf":as_of,"serverTimeMs":now,"summary":{"active":active_count,"retained":retained_count,"pending":reconciled.waiting},"storage":{"maximumBytes":16777216,"maximumBucketBytes":262144,"automaticDeletion":false}}),
    )
}

async fn account_profiles(
    host: &HostClient,
    records: &[Record],
) -> BTreeMap<String, (String, Option<String>)> {
    let mut needed: BTreeSet<&str> = records
        .iter()
        .filter_map(|record| record.account_id.as_deref())
        .collect();
    let mut names = BTreeMap::new();
    // 名称仅用于当前页面展示，读取失败或账号被删除时保留历史和稳定 ID
    // 每页账号去重后读取，不为每条封禁逐个发起管理请求
    let lookup = async {
        let mut cursor = None;
        while !needed.is_empty() {
            let Ok(page) = host
                .account_facts(AccountFactsQuery {
                    provider_id: None,
                    cursor: cursor.clone(),
                    limit: 200,
                })
                .await
            else {
                break;
            };
            for account in page.accounts {
                if needed.remove(account.account_id.as_str()) {
                    let email = account.email.filter(|email| !email.trim().is_empty());
                    let name = if account.name.trim().is_empty() {
                        email.clone().unwrap_or_default()
                    } else {
                        account.name
                    };
                    if !name.trim().is_empty() {
                        names.insert(account.account_id, (name, email));
                    }
                }
            }
            match page.next_cursor {
                Some(next) if cursor.as_ref().is_none_or(|previous| next > *previous) => {
                    cursor = Some(next)
                }
                _ => break,
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(2), lookup).await;
    names
}

fn number(pairs: &BTreeMap<String, String>, name: &str, default: u64) -> Result<u64, PluginFault> {
    pairs
        .get(name)
        .map(|value| value.parse::<u64>().map_err(|_| invalid()))
        .unwrap_or(Ok(default))
}
fn invalid() -> PluginFault {
    fault(ErrorCode::InvalidInput, "管理接口参数无效")
}

/// 保留历史接口的 UTC 兼容字段
/// @param milliseconds UTC Unix 毫秒时间
/// @return 可直接显示的 UTC 时间
fn utc(milliseconds: u64) -> String {
    i64::try_from(milliseconds / 1000)
        .ok()
        .and_then(|seconds| OffsetDateTime::from_unix_timestamp(seconds).ok())
        .and_then(|time| time.format(&Rfc3339).ok())
        .unwrap_or_else(|| "时间不可用".into())
}

/// 将 Unix 时间点投影为北京时间，封禁时限仍用原始毫秒计算
/// @param milliseconds Unix 毫秒时间
/// @return 北京时间展示文本
pub(crate) fn beijing(milliseconds: u64) -> String {
    i64::try_from(milliseconds / 1000)
        .ok()
        .and_then(|seconds| OffsetDateTime::from_unix_timestamp(seconds).ok())
        .and_then(|time| time.checked_to_offset(UtcOffset::from_hms(8, 0, 0).ok()?))
        .map(|time| {
            format!(
                "{} {:02}:{:02}:{:02}",
                time.date(),
                time.hour(),
                time.minute(),
                time.second()
            )
        })
        .unwrap_or_else(|| "时间不可用".into())
}
