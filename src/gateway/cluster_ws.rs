//! Cluster WebSocket handler for node-to-node communication.
//!
//! This endpoint handles cluster mesh communication between ZeroClaw nodes.

use super::AppState;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        Query, State, WebSocketUpgrade,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct ClusterQuery {
    pub token: Option<String>,
}

fn stable_node_id(node_name: &str) -> crate::cluster::NodeId {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    node_name.hash(&mut hasher);
    let hash = hasher.finish();
    crate::cluster::NodeId::from_u128(u128::from(hash))
}

pub async fn handle_ws_cluster(
    State(state): State<AppState>,
    Query(params): Query<ClusterQuery>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let config = state.config.lock();

    if !config.cluster.enabled {
        return (axum::http::StatusCode::FORBIDDEN, "Cluster is not enabled").into_response();
    }

    if let Some(ref expected_token) = config.cluster.node_token {
        let provided_token = params.token.as_deref().unwrap_or("");
        if provided_token != expected_token {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "Invalid cluster token",
            )
                .into_response();
        }
    }

    let node_name = config
        .cluster
        .node_name
        .clone()
        .unwrap_or_else(|| "gateway".to_string());
    let node_id = stable_node_id(&node_name);

    drop(config);

    ws.on_upgrade(move |socket| handle_cluster_socket(socket, state, node_id, node_name))
        .into_response()
}

async fn handle_cluster_socket(
    socket: WebSocket,
    state: AppState,
    node_id: crate::cluster::NodeId,
    node_name: String,
) {
    let (mut sender, mut receiver) = socket.split();

    tracing::info!(
        "Cluster WebSocket connection established for node {} ({})",
        node_name,
        node_id
    );

    let role = {
        let config = state.config.lock();
        config
            .cluster
            .static_role
            .unwrap_or(crate::config::ClusterNodeRole::Coordinator)
    };

    tracing::debug!("Waiting for handshake from remote node...");

    let self_info = crate::cluster::NodeInfo::new(
        node_id,
        node_name.clone(),
        "0.0.0.0:0".parse().unwrap(),
        role,
        vec![],
    );

    while let Some(msg) = receiver.next().await {
        let msg = match msg {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };

        if let Ok(cluster_msg) = serde_json::from_str::<crate::cluster::ClusterMessage>(&msg) {
            tracing::debug!(
                "Received cluster message: {:?}",
                std::mem::discriminant(&cluster_msg)
            );

            match &cluster_msg {
                crate::cluster::ClusterMessage::Handshake { node_info, token } => {
                    tracing::info!(
                        "Handshake received from node: {} ({}), token length: {}",
                        node_info.name,
                        node_info.id,
                        token.len()
                    );

                    let ack = crate::cluster::ClusterMessage::handshake_ack(self_info.clone());

                    if let Ok(encoded) = serde_json::to_string(&ack) {
                        tracing::debug!("Sending handshake ack");
                        let _ = sender.send(Message::Text(encoded.into())).await;
                        tracing::info!("Handshake ack sent to {}", node_info.name);
                    }
                }

                crate::cluster::ClusterMessage::DelegateCrossNode {
                    id,
                    agent_name,
                    prompt,
                    context,
                    ..
                } => {
                    tracing::info!(
                        "Delegation request: agent={}, prompt={}",
                        agent_name,
                        prompt.chars().take(50).collect::<String>()
                    );

                    let cfg = { state.config.lock().clone() };

                    let full_prompt = if let Some(ctx) = context {
                        format!("[Context]\n{}\n\n[Task]\n{}", ctx, prompt)
                    } else {
                        prompt.to_string()
                    };

                    match crate::agent::process_message(cfg, &full_prompt).await {
                        Ok(result) => {
                            let response = crate::cluster::ClusterMessage::DelegateResponse {
                                id: *id,
                                result,
                                source: node_id,
                                error: None,
                            };

                            if let Ok(encoded) = serde_json::to_string(&response) {
                                let _ = sender.send(Message::Text(encoded.into())).await;
                            }
                        }
                        Err(e) => {
                            let response = crate::cluster::ClusterMessage::DelegateResponse {
                                id: *id,
                                result: String::new(),
                                source: node_id,
                                error: Some(e.to_string()),
                            };

                            if let Ok(encoded) = serde_json::to_string(&response) {
                                let _ = sender.send(Message::Text(encoded.into())).await;
                            }
                        }
                    }
                }

                crate::cluster::ClusterMessage::Ping => {
                    let pong = crate::cluster::ClusterMessage::pong();
                    if let Ok(encoded) = serde_json::to_string(&pong) {
                        let _ = sender.send(Message::Text(encoded.into())).await;
                    }
                }

                _ => {
                    tracing::debug!("Unhandled cluster message type");
                }
            }
        }
    }

    tracing::info!("Cluster WebSocket connection closed");
}
