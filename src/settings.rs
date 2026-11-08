//! 复用宿主实例更新合同保存参数，不在插件私有状态另存配置

use cpr_cyber_session_block::{fault, native, policy::Config};
use gateway_plugin_sdk::{ErrorCode, PluginFault, client::HostClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, from_value, json};

/// 只允许操作握手确认的当前实例和制品
pub(crate) struct SettingsControl {
    instance_id: String,
    artifact_sha256: String,
}

/// 原生实例更新所需的当前事实，省略敏感配置内容
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Instance {
    id: String,
    name: String,
    artifact_sha256: String,
    enabled: bool,
    configuration: Config,
    bindings: Vec<Value>,
    revision: u64,
}

/// 页面必须带读取时的版本提交完整参数
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SaveSettings {
    expected_revision: u64,
    configuration: RequiredConfig,
}

/// 参数保存必须显式提供时长，不能靠默认值掩盖遗漏
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequiredConfig {
    block_seconds: u32,
}

/// 当前实例的设置快照
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SettingsView {
    revision: u64,
    configuration: Config,
}

impl SettingsControl {
    /// 固定管理对象，页面不能传入其他实例或制品
    /// @param instance_id 握手确认的实例 ID
    /// @param artifact_sha256 当前制品摘要
    /// @return 当前实例的设置入口
    pub(crate) fn new(instance_id: String, artifact_sha256: String) -> Self {
        Self {
            instance_id,
            artifact_sha256,
        }
    }

    async fn instance(&self, host: &HostClient) -> Result<Instance, PluginFault> {
        let data = native::request(host, "GET", "/api/admin/plugins/instances", None)
            .await?
            .ok_or_else(unavailable)?;
        let value = data
            .as_array()
            .and_then(|items| {
                items.iter().find(|item| {
                    item.get("id").and_then(Value::as_str) == Some(self.instance_id.as_str())
                })
            })
            .ok_or_else(conflict)?;
        let instance: Instance = from_value(value.clone()).map_err(|_| unavailable())?;
        if !instance.enabled || instance.artifact_sha256 != self.artifact_sha256 {
            return Err(conflict());
        }
        Ok(instance)
    }

    /// 读取当前有效参数，沿用原生 revision
    /// @param host 当前管理员调用的受管客户端
    /// @return 参数与版本快照
    pub(crate) async fn view(&self, host: &HostClient) -> Result<SettingsView, PluginFault> {
        let instance = self.instance(host).await?;
        Ok(SettingsView {
            revision: instance.revision,
            configuration: instance.configuration,
        })
    }

    /// 仅替换参数，完整保留宿主已有绑定、名称和启用状态
    /// @param host 当前管理员调用的受管客户端
    /// @param input 带精确版本的参数
    /// @return 宿主已确认的保存结果，不自动重试结果未知的更新
    pub(crate) async fn save(
        &self,
        host: &HostClient,
        input: SaveSettings,
    ) -> Result<Value, PluginFault> {
        let configuration = Config {
            block_seconds: input.configuration.block_seconds,
        };
        configuration
            .validate()
            .map_err(|message| fault(ErrorCode::InvalidInput, message))?;
        let instance = self.instance(host).await?;
        if instance.revision != input.expected_revision {
            return Err(conflict());
        }
        native::request(host, "POST", "/api/admin/plugins/instances/update", Some(json!({"id":instance.id,"instance":{"expectedRevision":input.expected_revision,"name":instance.name,"artifactSha256":instance.artifact_sha256,"enabled":instance.enabled,"configuration":configuration,"bindings":instance.bindings}}))).await?.ok_or_else(unavailable)?;
        Ok(json!({"saved":true}))
    }
}

fn conflict() -> PluginFault {
    fault(ErrorCode::Conflict, "插件设置已变化，请重新读取后再保存")
}
fn unavailable() -> PluginFault {
    fault(ErrorCode::Uncertain, "无法确认操作结果，请刷新页面核对")
}
