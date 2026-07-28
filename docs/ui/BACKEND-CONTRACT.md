# R-Code 新 UI：后端接入合同

## 当前状态

新 UI 已接入真实 Tauri IPC：项目仪表盘、项目/全局活动流、批量任务详情、可已读通知中心，以及审核阶段的“请求修改”闭环均已落地。项目仪表盘不再从前端逐任务派生统计；统计口径、待处理项、变更摘要和最近完成记录均由后端聚合返回。

浏览器预览会使用 `src-tauri/frontend/src/lib/mock-data.ts` 中与真实接口同形状的演示数据，以便独立演示交互；Tauri 桌面端始终调用真实 IPC，真实 IPC 失败会原样报错，不会悄悄回退到 mock。

## 已可直接接入的能力

| UI 功能 | 现有命令 |
| --- | --- |
| 项目、会话与任务列表 | `cmd_workspace_list`、`cmd_task_list`、`cmd_task_detail` |
| 新对话、发送消息、停止与子代理 | `cmd_task_create`、`cmd_agent_send`、`cmd_agent_abort`、`cmd_agent_abort_subagent` |
| 待授权与操作决策 | `cmd_permission_pending`、`cmd_permission_approve` |
| 审核、接受与回滚改动 | `cmd_changes_list`、`cmd_change_diff`、`cmd_accept_task`、`cmd_rollback_file`、`cmd_rollback_task` |
| 验证记录 | `cmd_run_verification`、`cmd_verification_list`、`cmd_verification_output` |
| 项目文件与快速打开 | `cmd_file_list`、`cmd_file_read`、`cmd_file_write`、`cmd_quick_open` |
| 终端、项目记忆与设置 | `cmd_terminal_*`、`cmd_memory_get/set`、`cmd_settings_*` |

## 新增接口

| UI 能力 | Tauri 命令 | 返回 / 行为 |
| --- | --- | --- |
| 项目仪表盘 | `cmd_workspace_dashboard(workspace_path)` | 返回 `workspace`、`generated_at`、稳定的 `metrics`、任务摘要、可直接操作的 `attention` 与 `completed`。统计包括任务数、待授权数、待审核数、运行中数、活跃子代理数和近 1 小时完成数。 |
| 项目动态 | `cmd_project_activity_list(workspace_path, cursor, limit)` | 返回项目范围内、按时间倒序的 `ProjectActivityPage`；每项有 `id`、`at`、`kind`、`summary`、任务/运行归属、`actor` 与 `metadata`。 |
| 全局活动 | `cmd_activity_list(cursor, limit)` | 与项目动态使用同一分页形状，但跨工作区返回；全局活动页使用它，项目仪表盘不会混入这份数据。 |
| 批量任务详情 | `cmd_task_detail_batch(task_ids)` | 返回与 `cmd_task_detail` 同形状的详情数组；请求会去重并限制为 80 个任务，消除 IPC N+1。 |
| 通知中心 | `cmd_notification_list(cursor, limit, unread_only)`、`cmd_notification_mark_read(notification_id)`、`cmd_notification_mark_all_read()` | 通知持久化在本地库中；当前会同步待授权和待审核两类通知，支持未读总数、分页和已读状态。 |
| 审核请求修改 | `cmd_change_request(task_id, message)` | 仅允许 `review_ready` 任务；校验非空且最多 8,000 字，写入审核事件、关闭当前审核通知，并以审核反馈启动下一轮 Agent 运行。 |

`cursor` 均为不透明字符串，调用方不得自行解析；未传时读取第一页。接口层已支持分页，当前 UI 默认展示首屏，后续可以在活动/通知列表增加“加载更多”而不用改动后端协议。

## 推荐的数据边界

- **项目动态只属于项目仪表盘。** 全局活动页应使用跨项目活动查询，项目动态应只查询当前 `workspace_path`。
- **审核摘要是任务详情。** 关闭时仅收缩为同一份审核摘要的窄栏；不应被任何项目动态或全局活动内容替换。
- **统计口径由后端定义。** 例如“近 1 小时完成”“子代理数”和“待处理”应随 dashboard 响应返回计算时间与口径，避免前端在不同时区或不同轮询周期下得到不同结果。

## 前端使用位置

| 页面 / 组件 | 使用的真实接口 |
| --- | --- |
| `DashboardScene` | `workspaceDashboard`、`projectActivityList`；只有这个页面呈现项目动态栏。 |
| `ActivityScene` | `activityList`；不会呈现项目动态栏。 |
| `InboxScene` 与任务审核面板 | `changeRequest`；审核摘要收起/展开仍是同一张详情卡。 |
| 顶栏 `MenuBar` | `notificationList`、通知已读接口；以 15 秒轮询刷新。 |
| 任务状态缓存 | `taskDetailBatch`；旧桌面端运行时仍可安全回退到逐条详情读取。 |

## 后续可选优化

- 活动流与通知接口已支持 cursor；当记录量变大时，可在列表末尾增加“加载更多”。
- 当前通知通过轮询同步。若后端后续提供事件推送，可直接替换顶栏的 15 秒刷新，不影响数据合同或已读状态。
