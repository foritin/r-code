//! Native desktop notification delivery and routing.
//!
//! Product notifications are first persisted in the Notification Center. This module then routes
//! each newly observed live source once per runtime: focused windows receive the existing in-app
//! toast event, while unfocused windows use the operating-system notification layer. A denied,
//! unrequested, or unavailable OS permission always falls back to the in-app event.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use r_code_core::dto::{
    AgentEvent, Notification, NotificationKind, PermissionRequest, ReviewState, Task, TaskState,
};
use r_code_core::UserFacingError;
use r_code_store::{AgentRunRepository, Database, NotificationRepository, TaskRepository};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::{NotificationExt, PermissionState};

pub const NATIVE_NOTIFICATION_EVENT: &str = "r-code:native-notification";
pub const NATIVE_NOTIFICATION_OPEN_EVENT: &str = "r-code:native-notification-open";
const DELIVERED_SOURCE_CAPACITY: usize = 512;

const ZH_CN_CATALOG_JSON: &str = include_str!("../frontend/src/i18n/locales/zh-CN.json");
const EN_US_CATALOG_JSON: &str = include_str!("../frontend/src/i18n/locales/en-US.json");
static ZH_CN_CATALOG: OnceLock<Value> = OnceLock::new();
static EN_US_CATALOG: OnceLock<Value> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeNotificationPermissionState {
    Granted,
    Denied,
    Prompt,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeNotificationKind {
    PermissionRequired,
    RunFailed,
    ReviewReady,
    /// Payload and copy are reserved for the Automation milestone; no producer exists yet.
    AutomationCompleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeNotificationDelivery {
    InApp,
    System,
}

/// Stable deep-link payload shared by system-notification activation and future Automation runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeNotificationTarget {
    Task {
        task_id: String,
    },
    AutomationRun {
        automation_id: String,
        run_id: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct NativeNotificationEvent {
    pub notification_id: String,
    pub source_key: String,
    pub kind: NativeNotificationKind,
    pub title: String,
    pub body: String,
    pub target: NativeNotificationTarget,
    pub delivery: NativeNotificationDelivery,
}

#[derive(Debug, Clone, Serialize)]
pub struct NativeNotificationOpenPayload {
    pub notification_id: String,
    pub target: NativeNotificationTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeLocale {
    ZhCn,
    EnUs,
}

impl NativeLocale {
    fn parse(value: &str) -> Self {
        if value.trim().to_ascii_lowercase().starts_with("en") {
            Self::EnUs
        } else {
            Self::ZhCn
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::EnUs => "en-US",
        }
    }
}

/// Minimal native-side renderer backed by the exact catalogs compiled into the WebView bundle.
/// Native delivery can happen while JavaScript is suspended, so it cannot call i18next directly;
/// sharing catalog keys keeps both surfaces on one copy contract.
#[derive(Debug, Clone, Copy)]
pub struct NativeLocalizer {
    locale: NativeLocale,
}

impl NativeLocalizer {
    pub fn for_locale(locale: &str) -> Self {
        Self {
            locale: NativeLocale::parse(locale),
        }
    }

    fn from_native_locale(locale: NativeLocale) -> Self {
        Self { locale }
    }

    pub fn text(self, key: &str, args: &[(&str, &str)]) -> String {
        let catalog = match self.locale {
            NativeLocale::ZhCn => ZH_CN_CATALOG.get_or_init(|| {
                serde_json::from_str(ZH_CN_CATALOG_JSON)
                    .expect("embedded zh-CN notification catalog must be valid JSON")
            }),
            NativeLocale::EnUs => EN_US_CATALOG.get_or_init(|| {
                serde_json::from_str(EN_US_CATALOG_JSON)
                    .expect("embedded en-US notification catalog must be valid JSON")
            }),
        };
        let template = key
            .split('.')
            .try_fold(catalog, |value, segment| value.get(segment))
            .and_then(Value::as_str)
            .unwrap_or(key);
        args.iter()
            .fold(template.to_string(), |rendered, (name, value)| {
                rendered.replace(&format!("{{{{{name}}}}}"), value)
            })
    }
}

/// Runtime-only delivery state. Notification history and unread state remain SQLite-authoritative.
pub struct NativeNotificationState {
    locale: Mutex<NativeLocale>,
    delivered_sources: Mutex<VecDeque<String>>,
}

impl Default for NativeNotificationState {
    fn default() -> Self {
        Self {
            locale: Mutex::new(NativeLocale::ZhCn),
            delivered_sources: Mutex::new(VecDeque::new()),
        }
    }
}

fn claim_delivery_source(app: &AppHandle, source_key: &str, inserted: bool) -> bool {
    let Some(state) = app.try_state::<NativeNotificationState>() else {
        return inserted;
    };
    let mut sources = state
        .delivered_sources
        .lock()
        .unwrap_or_else(r_code_core::sync_util::recover_poisoned_guard);
    if sources.iter().any(|source| source == source_key) {
        return false;
    }
    sources.push_back(source_key.to_string());
    if sources.len() > DELIVERED_SOURCE_CAPACITY {
        sources.pop_front();
    }
    true
}

pub fn permission_source_key(request_id: &str) -> String {
    format!("permission:{request_id}")
}

pub fn review_source_key(task_id: &str, run_id: Option<&str>) -> String {
    format!("review:{task_id}:{}", run_id.unwrap_or("task"))
}

pub fn run_failure_source_key(task_id: &str, run_id: &str) -> String {
    format!("run_failed:{task_id}:{run_id}")
}

pub fn set_locale(app: &AppHandle, locale: &str) -> Result<(), UserFacingError> {
    let state = app.try_state::<NativeNotificationState>().ok_or_else(|| {
        UserFacingError::new("notifications.service_unavailable")
            .with_debug_detail("NativeNotificationState is not managed by the application")
    })?;
    *state
        .locale
        .lock()
        .unwrap_or_else(r_code_core::sync_util::recover_poisoned_guard) =
        NativeLocale::parse(locale);
    Ok(())
}

pub fn locale_code(app: &AppHandle) -> &'static str {
    current_locale(app).code()
}

pub fn permission_state(app: &AppHandle) -> NativeNotificationPermissionState {
    match app.notification().permission_state() {
        Ok(state) => map_permission_state(state),
        Err(error) => {
            tracing::warn!(%error, "native notification permission state is unavailable");
            NativeNotificationPermissionState::Unavailable
        }
    }
}

pub fn request_permission(
    app: &AppHandle,
) -> Result<NativeNotificationPermissionState, UserFacingError> {
    app.notification()
        .request_permission()
        .map(map_permission_state)
        .map_err(|error| {
            UserFacingError::new("notifications.permission_request_failed")
                .with_debug_detail(error.to_string())
        })
}

fn map_permission_state(state: PermissionState) -> NativeNotificationPermissionState {
    match state {
        PermissionState::Granted => NativeNotificationPermissionState::Granted,
        PermissionState::Denied => NativeNotificationPermissionState::Denied,
        PermissionState::Prompt | PermissionState::PromptWithRationale => {
            NativeNotificationPermissionState::Prompt
        }
    }
}

fn current_locale(app: &AppHandle) -> NativeLocale {
    app.try_state::<NativeNotificationState>()
        .map(|state| {
            *state
                .locale
                .lock()
                .unwrap_or_else(r_code_core::sync_util::recover_poisoned_guard)
        })
        .unwrap_or(NativeLocale::ZhCn)
}

fn current_localizer(app: &AppHandle) -> NativeLocalizer {
    NativeLocalizer::from_native_locale(current_locale(app))
}

fn task_label(task: &Task, localizer: NativeLocalizer) -> String {
    let title = task.title.trim();
    if !title.is_empty() {
        return title.to_string();
    }
    let goal = task.goal.trim();
    if !goal.is_empty() {
        return goal.to_string();
    }
    localizer.text("notifications.unnamedTask", &[])
}

/// Reserved copy for the Automation milestone. Keeping it here freezes the bilingual contract
/// without creating schedules, runs, persistence, or any other Automation behavior.
pub fn automation_completion_copy(locale: &str, automation_title: &str) -> (String, String) {
    let localizer = NativeLocalizer::for_locale(locale);
    (
        localizer.text(
            "notifications.native.automationTitle",
            &[("automation", automation_title)],
        ),
        localizer.text("notifications.native.automationBody", &[]),
    )
}

fn main_window_is_foreground(app: &AppHandle) -> bool {
    let Some(window) = app.get_webview_window("main") else {
        return false;
    };
    window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false)
}

fn activate_notification_target(app: &AppHandle, payload: NativeNotificationOpenPayload) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    if let Err(error) = app.emit(NATIVE_NOTIFICATION_OPEN_EVENT, payload) {
        tracing::warn!(%error, "failed to emit native notification deep link");
    }
}

fn response_activates_notification(response: &notify_rust::NotificationResponse) -> bool {
    matches!(
        response,
        notify_rust::NotificationResponse::Default | notify_rust::NotificationResponse::Action(_)
    )
}

fn show_system_notification(
    app: &AppHandle,
    event: &NativeNotificationEvent,
    payload: NativeNotificationOpenPayload,
) -> Result<(), String> {
    let mut notification = notify_rust::Notification::new();
    notification
        .summary(&event.title)
        .body(&event.body)
        .auto_icon();

    // XDG daemons only emit ActionInvoked for a body click when the conventional `default`
    // action is declared. Closed/expired responses remain non-activating below.
    #[cfg(all(unix, not(target_os = "macos")))]
    notification.action("default", "");

    // Match tauri-plugin-notification's platform identity setup while retaining notify-rust's
    // response handle, which the plugin currently discards on desktop.
    #[cfg(target_os = "windows")]
    {
        use std::path::MAIN_SEPARATOR;

        let executable =
            tauri::utils::platform::current_exe().map_err(|error| error.to_string())?;
        let directory = executable
            .parent()
            .ok_or_else(|| "notification executable has no parent directory".to_string())?;
        let directory = directory.display().to_string();
        if !(directory.ends_with(format!("{MAIN_SEPARATOR}target{MAIN_SEPARATOR}debug").as_str())
            || directory
                .ends_with(format!("{MAIN_SEPARATOR}target{MAIN_SEPARATOR}release").as_str()))
        {
            notification.app_id(&app.config().identifier);
        }
    }
    #[cfg(target_os = "macos")]
    {
        let application = if tauri::is_dev() {
            "com.apple.Terminal"
        } else {
            &app.config().identifier
        };
        let _ = notify_rust::set_application(application);
    }

    let handle = notification.show().map_err(|error| error.to_string())?;
    let activation_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(error) =
            handle.wait_for_response(move |response: &notify_rust::NotificationResponse| {
                if response_activates_notification(response) {
                    activate_notification_target(&activation_app, payload);
                }
            })
        {
            tracing::warn!(%error, "failed while waiting for system notification activation");
        }
    });
    Ok(())
}

fn deliver(app: &AppHandle, mut event: NativeNotificationEvent) {
    if !main_window_is_foreground(app)
        && permission_state(app) == NativeNotificationPermissionState::Granted
    {
        let payload = NativeNotificationOpenPayload {
            notification_id: event.notification_id.clone(),
            target: event.target.clone(),
        };
        match show_system_notification(app, &event, payload.clone()) {
            Ok(()) => {
                event.delivery = NativeNotificationDelivery::System;
            }
            Err(error) => {
                tracing::warn!(%error, "system notification failed; using in-app fallback");
            }
        }
    }

    if let Err(error) = app.emit(NATIVE_NOTIFICATION_EVENT, event) {
        tracing::warn!(%error, "failed to emit native notification bridge event");
    }
}

pub fn record_permission_request(
    app: &AppHandle,
    db: &Database,
    request: &PermissionRequest,
) -> Result<(), String> {
    let task = TaskRepository::new(db)
        .get(&request.task_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            format!(
                "task not found for permission notification: {}",
                request.task_id
            )
        })?;
    let localizer = current_localizer(app);
    let label = task_label(&task, localizer);
    let title = localizer.text("notifications.native.permissionTitle", &[("task", &label)]);
    let native_body = localizer.text(
        "notifications.native.permissionBody",
        &[("tool", &request.tool_name)],
    );
    let stored_body = if request.input_summary.trim().is_empty() {
        native_body.clone()
    } else {
        request.input_summary.clone()
    };
    let notification = Notification::new(
        NotificationKind::PermissionRequested,
        title.clone(),
        stored_body,
        Some(task.id.clone()),
        task.workspace_path.clone(),
    );
    let source_key = permission_source_key(&request.id);
    let notifications = NotificationRepository::new(db);
    let inserted = notifications
        .upsert(&source_key, &notification)
        .map_err(|error| error.to_string())?;
    if !claim_delivery_source(app, &source_key, inserted) {
        return Ok(());
    }
    let notification_id = if inserted {
        notification.id.clone()
    } else {
        notifications
            .id_for_source(&source_key)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("notification source disappeared after upsert: {source_key}"))?
    };

    deliver(
        app,
        NativeNotificationEvent {
            notification_id,
            source_key,
            kind: NativeNotificationKind::PermissionRequired,
            title,
            body: native_body,
            target: NativeNotificationTarget::Task { task_id: task.id },
            delivery: NativeNotificationDelivery::InApp,
        },
    );
    Ok(())
}

pub fn record_agent_event(
    app: &AppHandle,
    db: &Database,
    task_id: &str,
    event: &AgentEvent,
) -> Result<(), String> {
    let AgentEvent::State { state } = event else {
        return Ok(());
    };
    if !matches!(state, TaskState::ReviewReady | TaskState::Idle) {
        return Ok(());
    }

    let task = TaskRepository::new(db)
        .get(task_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("task not found for native notification: {task_id}"))?;
    let latest_run = AgentRunRepository::new(db)
        .get_latest_main_run(task_id)
        .map_err(|error| error.to_string())?;
    let Some(run) = latest_run.filter(|run| run.ended_at.is_some()) else {
        return Ok(());
    };
    let localizer = current_localizer(app);
    let label = task_label(&task, localizer);

    let (source_key, notification_kind, native_kind, title_key, body_key) =
        if *state == TaskState::ReviewReady {
            (
                review_source_key(task_id, Some(&run.id)),
                NotificationKind::ReviewReady,
                NativeNotificationKind::ReviewReady,
                "notifications.native.reviewTitle",
                "notifications.native.reviewBody",
            )
        } else if run.review_state == ReviewState::Failed {
            (
                run_failure_source_key(task_id, &run.id),
                NotificationKind::RunFailed,
                NativeNotificationKind::RunFailed,
                "notifications.native.failureTitle",
                "notifications.native.failureBody",
            )
        } else {
            return Ok(());
        };
    let title = localizer.text(title_key, &[("task", &label)]);
    let body = localizer.text(body_key, &[]);

    let notification = Notification::new(
        notification_kind,
        title.clone(),
        body.clone(),
        Some(task.id.clone()),
        task.workspace_path.clone(),
    );
    let notifications = NotificationRepository::new(db);
    let inserted = notifications
        .upsert(&source_key, &notification)
        .map_err(|error| error.to_string())?;
    if !claim_delivery_source(app, &source_key, inserted) {
        return Ok(());
    }
    let notification_id = if inserted {
        notification.id.clone()
    } else {
        notifications
            .id_for_source(&source_key)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("notification source disappeared after upsert: {source_key}"))?
    };

    deliver(
        app,
        NativeNotificationEvent {
            notification_id,
            source_key,
            kind: native_kind,
            title,
            body,
            target: NativeNotificationTarget::Task {
                task_id: task.id.clone(),
            },
            delivery: NativeNotificationDelivery::InApp,
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::response_activates_notification;
    use notify_rust::{CloseReason, NotificationResponse};

    const MAIN_SOURCE: &str = include_str!("main.rs");
    const NOTIFICATION_SOURCE: &str = include_str!("native_notification.rs");

    #[test]
    fn only_real_notification_activation_responses_open_the_target() {
        assert!(response_activates_notification(
            &NotificationResponse::Default
        ));
        assert!(response_activates_notification(
            &NotificationResponse::Action("open".to_string())
        ));
        assert!(!response_activates_notification(
            &NotificationResponse::Closed(CloseReason::Dismissed)
        ));
        assert!(!response_activates_notification(
            &NotificationResponse::Closed(CloseReason::Expired)
        ));
        assert!(!response_activates_notification(
            &NotificationResponse::Reply("ignored".to_string())
        ));
    }

    #[test]
    fn ordinary_window_focus_has_no_notification_activation_path() {
        assert!(!MAIN_SOURCE.contains("handle_main_window_focus"));
        assert!(!MAIN_SOURCE.contains("WindowEvent::Focused"));
        assert!(!NOTIFICATION_SOURCE.contains(concat!("pending_", "activation")));
    }
}
