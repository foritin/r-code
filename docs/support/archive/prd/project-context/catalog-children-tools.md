# M1a 工具目录变更说明（D9 · children 宿主目录工具）

PRD §5.3.5 / D9：`children_spawn` / `children_wait` / `children_close` 作为**宿主目录工具**进入 tool catalog，
参与 `RunSnapshotMaterial.tool_catalog_sha256` 的内容冻结与 capability 协商；native 插件零改造。

## 目录成员（M1a 起）

主 run（非子 run、非 WorkUnit 子 run）的规划工具目录在原有只读工具（read_file / list_files / search / glob + git 只读投影）
之上追加三个宿主控制工具：

| 工具 | 参数 | 说明 |
| --- | --- | --- |
| `children_spawn` | `objective`（必填）、`ceiling?`（默认 read-only）、`harness?`、`budget_share?` | 委派一个自包含子任务；描述内嵌委派纪律（四要素契约占位，M1b 打磨） |
| `children_wait` | `child_task_id`、`timeout_ms?`（默认 300000，上限 1800000） | 阻塞等待单个子代理完成并返回报告（condvar，无忙轮询） |
| `children_close` | `child_task_id` | 显式回收并发额度（完成不释放，close 才释放） |

## 身份与兼容

- 目录 digest 经 `tool_catalog_digest()`（排序 → canonical hash）冻结进 run 快照：目录集合变化 → 快照身份变化，这是**预期的版本语义**。
- 旧行为兼容：`RunSnapshotMaterial.instructions` 等 M1a 新字段全部 `skip_serializing_if` 空跳过，pre-M1a 存量快照反序列化后身份逐字节不变
  （`m1a_instruction_freeze::empty_sets_keep_snapshot_identity_byte_stable` 钉死）。
- 旧 catalog 任务不受影响：digest 是内容寻址字符串，旧快照携带旧 digest 依旧可校验加载。
- 子 run（id 形如 `{parent}-child-N`）与 WorkUnit 子 run 的目录**不包含** children 工具——嵌套限制（默认 1 层）由该结构性缺席保证，
  并有 e2e 钉死（`m1a_children_executor` 第 4 组断言）。

## 审计

子 run 的工具执行带 `caller=subagent:child-N`（经 PlanningToolService 的 caller 通道），网关子代理闸与只读白名单照常生效
（house gateway 测试面覆盖；`subagent_cannot_invoke_host_only_tool_even_with_full_workspace_access` 等）。
