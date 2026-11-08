//! 仅依据结构化 cyber_policy 识别上游拒绝，不匹配通用故障文案

use gateway_plugin_sdk::PluginFault;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 当前插件实例的封禁参数
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    pub block_seconds: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            block_seconds: 3600,
        }
    }
}

impl Config {
    /// 检查封禁时长，避免溢出或意外永久封禁
    /// @return 参数合法时成功，否则返回中文提示
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=2_592_000).contains(&self.block_seconds) {
            return Err("封禁时长须为 1～2592000 秒".into());
        }
        Ok(())
    }
}

fn cyber(code: &str) -> bool {
    code.trim().eq_ignore_ascii_case("cyber_policy")
}

/// 只读取实际 Provider 的上游错误码，不根据 kind、状态或 message 扩大封禁
/// @param error next 返回的完整错误快照
/// @return Provider 明确报告 cyber_policy 时返回 true
pub fn fault_hit(error: &PluginFault) -> bool {
    error
        .details
        .as_ref()
        .and_then(|details| details.pointer("/provider/code"))
        .and_then(Value::as_str)
        .is_some_and(cyber)
}
