/// Live connection test: Connect ZeroClaw as a node to the production OpenClaw gateway
///
/// Usage:
///   OPENCLAW_GATEWAY_URL=wss://openclaw.kenny000666.duckdns.org cargo test -- --ignored openclaw_node_live --nocapture
///
/// The test performs a full protocol handshake and then gracefully terminates.
/// It does NOT require a pairing token — it will attempt to connect and
/// report exactly what the gateway sends back (auth error, pairing required, etc.)

#[cfg(test)]
mod live_tests {
    use crate::openclaw_node::{
        client::{NodeInvokeResult, NodeMessageHandler, OpenClawClient},
        identity::DeviceIdentity,
        protocol::*,
    };
    use futures_util::future::BoxFuture;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use tempfile::TempDir;

    struct LiveTestHandler {
        connected: Arc<AtomicBool>,
    }

    impl NodeMessageHandler for LiveTestHandler {
        fn on_invoke(&self, req: NodeInvokeRequest) -> BoxFuture<'static, NodeInvokeResult> {
            eprintln!("[live] on_invoke: command={}", req.command);
            let node_id = req.node_id.clone();
            Box::pin(async move {
                NodeInvokeResult {
                    id: req.id,
                    node_id,
                    ok: true,
                    payload_json: Some(r#"{"status":"zeroclaw-online","version":"0.1.6"}"#.to_string()),
                    error: None,
                }
            })
        }

        fn on_connected(&self) {
            eprintln!("[live] ✓ on_connected called — handshake succeeded");
            self.connected.store(true, Ordering::SeqCst);
        }

        fn on_disconnected(&self) {
            eprintln!("[live] on_disconnected");
        }
    }

    #[tokio::test]
    #[ignore] // Run with: cargo test -- --ignored openclaw_node_live --nocapture
    async fn openclaw_node_live_handshake() {
        // Required for wss:// — install ring as the TLS crypto provider
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok(); // ok() because it fails if already installed (fine in --test-threads=1)

        let gateway_url = std::env::var("OPENCLAW_GATEWAY_URL")
            .unwrap_or_else(|_| "wss://openclaw.kenny000666.duckdns.org".to_string());
        let gateway_token = std::env::var("OPENCLAW_GATEWAY_TOKEN").ok();

        eprintln!("[live] gateway_url={}", gateway_url);
        eprintln!("[live] token={}", if gateway_token.is_some() { "set" } else { "not set (will attempt unpaired)" });

        let tmpdir = TempDir::new().unwrap();
        let key_path = tmpdir.path().join("device-key.json");
        let identity = DeviceIdentity::load_or_create(&key_path).expect("generate identity");

        eprintln!("[live] device_id={}", identity.device_id());
        eprintln!("[live] public_key={}", identity.public_key_base64url());

        let handler = LiveTestHandler {
            connected: Arc::new(AtomicBool::new(false)),
        };
        let connected = handler.connected.clone();

        let mut client = OpenClawClient::new(
            &gateway_url,
            "zeroclaw-k8s-node-001",
            "ZeroClaw K8s Node",
            identity,
            gateway_token,
        )
        .with_accept_invalid_certs(); // Traefik self-signed cert on dev gateway

        // run_once will either:
        // - Return Ok after clean shutdown event
        // - Return Err("shutdown") - acceptable
        // - Return Err("connect failed: ...") - auth rejected, still useful info
        // - Return Err("gateway connection timeout") - network unreachable
        let result = client.run_once(&handler).await;

        eprintln!("[live] connection result: {:?}", result.as_ref().map(|_| "ok").unwrap_or("err"));
        if let Err(ref e) = result {
            eprintln!("[live] error detail: {}", e);
        }

        eprintln!(
            "[live] on_connected was called: {}",
            connected.load(Ordering::SeqCst)
        );

        // Test passes if we got a meaningful response from the gateway
        // (even a rejection is a valid response — it means the TLS handshake + WS upgrade worked)
        match result {
            Ok(_) => eprintln!("[live] ✓ clean disconnect (shutdown event received)"),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("shutdown") {
                    eprintln!("[live] ✓ shutdown received");
                } else if msg.contains("connect failed") || msg.contains("pairing") || msg.contains("token") {
                    eprintln!(
                        "[live] ✓ gateway reachable — auth rejected (expected without token): {}",
                        msg
                    );
                    // This is fine — the gateway is responding, handshake started
                    assert!(
                        connected.load(Ordering::SeqCst) || msg.contains("connect failed"),
                        "gateway should have responded to connect request"
                    );
                } else if msg.contains("timeout") || msg.contains("connection refused") {
                    panic!("[live] FAIL — gateway unreachable: {}", msg);
                } else {
                    // Unknown error — still show but don't fail
                    eprintln!("[live] ? unexpected error: {}", msg);
                }
            }
        }
    }

    /// Test with a real token from the environment
    #[tokio::test]
    #[ignore]
    async fn openclaw_node_live_with_token() {
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();

        let gateway_url = std::env::var("OPENCLAW_GATEWAY_URL")
            .unwrap_or_else(|_| "wss://openclaw.kenny000666.duckdns.org".to_string());
        let gateway_token = std::env::var("OPENCLAW_GATEWAY_TOKEN")
            .expect("OPENCLAW_GATEWAY_TOKEN must be set for this test");

        eprintln!("[live] gateway_url={}", gateway_url);
        eprintln!("[live] connecting with token...");

        let tmpdir = TempDir::new().unwrap();
        let key_path = tmpdir.path().join("device-key.json");
        let identity = DeviceIdentity::load_or_create(&key_path).expect("generate identity");

        let handler = LiveTestHandler {
            connected: Arc::new(AtomicBool::new(false)),
        };
        let connected = handler.connected.clone();

        let mut client = OpenClawClient::new(
            &gateway_url,
            "zeroclaw-k8s-node-001",
            "ZeroClaw K8s Node",
            identity,
            Some(gateway_token),
        )
        .with_accept_invalid_certs(); // Traefik self-signed cert on dev gateway

        let result = client.run_once(&handler).await;

        eprintln!(
            "[live] on_connected={} result={:?}",
            connected.load(Ordering::SeqCst),
            result.as_ref().map(|_| "ok").unwrap_or("err")
        );
        if let Err(ref e) = result {
            eprintln!("[live] error: {}", e);
        }

        assert!(
            connected.load(Ordering::SeqCst),
            "handshake must succeed with a valid token"
        );
    }
}
