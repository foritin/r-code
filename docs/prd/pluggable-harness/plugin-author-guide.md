# 插件作者指南

> 面向用 Rust 编写第三方 Harness 的作者。示例：`examples/repair-harness`。

## 五分钟上手

1. 依赖只有 SDK 与协议：

```toml
[dependencies]
r-code-harness-sdk = { path = "crates/r-code-harness-sdk" }
r-code-harness-protocol = { path = "crates/r-code-harness-protocol" }
```

2. 实现生命周期 trait 并 serve：

```rust
struct MyHarness;

#[async_trait::async_trait]
impl HarnessHandlers for MyHarness {
    async fn on_start(&self, handle: SdkHandle, params: HarnessStartParams)
        -> Result<serde_json::Value, SdkError>
    {
        let tools = handle.tools_list().await?;
        let reply = handle.tools_call("read_file", json!({"path": "README.md"}), Some("inspect-1")).await?;
        handle.emit_event(EventKind::Progress, json!({"stage": "planning"})).await?;
        handle.save_checkpoint(b"state", 1).await?;
        handle.propose_completion(CompletionProposalRequest {
            kind: ProposalKind::PlanDraft,
            summary: "plan ready".into(),
            candidate_digest: None,
            work_unit_statuses: vec![],
        }).await?;
        Ok(json!({"started": true}))
    }
}

#[tokio::main]
async fn main() { r_code_harness_sdk::serve(MyHarness).await }
```

3. 打包 `harness.json` + `bin/your-harness` 目录（或 ZIP），经宿主安装。禁止安装脚本、路径逃逸、符号链接；同身份不同字节会被拒绝。

## 规则要点

- **永不说“已验证”**：`host.completion.propose` 只是申请；宿主根据冻结检查的 Host 证据裁决 verified/unverified/blocked。
- **operation_key**：每个副作用调用带上 Attempt 内稳定的 key——重试重放回执，换 payload 会被拒。
- **审批只能引用宿主 pending operation**：`host.approvals.request` 的引用由宿主创建；普通问题永远不能授权。
- **句柄限本 Run**：进程句柄带 run 前缀，跨 Run 使用直接被拒。
- **取消协作**：收到 `harness.cancel` 后尽快返回 `{acknowledged:true}`；超时 5 s 后进程树被终止。
- **凭据**：永远接触不到 Provider 密钥——模型选择是不透明字符串，由宿主代理解析。
- **声明式进程约束**：受管子进程的 NDJSON 帧在完整帧边界被宿主中性解释器校验（方法白名单 + JSON-pointer 绑定宿主值）；协议规则作为包数据提供，宿主内核不含你的方法名。

## 调试

- stderr 自由写诊断（宿主有界保留尾部）。
- stdout 只能输出协议帧——多余打印会破坏帧解析并 fault 连接。
- 参考一致性套件：`cargo run -p r-code-evals --bin harness-conformance`。
