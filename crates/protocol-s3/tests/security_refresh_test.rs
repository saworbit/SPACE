#![cfg(feature = "advanced-security")]

use common::security::ebpf_gateway::{EbpfGateway, ZeroTrustConfig};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_tungstenite::{accept_async, tungstenite::Message};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spiffe_allow_list_refreshes_from_stub() {
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![
        vec!["spiffe://demo/a".to_string()],
        vec!["spiffe://demo/b".to_string()],
    ])));

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub listener");
    let addr = listener.local_addr().expect("local addr");
    let stub_handle = tokio::spawn(run_stub(listener, responses.clone()));

    let config = ZeroTrustConfig {
        allowed_spiffe_ids: vec!["spiffe://bootstrap".into()],
        header_name: "x-spiffe-id".into(),
        spiffe_endpoint: Some(format!("ws://{}", addr)),
        refresh_interval_secs: 1,
        ..ZeroTrustConfig::default()
    };

    let gateway = EbpfGateway::new(config).expect("gateway init");
    {
        let identities = gateway.allowed_identities();
        let guard = identities.read().unwrap();
        assert!(guard.contains("spiffe://bootstrap"));
    }

    let (client, _interval) = gateway
        .workload_source()
        .expect("workload client available");

    let gateway_clone = gateway.clone();
    tokio::spawn(async move {
        for _ in 0..2 {
            let ids = client.fetch_allowed().await.expect("fetch allowed");
            if !ids.is_empty() {
                gateway_clone.update_allow_list(ids);
            }
        }
    })
    .await
    .expect("refresh loop");

    let identities = gateway.allowed_identities();
    let guard = identities.read().unwrap();
    assert!(guard.contains("spiffe://demo/b"));
    assert_eq!(guard.len(), 1);

    stub_handle.abort();
}

async fn run_stub(listener: TcpListener, responses: Arc<Mutex<VecDeque<Vec<String>>>>) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => break,
        };

        let responses = responses.clone();
        tokio::spawn(async move {
            if let Ok(mut ws) = accept_async(stream).await {
                while let Some(msg) = ws.next().await {
                    match msg {
                        Ok(Message::Text(_request)) => {
                            let payload = {
                                let mut guard = responses.lock().await;
                                guard.pop_front().unwrap_or_default()
                            };
                            let json = json!({ "allowed": payload }).to_string();
                            let _ = ws.send(Message::Text(json)).await;
                            break;
                        }
                        Ok(Message::Close(_)) => break,
                        _ => continue,
                    }
                }
            }
        });
    }
}
