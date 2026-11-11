//! 组合会话准入、同步错误与请求完成观察，正常响应由宿主直接交付

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use cpr_cyber_session_block::{
    fault,
    identity::identify,
    ledger::{self, Verdict},
    policy::{Config, fault_hit},
    store::{Evidence, Pending, Repository},
};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        host::{LogLevel, LogRequest, LogResult},
        middleware::{
            MiddlewareBodyFrame, MiddlewareBodyFraming, MiddlewareHeader, MiddlewareMount,
            MiddlewareTransport,
        },
        observation::{Event, RequestOutcome},
        policy::{RetryDecision, RetryDecisionRequest},
    },
    client::{
        Empty, HostClient, MiddlewareBody, MiddlewareResponse, RequestCall, TypedCall, TypedReply,
    },
};
use serde_json::{Value, from_value, json, to_value, to_vec};

use crate::{management::beijing, settings::SettingsControl};

// 完成通知可能因过载丢失，只缓存最小归属并限制遗留条目，不能阻断正常模型响应
const MAXIMUM_TRACKED_REQUESTS: usize = 1024;

/// 同一逻辑请求的最新尝试归属，供重试与异步完成事件精确核实
struct RequestState {
    evidence: Mutex<Evidence>,
    started_at_ms: u64,
    captured: AtomicBool,
}

impl RequestState {
    /// 复制当前尝试归属，宿主回调期间不持有同步锁
    /// @return 当前逻辑请求与实际尝试的最小快照
    fn evidence(&self) -> Evidence {
        self.evidence
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

type Requests = Arc<Mutex<HashMap<String, Arc<RequestState>>>>;

/// 首批准备被取消或失败时释放关联，成功响应交给完成观察器回收
struct RequestGuard {
    requests: Requests,
    request_id: String,
    retained: bool,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        if !self.retained {
            self.requests
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&self.request_id);
        }
    }
}

/// 每个插件进程的不可变配置与受管请求关联
pub(crate) struct App {
    pub(crate) config: Config,
    pub(crate) repository: Repository,
    pub(crate) settings: SettingsControl,
    requests: Requests,
}

/// 核实过程既可能确认封禁，也可能仍有未写完的原生记录
pub(crate) struct Reconciled {
    pub(crate) waiting: bool,
    hit: bool,
}

impl App {
    /// 创建实例，封禁事实始终来自宿主私有状态
    /// @param config 握手冻结的配置
    /// @param settings 当前实例的原生配置管理器
    /// @return 当前进程的处理器组合
    pub(crate) fn new(config: Config, settings: SettingsControl) -> Self {
        Self {
            config,
            settings,
            repository: Repository,
            requests: Arc::default(),
        }
    }

    /// 在请求开始检查封禁，在上游尝试中识别真实策略拒绝
    /// @param call 宿主解码并认证后的模型请求或尝试
    /// @return 原响应或本地封禁响应，未命中请求保持原字节
    pub(crate) async fn middleware(
        self: &Arc<Self>,
        call: RequestCall,
    ) -> Result<MiddlewareResponse, PluginFault> {
        if call.request.head.protocol != "openai"
            || !matches!(
                call.request.head.endpoint.as_str(),
                "/v1/responses" | "/v1/chat/completions"
            )
        {
            return call.next.run(call.request).await;
        }
        let request_id = call.request.head.request_id.clone();
        let mount = call.request.head.mount;
        let mut guard = None;
        let state = if call.request.head.mount == MiddlewareMount::Request {
            let Some(identity) = identify(
                &call.request.head.client_key_id,
                &call.request.head.headers,
                &call.request.body,
            ) else {
                return call.next.run(call.request).await;
            };
            match self
                .reconcile(&call.host, Some(&identity.session_hash), Some(&request_id))
                .await
            {
                Ok(result) if result.waiting => {
                    return direct(
                        call.request.head.transport,
                        503,
                        "session_policy_pending",
                        "该会话的上一轮故障正在核实，请稍后重试。",
                        None,
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    report(&call.host, &request_id, &error).await;
                    return direct(
                        call.request.head.transport,
                        503,
                        "session_policy_unavailable",
                        "暂时无法核实会话策略，请稍后重试。",
                        None,
                    );
                }
            }
            match self
                .repository
                .reject(&call.host, &identity, wall_ms())
                .await
            {
                Ok(Some(record)) => {
                    return direct(
                        call.request.head.transport,
                        403,
                        "session_blocked_by_cyber_policy",
                        &format!(
                            "此会话因 cyber_policy 已封禁，解除时间为北京时间 {}。",
                            beijing(record.expires_at_ms)
                        ),
                        Some(record.expires_at_ms),
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    report(&call.host, &request_id, &error).await;
                    return direct(
                        call.request.head.transport,
                        503,
                        "session_policy_unavailable",
                        "会话策略存储暂时不可用。",
                        None,
                    );
                }
            }
            let state = Arc::new(RequestState {
                evidence: Mutex::new(Evidence {
                    identity,
                    request_id: request_id.clone(),
                    account_id: None,
                    model: call.request.head.model.clone(),
                    attempt_index: None,
                    transport: transport(call.request.head.transport).into(),
                }),
                started_at_ms: wall_ms(),
                captured: AtomicBool::new(false),
            });
            let evicted = {
                let mut requests = self
                    .requests
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let evicted = if requests.len() >= MAXIMUM_TRACKED_REQUESTS {
                    let oldest = requests
                        .iter()
                        .min_by_key(|(_, state)| state.started_at_ms)
                        .map(|(id, _)| id.clone());
                    oldest.inspect(|id| {
                        requests.remove(id);
                    })
                } else {
                    None
                };
                requests.insert(request_id.clone(), Arc::clone(&state));
                evicted
            };
            if let Some(evicted) = evicted {
                report(
                    &call.host,
                    &evicted,
                    &fault(ErrorCode::Capacity, "请求完成观察关联缓存已满"),
                )
                .await;
            }
            guard = Some(RequestGuard {
                requests: Arc::clone(&self.requests),
                request_id: request_id.clone(),
                retained: false,
            });
            Some(state)
        } else {
            self.requests
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&request_id)
                .cloned()
        };
        let Some(state) = state else {
            return call.next.run(call.request).await;
        };
        let mut evidence = state.evidence();
        if call.request.head.mount == MiddlewareMount::Attempt {
            // 排队中的请求可能在准入检查后被另一轮命中封禁，发送前须再次读取真实状态
            match self
                .reconcile(
                    &call.host,
                    Some(&evidence.identity.session_hash),
                    Some(&request_id),
                )
                .await
            {
                Ok(result) if result.waiting => {
                    return direct(
                        call.request.head.transport,
                        503,
                        "session_policy_pending",
                        "该会话的上一轮故障正在核实，请稍后重试。",
                        None,
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    report(&call.host, &request_id, &error).await;
                    return direct(
                        call.request.head.transport,
                        503,
                        "session_policy_unavailable",
                        "暂时无法核实会话策略，请稍后重试。",
                        None,
                    );
                }
            }
            match self
                .repository
                .reject(&call.host, &evidence.identity, wall_ms())
                .await
            {
                Ok(Some(record)) => {
                    return direct(
                        call.request.head.transport,
                        403,
                        "session_blocked_by_cyber_policy",
                        &format!(
                            "此会话因 cyber_policy 已封禁，解除时间为北京时间 {}。",
                            beijing(record.expires_at_ms)
                        ),
                        Some(record.expires_at_ms),
                    );
                }
                Ok(None) if !state.captured.load(Ordering::Acquire) => {}
                Ok(None) => {
                    return direct(
                        call.request.head.transport,
                        403,
                        "session_blocked_by_cyber_policy",
                        "此会话在本次请求中已触发 cyber_policy 封禁。",
                        None,
                    );
                }
                Err(error) => {
                    report(&call.host, &request_id, &error).await;
                    return direct(
                        call.request.head.transport,
                        503,
                        "session_policy_unavailable",
                        "会话策略存储暂时不可用。",
                        None,
                    );
                }
            }
        }
        if mount == MiddlewareMount::Attempt {
            evidence
                .account_id
                .clone_from(&call.request.head.account_id);
            evidence.attempt_index = call.request.head.attempt_index;
            evidence.model.clone_from(&call.request.head.model);
            *state
                .evidence
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = evidence.clone();
        }
        let observe = call
            .request
            .head
            .provider
            .as_deref()
            .is_none_or(|provider| provider == "openai");
        let result = call.next.run(call.request).await;
        match result {
            Err(error) => {
                if observe && fault_hit(&error) {
                    self.capture(&call.host, &state, &evidence).await;
                } else if observe && mount == MiddlewareMount::Attempt {
                    self.defer(&call.host, &state, &evidence, &error).await;
                }
                Err(error)
            }
            Ok(response) => {
                if mount == MiddlewareMount::Attempt {
                    if let Some(metadata) = &response.metadata {
                        evidence.account_id = Some(metadata.provider_account_id.clone());
                        evidence.model.clone_from(&metadata.upstream_model);
                        *state
                            .evidence
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()) = evidence;
                    }
                } else if let Some(guard) = &mut guard {
                    guard.retained = true;
                }
                // 原始句柄直接返还宿主，正常正文与完成帧不进入插件读帧或流控通道
                Ok(response)
            }
        }
    }

    /// 核实失败请求的原生错误记录，正常完成只释放进程内关联
    /// @param call 宿主异步投递的最终请求事实，不读取响应正文
    /// @return 观察处理结果，失败不会改写原请求的响应
    pub(crate) async fn observe(
        &self,
        call: TypedCall<Event>,
    ) -> Result<TypedReply<Empty>, PluginFault> {
        let Event::RequestCompleted(completed) = call.request else {
            return Ok(TypedReply::new(Empty {}));
        };
        let state = self
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&completed.request_id)
            .cloned();
        let Some(state) = state else {
            return Ok(TypedReply::new(Empty {}));
        };
        let evidence = state.evidence();
        if completed.client_key_id.as_deref() != Some(evidence.identity.client_key_id.as_str()) {
            return Err(fault(ErrorCode::InvalidInput, "请求完成观察归属不匹配"));
        }
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&completed.request_id);
        if state.captured.load(Ordering::Acquire)
            || completed.provider.as_deref() != Some("openai")
            || !matches!(
                completed.terminal.outcome,
                RequestOutcome::Failed | RequestOutcome::Incomplete
            )
            || completed.terminal.attempt_count == 0
        {
            return Ok(TypedReply::new(Empty {}));
        }
        if evidence.account_id != completed.account_id
            || evidence.attempt_index != Some(completed.terminal.attempt_count)
        {
            return Err(fault(ErrorCode::InvalidInput, "请求完成观察尝试归属不匹配"));
        }
        let session_hash = evidence.identity.session_hash.clone();
        // 完成通知先于异步落库时继续使用现有待核实机制，封禁从真实完成时间计算
        self.repository
            .defer(
                &call.host,
                Pending {
                    evidence,
                    observed_at_ms: completed.completed_at_ms,
                    block_seconds: self.config.block_seconds,
                },
            )
            .await?;
        self.reconcile(&call.host, Some(&session_hash), None)
            .await?;
        Ok(TypedReply::new(Empty {}))
    }

    async fn capture(&self, host: &HostClient, state: &RequestState, evidence: &Evidence) {
        // 先禁止本次请求继续换号，再等待持久化；内外层 next 错误不会重复记账
        if state.captured.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Err(error) = self
            .repository
            .register(host, evidence, self.config.block_seconds, wall_ms())
            .await
        {
            report(host, &evidence.request_id, &error).await;
        }
    }

    async fn defer(
        &self,
        host: &HostClient,
        state: &RequestState,
        evidence: &Evidence,
        error: &PluginFault,
    ) {
        if state.captured.load(Ordering::Acquire)
            || !matches!(error.code, ErrorCode::Upstream | ErrorCode::Fault)
            || error
                .details
                .as_ref()
                .and_then(|value| value.pointer("/provider/code"))
                .and_then(Value::as_str)
                .is_some()
        {
            return;
        }
        if let Err(error) = self
            .repository
            .defer(
                host,
                Pending {
                    evidence: evidence.clone(),
                    observed_at_ms: wall_ms(),
                    block_seconds: self.config.block_seconds,
                },
            )
            .await
        {
            report(host, &evidence.request_id, &error).await;
        }
    }

    /// 按原始请求记录补偿流式错误，下一轮准入和周期维护共用同一核实入口
    /// @param host 当前父调用的宿主客户端
    /// @param session_hash 请求准入时限定当前会话，维护时枚举全部分片
    /// @param skip_request 不等待当前逻辑请求自己结束，避免影响宿主原有重试
    /// @return 是否仍有未核实项，以及本轮是否确认命中
    pub(crate) async fn reconcile(
        &self,
        host: &HostClient,
        session_hash: Option<&str>,
        skip_request: Option<&str>,
    ) -> Result<Reconciled, PluginFault> {
        let pending = self.repository.pending(host, session_hash).await?;
        let mut result = Reconciled {
            waiting: false,
            hit: false,
        };
        let mut checked = 0;
        for pending in pending {
            if skip_request == Some(pending.evidence.request_id.as_str()) {
                continue;
            }
            if checked >= 128 {
                result.waiting = true;
                break;
            }
            checked += 1;
            let now = wall_ms();
            // 核实延迟不延长封禁，原时长已过的未知事件也不能永久卡住会话
            let expired = pending
                .observed_at_ms
                .saturating_add(u64::from(pending.block_seconds) * 1000)
                <= now;
            let verdict = match ledger::lookup(host, &pending).await {
                Ok(Verdict::Pending) | Err(_) if expired => Verdict::Clear,
                Ok(verdict) => verdict,
                Err(error) => return Err(error),
            };
            match verdict {
                Verdict::Pending => result.waiting = true,
                Verdict::Hit | Verdict::Clear => {
                    self.repository
                        .settle(host, &pending, verdict == Verdict::Hit, now)
                        .await?;
                    result.hit |= verdict == Verdict::Hit;
                }
            }
        }
        Ok(result)
    }

    /// 停止已经由本实例确认命中的请求，其他故障继续使用宿主策略
    /// @param call 本次失败允许的重试事实
    /// @return Stop 或 Delegate，不扩大宿主重试资格
    pub(crate) async fn retry(
        &self,
        call: TypedCall<RetryDecisionRequest>,
    ) -> Result<TypedReply<RetryDecision>, PluginFault> {
        let state = self
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&call.request.request_id)
            .cloned();
        if let Some(state) = &state
            && !state.captured.load(Ordering::Acquire)
        {
            let evidence = state.evidence();
            match self
                .reconcile(&call.host, Some(&evidence.identity.session_hash), None)
                .await
            {
                Ok(result) if result.hit => {
                    state.captured.store(true, Ordering::Release);
                }
                Ok(_) => {}
                Err(error) => report(&call.host, &call.request.request_id, &error).await,
            }
        }
        let hit = state.is_some_and(|state| state.captured.load(Ordering::Acquire));
        Ok(TypedReply::new(if hit {
            RetryDecision::Stop
        } else {
            RetryDecision::Delegate
        }))
    }
}

fn transport(value: MiddlewareTransport) -> &'static str {
    match value {
        MiddlewareTransport::HttpJson => "http_json",
        MiddlewareTransport::HttpSse => "http_sse",
        MiddlewareTransport::WebSocket => "websocket",
        MiddlewareTransport::Internal => "internal",
    }
}

fn direct(
    transport: MiddlewareTransport,
    status: u16,
    code: &str,
    message: &str,
    expires_at_ms: Option<u64>,
) -> Result<MiddlewareResponse, PluginFault> {
    if transport == MiddlewareTransport::WebSocket {
        // 复用宿主标准拒绝映射，避免自行构造需要模型元数据的 WS 直出正文
        return Err(fault(
            if status == 403 {
                ErrorCode::Rejected
            } else {
                ErrorCode::Fault
            },
            message,
        ));
    }
    let error_type = if status == 403 {
        "permission_error"
    } else {
        "server_error"
    };
    let payload = json!({"error":{"type":error_type,"code":code,"message":message},"blocked_until_ms":expires_at_ms});
    let payload =
        to_vec(&payload).map_err(|_| fault(ErrorCode::Fault, "会话策略响应编码失败。"))?;
    Ok(MiddlewareResponse::direct(
        "openai",
        status,
        vec![MiddlewareHeader {
            name: "content-type".into(),
            value: b"application/json".to_vec(),
        }],
        MiddlewareBody::from_frames(
            MiddlewareBodyFraming::RawBytes,
            vec![MiddlewareBodyFrame::new(payload, true)],
        ),
    ))
}

/// 获取持久化和 TTL 使用的 UTC 毫秒，不依赖宿主或浏览器本地时区
/// @return 当前 Unix 毫秒时间
pub(crate) fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

async fn report(host: &HostClient, request_id: &str, error: &PluginFault) {
    let request = LogRequest {
        level: LogLevel::Error,
        event: "cyber_session_state_failed".into(),
        fields: BTreeMap::from([
            ("requestId".into(), json!(request_id)),
            (
                "errorCode".into(),
                to_value(error.code).unwrap_or(Value::Null),
            ),
        ]),
    };
    if let Ok(params) = to_value(request) {
        let _ = host
            .call("host.log", params, Vec::new())
            .await
            .map(|reply| from_value::<LogResult>(reply.result));
    }
}
