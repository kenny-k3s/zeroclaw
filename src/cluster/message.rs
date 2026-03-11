use crate::cluster::node::{NodeId, NodeInfo};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClusterMessage {
    Handshake {
        node_info: NodeInfo,
        token: String,
    },
    HandshakeAck {
        node_info: NodeInfo,
    },
    HandshakeNack {
        reason: String,
    },
    Heartbeat {
        node_id: NodeId,
        load_score: u32,
    },
    Election {
        candidate_id: NodeId,
        term: u64,
    },
    ElectionResult {
        winner_id: NodeId,
        term: u64,
    },
    TaskRequest {
        id: Uuid,
        task_type: TaskType,
        payload: String,
        source: NodeId,
        reply_target: Option<NodeId>,
    },
    TaskResponse {
        id: Uuid,
        result: String,
        source: NodeId,
        error: Option<String>,
    },
    DelegateCrossNode {
        id: Uuid,
        agent_name: String,
        prompt: String,
        context: Option<String>,
        source: NodeId,
        target: Option<NodeId>,
    },
    DelegateResponse {
        id: Uuid,
        result: String,
        source: NodeId,
        error: Option<String>,
    },
    StateQuery {
        query_type: StateQueryType,
    },
    StateSync {
        agent_count: usize,
        memory_count: usize,
    },
    PeerList {
        peers: Vec<NodeInfo>,
    },
    PeerJoined {
        node_info: NodeInfo,
    },
    PeerLeft {
        node_id: NodeId,
    },
    Ping,
    Pong,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    Chat,
    Agentic,
    MemoryRecall,
    MemoryStore,
    Custom(String),
}

impl TaskType {
    pub fn required_capability(&self) -> Option<&str> {
        match self {
            TaskType::MemoryRecall | TaskType::MemoryStore => Some("memory"),
            TaskType::Chat | TaskType::Agentic | TaskType::Custom(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateQueryType {
    AgentCount,
    MemoryCount,
    Full,
}

impl ClusterMessage {
    pub fn handshake(node_info: NodeInfo, token: String) -> Self {
        Self::Handshake { node_info, token }
    }

    pub fn handshake_ack(node_info: NodeInfo) -> Self {
        Self::HandshakeAck { node_info }
    }

    pub fn handshake_nack(reason: impl Into<String>) -> Self {
        Self::HandshakeNack {
            reason: reason.into(),
        }
    }

    pub fn heartbeat(node_id: NodeId, load_score: u32) -> Self {
        Self::Heartbeat {
            node_id,
            load_score,
        }
    }

    pub fn election(candidate_id: NodeId, term: u64) -> Self {
        Self::Election { candidate_id, term }
    }

    pub fn election_result(winner_id: NodeId, term: u64) -> Self {
        Self::ElectionResult { winner_id, term }
    }

    pub fn task_request(task_type: TaskType, payload: String, source: NodeId) -> Self {
        Self::TaskRequest {
            id: Uuid::new_v4(),
            task_type,
            payload,
            source,
            reply_target: None,
        }
    }

    pub fn task_response(id: Uuid, result: String, source: NodeId) -> Self {
        Self::TaskResponse {
            id,
            result,
            source,
            error: None,
        }
    }

    pub fn task_response_error(id: Uuid, error: String, source: NodeId) -> Self {
        Self::TaskResponse {
            id,
            result: String::new(),
            source,
            error: Some(error),
        }
    }

    pub fn delegate_cross_node(
        agent_name: String,
        prompt: String,
        context: Option<String>,
        source: NodeId,
        target: Option<NodeId>,
    ) -> Self {
        Self::DelegateCrossNode {
            id: Uuid::new_v4(),
            agent_name,
            prompt,
            context,
            source,
            target,
        }
    }

    pub fn ping() -> Self {
        Self::Ping
    }

    pub fn pong() -> Self {
        Self::Pong
    }

    pub fn peer_joined(node_info: NodeInfo) -> Self {
        Self::PeerJoined { node_info }
    }

    pub fn peer_left(node_id: NodeId) -> Self {
        Self::PeerLeft { node_id }
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn decode(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClusterNodeRole;

    #[test]
    fn message_encode_decode() {
        let msg = ClusterMessage::ping();
        let encoded = msg.encode();
        let decoded = ClusterMessage::decode(&encoded).unwrap();
        assert!(matches!(decoded, ClusterMessage::Ping));
    }

    #[test]
    fn handshake_message() {
        let node_info = NodeInfo::new(
            NodeId::new(),
            "test".into(),
            "127.0.0.1:9090".parse().unwrap(),
            ClusterNodeRole::Worker,
            vec!["test".into()],
        );

        let msg = ClusterMessage::handshake(node_info, "token".into());
        let encoded = msg.encode();
        let decoded = ClusterMessage::decode(&encoded).unwrap();

        assert!(matches!(decoded, ClusterMessage::Handshake { .. }));
    }
}
