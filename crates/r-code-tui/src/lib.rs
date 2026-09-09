//! r-code-tui 核心状态与事件→widget 映射（pi-alignment PRD §4.1 R-TUI-01/M8-01）。
//!
//! T35 起会话编排全部经共享 r-code-service 守护进程（v2 Harness 协议，
//! [`crate::engine::V2ChatClient`]），不再依赖 r-code-host/Tauri。事件源是
//! 守护进程 journal 的 EventEnvelope，TUI 侧投影为 [`TranscriptEvent`]。
//!
//! 本模块是**可单测的纯状态机**（不含终端 IO）：`TuiState` 消费 transcript
//! 事件维护 transcript 视图；ratatui 渲染与 crossterm 输入在 `app.rs`/
//! `main.rs` 壳层消费它。

pub mod app;
pub mod approval;
pub mod approval_overlay;
pub mod bang_command;
pub mod bang_exec;
pub mod clipboard;
pub mod display;
pub mod engine;
pub mod export;
pub mod external_editor;
pub mod harness_client;
pub mod history;
pub mod image_attach;
pub mod ime;
pub mod inline_render;
pub mod input;
pub mod interaction;
pub mod login_flow;
pub mod mention;
pub mod model_selector;
pub mod paste;
pub mod session_ops;
pub mod session_picker;
pub mod session_tree;
pub mod setup_flow;
pub mod slash_menu;
pub mod snapshot;
pub mod status_bar;
pub mod task_mode;
pub mod thinking;
pub mod transcript_view;
pub mod window;

/// v2 journal 事件的 transcript 投影（EventEnvelope → 行；见
/// [`engine::transcript_events_of`] / [`engine::project_events`]）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptEvent {
    /// 用户消息（message_id = journal input.queued 的稳定标识，/fork 消费）。
    User { message_id: String, text: String },
    /// 助手消息（v2 为整轮文本，无 delta 流）。
    Assistant { run_id: String, text: String },
    /// 工具调用（input 为守护进程截断的预览载荷）。
    ToolCall {
        run_id: String,
        name: String,
        input: serde_json::Value,
    },
    /// 工具结果（错误位回写最后一张工具卡）。
    ToolResult {
        run_id: String,
        name: String,
        ok: bool,
        output: serde_json::Value,
    },
}

/// transcript 的一行（事件 → widget 映射的产物）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptRow {
    /// 用户消息。
    User { text: String },
    /// 助手消息（流式 delta 已合并；`complete` 标记是否收口）。
    Assistant { text: String, complete: bool },
    /// 工具卡（折叠态展示名 + 摘要；展开由 UI 交互层处理）。
    ToolCard {
        name: String,
        summary: String,
        is_error: bool,
    },
    /// 系统提示行（错误/状态）。
    System { text: String },
    /// !command 本地 shell 直执行（OSC 133 prompt/output 段；与 ToolCard
    /// 类型层区分——Agent 工具输出走 ToolCard）。
    Shell(crate::bang_command::ShellRow),
    /// 图片附件（G6：粘贴/`@file` 图片）。`preview` 是已渲染的半块 ANSI 行
    /// （pi terminal-image 形态；空 = 占位行——重建历史时无字节只有元数据）。
    Image {
        name: String,
        width: u32,
        height: u32,
        #[serde(default)]
        preview: Vec<String>,
    },
}

/// TUI 会话状态（纯逻辑；单线程模型——事件经 Mutex 共享投递）。
#[derive(Debug, Default)]
pub struct TuiState {
    rows: Vec<TranscriptRow>,
    /// 当前流式 assistant 累积文本（delta 合并缓冲）。
    streaming: Option<String>,
    /// 已落行的流式行索引（flush/收口原位替换，不逐帧追加碎片行；
    /// None = 当前没有打开的流式行）。
    streaming_row: Option<usize>,
    /// 会话是否运行中（发送/steer 可用性判断）。
    running: bool,
    /// 当前 (provider, model) 选择（M2-01 footer 右侧联动；权威在 Task，
    /// 此处仅投影，选中写回成功后更新）。
    model_selection: Option<(String, String)>,
    /// 当前思考档位（M2-02 footer `• thinking` 段；None = 未设/不支持省略段）。
    thinking: Option<String>,
    /// 当前任务模式（M2-03 Shift+Tab 循环投影；权威在 Task.mode）。
    task_mode: String,
    /// 待发送队列的展示镜像（M2-04：权威在守护进程持久化队列；此处仅渲染
    /// 投影，新 run 启动即清空——守护进程在 run 结束时自动派发队列）。
    queued: Vec<String>,
    /// 待审批请求投影（M2-05：权威在 PermissionEngine pending 队列）。
    pending_approval: Option<crate::approval_overlay::PendingApproval>,
    /// 会话累计用量投影（M3-01：权威在 task.detail.usage）。
    usage: Option<crate::status_bar::UsageStats>,
    /// 当前任务 ID（G8：resume/分叉后切换；空 = 不过滤事件，print 模式/测试）。
    task_id: String,
    /// 视图代际（G8：transcript 整体重建时 +1——渲染壳层据此清屏重排，
    /// 而不是把重建行增量追加进 scrollback）。
    view_epoch: u64,
}

impl TuiState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 消费一枚 v2 transcript 事件（实时泵与重建共用落行逻辑）。
    ///
    /// - User：本地路径由发送方 `push_user` 先行追加（泵不投 User），此臂
    ///   仅供重建调用；
    /// - Assistant：v2 是整轮文本——原位收口单行（与封口帧同语义）；
    /// - ToolCall/ToolResult：工具卡 + 错误位回写。
    pub fn apply_transcript_event(&mut self, event: &TranscriptEvent) {
        match event {
            TranscriptEvent::User { text, .. } => {
                self.rows.push(TranscriptRow::User { text: text.clone() });
            }
            TranscriptEvent::Assistant { text, .. } => {
                self.land_streaming_row(text.clone(), true);
                self.streaming = None;
                self.streaming_row = None;
            }
            TranscriptEvent::ToolCall { name, input, .. } => {
                self.rows.push(TranscriptRow::ToolCard {
                    name: name.clone(),
                    summary: summarize_input(input),
                    is_error: false,
                });
            }
            TranscriptEvent::ToolResult { ok, .. } => {
                if let Some(TranscriptRow::ToolCard { is_error: slot, .. }) = self.rows.last_mut() {
                    *slot = !*ok;
                }
            }
        }
    }

    /// 运行态投影（run.started → true 并清队列镜像；收敛 → false 并收口
    /// 未闭合的流式缓冲）。v2 事件泵驱动。
    pub fn mark_running(&mut self, running: bool) {
        if running {
            self.running = true;
            // 新 run 启动 = 队列已派发/排空（守护进程 run 结束时自动派发）。
            self.queued.clear();
        } else {
            self.running = false;
            self.seal_streaming();
        }
    }

    /// 流式预览（未收口 assistant 缓冲；live 区呈现，收口后整行 commit）。
    pub fn streaming_preview(&self) -> Option<&str> {
        self.streaming.as_deref()
    }

    /// 流式缓冲刷屏（print 模式收口前调用：未收口的 assistant 也可见）。
    ///
    /// 不清空缓冲、不追加新行——把缓冲快照落到**同一行**（有打开的流式行则
    /// 原位替换）。此前渲染循环每帧调用会把逐 tick 的 delta 片段提交成几十条
    /// `complete:false` 碎片行；交互路径已改为 live 区预览（streaming_preview），
    /// 此方法仅作显式收口入口。
    pub fn flush_streaming(&mut self) {
        if let Some(text) = self.streaming.clone() {
            if !text.is_empty() {
                self.land_streaming_row(text, false);
            }
        }
    }

    pub fn rows(&self) -> &[TranscriptRow] {
        &self.rows
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// 用户输入 → transcript 行（发送前本地可见）。
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.rows.push(TranscriptRow::User { text: text.into() });
    }

    /// 系统/错误行 → transcript（M1-03：alt-screen 下 eprintln 不可见，
    /// 交互路径的用户可见错误一律走这里）。
    pub fn push_system(&mut self, text: impl Into<String>) {
        self.rows.push(TranscriptRow::System { text: text.into() });
    }

    /// 当前模型选择投影（权威在 Task；选中写回成功后由壳层更新）。
    pub fn model_selection(&self) -> Option<&(String, String)> {
        self.model_selection.as_ref()
    }

    pub fn set_model_selection(&mut self, provider: String, model: String) {
        self.model_selection = Some((provider, model));
    }

    /// 当前思考档位投影（权威在 Task.inference）。
    pub fn thinking(&self) -> Option<&str> {
        self.thinking.as_deref()
    }

    pub fn set_thinking(&mut self, level: Option<String>) {
        self.thinking = level;
    }

    /// 当前任务模式投影（默认 ask）。
    pub fn task_mode(&self) -> &str {
        &self.task_mode
    }

    pub fn set_task_mode(&mut self, mode: String) {
        self.task_mode = mode;
    }

    /// 运行中入队的展示镜像（发送经守护进程队列语义）。
    pub fn queue_message(&mut self, text: impl Into<String>) {
        self.queued.push(text.into());
    }

    pub fn queued(&self) -> &[String] {
        &self.queued
    }

    /// 待审批请求（Some 时浮层自动呈现，键位被审批契约接管）。
    pub fn pending_approval(&self) -> Option<&crate::approval_overlay::PendingApproval> {
        self.pending_approval.as_ref()
    }

    pub fn set_pending_approval(&mut self, pending: crate::approval_overlay::PendingApproval) {
        self.pending_approval = Some(pending);
    }

    /// 取走待审批请求（决策时消费；None = 已被处理）。
    pub fn take_pending_approval(&mut self) -> Option<crate::approval_overlay::PendingApproval> {
        self.pending_approval.take()
    }

    /// 追加一条已完成行（! 直通 shell 行等壳层产出）。
    pub fn push_row(&mut self, row: TranscriptRow) {
        self.rows.push(row);
    }

    /// 清空 transcript 视图（/clear；持久化 journal 不动，仅展示层）。
    /// 代际 +1 触发渲染壳层清屏重排——否则旧行仍在 scrollback，/clear 无可见效果。
    pub fn clear_transcript(&mut self) {
        self.rows.clear();
        self.streaming = None;
        self.streaming_row = None;
        self.view_epoch += 1;
    }

    /// 当前任务 ID（事件过滤 + 会话操作目标）。
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// 设置当前任务（resume/新建/克隆切换；同步事件过滤口径）。
    pub fn set_task_id(&mut self, task_id: impl Into<String>) {
        self.task_id = task_id.into();
    }

    /// 视图代际（渲染壳层检测整体重建）。
    pub fn view_epoch(&self) -> u64 {
        self.view_epoch
    }

    /// 用 v2 journal 投影（[`TranscriptEvent`]）整体重建 transcript 视图
    /// （G8：/resume 接续、分支切换后调用）。重建视图与旧 scrollback 内容
    /// 天然重复——代际 +1 让渲染壳层清屏后重排整棵视图树。
    pub fn rebuild_from_events(&mut self, events: &[TranscriptEvent]) {
        self.rows.clear();
        self.streaming = None;
        self.streaming_row = None;
        self.queued.clear();
        for event in events {
            self.apply_transcript_event(event);
        }
        self.view_epoch += 1;
    }

    /// 用量投影（footer 统计；由壳层周期性从 task.detail 刷新）。
    pub fn usage(&self) -> Option<crate::status_bar::UsageStats> {
        self.usage
    }

    pub fn set_usage(&mut self, stats: crate::status_bar::UsageStats) {
        self.usage = Some(stats);
    }

    /// 流式文本落成 Assistant 行：有打开的流式行则原位替换（记住索引），
    /// 否则追加新行并登记索引——保证一次 run 只占一行，直到封口。
    fn land_streaming_row(&mut self, text: String, complete: bool) {
        let row = TranscriptRow::Assistant { text, complete };
        match self.streaming_row {
            Some(index)
                if matches!(self.rows.get(index), Some(TranscriptRow::Assistant { .. })) =>
            {
                self.rows[index] = row;
            }
            _ => {
                self.rows.push(row);
                self.streaming_row = Some(self.rows.len() - 1);
            }
        }
    }

    /// 运行收敛时收口未闭合的流式缓冲：落成单行 complete 行，并关闭流式行
    /// 索引（下一次 run 从新行开始）。
    fn seal_streaming(&mut self) {
        if let Some(text) = self.streaming.take() {
            if !text.is_empty() {
                self.land_streaming_row(text, true);
            }
        }
        self.streaming_row = None;
    }
}

/// M1-03/R2：把运行失败文案翻译成可操作指引。
///
/// 未配置模型类错误（v2 "unknown model selection"/空 registry）由
/// [`engine::failure_guidance`] 映射 /setup 引导；此处为其余 provider 类
/// 错误附上 v2 settings 文件路径（profile root 下的 settings.json）。
pub fn provider_error_guidance(error: &str, profile_root: &std::path::Path) -> String {
    let provider_related = ["模型服务", "未找到默认", "尚未就绪", "provider"]
        .iter()
        .any(|needle| error.contains(needle));
    if !provider_related {
        return error.to_string();
    }
    let settings_file = profile_root.join("settings.json");
    format!(
        "{error}\n配置途径：输入 /setup 选择预设并保存，或直接编辑 {}。",
        settings_file.display()
    )
}

/// M2-04：排队显示行（codex 形态：`• Queued follow-up inputs` + `  ↳`）。
pub fn queue_lines(queued: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    if !queued.is_empty() {
        lines.push("• Queued follow-up inputs".to_string());
        for text in queued {
            lines.push(format!("  ↳ {text}"));
        }
    }
    lines
}

/// M2-04：发送路由——运行中 Enter = 排队（不打断当前 run；守护进程在 run
/// 结束时自动派发队列），空闲 Enter = 正常发送。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendRoute {
    Send,
    Queue,
}

pub fn route_send(running: bool) -> SendRoute {
    if running {
        SendRoute::Queue
    } else {
        SendRoute::Send
    }
}

/// R1/R2：无 provider 配置时的显式引导（不降级、不回放演示）。
///
/// v2 判定走本地 SettingsStore（profile root 的 settings.json + 平台凭据
/// 感知的 availability()），不连 daemon——首屏快速路径；密钥可能存放在
/// OS 凭据库，深校验留给发送路径（无凭据的 provider 发送即报可操作错误）。
pub fn provider_config_guidance(profile_root: &std::path::Path) -> Option<String> {
    use r_code_runtime::services::settings_store::SettingsStore;
    let settings = SettingsStore::new(profile_root.to_path_buf()).load();
    let settings_file = profile_root.join("settings.json");
    match &settings.default_selection {
        Some(selection) if settings.providers.iter().any(|p| &p.selection == selection) => None,
        _ => Some(format!(
            "未找到默认模型服务。两种配置途径：\n  1) 本 TUI 输入 /setup（选预设 + 输 API key，保存即默认；Tab 可切环境变量鉴权）；\n  2) 直接编辑 {}。",
            settings_file.display()
        )),
    }
}

/// M1-04：无配置时的首屏引导行（进 transcript 头部；已配置返回空——
/// 首屏不出现引导）。发送路径由真实模式兜底：发送即出 provider 引导错误。
pub fn onboarding_lines(profile_root: &std::path::Path) -> Vec<String> {
    match provider_config_guidance(profile_root) {
        Some(guidance) => vec!["R-Code CLI 尚未配置模型服务".to_string(), guidance],
        None => Vec::new(),
    }
}

fn summarize_input(input: &serde_json::Value) -> String {
    match input {
        serde_json::Value::Object(map) => {
            // 取首个短字符串字段做摘要（command/cwd/path 等常见键优先）。
            for key in ["command", "path", "pattern", "query", "url"] {
                if let Some(value) = map.get(key).and_then(|v| v.as_str()) {
                    return truncate(value, 60);
                }
            }
            if let Some((_, first)) = map.iter().next() {
                if let Some(text) = first.as_str() {
                    return truncate(text, 60);
                }
            }
            truncate(&input.to_string(), 60)
        }
        other => truncate(&other.to_string(), 60),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M2-04.A2：排队渲染行格式（• 标题 + ↳ 缩进条目；空队列为空）。
    #[test]
    fn queue_lines_follow_codex_format() {
        assert!(queue_lines(&[]).is_empty());
        let lines = queue_lines(&["先修这个".to_string(), "再加测试".to_string()]);
        assert_eq!(lines[0], "• Queued follow-up inputs");
        assert_eq!(lines[1], "  ↳ 先修这个");
        assert_eq!(lines[2], "  ↳ 再加测试");
    }

    /// M2-04.A1/A3：运行中 Enter 走 Queue 路由；镜像随新 run 清空。
    #[test]
    fn queue_mirror_lifecycle() {
        assert_eq!(route_send(false), SendRoute::Send);
        assert_eq!(route_send(true), SendRoute::Queue, "运行中不打断当前 run");
        let mut state = TuiState::new();
        state.queue_message("follow-up");
        assert_eq!(state.queued(), &["follow-up".to_string()]);
        // 新 run 启动（守护进程派发队列）→ 镜像清空。
        state.mark_running(true);
        assert!(state.queued().is_empty(), "run 启动后队列镜像必须清空");
    }

    /// M1-04.A1：空配置 → 首屏引导行（System 投影，含配置途径）。
    #[test]
    fn onboarding_lines_empty_config_lists_guidance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let profile_root = dir.path().join("harness-v2");
        std::fs::create_dir_all(&profile_root).expect("mkdir");
        let lines = onboarding_lines(&profile_root);
        assert!(!lines.is_empty(), "unconfigured must yield onboarding rows");
        let mut state = TuiState::new();
        for line in &lines {
            state.push_system(line.clone());
        }
        let rendered = state
            .rows()
            .iter()
            .filter_map(|row| match row {
                TranscriptRow::System { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert!(
            rendered.contains("尚未配置"),
            "first screen must state the unconfigured condition: {rendered}"
        );
        assert!(
            rendered.contains("settings.json"),
            "onboarding must name the v2 settings file: {rendered}"
        );
        assert!(
            rendered.contains("/setup"),
            "onboarding must point at the setup flow: {rendered}"
        );
    }

    /// M1-04.A2：已配置（settings.json 有默认 selection）→ 首屏引导行不存在。
    #[test]
    fn onboarding_lines_configured_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let profile_root = dir.path().join("harness-v2");
        std::fs::create_dir_all(&profile_root).expect("mkdir");
        std::fs::write(
            profile_root.join("settings.json"),
            serde_json::json!({
                "providers": [{"selection": "demo", "model": "demo-model"}],
                "default_selection": "demo",
            })
            .to_string(),
        )
        .expect("write settings");
        assert!(
            onboarding_lines(&profile_root).is_empty(),
            "configured default selection must not produce onboarding rows"
        );
    }

    /// M1-03.A1：交互路径错误进 transcript（System 行投影）。
    #[test]
    fn system_errors_project_into_transcript() {
        let mut state = TuiState::new();
        state.push_user("hello");
        state.push_system("发送失败：未找到默认模型服务");
        let rows = state.rows();
        assert_eq!(rows.len(), 2);
        assert!(
            matches!(&rows[1], TranscriptRow::System { text } if text.contains("发送失败")),
            "error must surface as a System row, got {:?}",
            rows[1]
        );
    }

    /// M1-03.A2：provider 配置类错误附 v2 settings 绝对路径与 /setup 途径；
    /// 其余错误不追加。
    #[test]
    fn provider_errors_carry_actionable_guidance() {
        let dir = std::path::Path::new("/tmp/tui-v2-guidance/profile");
        let provider_error = "未找到默认模型服务，请前往设置完成配置";
        let guided = provider_error_guidance(provider_error, dir);
        assert!(
            guided.contains(provider_error),
            "original error must remain"
        );
        assert!(
            guided.contains(dir.join("settings.json").to_str().unwrap()),
            "guidance must name the absolute settings path: {guided}"
        );
        assert!(
            guided.contains("/setup"),
            "guidance must point at the setup flow: {guided}"
        );
        // 非 provider 错误：原样返回，不追加误导性指引。
        let other = "任务队列已满";
        assert_eq!(provider_error_guidance(other, dir), other);
    }

    /// 工具卡错误位：失败结果回写（v2 ToolResult 的 ok 取反）。
    #[test]
    fn tool_card_error_flag_written_back() {
        let mut state = TuiState::new();
        state.apply_transcript_event(&TranscriptEvent::ToolCall {
            run_id: "r".into(),
            name: "bash".into(),
            input: serde_json::json!({"command": "ls"}),
        });
        state.apply_transcript_event(&TranscriptEvent::ToolResult {
            run_id: "r".into(),
            name: "bash".into(),
            ok: false,
            output: serde_json::json!(null),
        });
        assert!(matches!(
            state.rows().last(),
            Some(TranscriptRow::ToolCard { is_error: true, .. })
        ));
    }

    /// /clear 提升视图代际（渲染壳层据此清屏重排，否则 scrollback 旧行仍在，
    /// /clear 无可见效果）。
    #[test]
    fn clear_transcript_bumps_view_epoch() {
        let mut state = TuiState::new();
        state.push_user("旧内容");
        let epoch_before = state.view_epoch();
        state.clear_transcript();
        assert!(state.rows().is_empty());
        assert!(
            state.view_epoch() > epoch_before,
            "/clear 必须提升视图代际触发清屏"
        );
    }

    /// G8：rebuild_from_events——journal 投影 + 代际 +1 + 旧视图清空。
    #[test]
    fn rebuild_from_events_projects_rows_and_bumps_epoch() {
        let mut state = TuiState::new();
        state.push_user("旧视图残留");
        let epoch_before = state.view_epoch();
        let events = vec![
            TranscriptEvent::User {
                message_id: "m-1".into(),
                text: "第一条".into(),
            },
            TranscriptEvent::Assistant {
                run_id: "r1".into(),
                text: "回答".into(),
            },
            TranscriptEvent::ToolCall {
                run_id: "r1".into(),
                name: "bash".into(),
                input: serde_json::json!({"command": "ls"}),
            },
            TranscriptEvent::ToolResult {
                run_id: "r1".into(),
                name: "bash".into(),
                ok: false,
                output: serde_json::json!("boom"),
            },
        ];
        state.rebuild_from_events(&events);
        let rows = state.rows();
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(matches!(&rows[0], TranscriptRow::User { text } if text == "第一条"));
        assert!(matches!(&rows[1], TranscriptRow::Assistant { complete, .. } if *complete));
        assert!(
            matches!(&rows[2], TranscriptRow::ToolCard { name, is_error, .. } if name == "bash" && *is_error),
            "tool_result 错误位回写最后一张工具卡"
        );
        assert!(state.view_epoch() > epoch_before, "重建必须提升视图代际");
        assert!(state.streaming_preview().is_none());
    }
}
