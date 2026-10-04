# R-Code v1 Wave 3 安全边界（修订版）

## Objective

在 O-GATE 并行 WorkUnit 与 ChildSupervisor 激活前，关闭所有本地进程执行前置条件：持久树所有权、可实现的跨平台 spawn/proof、真实进程输出协议、fail-closed sandbox、Check/Process/Harness 全入口收敛、可恢复的进程副作用、不可变 Shell/网络批准，以及 daemon-only 只读 Git。

## End state

- 所有 Harness、Check、依赖准备、交互 Process 和 Shell 都经同一个 `ProcessSupervisor`；ownership 在用户代码 resume 前持久化，退出必须有完整树证明，否则 workspace quarantine。
- `host.process.read` 提供有 cursor/backpressure 的 stdout/stderr/exit 流；不再依赖私有旁路方法。
- `SafetyCapabilityReport` 内容寻址并绑定 OS/arch/boot/backend/policy/helper/executable/probe 身份；只有 `Activated` 满足运行与 O-GATE 前置，`SafeDisabled/Unsupported` 永不开放能力。
- Windows 采用 raw `CreateProcessW` suspended + handle/job/security-capability attribute list；Linux 采用 guardian-as-spawner + cgroup/pid namespace + bwrap/seccomp；macOS 采用 `PROC_PIDTBSDINFO` 身份与固定 `/usr/bin/sandbox-exec`，无法证明完整树或 Seatbelt 时保持 SafeDisabled。
- Check 默认 Offline；依赖下载是单独批准、审计的网络准备过程。Shell 的 effect class 与 `Offline/PublicInternetClient/HostNetwork` 上限冻结进 Plan/Run，并由独立 effect approval 扩权。
- 不受控进程在 resume 前持久保存 before manifest，退出后只测量一次 delta；重启先终止/证明旧树，再从 durable before 恢复，不重跑命令。
- Git status/log/diff 由 `gix 0.88.0` 的受限只读服务提供给工具/API；Git executable 和所有写 API 不可达，index/refs/objects/config/locks 内容与 mtime 不变。

## Fixed platform decisions

- Linux seccomp：`seccompiler 0.5.0`，只支持 x86_64/aarch64 audit arch；通过继承 FD 交给 `/usr/bin/bwrap >= 0.8.0`。拒绝 namespace/mount/ptrace/bpf/keyring/module/reboot 等逃逸系统调用及 clone namespace flags。缺少 bwrap/userns/cgroup/pidns/seccomp 任一项即 SafeDisabled。
- macOS：仅使用 immutable `/usr/bin/sandbox-exec` Seatbelt 入口；本 Wave 明确将 write-capable Check/Process/Shell 保持 SafeDisabled，因为 process-group/proc snapshots 不能证明 setsid/double-fork 未逃逸。普通 macOS 包可发布，但不得宣传或开放这些写能力与 O-GATE。
- macOS Native Harness 仅可使用 `NoWorkspaceSingleProcess` profile：Seatbelt 拒绝 `process-fork`，默认拒绝 `process-exec`，但为 sandbox-exec 的初次替换精确放行内容寻址的 Native Harness executable；只授予该包、stdio 与 probe 得出的 immutable dyld/system runtime read roots，并以 birth identity+kqueue/wait 证明唯一 leader。`/bin/sh` 等所有非批准 executable、用户目录、workspace、keychain、network 与 host IPC 均拒绝；executable/runtime-policy digest 变化立即使报告失效。
- Windows：稳定 AppContainer SID；所有临时 ACL 在 V1Store 以 before/after self-relative security descriptor、路径物理身份和 fencing 持久化，CAS 恢复；`.git` 从不获授权。
- 网络语义不跨平台伪等价：Windows 可激活 `PublicInternetClient`；Linux/macOS host network 为 `HostNetwork`，需要不同的显式批准。
- 规则 CI 可验证 SafeDisabled；只有目标/能力被产品宣传为 write-enabled 时，对应 release job 才必须得到 Activated。portability-only 或隐藏能力可以保持 SafeDisabled，但不得满足 O-GATE 激活谓词。

## Components

1. ownership/quarantine
2. process wire/output
3. supervisor state machine
4. platform tree containment
5. capability report/activation
6. platform sandbox
7. launch-path convergence
8. durable process effects
9. Shell/effect/network approval
10. daemon read-only Git
11. conformance/release gate

## Execution order

`tasks.json` 包含 39 个 100–400 LOC 的 PR 单元：P00–P05 建立稳定 boot identity、持久 ownership、wire 与 supervisor；P06–P18 完成三平台树控制、activation、sandbox 与 quarantine retry；P19A/P19B-R/P19B-C 冻结并接通 runtime 与三类客户端的 effect approval；P20–P24H 收敛全部 launch 入口并打包 helper；P25–P27 完成带 quota/retention 的 process-effect 恢复；P28 开放批准后的 Shell；P29–P30 提供 gix；P31–P32 关闭 conformance/packaging/release gate。公开协议仍为 API major 1，Wave 3 additive minor 顺序固定为 v1.1 ProcessRead、v1.2 WorkUnit effect/network、v1.3 Harness child-process guarantee。

## Non-goals

- 本批次仍不实现 O-GATE scheduler、LeaseFamily 或 ChildSupervisor。
- 不创建 worktree，不自动 stage/commit/push，不写 Git refs/index/submodule。
- 不提供 unsandboxed fallback，不把 Unsupported 当作 Activated。

按 `tasks.json.order` 使用无 VCS 的 Engineer/QA loop 执行；`tasks.json` 是权威合同。每个任务先把声明文件复制到 `sandbox/r-code-v1-safety/iter/<task>/before` 并记录内容哈希，Engineer 只改生产文件，QA 只改测试；任务测试、累积回归、Clippy、格式和质量门全绿才保留。失败时只对仍等于该任务 after-hash 的输出做 CAS 恢复。全过程禁止 `git add/commit/push`、分支或 worktree 创建。
