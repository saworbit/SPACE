//! Frontend components using Leptos reactive framework.
//!
//! This module provides the browser-side components for the mesh
//! data system web interface. It's feature-gated with the "frontend" flag.

use leptos::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[cfg(target_arch = "wasm32")]
use gloo::net::http::Request;

#[cfg(target_arch = "wasm32")]
use web_sys::{MessageEvent, WebSocket};

/// Peer information from the API
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PeerInfo {
    pub id: String,
    pub addr: String,
    pub role: String,
    pub storage_usage: u64,
    pub status: String,
    pub gossip_version: u32,
    pub last_gossip_heartbeat: u64,
}

/// Peers API response
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeersResponse {
    pub peers: Vec<PeerInfo>,
    pub gossip_metrics: HashMap<String, f64>,
    pub total_count: usize,
}

/// Main dashboard component
#[component]
pub fn MeshGossipDashboard() -> impl IntoView {
    let (peers_data, _set_peers_data) = create_signal(None::<PeersResponse>);
    let (error_msg, _set_error_msg) = create_signal(None::<String>);
    let (ws_connected, _set_ws_connected) = create_signal(false);
    let (ws_messages, _set_ws_messages) = create_signal(Vec::<String>::new());

    // Fetch peers on mount
    create_effect(move |_| {
        #[cfg(target_arch = "wasm32")]
        spawn_local(async move {
            match fetch_peers().await {
                Ok(data) => set_peers_data(Some(data)),
                Err(e) => set_error_msg(Some(format!("Failed to fetch peers: {}", e))),
            }
        });
    });

    view! {
        <div class="container">
            <header>
                <h1>"SPACE Mesh & Gossip Dashboard"</h1>
                <WebSocketStatus connected=ws_connected />
            </header>

            <main>
                {move || match error_msg.get() {
                    Some(err) => view! {
                        <div class="error">
                            <p>"Error: " {err}</p>
                        </div>
                    }.into_view(),
                    None => view! { <div></div> }.into_view(),
                }}

                <PeersSection peers_data=peers_data />
                <GossipMetricsSection peers_data=peers_data />
                <LiveUpdatesSection messages=ws_messages />
            </main>
        </div>
    }
}

/// WebSocket status indicator component
#[component]
fn WebSocketStatus(connected: ReadSignal<bool>) -> impl IntoView {
    view! {
        <div class="ws-status">
            <span class="status-indicator" class:connected=move || connected.get()>
                {move || if connected.get() { "●" } else { "○" }}
            </span>
            <span>{move || if connected.get() { "Connected" } else { "Disconnected" }}</span>
        </div>
    }
}

/// Peers list section
#[component]
fn PeersSection(peers_data: ReadSignal<Option<PeersResponse>>) -> impl IntoView {
    view! {
        <section class="peers-section">
            <h2>"Connected Peers"</h2>
            {move || match peers_data.get() {
                Some(data) => view! {
                    <div>
                        <p>"Total peers: " {data.total_count}</p>
                        <table class="peers-table">
                            <thead>
                                <tr>
                                    <th>"ID"</th>
                                    <th>"Address"</th>
                                    <th>"Role"</th>
                                    <th>"Storage"</th>
                                    <th>"Status"</th>
                                    <th>"Last Heartbeat"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {data.peers.into_iter().map(|peer| view! {
                                    <tr>
                                        <td>{peer.id}</td>
                                        <td>{peer.addr}</td>
                                        <td>{peer.role}</td>
                                        <td>{format_bytes(peer.storage_usage)}</td>
                                        <td class="status">{peer.status}</td>
                                        <td>{format_timestamp(peer.last_gossip_heartbeat)}</td>
                                    </tr>
                                }).collect::<Vec<_>>()}
                            </tbody>
                        </table>
                    </div>
                }.into_view(),
                None => view! { <p>"Loading peers..."</p> }.into_view(),
            }}
        </section>
    }
}

/// Gossip metrics section
#[component]
fn GossipMetricsSection(peers_data: ReadSignal<Option<PeersResponse>>) -> impl IntoView {
    view! {
        <section class="metrics-section">
            <h2>"Gossip Protocol Metrics"</h2>
            {move || match peers_data.get() {
                Some(data) => view! {
                    <div class="metrics-grid">
                        {data.gossip_metrics.into_iter().map(|(key, value)| view! {
                            <div class="metric-card">
                                <h3>{key}</h3>
                                <p class="metric-value">{format!("{:.2}", value)}</p>
                            </div>
                        }).collect::<Vec<_>>()}
                    </div>
                }.into_view(),
                None => view! { <p>"Loading metrics..."</p> }.into_view(),
            }}
        </section>
    }
}

/// Live updates section showing WebSocket messages
#[component]
fn LiveUpdatesSection(messages: ReadSignal<Vec<String>>) -> impl IntoView {
    view! {
        <section class="live-updates">
            <h2>"Live Updates"</h2>
            <div class="messages-container">
                {move || {
                    let msgs = messages.get();
                    if msgs.is_empty() {
                        view! { <p>"No updates yet..."</p> }.into_view()
                    } else {
                        view! {
                            <ul>
                                {msgs.into_iter().rev().take(20).map(|msg| view! {
                                    <li>{msg}</li>
                                }).collect::<Vec<_>>()}
                            </ul>
                        }.into_view()
                    }
                }}
            </div>
        </section>
    }
}

/// Fetch peers from the API
#[cfg(target_arch = "wasm32")]
async fn fetch_peers() -> Result<PeersResponse, String> {
    let response = Request::get("/api/peers")
        .send()
        .await
        .map_err(|e| format!("Request failed: {:?}", e))?;

    if !response.ok() {
        return Err(format!("HTTP error: {}", response.status()));
    }

    response
        .json::<PeersResponse>()
        .await
        .map_err(|e| format!("Failed to parse JSON: {:?}", e))
}

/// Format bytes into human-readable format
fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;

    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }

    format!("{:.2} {}", size, UNITS[unit_idx])
}

/// Format Unix timestamp into human-readable format
fn format_timestamp(timestamp: u64) -> String {
    if timestamp == 0 {
        return "Never".to_string();
    }

    let now = js_sys::Date::now() / 1000.0;
    let diff = now as u64 - timestamp;

    if diff < 60 {
        format!("{} sec ago", diff)
    } else if diff < 3600 {
        format!("{} min ago", diff / 60)
    } else if diff < 86400 {
        format!("{} hr ago", diff / 3600)
    } else {
        format!("{} days ago", diff / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(512), "512.00 B");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.00 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GB");
    }
}
