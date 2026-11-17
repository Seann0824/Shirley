//! Connector events into bots' task pools (`crate::connectors`).

use std::pin::Pin;
use std::sync::Arc;

use super::BotRuntimeManager;
use crate::connectors::Dispatch;

/// The manager as the connectors' way into the pools.
pub struct PoolDispatch(pub Arc<BotRuntimeManager>);

impl Dispatch for PoolDispatch {
    fn dispatch<'a>(
        &'a self,
        bot: &'a str,
        title: &'a str,
        brief: &'a str,
        source: &'a str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            self.0.pool.lock().await.enqueue_event(bot, title, brief, source)?;
            self.0.pool_pump().await
        })
    }
}

impl BotRuntimeManager {
    /// Rebuild every bot once idle: a connection came or went, and with it `reply_to_event`.
    pub async fn reload_all(&self) {
        let bots: Vec<String> = self.runtimes.lock().await.keys().cloned().collect();
        for bot in bots {
            self.reload_bot(&bot).await;
        }
    }
}
