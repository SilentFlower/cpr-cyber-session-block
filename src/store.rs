//! 通过确定性分片与宿主精确 CAS 原子保存封禁和历史，不维护第二份内存封禁表

use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::host::{
        STATE_GET, STATE_PUT, StateGetRequest, StateGetResult, StateMigrationChange,
        StateMigrationRequest, StateMigrationResult, StatePutRequest, StatePutResult,
    },
    client::{HostClient, SessionError},
};
use serde::{Deserialize, Serialize};
use serde_json::{from_value, to_value, to_vec};
use uuid::Uuid;

use crate::{fault, identity::Identity};

/// 固定分片可直接枚举，封禁与历史在同一状态对象内原子更新
pub const BUCKET_COUNT: usize = 64;
const MAXIMUM_BUCKET_BYTES: usize = 262_144;
const MAXIMUM_CAS_ATTEMPTS: usize = 8;

/// 一次真实上游命中对应的封禁记录，不保存对话正文、认证信息或完整会话 ID
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Record {
    pub id: String,
    pub session_hash: String,
    pub client_key_id: String,
    pub request_id: String,
    pub account_id: Option<String>,
    pub model: Option<String>,
    pub attempt_index: Option<u32>,
    pub transport: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub released_at_ms: Option<u64>,
    pub rejected_requests: u64,
    pub last_rejected_at_ms: Option<u64>,
}

impl Record {
    /// 根据持久事实与查询时间计算状态，不另存可过期的状态标签
    /// @param now_ms 当前 UTC 毫秒时间
    /// @return active、expired 或 released
    pub fn status(&self, now_ms: u64) -> &'static str {
        if self.released_at_ms.is_some() {
            "released"
        } else if self.expires_at_ms <= now_ms {
            "expired"
        } else {
            "active"
        }
    }
}

/// 进入上游尝试时冻结的最小请求归属
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Evidence {
    pub identity: Identity,
    pub request_id: String,
    pub account_id: Option<String>,
    pub model: Option<String>,
    pub attempt_index: Option<u32>,
    pub transport: String,
}

/// 保存失败请求的最小归属，等待原生请求记录核实真实上游错误码
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Pending {
    pub evidence: Evidence,
    pub observed_at_ms: u64,
    pub block_seconds: u32,
}

/// 同一会话及其所有封禁历史固定落在同一分片
#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bucket {
    pub records: Vec<Record>,
    #[serde(default)]
    pub pending: Vec<Pending>,
}

impl Bucket {
    /// 查询当前会话尚未解除且未到期的封禁
    /// @param session_hash 带 Key 隔离的会话摘要
    /// @param now_ms 当前 UTC 毫秒时间
    /// @return 当前有效封禁
    pub fn active(&self, session_hash: &str, now_ms: u64) -> Option<&Record> {
        self.records
            .iter()
            .rev()
            .find(|record| record.session_hash == session_hash && record.status(now_ms) == "active")
    }

    /// 登记首次真实命中，并发命中不延长已有封禁
    /// @param evidence 上游尝试的脱敏归属
    /// @param seconds 本次命中采用的封禁时长
    /// @param now_ms 命中发生时的 UTC 毫秒时间
    /// @return 当前会话的封禁记录
    pub fn register(&mut self, evidence: &Evidence, seconds: u32, now_ms: u64) -> Record {
        self.pending
            .retain(|pending| pending.evidence.request_id != evidence.request_id);
        // 同一触发事实不会因补偿读取或进程重启，在解除后再次建立封禁
        if let Some(existing) = self.records.iter().find(|record| {
            record.request_id == evidence.request_id
                && record.session_hash == evidence.identity.session_hash
        }) {
            return existing.clone();
        }
        if let Some(existing) = self.active(&evidence.identity.session_hash, now_ms) {
            return existing.clone();
        }
        let record = Record {
            id: Uuid::new_v4().to_string(),
            session_hash: evidence.identity.session_hash.clone(),
            client_key_id: evidence.identity.client_key_id.clone(),
            request_id: evidence.request_id.clone(),
            account_id: evidence.account_id.clone(),
            model: evidence.model.clone(),
            attempt_index: evidence.attempt_index,
            transport: evidence.transport.clone(),
            created_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(u64::from(seconds) * 1000),
            released_at_ms: None,
            rejected_requests: 0,
            last_rejected_at_ms: None,
        };
        self.records.push(record.clone());
        record
    }
}

/// 保存读取版本，所有写入必须在宿主事务中复验
struct Loaded {
    bucket: Bucket,
    version: Option<u64>,
}

/// 封禁与历史的宿主私有状态入口，不直接访问数据库
#[derive(Default)]
pub struct Repository;

impl Repository {
    async fn load(&self, host: &HostClient, index: usize) -> Result<Loaded, PluginFault> {
        let reply = host
            .call(
                STATE_GET,
                to_value(StateGetRequest {
                    namespace: "blocks".into(),
                    key: format!("bucket/{index:02x}"),
                })
                .map_err(encoding)?,
                Vec::new(),
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let result: StateGetResult = from_value(reply.result).map_err(encoding)?;
        match result.record {
            Some(record) => Ok(Loaded {
                bucket: from_value(record.value).map_err(encoding)?,
                version: Some(record.version),
            }),
            None => Ok(Loaded {
                bucket: Bucket::default(),
                version: None,
            }),
        }
    }

    async fn save(
        &self,
        host: &HostClient,
        index: usize,
        loaded: &Loaded,
    ) -> Result<(), PluginFault> {
        let value = to_value(&loaded.bucket).map_err(encoding)?;
        if to_vec(&value).map_err(encoding)?.len() > MAXIMUM_BUCKET_BYTES {
            return Err(fault(ErrorCode::Capacity, "封禁记录分片已达宿主配额"));
        }
        let reply = host
            .call(
                STATE_PUT,
                to_value(StatePutRequest {
                    namespace: "blocks".into(),
                    key: format!("bucket/{index:02x}"),
                    value,
                    expected_version: loaded.version,
                })
                .map_err(encoding)?,
                Vec::new(),
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let _: StatePutResult = from_value(reply.result).map_err(encoding)?;
        Ok(())
    }

    /// 保存真实上游命中，遇到明确 CAS 冲突才重读，不重放结果未知的写入
    /// @param host 当前父调用的宿主客户端
    /// @param evidence 本次尝试的脱敏证据
    /// @param seconds 封禁时长
    /// @param now_ms 命中时间
    /// @return 已持久化或并发请求已建立的有效封禁
    pub async fn register(
        &self,
        host: &HostClient,
        evidence: &Evidence,
        seconds: u32,
        now_ms: u64,
    ) -> Result<Record, PluginFault> {
        let index = bucket_index(&evidence.identity.session_hash)?;
        for _ in 0..MAXIMUM_CAS_ATTEMPTS {
            let mut loaded = self.load(host, index).await?;
            if let Some(record) = loaded
                .bucket
                .active(&evidence.identity.session_hash, now_ms)
                && !loaded
                    .bucket
                    .pending
                    .iter()
                    .any(|pending| pending.evidence.request_id == evidence.request_id)
            {
                return Ok(record.clone());
            }
            let record = loaded.bucket.register(evidence, seconds, now_ms);
            match self.save(host, index, &loaded).await {
                Ok(()) => return Ok(record),
                Err(error) if error.code == ErrorCode::Conflict => continue,
                Err(error) => return Err(error),
            }
        }
        Err(fault(ErrorCode::Conflict, "封禁记录并发更新未完成"))
    }

    /// 原子检查并记录一次本地拦截，已过期和已解除的记录不再阻断
    /// @param host 当前模型请求的宿主客户端
    /// @param identity 当前认证 Key 与明确会话的摘要
    /// @param now_ms 检查时间
    /// @return 有效封禁；计数不会延长到期时间
    pub async fn reject(
        &self,
        host: &HostClient,
        identity: &Identity,
        now_ms: u64,
    ) -> Result<Option<Record>, PluginFault> {
        let index = bucket_index(&identity.session_hash)?;
        for _ in 0..MAXIMUM_CAS_ATTEMPTS {
            let mut loaded = self.load(host, index).await?;
            let Some(record) = loaded.bucket.records.iter_mut().rev().find(|record| {
                record.session_hash == identity.session_hash && record.status(now_ms) == "active"
            }) else {
                return Ok(None);
            };
            record.rejected_requests = record.rejected_requests.saturating_add(1);
            record.last_rejected_at_ms = Some(now_ms);
            let record = record.clone();
            match self.save(host, index, &loaded).await {
                Ok(()) => return Ok(Some(record)),
                Err(error) if error.code == ErrorCode::Conflict => continue,
                Err(error) => return Err(error),
            }
        }
        Err(fault(ErrorCode::Conflict, "封禁记录并发更新未完成"))
    }

    /// 枚举固定分片读取完整历史，不依赖不可恢复的内存索引
    /// @param host 当前管理员调用的宿主客户端
    /// @return 所有已持久化记录
    pub async fn records(&self, host: &HostClient) -> Result<Vec<Record>, PluginFault> {
        let mut records = Vec::new();
        for index in 0..BUCKET_COUNT {
            records.extend(self.load(host, index).await?.bucket.records);
        }
        Ok(records)
    }

    /// 保存同步错误或完成观察中的待核实归属，不把通用错误当作 cyber_policy
    /// @param host 当前宿主调用
    /// @param pending 失败请求与当时冻结的时长
    /// @return 已确认持久化或明确的存储错误
    pub async fn defer(&self, host: &HostClient, pending: Pending) -> Result<(), PluginFault> {
        let index = bucket_index(&pending.evidence.identity.session_hash)?;
        for _ in 0..MAXIMUM_CAS_ATTEMPTS {
            let mut loaded = self.load(host, index).await?;
            // 异步完成通知可能晚于手动解除，复用解除时间排除当时已经发生的失败
            if loaded.bucket.records.iter().any(|record| {
                record.session_hash == pending.evidence.identity.session_hash
                    && record
                        .released_at_ms
                        .is_some_and(|released| pending.observed_at_ms <= released)
            }) {
                return Ok(());
            }
            if loaded
                .bucket
                .records
                .iter()
                .any(|record| record.request_id == pending.evidence.request_id)
                || loaded.bucket.pending.iter().any(|value| {
                    value.evidence.request_id == pending.evidence.request_id
                        && value.evidence.attempt_index == pending.evidence.attempt_index
                })
            {
                return Ok(());
            }
            loaded.bucket.pending.push(pending.clone());
            match self.save(host, index, &loaded).await {
                Ok(()) => return Ok(()),
                Err(error) if error.code == ErrorCode::Conflict => continue,
                Err(error) => return Err(error),
            }
        }
        Err(fault(ErrorCode::Conflict, "待核实记录并发更新未完成"))
    }

    /// 查询指定会话或所有分片的待核实请求
    /// @param host 当前宿主调用
    /// @param session_hash 有值时只读取该会话所属分片
    /// @return 不包含正文或凭据的待核实归属
    pub async fn pending(
        &self,
        host: &HostClient,
        session_hash: Option<&str>,
    ) -> Result<Vec<Pending>, PluginFault> {
        let indices = match session_hash {
            Some(hash) => vec![bucket_index(hash)?],
            None => (0..BUCKET_COUNT).collect(),
        };
        let mut pending = Vec::new();
        for index in indices {
            pending.extend(
                self.load(host, index)
                    .await?
                    .bucket
                    .pending
                    .into_iter()
                    .filter(|value| {
                        session_hash.is_none_or(|hash| value.evidence.identity.session_hash == hash)
                    }),
            );
        }
        Ok(pending)
    }

    /// 原子完成核实并移除待处理项，解除与迟到核实不能相互覆盖
    /// @param host 当前宿主调用
    /// @param pending 读取时的精确归属快照
    /// @param hit 原生记录是否明确命中 cyber_policy
    /// @param now_ms 当前时间，用于检查已有有效封禁
    /// @return 已完成核实；待处理项已变化时保持当前事实
    pub async fn settle(
        &self,
        host: &HostClient,
        pending: &Pending,
        hit: bool,
        now_ms: u64,
    ) -> Result<(), PluginFault> {
        let index = bucket_index(&pending.evidence.identity.session_hash)?;
        for _ in 0..MAXIMUM_CAS_ATTEMPTS {
            let mut loaded = self.load(host, index).await?;
            let Some(position) = loaded
                .bucket
                .pending
                .iter()
                .position(|value| value == pending)
            else {
                return Ok(());
            };
            loaded.bucket.pending.remove(position);
            if hit
                && loaded
                    .bucket
                    .active(&pending.evidence.identity.session_hash, now_ms)
                    .is_none()
            {
                loaded.bucket.register(
                    &pending.evidence,
                    pending.block_seconds,
                    pending.observed_at_ms,
                );
            }
            match self.save(host, index, &loaded).await {
                Ok(()) => return Ok(()),
                Err(error) if error.code == ErrorCode::Conflict => continue,
                Err(error) => return Err(error),
            }
        }
        Err(fault(ErrorCode::Conflict, "记录核实并发更新未完成"))
    }

    /// 提前解除指定封禁，稳定记录 ID 防止旧页面解除后来建立的新封禁
    /// @param host 当前管理员调用的宿主客户端
    /// @param session_hash 记录所属的会话摘要
    /// @param id 页面读取的精确记录 ID
    /// @param now_ms 操作发生时间
    /// @return 记录存在时返回解除后的记录，重复解除保持原事实
    pub async fn release(
        &self,
        host: &HostClient,
        session_hash: &str,
        id: &str,
        now_ms: u64,
    ) -> Result<Record, PluginFault> {
        let index = bucket_index(session_hash)?;
        for _ in 0..MAXIMUM_CAS_ATTEMPTS {
            let mut loaded = self.load(host, index).await?;
            let record = loaded
                .bucket
                .records
                .iter_mut()
                .find(|record| record.id == id && record.session_hash == session_hash)
                .ok_or_else(|| fault(ErrorCode::Rejected, "封禁记录不存在"))?;
            if record.status(now_ms) != "active" {
                return Ok(record.clone());
            }
            record.released_at_ms = Some(now_ms);
            let record = record.clone();
            // 解除也消费当时已发生的待核实失败，迟到的查询不能重新封禁旧事件
            loaded.bucket.pending.retain(|pending| {
                pending.evidence.identity.session_hash != session_hash
                    || pending.observed_at_ms > now_ms
            });
            match self.save(host, index, &loaded).await {
                Ok(()) => return Ok(record),
                Err(error) if error.code == ErrorCode::Conflict => continue,
                Err(error) => return Err(error),
            }
        }
        Err(fault(ErrorCode::Conflict, "封禁记录并发更新未完成"))
    }
}

/// 将原有封禁历史升级为可保存待核实归属的原子分片
/// @param request 宿主提供的迁移批次，不自行选择源记录
/// @return 保留全部历史并补齐空待处理列表
pub fn migrate(request: StateMigrationRequest) -> Result<StateMigrationResult, PluginFault> {
    if request.namespace != "blocks"
        || request.from_schema_version != 1
        || request.to_schema_version != 2
    {
        return Err(fault(ErrorCode::Unsupported, "不支持该状态版本迁移"));
    }
    let changes = request
        .records
        .into_iter()
        .map(|record| {
            let bucket: Bucket = from_value(record.value).map_err(encoding)?;
            Ok(StateMigrationChange::Replace {
                key: record.key,
                value: to_value(bucket).map_err(encoding)?,
            })
        })
        .collect::<Result<Vec<_>, PluginFault>>()?;
    Ok(StateMigrationResult { changes })
}

/// 由摘要定位固定分片，拒绝页面传入任意状态键
/// @param session_hash 64 位小写 SHA-256 摘要
/// @return 宿主私有状态中的分片序号
pub fn bucket_index(session_hash: &str) -> Result<usize, PluginFault> {
    if session_hash.len() != 64
        || !session_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(fault(ErrorCode::InvalidInput, "会话摘要无效"));
    }
    Ok(usize::from(
        u8::from_str_radix(&session_hash[..2], 16)
            .map_err(|_| fault(ErrorCode::InvalidInput, "会话摘要无效"))?,
    ) % BUCKET_COUNT)
}

fn encoding(_: serde_json::Error) -> PluginFault {
    fault(ErrorCode::Fault, "封禁记录格式无效")
}
