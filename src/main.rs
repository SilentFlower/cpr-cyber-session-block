//! 启动独立插件进程，通过公开 SDK 注册准入、完成观察、重试与管理能力

mod app;
mod management;
mod settings;

use std::{error::Error, sync::Arc};

use cpr_cyber_session_block::{policy::Config, store::migrate};
use gateway_plugin_sdk::{
    Manifest,
    client::{
        Empty, PluginBuilder, PluginSession, RequestCall, SessionConfig, TypedReply, methods,
    },
};

use app::App;
use settings::SettingsControl;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let session = PluginSession::accept(
        tokio::io::stdin(),
        tokio::io::stdout(),
        SessionConfig::default(),
    )
    .await?;
    let manifest = Manifest::from_author_slice(include_bytes!("../plugin.json"))?;
    if session.handshake().plugin_id != manifest.plugin_id()?
        || session.handshake().contributes != manifest.contributes
    {
        return Err("插件握手身份不匹配".into());
    }
    let config: Config = serde_json::from_value(session.handshake().configuration.clone())?;
    config.validate()?;
    let app = Arc::new(App::new(
        config,
        SettingsControl::new(
            session.handshake().instance_id.clone(),
            session.handshake().artifact_sha256.clone(),
        ),
    ));
    let middleware = Arc::clone(&app);
    let retry = Arc::clone(&app);
    let maintenance = Arc::clone(&app);
    let observer = Arc::clone(&app);
    let plugin = PluginBuilder::from_json(include_bytes!("../plugin.json"))?
        .middleware(move |call: RequestCall| {
            let app = Arc::clone(&middleware);
            async move { app.middleware(call).await }
        })?
        .on(methods::RETRY_DECISION, move |call| {
            let app = Arc::clone(&retry);
            async move { app.retry(call).await }
        })?
        .on(methods::OBSERVE, move |call| {
            let app = Arc::clone(&observer);
            async move { app.observe(call).await }
        })?
        .on(methods::RECONCILE, move |call| {
            let app = Arc::clone(&maintenance);
            async move {
                app.reconcile(&call.host, None, None).await?;
                Ok(TypedReply::new(Empty {}))
            }
        })?
        .on(methods::STATE_MIGRATE, |call| async move {
            Ok(TypedReply::new(migrate(call.request)?))
        })?
        .management(management::registration(), move |call| {
            let app = Arc::clone(&app);
            async move { management::handle(&app, call).await }
        })?
        .build()?;
    session.run(plugin).await?;
    Ok(())
}
