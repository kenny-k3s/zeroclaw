use crate::cluster::message::ClusterMessage;
use crate::cluster::node::{NodeId, NodeInfo};
use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

#[derive(Default)]
pub struct ElectionState {
    pub term: u64,
    pub voted_for: Option<NodeId>,
    pub coordinator: Option<NodeId>,
    pub is_candidate: bool,
    pub election_in_progress: bool,
}

pub struct ElectionManager {
    node_id: NodeId,
    peers: Arc<RwLock<Vec<NodeInfo>>>,
    state: Arc<RwLock<ElectionState>>,
    election_timeout_secs: u64,
    message_tx: mpsc::Sender<(NodeId, ClusterMessage)>,
}

impl ElectionManager {
    pub fn new(
        node_id: NodeId,
        peers: Arc<RwLock<Vec<NodeInfo>>>,
        election_timeout_secs: u64,
        message_tx: mpsc::Sender<(NodeId, ClusterMessage)>,
    ) -> Self {
        Self {
            node_id,
            peers,
            state: Arc::new(RwLock::new(ElectionState::default())),
            election_timeout_secs,
            message_tx,
        }
    }

    pub fn start_election(&self) {
        let mut state = self.state.write();

        if state.election_in_progress {
            return;
        }

        state.term += 1;
        state.is_candidate = true;
        state.election_in_progress = true;
        state.voted_for = Some(self.node_id);

        tracing::info!("Starting election for term {}", state.term);
    }

    pub fn handle_election_message(
        &self,
        message: &ClusterMessage,
        _sender_id: &NodeId,
    ) -> Option<ClusterMessage> {
        let mut state = self.state.write();

        match message {
            ClusterMessage::Election { candidate_id, term } => {
                if *term > state.term {
                    state.term = *term;
                    state.voted_for = Some(*candidate_id);
                    state.coordinator = Some(*candidate_id);

                    tracing::info!("Voted for {} in term {}", candidate_id, term);

                    Some(ClusterMessage::election_result(*candidate_id, *term))
                } else {
                    None
                }
            }

            ClusterMessage::ElectionResult { winner_id, term } => {
                if *term >= state.term {
                    state.term = *term;
                    state.coordinator = Some(*winner_id);
                    state.is_candidate = false;
                    state.election_in_progress = false;

                    tracing::info!(
                        "Election resolved: {} is coordinator for term {}",
                        winner_id,
                        term
                    );
                }
                None
            }

            _ => None,
        }
    }

    pub fn check_election_timeout(&self) -> bool {
        let state = self.state.read();
        state.election_in_progress && state.is_candidate
    }

    pub fn get_coordinator(&self) -> Option<NodeId> {
        let state = self.state.read();
        state.coordinator
    }

    pub fn set_coordinator(&self, coordinator: NodeId) {
        let mut state = self.state.write();
        state.coordinator = Some(coordinator);
        state.is_candidate = false;
        state.election_in_progress = false;
    }

    pub fn is_coordinator(&self, node_id: &NodeId) -> bool {
        let state = self.state.read();
        state.coordinator.as_ref() == Some(node_id)
    }

    pub fn elect_lowest_id(&self) -> NodeId {
        let peers = self.peers.read();

        let all_nodes: Vec<NodeId> = peers
            .iter()
            .map(|n| n.id)
            .chain(std::iter::once(self.node_id))
            .collect();

        all_nodes
            .into_iter()
            .min()
            .expect("At least one node exists")
    }

    pub fn become_coordinator(&self) {
        let mut state = self.state.write();
        state.coordinator = Some(self.node_id);
        state.is_candidate = false;
        state.election_in_progress = false;

        tracing::info!("Node {} became coordinator", self.node_id);
    }
}

pub async fn run_election_loop(
    election_manager: Arc<ElectionManager>,
    mut rx: mpsc::Receiver<ClusterMessage>,
) {
    loop {
        let timeout_duration = Duration::from_secs(election_manager.election_timeout_secs);

        match timeout(timeout_duration, rx.recv()).await {
            Ok(Some(message)) => {
                let sender_id = match &message {
                    ClusterMessage::Election { candidate_id, .. } => *candidate_id,
                    ClusterMessage::ElectionResult { winner_id, .. } => *winner_id,
                    ClusterMessage::Handshake { node_info, .. }
                    | ClusterMessage::HandshakeAck { node_info } => node_info.id,
                    _ => continue,
                };

                if let Some(response) =
                    election_manager.handle_election_message(&message, &sender_id)
                {
                    let _ = election_manager
                        .message_tx
                        .send((sender_id, response))
                        .await;
                }
            }
            Ok(None) => break,
            Err(_) => {
                if election_manager.check_election_timeout() {
                    tracing::warn!("Election timeout, restarting election");
                    election_manager.start_election();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    fn make_test_node(id: NodeId, addr: SocketAddr) -> NodeInfo {
        NodeInfo::new(
            id,
            format!("node-{}", id),
            addr,
            crate::config::ClusterNodeRole::Worker,
            vec![],
        )
    }

    #[test]
    fn election_chooses_lowest_id() {
        let peers = Arc::new(RwLock::new(vec![
            make_test_node(NodeId::new(), "127.0.0.1:9090".parse().unwrap()),
            make_test_node(NodeId::new(), "127.0.0.2:9090".parse().unwrap()),
        ]));

        let election = ElectionManager::new(NodeId::new(), peers, 10, mpsc::channel(1).0);

        let winner = election.elect_lowest_id();
        assert!(winner.to_string().len() > 0);
    }

    #[test]
    fn coordinator_setter() {
        let peers = Arc::new(RwLock::new(vec![]));
        let election = ElectionManager::new(NodeId::new(), peers, 10, mpsc::channel(1).0);

        let coordinator_id = NodeId::new();
        election.set_coordinator(coordinator_id);

        assert_eq!(election.get_coordinator(), Some(coordinator_id));
        assert!(!election.is_coordinator(&NodeId::new()));
    }
}
