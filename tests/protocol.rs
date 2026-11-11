//! 使用真实插件子进程验证正常响应直通、完成观察、停止重试与持久封禁

use std::{
    collections::{HashMap, HashSet, VecDeque},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gateway_plugin_sdk::{
    CallContext, ErrorCode, Frame, Handshake, Manifest, Message, PluginFault, SendState, Stage,
    call::middleware::MiddlewareBodyFrame,
    client::{read_frame, write_frame},
};
use serde_json::{Value, json};
use tokio::{
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

type State = HashMap<(String, String), (Value, u64)>;
type BodyFrames = VecDeque<Result<(Vec<u8>, bool), PluginFault>>;

/// 有界 RPC 测试宿主，状态可在子进程重启后继续使用
struct Host {
    _child: Child,
    input: ChildStdin,
    output: ChildStdout,
    next: HashMap<u64, u64>,
    results: HashMap<u64, (Value, Vec<u8>)>,
    errors: HashMap<u64, PluginFault>,
    ended: HashSet<u64>,
    chunks: HashMap<u64, Vec<Vec<u8>>>,
    state: State,
    bodies: HashMap<String, BodyFrames>,
    source_ids: HashMap<String, u64>,
    usage: HashMap<String, Value>,
    http_bodies: HashMap<String, VecDeque<Vec<u8>>>,
    body_reads: usize,
    lookups: usize,
    conflict_puts: usize,
    fail_put: bool,
    fail_get: bool,
    puts: usize,
    version: u64,
}

impl Host {
    async fn start(state: State) -> Self {
        let executable = std::env::var_os("CPR_PLUGIN_TEST_BINARY")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_cpr-cyber-session-block").into());
        let mut child = Command::new(executable)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut host = Self {
            input: child.stdin.take().unwrap(),
            output: child.stdout.take().unwrap(),
            _child: child,
            next: HashMap::new(),
            results: HashMap::new(),
            errors: HashMap::new(),
            ended: HashSet::new(),
            chunks: HashMap::new(),
            state,
            bodies: HashMap::new(),
            source_ids: HashMap::new(),
            usage: HashMap::new(),
            http_bodies: HashMap::new(),
            body_reads: 0,
            lookups: 0,
            conflict_puts: 0,
            fail_put: false,
            fail_get: false,
            puts: 0,
            version: 100,
        };
        let manifest = Manifest::from_author_slice(include_bytes!("../plugin.json")).unwrap();
        host.send(
            Message::Hello {
                handshake: Handshake {
                    protocol_version: 7,
                    artifact_sha256: "a".repeat(64),
                    plugin_id: manifest.plugin_id().unwrap(),
                    instance_id: "test-instance".into(),
                    generation: 1,
                    incarnation: "test-incarnation".into(),
                    configuration: json!({"blockSeconds":3600}),
                    contributes: manifest.contributes,
                },
            },
            Vec::new(),
        )
        .await;
        assert!(matches!(
            host.read().await.message,
            Message::Ready {
                protocol_version: 7,
                ..
            }
        ));
        host
    }

    async fn send(&mut self, message: Message, payload: Vec<u8>) {
        write_frame(&mut self.input, &Frame { message, payload })
            .await
            .unwrap();
    }
    async fn read(&mut self) -> Frame {
        timeout(Duration::from_secs(15), read_frame(&mut self.output))
            .await
            .expect("插件应在预算内响应")
            .expect("插件帧应完整")
    }
    async fn respond(&mut self, id: u64, result: Value, payload: Vec<u8>) {
        self.send(Message::Result { id, result }, payload).await;
    }

    async fn call(
        &mut self,
        id: u64,
        stage: Stage,
        method: &str,
        request: Option<&str>,
        params: Value,
        payload: Vec<u8>,
    ) {
        self.send(
            Message::Call {
                id,
                method: method.into(),
                context: CallContext {
                    request: None,
                    call_id: id,
                    instance_id: "test-instance".into(),
                    generation: 1,
                    incarnation: "test-incarnation".into(),
                    stage,
                    timeout_ms: if matches!(stage, Stage::Retry | Stage::Observation) {
                        2000
                    } else {
                        30000
                    },
                    resource_stream: matches!(stage, Stage::Request | Stage::Attempt),
                    resource_scope_id: format!("scope-{id}"),
                    request_id: request.map(str::to_owned),
                    attempt_id: (stage == Stage::Attempt)
                        .then(|| format!("{}:1", request.unwrap())),
                    account_id: (stage == Stage::Attempt).then(|| "account-test".into()),
                    credential_revision: None,
                },
                params,
            },
            payload,
        )
        .await;
        if matches!(stage, Stage::Request | Stage::Attempt) {
            self.send(
                Message::Credit {
                    id,
                    bytes: 262_144,
                    frames: 32,
                },
                Vec::new(),
            )
            .await;
        }
    }

    async fn model(
        &mut self,
        id: u64,
        request: &str,
        attempt: bool,
        key: &str,
        transport: &str,
        body: &[u8],
    ) {
        self.call(id, if attempt { Stage::Attempt } else { Stage::Request }, "middleware.handle", Some(request), json!({"settings_sources":{},"settings":null,"request_id":request,"client_key_id":key,"account_group_ids":[],"mount":if attempt {"attempt"} else {"request"},"attempt_index":if attempt {Some(1)} else {None},"operation":"generate","protocol":"openai","endpoint":"/v1/responses","transport":transport,"provider":if attempt {Some("openai")} else {None},"model":"test-model","account_id":if attempt {Some("account-test")} else {None},"headers":[]}), body.to_vec()).await;
    }

    async fn pump(&mut self) {
        let frame = self.read().await;
        match frame.message {
            Message::Callback {
                id,
                parent_id,
                method,
                params,
            } => match method.as_str() {
                "host.middleware.next" => {
                    self.next.insert(parent_id, id);
                }
                "host.state.get" => {
                    if self.fail_get {
                        self.send(
                            Message::Error {
                                id,
                                error: PluginFault::new(ErrorCode::Fault, "测试存储不可用"),
                            },
                            Vec::new(),
                        )
                        .await;
                        return;
                    }
                    let key = (
                        params["namespace"].as_str().unwrap().into(),
                        params["key"].as_str().unwrap().into(),
                    );
                    let record = self.state.get(&key).map(|(value, version)| json!({"value":value,"version":version,"schema_version":2}));
                    self.respond(id, json!({"record":record}), Vec::new()).await;
                }
                "host.state.put" => {
                    self.puts += 1;
                    if self.fail_put {
                        self.send(
                            Message::Error {
                                id,
                                error: PluginFault::new(ErrorCode::Capacity, "测试配额已满"),
                            },
                            Vec::new(),
                        )
                        .await;
                        return;
                    }
                    let key = (
                        params["namespace"].as_str().unwrap().into(),
                        params["key"].as_str().unwrap().into(),
                    );
                    let version = self.state.get(&key).map(|(_, version)| *version);
                    if self.conflict_puts > 0 || params["expected_version"] != json!(version) {
                        self.conflict_puts = self.conflict_puts.saturating_sub(1);
                        self.send(
                            Message::Error {
                                id,
                                error: PluginFault::new(ErrorCode::Conflict, "测试并发更新"),
                            },
                            Vec::new(),
                        )
                        .await;
                        return;
                    }
                    self.version += 1;
                    self.state
                        .insert(key, (params["value"].clone(), self.version));
                    self.respond(id, json!({"version":self.version}), Vec::new())
                        .await;
                }
                "host.middleware.body_read" => {
                    self.body_reads += 1;
                    let handle = params["handle"].as_str().unwrap().to_owned();
                    let item = self.bodies.get_mut(&handle).and_then(VecDeque::pop_front);
                    match item {
                        Some(Ok((bytes, terminal))) => {
                            if bytes.len() > params["maximum_bytes"].as_u64().unwrap() as usize {
                                self.send(Message::Error { id, error: PluginFault::new(ErrorCode::InvalidInput, "middleware input is invalid") }, Vec::new()).await;
                                return;
                            }
                            let source = self.source_ids.entry(handle).or_default(); *source += 1; let source = *source;
                            self.respond(id, json!({"framing":"sse_event","source_id":source,"eof":false,"terminal":terminal}), bytes).await;
                        }
                        Some(Err(error)) => self.send(Message::Error { id, error }, Vec::new()).await,
                        None => self.respond(id, json!({"framing":"sse_event","source_id":0,"eof":true,"terminal":false}), Vec::new()).await,
                    }
                }
                "host.middleware.body_close" => {
                    self.respond(id, json!({}), Vec::new()).await;
                }
                "host.http.dispatch" => {
                    self.lookups += 1;
                    assert_eq!(params["method"], "GET");
                    let uri = params["uri"].as_str().unwrap();
                    let query = uri
                        .strip_prefix("/api/admin/usage/records/detail?")
                        .expect("只查询原生请求详情");
                    let request = form_urlencoded::parse(query.as_bytes())
                        .find(|(key, _)| key == "id")
                        .unwrap()
                        .1
                        .into_owned();
                    let data = self.usage.get(&request);
                    let status = if data.is_some() { 200 } else { 404 };
                    let bytes = serde_json::to_vec(&json!({"code":status,"data":data})).unwrap();
                    let handle = format!("http-{id}");
                    self.http_bodies
                        .insert(handle.clone(), VecDeque::from([bytes]));
                    self.respond(id, json!({"response":null,"session":false,"status":status,"version":"HTTP/1.1","headers":[],"body":{"kind":"handle","handle":handle}}), Vec::new()).await;
                }
                "host.http.body_read" => {
                    let handle = params["handle"].as_str().unwrap();
                    let bytes = self.http_bodies.get_mut(handle).unwrap().pop_front();
                    self.respond(
                        id,
                        json!({"eof":bytes.is_none(),"trailers":null}),
                        bytes.unwrap_or_default(),
                    )
                    .await;
                }
                "host.log" => {
                    let text = serde_json::to_string(&params).unwrap();
                    assert!(!text.contains("session-private") && !text.contains("DO-NOT-LOG-BODY"));
                    self.respond(id, json!({"recorded":true}), Vec::new()).await;
                }
                _ => panic!("未预期的宿主回调：{method}"),
            },
            Message::Resume { id, epoch } => {
                self.send(Message::Resumed { id, epoch }, Vec::new()).await
            }
            Message::Result { id, result } => {
                self.results.insert(id, (result, frame.payload));
            }
            Message::Error { id, error } => {
                self.errors.insert(id, error);
            }
            Message::Stream { id, .. } => {
                self.chunks.entry(id).or_default().push(frame.payload);
            }
            Message::End { id, error } => {
                if let Some(error) = error {
                    self.errors.insert(id, error);
                }
                self.ended.insert(id);
            }
            Message::Cancelled { id } => {
                self.ended.insert(id);
            }
            Message::Suspend { .. } | Message::CallbackConsumed { .. } | Message::Credit { .. } => {
            }
            other => panic!("未预期的插件消息：{other:?}"),
        }
    }

    async fn wait_next(&mut self, id: u64) {
        while !self.next.contains_key(&id) {
            self.pump().await;
        }
    }
    async fn result(&mut self, id: u64) -> (Value, Vec<u8>) {
        while !self.results.contains_key(&id) {
            self.pump().await;
        }
        self.results.remove(&id).unwrap()
    }
    async fn error(&mut self, id: u64) -> PluginFault {
        while !self.errors.contains_key(&id) {
            self.pump().await;
        }
        self.errors.remove(&id).unwrap()
    }
    async fn end(&mut self, id: u64) {
        while !self.ended.contains(&id) {
            self.pump().await;
        }
    }
    async fn fail_next(&mut self, id: u64, error: PluginFault) {
        let callback = self.next.remove(&id).unwrap();
        self.send(
            Message::Error {
                id: callback,
                error,
            },
            Vec::new(),
        )
        .await;
    }
    async fn finish_next(&mut self, id: u64, frames: Option<BodyFrames>) {
        let handle = format!("body-{id}");
        let body = frames.map(|frames| {
            self.bodies.insert(handle.clone(), frames);
            json!({"handle":handle,"framing":"sse_event"})
        });
        let callback = self.next.remove(&id).unwrap();
        self.respond(callback, json!({"response":format!("response-{id}"),"protocol":"openai","status":200,"headers":[],"body":body}), Vec::new()).await;
    }

    async fn management(
        &mut self,
        id: u64,
        method: &str,
        path: &str,
        query: &str,
        body: Option<Value>,
    ) -> (Value, Value) {
        self.call(id, Stage::Management, "management.handle", None, json!({"method":method,"path":path,"query":query,"headers":[],"content_type":body.as_ref().map(|_| "application/json")}), body.map(|body| serde_json::to_vec(&body).unwrap()).unwrap_or_default()).await;
        let (response, bytes) = self.result(id).await;
        (response, serde_json::from_slice(&bytes).unwrap())
    }

    async fn hit(&mut self) {
        self.model(1, "request-one", false, "key-test", "http_sse", INPUT)
            .await;
        self.wait_next(1).await;
        self.model(3, "request-one", true, "key-test", "http_sse", INPUT)
            .await;
        self.wait_next(3).await;
        self.fail_next(3, cyber_error()).await;
        self.error(3).await;
        self.fail_next(1, cyber_error()).await;
        self.error(1).await;
    }

    fn records(&self) -> Vec<Value> {
        self.state
            .values()
            .flat_map(|(value, _)| value["records"].as_array().unwrap().iter().cloned())
            .collect()
    }

    fn pending(&self) -> Vec<Value> {
        self.state
            .values()
            .flat_map(|(value, _)| value["pending"].as_array().unwrap().iter().cloned())
            .collect()
    }

    async fn deferred(&mut self) {
        self.model(1, "request-stream", false, "key-test", "http_sse", INPUT)
            .await;
        self.wait_next(1).await;
        self.model(3, "request-stream", true, "key-test", "http_sse", INPUT)
            .await;
        self.wait_next(3).await;
        self.finish_next(3, Some(VecDeque::new())).await;
        assert_eq!(self.result(3).await.0["body"]["kind"], "pass_through");
        self.end(3).await;
        self.finish_next(1, Some(VecDeque::new())).await;
        self.result(1).await;
        self.end(1).await;
        self.completion(5, "request-stream", "incomplete").await;
        assert_eq!(self.pending().len(), 1);
        assert!(self.records().is_empty());
        assert_eq!(self.body_reads, 0);
    }

    async fn completion(&mut self, id: u64, request: &str, outcome: &str) {
        self.call(
            id,
            Stage::Observation,
            "observer.observe",
            Some(request),
            completion(request, outcome),
            Vec::new(),
        )
        .await;
        assert_eq!(self.result(id).await.0, json!({}));
    }
}

fn detail(code: &str) -> Value {
    json!({"requestId":"request-stream","clientApiKeyId":"key-test","provider":"openai","errorCode":code,"logicalOutcome":"incomplete","attemptIndex":1,"accountId":"account-test","attempts":[{"attemptIndex":1,"provider":"openai","providerErrorCode":code,"accountId":"account-test"}]})
}

fn completion(request: &str, outcome: &str) -> Value {
    json!({"event":"request_completed","data":{
        "event_id":format!("completed-{request}"),"request_id":request,"config_revision":1,
        "operation":"generate","client_key_id":"key-test","account_id":"account-test",
        "provider":"openai","upstream_model":"test-model","requested_model":"test-model",
        "completed_at_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64,
        "terminal":{"outcome":outcome,"send_state":"sent","attempt_count":1,"client_status_code":200,"error_code":"upstream_unavailable"},
        "usage":{}
    }})
}

const INPUT: &[u8] =
    br#"{"model":"test-model","session_id":"session-private","input":"DO-NOT-LOG-BODY"}"#;

fn cyber_error() -> PluginFault {
    let mut error = PluginFault::new(
        ErrorCode::Upstream,
        "OpenAI upstream failure: kind=unavailable, status=none",
    );
    error.send_state = SendState::Sent;
    error.details = Some(json!({"provider":{"code":"cyber_policy","response":null}}));
    error
}

#[tokio::test]
async fn first_hit_stops_retry_and_later_turns_are_rejected_before_next() {
    for transport in ["http_json", "http_sse", "web_socket"] {
        let mut host = Host::start(State::new()).await;
        host.model(1, "request-one", false, "key-test", transport, INPUT)
            .await;
        host.wait_next(1).await;
        host.model(3, "request-one", true, "key-test", transport, INPUT)
            .await;
        host.wait_next(3).await;
        host.fail_next(3, cyber_error()).await;
        assert_eq!(host.error(3).await, cyber_error());
        host.call(5, Stage::Retry, "policy.retry_decision", Some("request-one"), json!({"request_id":"request-one","attempt_index":1,"provider":"openai","model":"test-model","error_kind":"unavailable","upstream_status":null,"send_state":"sent","remaining_routing_attempts":2,"remaining_deadline_ms":1000,"allowed_actions":["stop","retry"]}), Vec::new()).await;
        assert_eq!(host.result(5).await.0, json!({"decision":"stop"}));
        host.fail_next(1, cyber_error()).await;
        host.error(1).await;
        let expiration = host.records()[0]["expiresAtMs"].clone();
        host.model(7, "request-two", false, "key-test", transport, INPUT)
            .await;
        if transport == "web_socket" {
            assert_eq!(host.error(7).await.code, ErrorCode::Rejected);
            assert!(!host.next.contains_key(&7));
            assert_eq!(host.records()[0]["expiresAtMs"], expiration);
            assert_eq!(host.records()[0]["rejectedRequests"], 1);
            continue;
        }
        let response = host.result(7).await.0;
        assert_eq!(response["status"], 403);
        assert_eq!(response["body"]["framing"], "raw_bytes");
        host.end(7).await;
        let frame = MiddlewareBodyFrame::decode(&host.chunks[&7][0]).unwrap();
        let payload: Value = serde_json::from_slice(&frame.payload).unwrap();
        assert_eq!(payload["error"]["code"], "session_blocked_by_cyber_policy");
        assert!(!host.next.contains_key(&7));
        assert_eq!(host.records()[0]["expiresAtMs"], expiration);
        assert_eq!(host.records()[0]["rejectedRequests"], 1);
    }
}

#[tokio::test]
async fn state_survives_process_restart_and_other_keys_remain_independent() {
    let mut host = Host::start(State::new()).await;
    host.hit().await;
    let state = host.state.clone();
    drop(host);
    let mut host = Host::start(state).await;
    host.model(
        1,
        "request-other-key",
        false,
        "key-other",
        "http_sse",
        INPUT,
    )
    .await;
    host.wait_next(1).await;
    host.finish_next(1, None).await;
    assert_eq!(host.result(1).await.0["response"], "response-1");
    host.model(3, "request-blocked", false, "key-test", "http_sse", INPUT)
        .await;
    assert_eq!(host.result(3).await.0["status"], 403);
    assert!(
        !serde_json::to_string(&host.records())
            .unwrap()
            .contains("session-private")
    );
}

#[tokio::test]
async fn manual_release_preserves_history_and_allows_the_next_turn() {
    let mut host = Host::start(State::new()).await;
    host.hit().await;
    let record = host.records().remove(0);
    let (response, value) = host
        .management(
            5,
            "POST",
            "release",
            "",
            Some(json!({"sessionHash":record["sessionHash"],"recordId":record["id"]})),
        )
        .await;
    assert_eq!(
        (response["status"].clone(), value["released"].clone()),
        (json!(200), json!(true))
    );
    host.model(
        7,
        "request-after-release",
        false,
        "key-test",
        "http_sse",
        INPUT,
    )
    .await;
    host.wait_next(7).await;
    host.finish_next(7, None).await;
    host.result(7).await;
    let (_, value) = host
        .management(9, "GET", "history", "status=released", None)
        .await;
    assert_eq!(value["pagination"]["total"], 1);
    assert_eq!(value["records"][0]["status"], "released");
}

#[tokio::test]
async fn expiration_is_checked_from_persistent_time_and_does_not_need_a_worker() {
    let mut host = Host::start(State::new()).await;
    host.hit().await;
    for (value, _) in host.state.values_mut() {
        value["records"][0]["expiresAtMs"] = json!(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64
                - 1
        );
    }
    host.model(
        5,
        "request-after-expiration",
        false,
        "key-test",
        "http_sse",
        INPUT,
    )
    .await;
    host.wait_next(5).await;
    host.finish_next(5, None).await;
    assert_eq!(host.result(5).await.0["response"], "response-5");
    let (_, value) = host
        .management(7, "GET", "history", "status=expired", None)
        .await;
    assert_eq!(value["records"][0]["status"], "expired");
}

#[tokio::test]
async fn normal_responses_preserve_large_terminal_frames_without_body_reads() {
    for transport in ["http_json", "http_sse", "web_socket"] {
        let mut host = Host::start(State::new()).await;
        for (index, bytes) in [262_143, 262_144, 262_145, 265_572, 274_599, 2 * 1024 * 1024]
            .into_iter()
            .enumerate()
        {
            let id = (index as u64) * 10 + 1;
            let request = format!("request-large-{index}");
            host.model(id, &request, false, "key-test", transport, INPUT)
                .await;
            host.wait_next(id).await;
            host.model(id + 2, &request, true, "key-test", transport, INPUT)
                .await;
            host.wait_next(id + 2).await;
            let mut value = json!({"type":"response.completed","response":{"id":"response-fixture","status":"completed","padding":""}});
            let prefix_bytes = serde_json::to_vec(&value).unwrap().len();
            value["response"]["padding"] = json!("x".repeat(bytes - prefix_bytes));
            let payload = serde_json::to_vec(&value).unwrap();
            assert_eq!(payload.len(), bytes);
            host.finish_next(id + 2, Some(VecDeque::from([Ok((payload.clone(), true))])))
                .await;
            assert_eq!(host.result(id + 2).await.0["body"]["kind"], "pass_through");
            host.end(id + 2).await;
            assert_eq!(
                host.bodies[&format!("body-{}", id + 2)][0]
                    .as_ref()
                    .unwrap()
                    .0,
                payload
            );
            host.finish_next(id, Some(VecDeque::from([Ok((payload.clone(), true))])))
                .await;
            assert_eq!(host.result(id).await.0["body"]["kind"], "pass_through");
            host.end(id).await;
            assert_eq!(
                host.bodies[&format!("body-{id}")][0].as_ref().unwrap().0,
                payload
            );
            host.completion(id + 4, &request, "succeeded").await;
        }
        assert_eq!((host.body_reads, host.lookups, host.puts), (0, 0, 0));
        assert!(host.chunks.is_empty() && host.records().is_empty());
    }
}

#[tokio::test]
async fn completed_failure_confirms_the_original_code_once_after_opaque_delivery() {
    let mut host = Host::start(State::new()).await;
    host.deferred().await;
    host.usage
        .insert("request-stream".into(), detail("cyber_policy"));
    host.call(
        7,
        Stage::Maintenance,
        "plugin.reconcile",
        None,
        json!({}),
        Vec::new(),
    )
    .await;
    host.result(7).await;
    assert_eq!(host.records().len(), 1);
    let writes = host.puts;
    host.completion(9, "request-stream", "incomplete").await;
    assert_eq!(host.puts, writes);
    host.model(11, "request-later", false, "key-test", "http_sse", INPUT)
        .await;
    assert_eq!(host.result(11).await.0["status"], 403);
    assert_eq!(host.body_reads, 0);
}

#[tokio::test]
async fn cancelling_initial_preparation_releases_the_call_without_creating_a_block() {
    let mut host = Host::start(State::new()).await;
    host.model(1, "request-cancelled", false, "key-test", "http_sse", INPUT)
        .await;
    host.wait_next(1).await;
    host.model(3, "request-cancelled", true, "key-test", "http_sse", INPUT)
        .await;
    host.wait_next(3).await;
    host.send(Message::Cancel { id: 3 }, Vec::new()).await;
    host.end(3).await;
    host.send(Message::Cancel { id: 1 }, Vec::new()).await;
    host.end(1).await;
    host.completion(5, "request-cancelled", "cancelled").await;
    assert!(host.records().is_empty() && host.pending().is_empty());
    host.model(
        7,
        "request-after-cancel",
        false,
        "key-test",
        "http_sse",
        INPUT,
    )
    .await;
    host.wait_next(7).await;
    host.finish_next(7, None).await;
    assert_eq!(host.result(7).await.0["response"], "response-7");
    host.end(7).await;
}

#[tokio::test]
async fn uncertain_or_capacity_failures_do_not_replay_a_state_write() {
    let mut host = Host::start(State::new()).await;
    host.fail_put = true;
    host.hit().await;
    assert_eq!(host.puts, 1);
    assert!(host.records().is_empty());
    host.fail_get = true;
    host.model(
        5,
        "request-storage-fault",
        false,
        "key-test",
        "http_sse",
        INPUT,
    )
    .await;
    assert_eq!(host.result(5).await.0["status"], 503);
    assert!(!host.next.contains_key(&5));
}

#[tokio::test]
async fn explicit_cas_conflicts_are_reloaded_without_duplicate_records() {
    let mut host = Host::start(State::new()).await;
    host.conflict_puts = 2;
    host.hit().await;
    assert_eq!((host.puts, host.records().len()), (3, 1));
}

#[tokio::test]
async fn unknown_settings_fields_are_rejected_before_native_management_is_called() {
    let mut host = Host::start(State::new()).await;
    let (response, _) = host.management(1, "POST", "settings", "", Some(json!({"expectedRevision":1,"configuration":{"blockSeconds":60,"unknownField":true}}))).await;
    assert_eq!(response["status"], 400);
}

#[tokio::test]
async fn a_request_that_waited_before_the_hit_is_rechecked_before_upstream_execution() {
    let mut host = Host::start(State::new()).await;
    host.model(1, "request-one", false, "key-test", "http_sse", INPUT)
        .await;
    host.wait_next(1).await;
    host.model(3, "request-waiting", false, "key-test", "http_sse", INPUT)
        .await;
    host.wait_next(3).await;
    host.model(5, "request-one", true, "key-test", "http_sse", INPUT)
        .await;
    host.wait_next(5).await;
    host.fail_next(5, cyber_error()).await;
    host.error(5).await;
    host.model(7, "request-waiting", true, "key-test", "http_sse", INPUT)
        .await;
    assert_eq!(host.result(7).await.0["status"], 403);
    assert!(!host.next.contains_key(&7));
}

#[tokio::test]
async fn missing_session_identity_does_not_create_a_key_wide_ban() {
    let mut host = Host::start(State::new()).await;
    let body =
        br#"{"model":"test-model","prompt_cache_key":"shared-cache","input":"DO-NOT-LOG-BODY"}"#;
    host.model(1, "request-no-session", false, "key-test", "http_sse", body)
        .await;
    host.wait_next(1).await;
    host.model(3, "request-no-session", true, "key-test", "http_sse", body)
        .await;
    host.wait_next(3).await;
    host.fail_next(3, cyber_error()).await;
    assert_eq!(host.error(3).await, cyber_error());
    host.fail_next(1, cyber_error()).await;
    host.error(1).await;
    assert_eq!(host.puts, 0);
    assert!(host.records().is_empty());
}

#[tokio::test]
async fn opaque_stream_failure_is_verified_before_the_next_upstream_request() {
    let mut host = Host::start(State::new()).await;
    host.deferred().await;
    let observed = host.pending()[0]["observedAtMs"].as_u64().unwrap();
    host.usage
        .insert("request-stream".into(), detail("cyber_policy"));
    host.model(7, "request-later", false, "key-test", "http_sse", INPUT)
        .await;
    assert_eq!(host.result(7).await.0["status"], 403);
    assert!(!host.next.contains_key(&7));
    assert!(host.pending().is_empty());
    assert_eq!(host.records()[0]["createdAtMs"], observed);
    assert_eq!(host.records()[0]["expiresAtMs"], observed + 3_600_000);
}

#[tokio::test]
async fn ordinary_stream_failure_is_cleared_and_the_same_session_can_continue() {
    let mut host = Host::start(State::new()).await;
    host.deferred().await;
    host.usage
        .insert("request-stream".into(), detail("fixture_unavailable"));
    host.model(7, "request-later", false, "key-test", "http_sse", INPUT)
        .await;
    host.wait_next(7).await;
    host.finish_next(7, None).await;
    assert_eq!(host.result(7).await.0["response"], "response-7");
    assert!(host.records().is_empty() && host.pending().is_empty());
}

#[tokio::test]
async fn an_unwritten_native_record_temporarily_holds_only_its_session() {
    let mut host = Host::start(State::new()).await;
    host.deferred().await;
    host.model(7, "request-later", false, "key-test", "http_sse", INPUT)
        .await;
    assert_eq!(host.result(7).await.0["status"], 503);
    assert!(!host.next.contains_key(&7));
    assert!(host.records().is_empty());
    host.model(
        9,
        "request-other-key",
        false,
        "other-key",
        "http_sse",
        INPUT,
    )
    .await;
    host.wait_next(9).await;
    host.finish_next(9, None).await;
    host.result(9).await;
}

#[tokio::test]
async fn maintenance_resolves_persistent_pending_after_process_restart() {
    let mut host = Host::start(State::new()).await;
    host.deferred().await;
    let state = host.state.clone();
    drop(host);
    let mut host = Host::start(state).await;
    host.usage
        .insert("request-stream".into(), detail("cyber_policy"));
    host.call(
        1,
        Stage::Maintenance,
        "plugin.reconcile",
        None,
        json!({}),
        Vec::new(),
    )
    .await;
    assert_eq!(host.result(1).await.0, json!({}));
    assert_eq!(host.records().len(), 1);
    assert!(host.pending().is_empty());
    let (_, value) = host.management(3, "GET", "history", "", None).await;
    assert_eq!(
        value["summary"]["active"],
        1,
        "history={value}, records={:?}",
        host.records()
    );
}

#[tokio::test]
async fn expired_unverified_failure_does_not_extend_or_permanently_lock_a_session() {
    let mut host = Host::start(State::new()).await;
    host.deferred().await;
    for (value, _) in host.state.values_mut() {
        value["pending"][0]["observedAtMs"] = json!(1);
    }
    host.model(7, "request-later", false, "key-test", "http_sse", INPUT)
        .await;
    host.wait_next(7).await;
    host.finish_next(7, None).await;
    host.result(7).await;
    assert!(host.pending().is_empty() && host.records().is_empty());
}

#[tokio::test]
async fn a_late_confirmed_hit_keeps_expired_history_without_a_new_block() {
    let mut host = Host::start(State::new()).await;
    host.deferred().await;
    for (value, _) in host.state.values_mut() {
        value["pending"][0]["observedAtMs"] = json!(1000);
    }
    host.usage
        .insert("request-stream".into(), detail("cyber_policy"));
    host.model(7, "request-later", false, "key-test", "http_sse", INPUT)
        .await;
    host.wait_next(7).await;
    host.finish_next(7, None).await;
    host.result(7).await;
    assert!(host.pending().is_empty());
    assert_eq!(host.records()[0]["createdAtMs"], 1000);
    assert_eq!(host.records()[0]["expiresAtMs"], 3_601_000);
    let (_, view) = host.management(9, "GET", "history", "", None).await;
    let record = &view["records"][0];
    assert_eq!(record["createdTimeDisplay"], "1970-01-01 08:00:01");
    assert_eq!(record["expiresTimeDisplay"], "1970-01-01 09:00:01");
    assert_eq!(record["createdTimeUtc"], "1970-01-01T00:00:01Z");
}

#[tokio::test]
async fn releasing_a_record_consumes_pending_failures_that_already_happened() {
    let mut host = Host::start(State::new()).await;
    host.deferred().await;
    let pending = host.pending()[0].clone();
    host.usage
        .insert("request-stream".into(), detail("cyber_policy"));
    let (_, view) = host.management(7, "GET", "history", "", None).await;
    let record = view["records"][0].clone();
    for (value, _) in host.state.values_mut() {
        let mut other = pending.clone();
        other["evidence"]["requestId"] = json!("request-concurrent");
        value["pending"] = json!([other]);
    }
    let (response, _) = host
        .management(
            9,
            "POST",
            "release",
            "",
            Some(json!({"sessionHash":record["sessionHash"],"recordId":record["id"]})),
        )
        .await;
    assert_eq!(response["status"], 200);
    assert!(host.pending().is_empty());
    assert!(host.records()[0]["releasedAtMs"].is_u64());
}

#[tokio::test]
async fn completion_uses_the_frozen_attempt_instead_of_gateway_error_text() {
    for outcome in ["failed", "incomplete", "cancelled", "succeeded"] {
        let mut host = Host::start(State::new()).await;
        host.model(1, "request-stream", false, "key-test", "http_sse", INPUT)
            .await;
        host.wait_next(1).await;
        host.model(3, "request-stream", true, "key-test", "http_sse", INPUT)
            .await;
        host.wait_next(3).await;
        host.finish_next(3, Some(VecDeque::new())).await;
        host.result(3).await;
        host.end(3).await;
        host.finish_next(1, Some(VecDeque::new())).await;
        host.result(1).await;
        host.end(1).await;
        host.usage
            .insert("request-stream".into(), detail("fixture_unavailable"));
        let mut event = completion("request-stream", outcome);
        event["data"]["terminal"]["error_code"] = json!("cyber_policy");
        host.call(
            5,
            Stage::Observation,
            "observer.observe",
            Some("request-stream"),
            event,
            Vec::new(),
        )
        .await;
        host.result(5).await;
        assert!(host.records().is_empty() && host.pending().is_empty());
        assert_eq!(
            host.lookups,
            usize::from(matches!(outcome, "failed" | "incomplete"))
        );
        let writes = host.puts;
        host.completion(7, "request-stream", "incomplete").await;
        assert_eq!(host.puts, writes, "完成观察应释放关联并忽略重复通知");
    }
}

#[tokio::test]
async fn a_finished_failed_request_is_blocked_from_its_original_completion_time() {
    let mut host = Host::start(State::new()).await;
    host.model(1, "request-stream", false, "key-test", "http_sse", INPUT)
        .await;
    host.wait_next(1).await;
    host.model(3, "request-stream", true, "key-test", "http_sse", INPUT)
        .await;
    host.wait_next(3).await;
    host.finish_next(3, Some(VecDeque::new())).await;
    host.result(3).await;
    host.end(3).await;
    host.finish_next(1, Some(VecDeque::new())).await;
    host.result(1).await;
    host.end(1).await;
    host.usage
        .insert("request-stream".into(), detail("cyber_policy"));
    let mut event = completion("request-stream", "failed");
    let completed_at = event["data"]["completed_at_ms"].as_u64().unwrap() - 5000;
    event["data"]["completed_at_ms"] = json!(completed_at);
    host.call(
        5,
        Stage::Observation,
        "observer.observe",
        Some("request-stream"),
        event,
        Vec::new(),
    )
    .await;
    host.result(5).await;
    assert_eq!(host.records().len(), 1);
    assert_eq!(host.records()[0]["createdAtMs"], completed_at);
    assert_eq!(host.records()[0]["expiresAtMs"], completed_at + 3_600_000);
    assert!(host.pending().is_empty());
    host.completion(7, "request-stream", "failed").await;
    assert_eq!(host.records().len(), 1);
}

#[tokio::test]
async fn a_delayed_completion_does_not_recreate_a_block_after_manual_release() {
    let mut host = Host::start(State::new()).await;
    host.model(1, "request-stream", false, "key-test", "http_sse", INPUT)
        .await;
    host.wait_next(1).await;
    host.model(3, "request-stream", true, "key-test", "http_sse", INPUT)
        .await;
    host.wait_next(3).await;
    host.finish_next(3, Some(VecDeque::new())).await;
    host.result(3).await;
    host.end(3).await;
    host.finish_next(1, Some(VecDeque::new())).await;
    host.result(1).await;
    host.end(1).await;

    host.model(5, "request-other-hit", false, "key-test", "http_sse", INPUT)
        .await;
    host.wait_next(5).await;
    host.model(7, "request-other-hit", true, "key-test", "http_sse", INPUT)
        .await;
    host.wait_next(7).await;
    host.fail_next(7, cyber_error()).await;
    host.error(7).await;
    host.fail_next(5, cyber_error()).await;
    host.error(5).await;
    let record = host.records().remove(0);
    let event = completion("request-stream", "incomplete");
    let (response, _) = host
        .management(
            9,
            "POST",
            "release",
            "",
            Some(json!({"sessionHash":record["sessionHash"],"recordId":record["id"]})),
        )
        .await;
    assert_eq!(response["status"], 200);
    host.usage
        .insert("request-stream".into(), detail("cyber_policy"));
    host.call(
        11,
        Stage::Observation,
        "observer.observe",
        Some("request-stream"),
        event,
        Vec::new(),
    )
    .await;
    host.result(11).await;
    assert!(host.pending().is_empty());
    assert_eq!(host.records().len(), 1);
    assert!(host.records()[0]["releasedAtMs"].is_u64());

    host.model(13, "request-new-hit", false, "key-test", "http_sse", INPUT)
        .await;
    host.wait_next(13).await;
    host.model(15, "request-new-hit", true, "key-test", "http_sse", INPUT)
        .await;
    host.wait_next(15).await;
    host.fail_next(15, cyber_error()).await;
    host.error(15).await;
    assert_eq!(host.records().len(), 2);
    assert!(
        host.records()
            .iter()
            .any(|record| record["requestId"] == "request-new-hit"
                && record["releasedAtMs"].is_null())
    );
}
