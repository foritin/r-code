//! r-code-harness-codex binary: serves the Codex harness plugin over stdio.

#[tokio::main]
async fn main() -> Result<(), r_code_harness_sdk::SdkError> {
    // The App Server workflow runs through host.process (profile-gated);
    // this first cut serves the lifecycle surface so the host can drive,
    // cancel and checkpoint Codex runs through the same plugin path.
    struct CodexPlugin;

    #[async_trait::async_trait]
    impl r_code_harness_sdk::HarnessHandlers for CodexPlugin {
        async fn on_initialize(
            &self,
            _params: r_code_harness_protocol::services::InitializeParams,
        ) -> Result<r_code_harness_protocol::services::InitializeResult, r_code_harness_sdk::SdkError>
        {
            Ok(r_code_harness_protocol::services::InitializeResult {
                harness_id: "codex.r-code".into(),
                harness_version: env!("CARGO_PKG_VERSION").into(),
                ready_checkpoint: None,
            })
        }

        async fn on_start(
            &self,
            handle: r_code_harness_sdk::SdkHandle,
            params: r_code_harness_protocol::services::HarnessStartParams,
        ) -> Result<serde_json::Value, r_code_harness_sdk::SdkError> {
            let cwd = params
                .contract
                .get("workspaceRoot")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            // Open + initialize the App Server through managed processes.
            let mut client = r_code_harness_codex::AppServerClient::open(&handle, &cwd).await?;
            let event = client.initialize(&handle, &cwd).await?;
            if let r_code_harness_codex::AppServerEvent::Initialized { thread_id } = &event {
                client
                    .send_user_turn(
                        &handle,
                        &cwd,
                        params
                            .contract
                            .get("objective")
                            .and_then(|value| value.as_str())
                            .unwrap_or(""),
                    )
                    .await?;
                // Persist the thread reference in a versioned checkpoint.
                let checkpoint = r_code_harness_codex::CodexCheckpoint {
                    resume: Some(r_code_harness_codex::ThreadResumeRef {
                        harness_id: "codex.r-code".into(),
                        package_digest: "pinned-by-catalog".into(),
                        config_hash: "pinned-by-run".into(),
                        thread_id: thread_id.clone(),
                    }),
                    consumed_input_seq: 1,
                };
                let payload = serde_json::to_vec(&checkpoint).unwrap_or_default();
                let _ = handle.save_checkpoint(&payload, 1).await?;
            }
            let _ = client.close(&handle).await;
            Ok(serde_json::to_value(&event).unwrap_or_default())
        }
    }

    r_code_harness_sdk::serve(CodexPlugin).await
}
