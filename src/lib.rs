//! Cyber 会话身份、精确错误识别与持久封禁规则

pub mod identity;
pub mod ledger;
pub mod native;
pub mod policy;
pub mod store;

use gateway_plugin_sdk::{ErrorCode, PluginFault};

/// 创建不包含外部正文或凭据的插件错误
/// @param code 宿主可识别的错误分类
/// @param message 当前出口使用的安全提示
/// @return 可跨进程返回的错误
pub fn fault(code: ErrorCode, message: impl Into<String>) -> PluginFault {
    PluginFault::new(code, message)
}
