use crate::config::ClusterNodeRole;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct NodeId(pub Uuid);

impl NodeId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_string(s: &str) -> Option<Self> {
        s.parse::<Uuid>().ok().map(Self)
    }

    pub fn from_u128(v: u128) -> Self {
        Self(Uuid::from_u128(v))
    }
}

impl Default for NodeId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for NodeId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.parse()?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    #[default]
    Connecting,
    HandshakeSent,
    HandshakeReceived,
    Healthy,
    Unhealthy,
    Disconnected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: NodeId,
    pub name: String,
    pub addr: SocketAddr,
    pub role: ClusterNodeRole,
    pub capabilities: Vec<String>,
    pub status: NodeStatus,
    /// Unix epoch seconds of last heartbeat
    pub last_heartbeat_secs: u64,
    pub load_score: u32,
}

impl NodeInfo {
    pub fn new(
        id: NodeId,
        name: String,
        addr: SocketAddr,
        role: ClusterNodeRole,
        capabilities: Vec<String>,
    ) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            id,
            name,
            addr,
            role,
            capabilities,
            status: NodeStatus::Connecting,
            last_heartbeat_secs: now,
            load_score: 0,
        }
    }

    pub fn is_healthy(&self) -> bool {
        matches!(self.status, NodeStatus::Healthy)
    }

    pub fn update_heartbeat(&mut self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.last_heartbeat_secs = now;
        self.status = NodeStatus::Healthy;
    }

    pub fn has_capability(&self, cap: &str) -> bool {
        self.capabilities.contains(&cap.to_string())
    }
}

impl PartialEq for NodeInfo {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for NodeInfo {}

impl PartialOrd for NodeInfo {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NodeInfo {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_display() {
        let id = NodeId::new();
        let s = id.to_string();
        assert!(!s.is_empty());
        assert_eq!(id, s.parse().unwrap());
    }

    #[test]
    fn node_info_ord_by_id() {
        let id1 = NodeId::new();
        let id2 = NodeId::new();

        let node1 = NodeInfo::new(
            id1,
            "node1".into(),
            "127.0.0.1:9090".parse().unwrap(),
            ClusterNodeRole::Worker,
            vec![],
        );
        let node2 = NodeInfo::new(
            id2,
            "node2".into(),
            "127.0.0.2:9090".parse().unwrap(),
            ClusterNodeRole::Worker,
            vec![],
        );

        let min = std::cmp::min(&node1, &node2);
        let max = std::cmp::max(&node1, &node2);

        assert_ne!(min.id, max.id);
    }
}
