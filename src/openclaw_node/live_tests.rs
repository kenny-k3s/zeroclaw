/// Live connection test: Connect ZeroClaw as a node to the production OpenClaw gateway
///
/// Usage:
///   OPENCLAW_GATEWAY_TOKEN=<token> cargo test --lib -- --ignored openclaw_node_live_handshake --nocapture
///
/// The identity is persisted at ~/.zeroclaw-live-test/device-key.json so the same
/// device is reused across runs. After the gateway issues a pairing request, approve it
/// with the printed command, then re-run to complete the handshake.

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

    /// Persistent identity path — reused across test runs so the same device can be approved once.
    fn live_identity_path() -> std::path::PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        std::path::PathBuf::from(home).join(".zeroclaw-live-test").join("device-key.json")
    }

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

    /// Extract requestId from a NOT_PAIRED error and print the approve command.
    fn maybe_print_pair_approve(err_msg: &str) {
        // Look for requestId in the error string
        if let Some(start) = err_msg.find("requestId\": String(\"") {
            let rest = &err_msg[start + "requestId\": String(\"".len()..];
            if let Some(end) = rest.find('"') {
                let request_id = &rest[..end];
                eprintln!("");
                eprintln!("[live] ──────────────────────────────────────────────────────────");
                eprintln!("[live]  PAIRING REQUIRED — approve this node in OpenClaw:");
                eprintln!("[live]");
                eprintln!("[live]    openclaw nodes pairing approve {}", request_id);
                eprintln!("[live]");
                eprintln!("[live]  Or open the OpenClaw web UI → Nodes → Pending → Approve");
                eprintln!("[live] ──────────────────────────────────────────────────────────");
                eprintln!("");
            }
        }
    }

    #[tokio::test]
    #[ignore] // Run with: OPENCLAW_GATEWAY_TOKEN=<token> cargo test -- --ignored openclaw_node_live_handshake --nocapture
    async fn openclaw_node_live_handshake() {
        // Required for wss:// — install ring as the TLS crypto provider
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();

        let gateway_url = std::env::var("OPENCLAW_GATEWAY_URL")
            .unwrap_or_else(|_| "wss://openclaw.kenny000666.duckdns.org".to_string());
        let gateway_token = std::env::var("OPENCLAW_GATEWAY_TOKEN").ok();

        let key_path = live_identity_path();
        let identity = DeviceIdentity::load_or_create(&key_path).expect("generate identity");

        eprintln!("[live] gateway_url={}", gateway_url);
        eprintln!("[live] token={}", if gateway_token.is_some() { "set" } else { "not set (will attempt unpaired)" });
        eprintln!("[live] device_id={}", identity.device_id());
        eprintln!("[live] public_key={}", identity.public_key_base64url());
        eprintln!("[live] identity_path={}", key_path.display());

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
        .with_accept_invalid_certs();

        let result = client.run_once(&handler).await;

        eprintln!("[live] connection result: {:?}", result.as_ref().map(|_| "ok").unwrap_or("err"));
        if let Err(ref e) = result {
            eprintln!("[live] error detail: {}", e);
        }
        eprintln!("[live] on_connected was called: {}", connected.load(Ordering::SeqCst));

        match result {
            Ok(_) => eprintln!("[live] ✓ clean disconnect"),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("shutdown") {
                    eprintln!("[live] ✓ shutdown received");
                } else if msg.contains("NOT_PAIRED") || msg.contains("PAIRING_REQUIRED") || msg.contains("not-paired") {
                    maybe_print_pair_approve(&msg);
                } else if msg.contains("connect failed") || msg.contains("token") {
                    eprintln!("[live] ✓ gateway reachable — auth response: {}", msg);
                } else if msg.contains("timeout") || msg.contains("connection refused") {
                    panic!("[live] FAIL — gateway unreachable: {}", msg);
                } else {
                    eprintln!("[live] ? unexpected error: {}", msg);
                }
            }
        }
    }

    /// Full handshake test — requires the device to be paired already.
    /// Run openclaw_node_live_handshake first, approve the node in OpenClaw, then run this.
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

        // Use the same persistent identity as openclaw_node_live_handshake
        let key_path = live_identity_path();
        let identity = DeviceIdentity::load_or_create(&key_path).expect("generate identity");

        eprintln!("[live] gateway_url={}", gateway_url);
        eprintln!("[live] device_id={}", identity.device_id());
        eprintln!("[live] identity_path={}", key_path.display());
        eprintln!("[live] connecting with token...");

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
        .with_accept_invalid_certs();

        let result = client.run_once(&handler).await;

        eprintln!(
            "[live] on_connected={} result={:?}",
            connected.load(Ordering::SeqCst),
            result.as_ref().map(|_| "ok").unwrap_or("err")
        );
        if let Err(ref e) = result {
            let msg = e.to_string();
            eprintln!("[live] error: {}", msg);
            if msg.contains("NOT_PAIRED") || msg.contains("PAIRING_REQUIRED") {
                maybe_print_pair_approve(&msg);
                panic!("[live] FAIL — device not yet paired. Approve it first, then re-run.");
            }
        }

        assert!(
            connected.load(Ordering::SeqCst),
            "handshake must succeed with a valid token for a paired device"
        );
    }
}
