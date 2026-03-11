//! UDP-based peer discovery for cluster nodes.
//!
//! This module implements dynamic peer discovery using UDP broadcast.
//! Nodes broadcast their presence periodically, and other nodes
//! connect to them via WebSocket upon discovery.

use crate::cluster::ws_transport::WsClusterTransport;
use crate::config::ClusterConfig;
use anyhow::Result;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::time::interval;

const DISCOVERY_MAGIC: &str = "ZEROCLAW";
const DISCOVERY_TIMEOUT_MULTIPLIER: u32 = 3;

pub struct DiscoveryService {
    node_name: String,
    ws_port: u16,
    discovery_port: u16,
    bind_addr: String,
    node_token: Option<String>,
    discovery_interval_secs: u64,
    known_nodes: Arc<RwLock<HashMap<String, (String, Instant)>>>,
}

impl DiscoveryService {
    pub fn new(config: &ClusterConfig, node_name: String) -> Self {
        let discovery_port = config.discovery_port.unwrap_or(config.bind_port);
        Self {
            node_name,
            ws_port: config.bind_port,
            discovery_port,
            bind_addr: config.bind_host.clone(),
            node_token: config.node_token.clone(),
            discovery_interval_secs: config.discovery_interval_secs.max(1),
            known_nodes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn start(&self, transport: Arc<WsClusterTransport>) -> Result<()> {
        let listen_addr = format!("{}:{}", self.bind_addr, self.discovery_port);
        let _broadcast_addr = format!("{}:{}", self.bind_addr, self.discovery_port);

        let socket_listen = UdpSocket::bind(&listen_addr).await?;
        socket_listen.set_broadcast(true)?;

        let socket_broadcast = UdpSocket::bind("0.0.0.0:0").await?;
        socket_broadcast.set_broadcast(true)?;

        tracing::info!(
            "Discovery service listening on {} (UDP broadcast)",
            listen_addr
        );

        let node_name = self.node_name.clone();
        let ws_port = self.ws_port;
        let discovery_interval = self.discovery_interval_secs;
        let node_token = self.node_token.clone();

        tokio::spawn(async move {
            Self::broadcast_loop(
                &socket_broadcast,
                &node_name,
                ws_port,
                discovery_interval,
                node_token,
            )
            .await;
        });

        let known_nodes = self.known_nodes.clone();
        Self::listen_and_connect_loop(
            socket_listen,
            transport,
            known_nodes,
            &self.node_name,
            discovery_interval,
        )
        .await;

        Ok(())
    }

    async fn broadcast_loop(
        socket: &UdpSocket,
        node_name: &str,
        ws_port: u16,
        interval_secs: u64,
        _node_token: Option<String>,
    ) {
        let mut ticker = interval(Duration::from_secs(interval_secs));

        loop {
            ticker.tick().await;

            let message = format!("{}|{}|{}", DISCOVERY_MAGIC, node_name, ws_port);

            let broadcast_addr: SocketAddr = if socket
                .local_addr()
                .map(|a| a.ip().is_loopback())
                .unwrap_or(false)
            {
                "127.255.255.255:65535".parse().unwrap()
            } else {
                "255.255.255.255:65535".parse().unwrap()
            };

            if let Err(e) = socket.send_to(message.as_bytes(), broadcast_addr).await {
                tracing::warn!("Discovery broadcast failed: {}", e);
            } else {
                tracing::debug!("Broadcast presence: {} on port {}", node_name, ws_port);
            }
        }
    }

    async fn listen_and_connect_loop(
        socket: UdpSocket,
        transport: Arc<WsClusterTransport>,
        known_nodes: Arc<RwLock<HashMap<String, (String, Instant)>>>,
        our_name: &str,
        discovery_interval_secs: u64,
    ) {
        let mut buf = [0u8; 512];
        let timeout = Duration::from_secs(
            discovery_interval_secs as u64 * DISCOVERY_TIMEOUT_MULTIPLIER as u64,
        );

        loop {
            tokio::select! {
                result = socket.recv_from(&mut buf) => {
                    match result {
                        Ok((len, addr)) => {
                            if let Ok(msg) = String::from_utf8(buf[..len].to_vec()) {
                                Self::handle_discovery_message(
                                    &msg,
                                    addr,
                                    &transport,
                                    &known_nodes,
                                    our_name,
                                ).await;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Discovery receive error: {}", e);
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    Self::cleanup_stale_nodes(&known_nodes, timeout);
                }
            }
        }
    }

    async fn handle_discovery_message(
        msg: &str,
        sender_addr: SocketAddr,
        transport: &Arc<WsClusterTransport>,
        known_nodes: &Arc<RwLock<HashMap<String, (String, Instant)>>>,
        our_name: &str,
    ) {
        let parts: Vec<&str> = msg.split('|').collect();
        if parts.len() < 3 || parts[0] != DISCOVERY_MAGIC {
            return;
        }

        let discovered_name = parts[1];
        let discovered_port = parts[2];

        if discovered_name == our_name {
            return;
        }

        let peer_addr = format!("{}:{}", sender_addr.ip(), discovered_port);

        let is_new;
        {
            let mut known = known_nodes.write();
            let entry = known.entry(peer_addr.clone()).or_insert_with(|| {
                tracing::info!("Discovered new node: {} at {}", discovered_name, peer_addr);
                (discovered_name.to_string(), Instant::now())
            });
            entry.1 = Instant::now();
            is_new = entry.1 == Instant::now();
        }

        if is_new {
            let transport = transport.clone();
            let peer_addr_clone = peer_addr.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if let Err(e) = transport.connect_to_peer(&peer_addr_clone).await {
                    tracing::debug!(
                        "Failed to connect to discovered peer {}: {}",
                        peer_addr_clone,
                        e
                    );
                }
            });
        }
    }

    fn cleanup_stale_nodes(
        known_nodes: &Arc<RwLock<HashMap<String, (String, Instant)>>>,
        timeout: Duration,
    ) {
        let now = Instant::now();
        let mut known = known_nodes.write();
        known.retain(|addr, (_, last_seen)| {
            if now.duration_since(*last_seen) > timeout {
                tracing::info!("Node {} timed out, removing from discovered peers", addr);
                false
            } else {
                true
            }
        });
    }
}
