use crate::cluster::message::ClusterMessage;
use crate::cluster::node::{NodeId, NodeInfo};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub trait ClusterNode: Send + Sync {
    fn node_id(&self) -> NodeId;

    fn node_name(&self) -> &str;

    async fn connect(&self, peer_addr: &str) -> Result<()>;

    async fn disconnect(&self) -> Result<()>;

    async fn send(&self, target: &NodeId, message: ClusterMessage) -> Result<()>;

    async fn broadcast(&self, message: ClusterMessage) -> Result<()>;

    async fn subscribe(&self, handler: Arc<dyn ClusterMessageHandler>) -> Result<()>;

    fn get_peers(&self) -> Vec<NodeInfo>;

    fn get_coordinator(&self) -> Option<NodeInfo>;

    fn is_coordinator(&self) -> bool;

    async fn wait_for_coordinator(&self, timeout_secs: u64) -> Result<NodeInfo>;
}

#[async_trait]
pub trait ClusterMessageHandler: Send + Sync {
    async fn handle(&self, source: &NodeId, message: ClusterMessage);

    fn name(&self) -> &str {
        "unnamed-handler"
    }
}

pub trait ClusterDelegate: Send + Sync {
    fn delegate_task(
        &self,
        agent_name: &str,
        prompt: &str,
        context: Option<&str>,
        target_node: Option<&NodeId>,
    ) -> impl std::future::Future<Output = Result<String>> + Send;
}
