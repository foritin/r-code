# Harness V2 架构（实现态）

> T41 交付。旧架构见 `docs/support/archive/architecture-before-harness-v2.md`。

## 运行拓扑

```
GUI(Tauri) ─┐
TUI ────────┼─► r-code-client ──► r-code-service（每 Profile 唯一属主）
MCP ────────┘       named pipe/     │
                     unix socket    ├─ V2Store（SQLite + 事件日志 + Blob）
                                    ├─ PluginCatalog（不可变安装目录）
                                    └─ PluginHost ─► 插件进程
                                         ├─ plugins/native（Native）
                                         ├─ plugins/codex（Codex）
                                         └─ 第三方（同一路径安装）
```

## Crate 边界（依赖方向）

| Crate | 职责 | 禁止依赖 |
| --- | --- | --- |
| r-code-harness-protocol | wire DTO、manifest、进程 profile 模式 | 全部宿主实现 |
| r-code-kernel | 任务/验收/恢复状态机（纯） | Tauri、SQLite、Gateway、插件 |
| r-code-store v2 | SQLite v2 schema、原子聚合+事件、receipt、租约 | 旧迁移路径 |
| r-code-runtime | 无 Tauri 服务装配、传输、路由、daemon | Tauri |
| r-code-client | 本地 RPC、outbox | 运行时内核 |
| r-code-harness-sdk | 插件侧 SDK | 全部宿主实现 |
| plugins/* | 独立可执行插件 | runtime/store/gateway/Tauri |

## 关键机制

- **属主**：`owner.lock` OS 独占句柄（Windows share_mode(0) / Unix flock），进程死即释放；`owner.json` 提供 pid/nonce/token。
- **鉴权**：`AuthorizationService` 统一工具/进程/验证准备四类动作；LaunchCapability 来自包 pin 的 ProcessProfile；审批只翻宿主创建的 pending op。
- **验证**：冻结验收控制材料（宿主侧字节库）+ 私有目录物化（候选字节+控制文件）+ `--locked`/`--ignore-scripts` 依赖准备；缓存身份 = lock+toolchain；证据绑定 (check, candidate, env) 且仅 Host 来源。
- **裁决**：`completion::arbitrate` 唯一签发 verified；失败→RepairRequired；环境缺失→Blocked。
- **取消**：代次吊销→子任务级联→工作停止→终止证明→租约释放；证明不了→blocked。
- **进程围栏**：Windows kill-on-close Job Object（assign 先于执行）；Unix daemon-EOF guardian + 进程组 TERM→KILL。
- **写锁**：按物理目录身份的用户级锁 + common_dir 锁 + 持久写屏障；跨 profile 冲突。

## 状态

P1–P5、T32/T37/T39/T40 已落地并测试；桌面/TUI 前端切换（T33–T36）、打包（T38）与旧链路退役（T42）见 progress.md。
