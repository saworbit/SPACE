//! WebSocket handlers for real-time updates.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
    routing::get,
    Router,
};
use futures::{sink::SinkExt, stream::StreamExt};
use mesh_core::GossipMessage;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::state::AppState;

/// WebSocket message types
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
enum WsMessage {
    /// Subscribe to a gossip topic
    Subscribe { topic: String },
    /// Unsubscribe from a topic
    Unsubscribe { topic: String },
    /// Ping message
    Ping,
    /// Pong response
    Pong,
    /// Gossip update
    GossipUpdate { topic: String, message: String },
    /// Error message
    Error { message: String },
}

/// Build WebSocket routes
pub fn routes() -> Router<AppState> {
    Router::new().route("/live", get(ws_handler))
}

/// WebSocket upgrade handler
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

/// Handle an individual WebSocket connection
async fn handle_socket(socket: WebSocket, state: AppState) {
    let conn_id = Uuid::new_v4().to_string();
    info!("New WebSocket connection: {}", conn_id);

    let (mut sender, mut receiver) = socket.split();

    // Create a channel for this connection
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // Register the connection
    state
        .ws_connections
        .write()
        .await
        .insert(conn_id.clone(), tx.clone());

    // Subscribed topics for this connection
    let mut subscriptions: Vec<String> = Vec::new();

    // Spawn a task to forward messages from the channel to the WebSocket
    let mut send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sender.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });

    // Main message handling loop
    let state_clone = state.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(msg) = receiver.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    debug!("Received WebSocket message: {}", text);

                    // Parse the message
                    match serde_json::from_str::<WsMessage>(&text) {
                        Ok(ws_msg) => {
                            handle_ws_message(
                                ws_msg,
                                &state_clone,
                                &tx,
                                &mut subscriptions,
                            )
                            .await;
                        }
                        Err(e) => {
                            error!("Failed to parse WebSocket message: {}", e);
                            let error_msg = WsMessage::Error {
                                message: format!("Invalid message format: {}", e),
                            };
                            if let Ok(json) = serde_json::to_string(&error_msg) {
                                let _ = tx.send(json);
                            }
                        }
                    }
                }
                Ok(Message::Ping(data)) => {
                    debug!("Received ping");
                    // Axum handles pongs automatically
                }
                Ok(Message::Pong(_)) => {
                    debug!("Received pong");
                }
                Ok(Message::Close(_)) => {
                    info!("WebSocket closed by client");
                    break;
                }
                Ok(Message::Binary(_)) => {
                    warn!("Received binary message, ignoring");
                }
                Err(e) => {
                    error!("WebSocket error: {}", e);
                    break;
                }
            }
        }
    });

    // Wait for either task to finish
    tokio::select! {
        _ = &mut send_task => {
            debug!("Send task finished");
            recv_task.abort();
        }
        _ = &mut recv_task => {
            debug!("Receive task finished");
            send_task.abort();
        }
    }

    // Cleanup: remove connection
    state.ws_connections.write().await.remove(&conn_id);
    info!("WebSocket connection closed: {}", conn_id);
}

/// Handle a parsed WebSocket message
async fn handle_ws_message(
    msg: WsMessage,
    state: &AppState,
    tx: &mpsc::UnboundedSender<String>,
    subscriptions: &mut Vec<String>,
) {
    match msg {
        WsMessage::Subscribe { topic } => {
            info!("Subscribing to topic: {}", topic);

            // Subscribe to gossip topic
            match state.gossip.subscribe(&topic).await {
                Ok(mut rx) => {
                    subscriptions.push(topic.clone());

                    // Spawn a task to forward gossip messages to WebSocket
                    let tx_clone = tx.clone();
                    let topic_clone = topic.clone();
                    tokio::spawn(async move {
                        while let Some(gossip_msg) = rx.recv().await {
                            let ws_msg = WsMessage::GossipUpdate {
                                topic: topic_clone.clone(),
                                message: format!("{:?}", gossip_msg),
                            };
                            if let Ok(json) = serde_json::to_string(&ws_msg) {
                                if tx_clone.send(json).is_err() {
                                    break;
                                }
                            }
                        }
                    });

                    let response = serde_json::json!({
                        "type": "subscribed",
                        "topic": topic,
                    });
                    let _ = tx.send(response.to_string());
                }
                Err(e) => {
                    error!("Failed to subscribe to topic {}: {}", topic, e);
                    let error_msg = WsMessage::Error {
                        message: format!("Failed to subscribe: {}", e),
                    };
                    if let Ok(json) = serde_json::to_string(&error_msg) {
                        let _ = tx.send(json);
                    }
                }
            }
        }
        WsMessage::Unsubscribe { topic } => {
            info!("Unsubscribing from topic: {}", topic);
            subscriptions.retain(|t| t != &topic);

            let response = serde_json::json!({
                "type": "unsubscribed",
                "topic": topic,
            });
            let _ = tx.send(response.to_string());
        }
        WsMessage::Ping => {
            debug!("Received ping, sending pong");
            let pong = WsMessage::Pong;
            if let Ok(json) = serde_json::to_string(&pong) {
                let _ = tx.send(json);
            }
        }
        _ => {
            warn!("Unhandled WebSocket message type: {:?}", msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_message_serialization() {
        let msg = WsMessage::Subscribe {
            topic: "test".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("Subscribe"));
        assert!(json.contains("test"));
    }

    #[test]
    fn test_ws_message_deserialization() {
        let json = r#"{"type":"Subscribe","topic":"test"}"#;
        let msg: WsMessage = serde_json::from_str(json).unwrap();
        match msg {
            WsMessage::Subscribe { topic } => assert_eq!(topic, "test"),
            _ => panic!("Wrong message type"),
        }
    }
}
