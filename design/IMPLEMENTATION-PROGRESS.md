# design/ 高保真设计落地 — 进度追踪

> 分支：`feature/design-r2-implementation`（基于 main `3d38857`）
> 设计稿：`design/index.html`（R2 版，已 approved，P01–P34 共 34 页，深浅双主题）
> 本文件由主理人（沙点兵）维护，每完成一项打 ✅ 并注明工位与日期。
> 契约权威：UI 令牌与组件状态以 `docs/design.md`（架构师产出）为准。

## 页面决策总览（来自 README）

- 30 页优化：P01–P12、P15–P24、P26–P34
- 3 页保留现状（不动）：P13 活动、P14 归档、P25 通知
- P34 为 P33 模型服务"放弃修改确认"补充状态

## 分组与现有组件映射

| 组 | 页面 | 现有落点 |
|----|------|----------|
| 工作区 | P01–03、P12、P15–16 | HomeScene / DashboardScene / ConversationsScene / InboxScene / ProjectsScene / EditorScene |
| 会话与工具 | P04–P10 | RoomScene + WorkbenchPage（workbench/，已起步未接路由） |
| 交互状态 | P11、P31–P34 | 权限确认 / SearchOverlay / provider 编辑与放弃确认 |
| 设置 | P17–P24、P26–P30 | SettingsScene 各 pane |

## 实施阶段（WAVE 模型）

| Wave | 内容 | 工位 | 状态 |
|------|------|------|------|
| W0 | T01 设计系统基础层（tokens 增量 + opt.css + 5 starter + main.tsx import） | 寇豆码（工位 A） | ✅ 完成（一次打回后复核通过：tokens 六项独立取证、opt-*.css 零字面色、tsc 0 错误） |
| W1 | T02 Shell 外壳→工位A；T03 工作区场景→工位B；T04 会话画布→工位C（并行） | A/B/C | ✅ 收口通过（tsc 0 错误、keep 页零 diff、零字面色全过；T03 曾因 UNC 丢盘打回重做一次；T04 超 800 行红线主动停手） |
| W2 | T05 设置中心→工位A（含 T05b：ui/Drawer、ConfirmDialog className 透传特批）；T06 搜索叠层→工位B；T07 P07-P11 消费接入→工位C（T04 拆分增量，架构师补卡） | A/B/C | ✅ 收口通过（tsc 0 错误、build exit 0 2.12s；⚠️ bundle-budget 警告：CSS 529.4KiB 超预算 527.3KiB，因新增 ~3700 行 opt-*.css，待定是否调预算或瘦身） |
| QA | T-QA01 → T-QA04 全链验证 | 严过关 | ✅ 完成 |

## 最终状态（2026-09-14 交付）

- **分支**：feature/design-r2-implementation，3 个 commit：`d097333`（主体落地）→ `b20f431`（QA 返工轮）→ `b2a311c`（清理）
- **e2e 回归**：25 例 R2 引入回归全部清零（16 例选择器失效→QA 更新测试定位器；9 例真实缺陷→工位修复）；app-shell 96/96、popover 4/4、run-guard 1/1、terminal 3/3、runs-panel/enhanced-review/m2-03-a6 全绿、design-impl 6/6
- **遗留（非本分支，main 同样失败）**：harness-plugins ×1、i18n-hardcoded ×1、m1-03 ×1、room-file-activity ×2、send-mode-switch ×1、偶发 ×1
- **契约沉淀**：docs/design.md（§5 归属标注：opt-editor 家族唯一归 opt-room.css）、双类并存模式（旧类+opt-* 类并列，新旧测试选择器共存）、conversation-status 四态语义恢复

## 契约追认与定案记录（W1/W2 期间）

- brand 行 64px = calc(2×--sp-10)（契约组合写法与目标值矛盾，取 64 定案值）
- opt-agent-stop 为正式契约类名；canvas-body 不挂 opt-panel-body；T03 跨文件消费 opt-inline-state/opt-panel-* 依「类名唯一定义点」契约
- T06：遮罩模糊走 --fx-blur；650px/210px 登记定案值；P32 空态按契约 opt-empty 口径（不按 renders 原稿）；新增类 opt-search-clear
- T05b：ui/Drawer、ui/ConfirmDialog 特批增加 className 透传（各 +4/+3 行，行为零改）；P33/P34 挂类点实际在 SettingsScene（1493/1921）
- 待 QA 核对：opt.css .opt-drawer .opt-panel-head 77px 三条规则匹配不到 ui/Drawer 实际 DOM（drawer-head/body/foot），是否覆盖由 QA 视觉核对后定

## 已知坑（全员复验要求）

- UNC 路径 Edit 工具偶发「报成功但不持久化」（T01 实证）：关键文件改后必须直读磁盘复验，不得只信编辑器状态。

## 硬约束（全体成员遵守）

1. **同构增量**：设计稿 DOM 与现有前端类名一致（app-shell / scene-* / --rc-rail-w），落地方式 = 增量合入，禁止重写。
2. **令牌纪律**：颜色走 tokens.css 变量、几何走 --sp-*/--text-*/--radius-* 刻度，零字面主题色（照 workbench-page.css 先例）。
3. **保留页**：P13 / P14 / P25 相关文件一律不碰。
4. **Shell 通道**：WSL 通道，命令 `wsl.exe -- bash -c "cd /root/project/r-code && <命令>"`；git 只在 WSL 内跑；禁止 Windows 侧 UNC git（曾致 207 文件 CRLF 污染）。
5. **文件行尾 LF**；测试文件归属以架构师分解为准；并行 wave 内不做仓库级验证（收口由主理人统一做）。

## 变更记录

- 2026-09-14 建档。分支已建，W0 架构设计启动。
