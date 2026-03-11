use crate::cluster::message::ClusterMessage;
use crate::cluster::node::{NodeId, NodeInfo};
use crate::cluster::traits::ClusterMessageHandler;
use crate::config::ClusterConfig;
use anyhow::{bail, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio::time::{interval, Duration};
use tokio_tungstenite::{
    accept_async, connect_async, tungstenite::protocol::Message, WebSocketStream,
};

#[derive(Clone)]
pub struct WsClusterTransport {
    node_id: NodeId,
    node_name: String,
    node_token: String,
    capabilities: Vec<String>,
    bind_addr: SocketAddr,

    peers: Arc<RwLock<HashMap<NodeId, NodeInfo>>>,
    peer_senders: Arc<RwLock<HashMap<NodeId, mpsc::Sender<String>>>>,

    message_tx: broadcast::Sender<(NodeId, ClusterMessage)>,
    running: Arc<RwLock<bool>>,
}

impl WsClusterTransport {
    pub fn new(config: &ClusterConfig, node_id: NodeId, node_name: String) -> Result<Self> {
        let node_token = config.node_token.clone().unwrap_or_default();
        let bind_addr: SocketAddr = format!("{}:{}", config.bind_host, config.bind_port).parse()?;

        let (message_tx, _) = broadcast::channel(1024);

        let peers = Arc::new(RwLock::new(HashMap::new()));
        let peer_senders = Arc::new(RwLock::new(HashMap::new()));
        let running = Arc::new(RwLock::new(false));

        let transport = Self {
            node_id,
            // Keep provided node_name (not token)
            node_name,
            node_token,
            capabilities: config.capabilities.clone(),
            bind_addr,
            peers: peers.clone(),
            peer_senders: peer_senders.clone(),
            message_tx,
            running,
        };

        Ok(transport)
    }

    pub async fn start_server(&self) -> Result<()> {
        let listener = TcpListener::bind(self.bind_addr).await?;
        tracing::info!("Cluster WebSocket server listening on {}", self.bind_addr);

        *self.running.write() = true;

        let running = self.running.clone();
        let peers = self.peers.clone();
        let peer_senders = self.peer_senders.clone();
        let message_tx = self.message_tx.clone();
        let node_id = self.node_id;
        let node_name = self.node_name.clone();
        let node_token = self.node_token.clone();
        let capabilities = self.capabilities.clone();
        tokio::spawn(async move {
            while *running.read() {
                if let Ok((stream, addr)) = listener.accept().await {
                    let peers = peers.clone();
                    let peer_senders = peer_senders.clone();
                    let message_tx = message_tx.clone();
                    let node_id = node_id;
                    let node_name = node_name.clone();
                    let node_token = node_token.clone();
                    let capabilities = capabilities.clone();

                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_incoming_connection(
                            stream,
                            addr,
                            peers,
                            peer_senders,
                            message_tx,
                            node_id,
                            node_name,
                            node_token,
                            capabilities,
                        )
                        .await
                        {
                            tracing::error!("Connection handler error: {}", e);
                        }
                    });
                }
            }
        });

        Ok(())
    }

    async fn handle_incoming_connection(
        stream: TcpStream,
        addr: SocketAddr,
        peers: Arc<RwLock<HashMap<NodeId, NodeInfo>>>,
        peer_senders: Arc<RwLock<HashMap<NodeId, mpsc::Sender<String>>>>,
        message_tx: broadcast::Sender<(NodeId, ClusterMessage)>,
        our_node_id: NodeId,
        our_node_name: String,
        our_token: String,
        our_capabilities: Vec<String>,
    ) -> Result<()> {
        let ws = accept_async(stream).await?;
        Self::handle_websocket(
            ws,
            addr,
            peers,
            peer_senders,
            message_tx,
            our_node_id,
            our_node_name,
            our_token,
            our_capabilities,
            true,
        )
        .await
    }

    pub async fn connect_to_peer(&self, addr: &str) -> Result<()> {
        let addr_owned = addr.to_string();
        let url = format!("ws://{}/ws/cluster", addr_owned);
        tracing::info!("Connecting to peer: {}", url);

        let (ws, response) = connect_async(&url).await?;

        tracing::debug!(
            "Peer WebSocket handshake response status: {}",
            response.status()
        );

        let peer_addr: SocketAddr = addr_owned.parse()?;

        let peers = self.peers.clone();
        let peer_senders = self.peer_senders.clone();
        let message_tx = self.message_tx.clone();
        let node_id = self.node_id;
        let node_name = self.node_name.clone();
        let node_token = self.node_token.clone();
        let capabilities = self.capabilities.clone();

        tokio::spawn(async move {
            if let Err(e) = Self::handle_websocket(
                ws,
                peer_addr,
                peers,
                peer_senders,
                message_tx,
                node_id,
                node_name,
                node_token,
                capabilities,
                false,
            )
            .await
            {
                tracing::error!("Peer connection error: {}", e);
            }
        });

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_websocket<S>(
        ws: WebSocketStream<S>,
        _addr: SocketAddr,
        peers: Arc<RwLock<HashMap<NodeId, NodeInfo>>>,
        peer_senders: Arc<RwLock<HashMap<NodeId, mpsc::Sender<String>>>>,
        message_tx: broadcast::Sender<(NodeId, ClusterMessage)>,
        our_node_id: NodeId,
        our_node_name: String,
        our_token: String,
        our_capabilities: Vec<String>,
        is_server: bool,
    ) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (mut sender, mut receiver) = ws.split();
        let (out_tx, mut out_rx) = mpsc::channel::<String>(64);

        // Outbound writer task (spawned after handshake)
        let mut remote_id: Option<NodeId> = None;

        // Client initiates handshake
        if !is_server {
            let self_info = NodeInfo::new(
                our_node_id,
                our_node_name.clone(),
                "0.0.0.0:0".parse().unwrap(),
                crate::config::ClusterNodeRole::Worker,
                our_capabilities.clone(),
            );
            let handshake = ClusterMessage::handshake(self_info, our_token.clone());
            sender
                .send(Message::Text(handshake.encode().into()))
                .await?;
        }

        // Handshake loop: wait until we know the remote node_id
        while remote_id.is_none() {
            let Some(msg) = receiver.next().await else {
                bail!("connection closed before handshake");
            };

            let text = match msg {
                Ok(Message::Text(t)) => t,
                Ok(Message::Close(_)) => bail!("connection closed"),
                Ok(_) => continue,
                Err(e) => bail!("websocket error: {e}"),
            };

            let Some(cluster_msg) = ClusterMessage::decode(&text) else {
                continue;
            };

            match cluster_msg {
                ClusterMessage::Handshake { node_info, token } => {
                    // Server validates token and replies with HandshakeAck
                    if is_server {
                        if token != our_token {
                            let nack = ClusterMessage::handshake_nack("invalid cluster token");
                            let _ = sender.send(Message::Text(nack.encode().into())).await;
                            bail!("invalid cluster token");
                        }

                        remote_id = Some(node_info.id);
                        peers.write().insert(
                            node_info.id,
                            NodeInfo::new(
                                node_info.id,
                                node_info.name.clone(),
                                node_info.addr,
                                node_info.role,
                                node_info.capabilities.clone(),
                            ),
                        );

                        let self_info = NodeInfo::new(
                            our_node_id,
                            our_node_name.clone(),
                            "0.0.0.0:0".parse().unwrap(),
                            crate::config::ClusterNodeRole::Worker,
                            our_capabilities.clone(),
                        );
                        let ack = ClusterMessage::handshake_ack(self_info);
                        sender.send(Message::Text(ack.encode().into())).await?;
                    }
                }
                ClusterMessage::HandshakeAck { node_info } => {
                    // Client receives ack and registers peer
                    tracing::info!(
                        "Received HandshakeAck from node {} ({})",
                        node_info.name,
                        node_info.id
                    );
                    remote_id = Some(node_info.id);
                    peers.write().insert(
                        node_info.id,
                        NodeInfo::new(
                            node_info.id,
                            node_info.name.clone(),
                            node_info.addr,
                            node_info.role,
                            node_info.capabilities.clone(),
                        ),
                    );
                    tracing::info!(
                        "Successfully connected to peer {} ({})",
                        node_info.name,
                        node_info.id
                    );
                }
                ClusterMessage::HandshakeNack { reason } => {
                    bail!("handshake rejected: {reason}");
                }
                _ => {
                    // ignore until handshake established
                }
            }
        }

        let remote_id = remote_id.expect("handshake sets remote id");

        // Register sender for this peer
        peer_senders.write().insert(remote_id, out_tx.clone());

        // Spawn writer task
        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if sender.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }
        });

        // Main receive loop
        while let Some(msg) = receiver.next().await {
            let text = match msg {
                Ok(Message::Text(t)) => t,
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => continue,
            };

            if let Some(cluster_msg) = ClusterMessage::decode(&text) {
                let _ = message_tx.send((remote_id, cluster_msg));
            }
        }

        peer_senders.write().remove(&remote_id);
        peers.write().remove(&remote_id);
        Ok(())
    }

    pub async fn connect_to_peers(&self, peer_addrs: &[String]) -> Result<()> {
        for addr in peer_addrs {
            if let Err(e) = self.connect_to_peer(addr).await {
                tracing::warn!("Failed to connect to peer {}: {}", addr, e);
            }
        }
        Ok(())
    }

    pub async fn send_to(&self, target: &NodeId, message: ClusterMessage) -> Result<()> {
        // Clone the sender while holding the lock, then await on the cloned sender.
        let maybe_sender = {
            let senders = self.peer_senders.read();
            senders.get(target).cloned()
        };

        if let Some(sender) = maybe_sender {
            sender.send(message.encode()).await?;
            Ok(())
        } else {
            bail!("Peer {} not found", target)
        }
    }

    pub async fn broadcast(&self, message: ClusterMessage) -> Result<()> {
        let encoded = message.encode();

        // Collect a cloned list of senders while holding the lock, then send without the lock.
        let senders_vec: Vec<tokio::sync::mpsc::Sender<String>> = {
            let senders = self.peer_senders.read();
            senders.values().cloned().collect()
        };

        for sender in senders_vec {
            let _ = sender.send(encoded.clone()).await;
        }

        Ok(())
    }

    pub fn get_peers(&self) -> Vec<NodeInfo> {
        self.peers.read().values().cloned().collect()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<(NodeId, ClusterMessage)> {
        self.message_tx.subscribe()
    }

    pub async fn run_heartbeat(&self, interval_secs: u64) {
        let mut ticker = interval(Duration::from_secs(interval_secs));
        let node_id = self.node_id;

        loop {
            ticker.tick().await;

            let heartbeat = ClusterMessage::heartbeat(node_id, 0);
            if let Err(e) = self.broadcast(heartbeat).await {
                tracing::debug!("Heartbeat broadcast error: {}", e);
            }

            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let mut peers_map = self.peers.write();
            peers_map.retain(|_, info| {
                now_secs.saturating_sub(info.last_heartbeat_secs) < (interval_secs * 3)
            });
        }
    }
}

#[async_trait]
impl crate::cluster::traits::ClusterNode for WsClusterTransport {
    fn node_id(&self) -> NodeId {
        self.node_id
    }

    fn node_name(&self) -> &str {
        &self.node_name
    }

    async fn connect(&self, peer_addr: &str) -> Result<()> {
        self.connect_to_peer(peer_addr).await
    }

    async fn disconnect(&self) -> Result<()> {
        *self.running.write() = false;
        Ok(())
    }

    async fn send(&self, target: &NodeId, message: ClusterMessage) -> Result<()> {
        self.send_to(target, message).await
    }

    async fn broadcast(&self, message: ClusterMessage) -> Result<()> {
        self.broadcast(message).await
    }

    async fn subscribe(&self, _handler: Arc<dyn ClusterMessageHandler>) -> Result<()> {
        Ok(())
    }

    fn get_peers(&self) -> Vec<NodeInfo> {
        self.get_peers()
    }

    fn get_coordinator(&self) -> Option<NodeInfo> {
        self.peers
            .read()
            .values()
            .find(|p| p.role == crate::config::ClusterNodeRole::Coordinator)
            .cloned()
    }

    fn is_coordinator(&self) -> bool {
        self.peers
            .read()
            .values()
            .any(|p| p.role == crate::config::ClusterNodeRole::Coordinator)
    }

    async fn wait_for_coordinator(&self, timeout_secs: u64) -> Result<NodeInfo> {
        let start = std::time::Instant::now();

        while start.elapsed() < Duration::from_secs(timeout_secs) {
            if let Some(coordinator) = self.get_coordinator() {
                return Ok(coordinator);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        bail!("Timeout waiting for coordinator")
    }
}
