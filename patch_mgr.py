import io

p = 'crates/r-code-runtime/src/bin/r-code-service.rs'
s = io.open(p, encoding='utf8').read()

# Replace the inline RemoteSurface with a wrapper delegating to RemoteManager
old_start = s.index('/// Everything the remote-control surface needs, owned by the daemon.')
old_end = s.index('fn method_error(')
new_surface = '''/// Everything the remote-control surface needs, owned by the daemon.
/// Management logic lives in [`r_code_runtime::remote::RemoteManager`] so
/// the console methods here are one-liners (R11 shares it with tests).
struct RemoteSurface {
    manager: Arc<r_code_runtime::remote::RemoteManager>,
}

impl std::ops::Deref for RemoteSurface {
    type Target = r_code_runtime::remote::RemoteManager;
    fn deref(&self) -> &Self::Target {
        &self.manager
    }
}

impl RemoteSurface {
    /// `remote.pairingStart` payload shape for the console.
    async fn pairing_start(&self) -> Result<serde_json::Value, String> {
        let reply = self
            .manager
            .pairing_start()
            .await
            .map_err(|e| e.to_string())?;
        let port = self.manager.listening_port().await.unwrap_or_default();
        Ok(serde_json::json!({
            "pairingCode": reply.pairing_code,
            "qrPayload": r_code_runtime::remote::pairing::qr_payload_v1(
                &self.manager.bind_ip.to_string(),
                port,
                &reply.pairing_code,
                &self.identity_fingerprint(),
            ),
            "lanEndpoints": reply.lan_endpoints,
            "expiresAtMs": reply.expires_at_ms,
            "port": port,
            "fingerprint": self.identity_fingerprint(),
        }))
    }

    fn identity_fingerprint(&self) -> String {
        self.manager.identity.fingerprint.clone()
    }
}

'''
s = s[:old_start] + new_surface + s[old_end:]

io.open(p, 'w', encoding='utf8', newline='').write(s)
print('surface replaced')
