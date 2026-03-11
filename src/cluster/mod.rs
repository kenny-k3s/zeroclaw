pub mod discovery;
pub mod election;
pub mod message;
pub mod node;
pub mod traits;
pub mod ws_transport;

pub use election::ElectionManager;
pub use message::{ClusterMessage, StateQueryType, TaskType};
pub use node::{NodeId, NodeInfo, NodeStatus};
pub use traits::{ClusterMessageHandler, ClusterNode};
pub use ws_transport::WsClusterTransport;

use crate::config::{ClusterConfig, ClusterNodeRole};
use anyhow::{bail, Result};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

pub struct Cluster {
    node_id: NodeId,
    node_name: String,
    config: ClusterConfig,
    role: ClusterNodeRole,
    coordinator_id: Option<NodeId>,

    peers: Arc<RwLock<HashMap<NodeId, NodeInfo>>>,
    message_tx: broadcast::Sender<(NodeId, ClusterMessage)>,

    running: Arc<RwLock<bool>>,
}

impl Cluster {
    pub fn new(config: &ClusterConfig) -> Result<Self> {
        if !config.enabled {
            bail!("Cluster is not enabled");
        }

        let node_id = NodeId::new();
        let node_name = config.node_name.clone().unwrap_or_else(|| {
            hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|_| format!("node-{}", Uuid::new_v4())[..8].to_string())
        });

        let (message_tx, _) = broadcast::channel(1024);

        Ok(Self {
            node_id,
            node_name,
            config: config.clone(),
            role: config.static_role.unwrap_or(ClusterNodeRole::Worker),
            coordinator_id: None,
            peers: Arc::new(RwLock::new(HashMap::new())),
            message_tx,
            running: Arc::new(RwLock::new(false)),
        })
    }

    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    pub fn role(&self) -> ClusterNodeRole {
        self.role
    }

    pub fn is_coordinator(&self) -> bool {
        self.role == ClusterNodeRole::Coordinator
    }

    pub fn get_peers(&self) -> Vec<NodeInfo> {
        self.peers.read().values().cloned().collect()
    }

    pub fn peer_count(&self) -> usize {
        self.peers.read().len()
    }

    pub fn get_coordinator(&self) -> Option<NodeInfo> {
        self.coordinator_id
            .and_then(|id| self.peers.read().get(&id).cloned())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<(NodeId, ClusterMessage)> {
        self.message_tx.subscribe()
    }

    pub fn add_peer(&self, info: NodeInfo) {
        self.peers.write().insert(info.id, info);
    }

    pub fn remove_peer(&self, node_id: &NodeId) {
        self.peers.write().remove(node_id);
    }

    pub fn set_coordinator(&mut self, node_id: NodeId) {
        self.coordinator_id = Some(node_id);

        if node_id == self.node_id {
            self.role = ClusterNodeRole::Coordinator;
        }
    }

    pub fn broadcast(&self, message: ClusterMessage) {
        let _ = self.message_tx.send((self.node_id, message));
    }

    pub fn is_running(&self) -> bool {
        *self.running.read()
    }

    pub fn set_running(&self, running: bool) {
        *self.running.write() = running;
    }
}

pub async fn create_cluster(config: &ClusterConfig) -> Result<Option<Arc<Cluster>>> {
    if !config.enabled {
        return Ok(None);
    }

    let cluster = Arc::new(Cluster::new(config)?);
    Ok(Some(cluster))
}

pub async fn run_cluster(config: &ClusterConfig) -> Result<()> {
    let Some(cluster) = create_cluster(config).await? else {
        tracing::info!("Cluster mode disabled");
        return Ok(());
    };

    let bind_addr: SocketAddr = format!("{}:{}", config.bind_host, config.bind_port).parse()?;

    tracing::info!(
        "Starting cluster node: {} ({})",
        cluster.node_name(),
        cluster.node_id()
    );
    tracing::info!("Cluster bind address: {}", bind_addr);
    tracing::info!("Static role: {:?}", cluster.role());

    let transport =
        WsClusterTransport::new(config, cluster.node_id(), cluster.node_name().to_string())?;

    transport.start_server().await?;

    if !config.peers.is_empty() {
        tracing::info!("Connecting to static peers: {:?}", config.peers);
        transport.connect_to_peers(&config.peers).await?;
    }

    let transport_for_discovery = transport.clone();
    if config.discovery_enabled {
        let discovery_interval = config.discovery_interval_secs.max(1);
        tracing::info!(
            "Starting UDP peer discovery (interval: {}s)",
            discovery_interval
        );

        let discovery = discovery::DiscoveryService::new(config, cluster.node_name().to_string());
        let transport_clone: Arc<WsClusterTransport> = Arc::new(transport_for_discovery);
        tokio::spawn(async move {
            if let Err(e) = discovery.start(transport_clone).await {
                tracing::error!("Discovery service error: {}", e);
            }
        });
    }

    cluster.set_running(true);

    let heartbeat_interval = config.heartbeat_interval_secs.max(5);
    let transport_clone = transport.clone();
    tokio::spawn(async move {
        transport_clone.run_heartbeat(heartbeat_interval).await;
    });

    tokio::signal::ctrl_c().await?;
    cluster.set_running(false);

    tracing::info!("Cluster shutdown complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_disabled_returns_none() {
        let config = ClusterConfig::default();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let res = rt.block_on(create_cluster(&config)).unwrap();
        assert!(res.is_none());
    }

    #[tokio::test]
    async fn cluster_enabled_creates_instance() {
        let mut config = ClusterConfig::default();
        config.enabled = true;
        config.node_name = Some("test-node".into());

        let cluster = create_cluster(&config).await.unwrap().unwrap();

        assert_eq!(cluster.node_name(), "test-node");
        assert!(!cluster.is_coordinator());
        assert_eq!(cluster.peer_count(), 0);
    }
}
