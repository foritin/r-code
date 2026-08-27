use std::collections::{BTreeMap, BTreeSet};

use r_code_core::dto::{Notification, NotificationKind};
use r_code_core::UserFacingError;
use r_code_host::native_notification::{
    automation_completion_copy, permission_source_key, review_source_key, run_failure_source_key,
    NativeLocalizer, NativeNotificationDelivery, NativeNotificationEvent, NativeNotificationKind,
    NativeNotificationOpenPayload, NativeNotificationPermissionState, NativeNotificationTarget,
    NATIVE_NOTIFICATION_EVENT, NATIVE_NOTIFICATION_OPEN_EVENT,
};
use r_code_store::{Database, NotificationRepository};
use serde_json::{json, Value};

const ZH_CN_CATALOG: &str = include_str!("../frontend/src/i18n/locales/zh-CN.json");
const EN_US_CATALOG: &str = include_str!("../frontend/src/i18n/locales/en-US.json");
const NATIVE_NOTIFICATION_SOURCE: &str = include_str!("../src/native_notification.rs");
const MAIN_SOURCE: &str = include_str!("../src/main.rs");
const TAURI_COMMANDS_SOURCE: &str = include_str!("../src/tauri_commands.rs");

fn leaf_strings(value: &Value, prefix: &str, output: &mut BTreeMap<String, String>) {
    match value {
        Value::Object(entries) => {
            for (key, value) in entries {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                leaf_strings(value, &path, output);
            }
        }
        Value::String(text) => {
            output.insert(prefix.to_string(), text.clone());
        }
        _ => {}
    }
}

fn placeholders(text: &str) -> BTreeSet<String> {
    let mut remaining = text;
    let mut result = BTreeSet::new();
    while let Some(start) = remaining.find("{{") {
        let after_start = &remaining[start + 2..];
        let Some(end) = after_start.find("}}") else {
            break;
        };
        result.insert(after_start[..end].trim().to_string());
        remaining = &after_start[end + 2..];
    }
    result
}

#[test]
fn native_notification_wire_contract_covers_all_states_kinds_and_targets() {
    let permission_states = [
        (NativeNotificationPermissionState::Granted, "granted"),
        (NativeNotificationPermissionState::Denied, "denied"),
        (NativeNotificationPermissionState::Prompt, "prompt"),
        (
            NativeNotificationPermissionState::Unavailable,
            "unavailable",
        ),
    ];
    for (state, wire_name) in permission_states {
        assert_eq!(serde_json::to_value(state).unwrap(), json!(wire_name));
    }

    let kinds = [
        (
            NativeNotificationKind::PermissionRequired,
            "permission_required",
        ),
        (NativeNotificationKind::RunFailed, "run_failed"),
        (NativeNotificationKind::ReviewReady, "review_ready"),
        (
            NativeNotificationKind::AutomationCompleted,
            "automation_completed",
        ),
    ];
    for (kind, wire_name) in kinds {
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(wire_name));
    }

    assert_eq!(
        serde_json::to_value(NativeNotificationDelivery::InApp).unwrap(),
        json!("in_app")
    );
    assert_eq!(
        serde_json::to_value(NativeNotificationDelivery::System).unwrap(),
        json!("system")
    );

    let task_target = NativeNotificationTarget::Task {
        task_id: "task-1".to_string(),
    };
    let automation_target = NativeNotificationTarget::AutomationRun {
        automation_id: "automation-1".to_string(),
        run_id: "run-1".to_string(),
    };
    assert_eq!(
        serde_json::to_value(&task_target).unwrap(),
        json!({ "type": "task", "task_id": "task-1" })
    );
    assert_eq!(
        serde_json::to_value(&automation_target).unwrap(),
        json!({
            "type": "automation_run",
            "automation_id": "automation-1",
            "run_id": "run-1"
        })
    );

    let event = NativeNotificationEvent {
        notification_id: "notification-1".to_string(),
        source_key: "review:task-1:run-1".to_string(),
        kind: NativeNotificationKind::ReviewReady,
        title: "Ready".to_string(),
        body: "Review the result".to_string(),
        target: task_target.clone(),
        delivery: NativeNotificationDelivery::System,
    };
    assert_eq!(
        serde_json::to_value(event).unwrap(),
        json!({
            "notification_id": "notification-1",
            "source_key": "review:task-1:run-1",
            "kind": "review_ready",
            "title": "Ready",
            "body": "Review the result",
            "target": { "type": "task", "task_id": "task-1" },
            "delivery": "system"
        })
    );
    assert_eq!(
        serde_json::to_value(NativeNotificationOpenPayload {
            notification_id: "notification-2".to_string(),
            target: automation_target,
        })
        .unwrap(),
        json!({
            "notification_id": "notification-2",
            "target": {
                "type": "automation_run",
                "automation_id": "automation-1",
                "run_id": "run-1"
            }
        })
    );
    assert_eq!(NATIVE_NOTIFICATION_EVENT, "r-code:native-notification");
    assert_eq!(
        NATIVE_NOTIFICATION_OPEN_EVENT,
        "r-code:native-notification-open"
    );
}

#[test]
fn source_keys_are_stable_idempotency_keys_and_do_not_alias() {
    let keys = [
        permission_source_key("permission-1"),
        review_source_key("task-1", Some("run-1")),
        review_source_key("task-1", None),
        run_failure_source_key("task-1", "run-1"),
    ];
    assert_eq!(keys[0], "permission:permission-1");
    assert_eq!(keys[1], "review:task-1:run-1");
    assert_eq!(keys[2], "review:task-1:task");
    assert_eq!(keys[3], "run_failed:task-1:run-1");
    assert_eq!(keys.iter().collect::<BTreeSet<_>>().len(), keys.len());
    assert_eq!(permission_source_key("permission-1"), keys[0]);
    assert_eq!(review_source_key("task-1", Some("run-1")), keys[1]);
    assert_eq!(run_failure_source_key("task-1", "run-1"), keys[3]);
}

#[test]
fn persisted_notification_source_is_idempotent_and_preserves_read_state() {
    let db = Database::open_in_memory().expect("open notification contract database");
    let repository = NotificationRepository::new(&db);
    let source_key = permission_source_key("permission-1");
    let first = Notification::new(
        NotificationKind::PermissionRequested,
        "Approval required",
        "terminal needs approval",
        None,
        None,
    );
    assert!(repository.upsert(&source_key, &first).unwrap());
    assert_eq!(
        repository.id_for_source(&source_key).unwrap().as_deref(),
        Some(first.id.as_str())
    );
    repository.mark_source_read(&source_key).unwrap();

    let refreshed = Notification::new(
        NotificationKind::PermissionRequested,
        "Updated approval copy",
        "updated body",
        None,
        None,
    );
    assert!(!repository.upsert(&source_key, &refreshed).unwrap());
    assert_eq!(
        repository.id_for_source(&source_key).unwrap().as_deref(),
        Some(first.id.as_str()),
        "an existing source must retain its stable deep-link identifier"
    );

    let rows = repository.list(None, 20, false).unwrap();
    assert_eq!(rows.len(), 1, "one source must persist as one notification");
    let persisted = &rows[0].1;
    assert_eq!(persisted.id, first.id);
    assert_eq!(persisted.title, "Updated approval copy");
    assert_eq!(persisted.body, "updated body");
    assert!(
        persisted.read_at.is_some(),
        "copy refresh must not make a read source unread"
    );
    assert_eq!(repository.unread_count().unwrap(), 0);
}

#[test]
fn native_localizer_uses_the_same_bilingual_catalog_and_placeholder_contract() {
    let zh_catalog: Value = serde_json::from_str(ZH_CN_CATALOG).unwrap();
    let en_catalog: Value = serde_json::from_str(EN_US_CATALOG).unwrap();
    let mut zh = BTreeMap::new();
    let mut en = BTreeMap::new();
    leaf_strings(
        &zh_catalog["notifications"]["native"],
        "notifications.native",
        &mut zh,
    );
    leaf_strings(
        &en_catalog["notifications"]["native"],
        "notifications.native",
        &mut en,
    );

    assert_eq!(zh.keys().collect::<Vec<_>>(), en.keys().collect::<Vec<_>>());
    for key in zh.keys() {
        assert_eq!(
            placeholders(&zh[key]),
            placeholders(&en[key]),
            "placeholder mismatch for {key}"
        );
    }

    let zh_localizer = NativeLocalizer::for_locale("zh-CN");
    let en_localizer = NativeLocalizer::for_locale("en-US");
    assert_eq!(
        zh_localizer.text(
            "notifications.native.permissionTitle",
            &[("task", "修复登录")]
        ),
        "需要批准：修复登录"
    );
    assert_eq!(
        en_localizer.text(
            "notifications.native.permissionBody",
            &[("tool", "terminal")]
        ),
        "terminal needs your approval to continue."
    );
    assert_eq!(
        NativeLocalizer::for_locale("en-GB").text("notifications.native.reviewBody", &[]),
        en_localizer.text("notifications.native.reviewBody", &[])
    );
    assert_eq!(
        NativeLocalizer::for_locale("fr-FR").text("notifications.native.reviewBody", &[]),
        zh_localizer.text("notifications.native.reviewBody", &[])
    );

    let (zh_title, zh_body) = automation_completion_copy("zh-CN", "每日检查");
    let (en_title, en_body) = automation_completion_copy("en-US", "Daily check");
    assert_eq!(zh_title, "自动化已完成：每日检查");
    assert_eq!(zh_body, "打开运行记录查看结果。");
    assert_eq!(en_title, "Automation completed: Daily check");
    assert_eq!(en_body, "Open the run to review the result.");
    for rendered in [zh_title, zh_body, en_title, en_body] {
        assert!(
            !rendered.contains("{{"),
            "unexpanded native copy: {rendered}"
        );
    }

    assert_eq!(
        zh_localizer.text("settings.notifications.status.denied.description", &[]),
        "提醒仍会保留在应用内；如果系统不再弹出授权框，请在操作系统的通知设置中允许 R-Code。"
    );
    assert_eq!(
        en_localizer.text("settings.notifications.status.denied.description", &[]),
        "Alerts will still appear in the app. If no permission prompt appears, allow R-Code in the operating system's notification settings."
    );
}

#[test]
fn system_delivery_is_granted_only_and_other_permission_outcomes_fall_back_in_app() {
    assert!(NATIVE_NOTIFICATION_SOURCE
        .contains("permission_state(app) == NativeNotificationPermissionState::Granted"));
    assert!(NATIVE_NOTIFICATION_SOURCE
        .contains("PermissionState::Prompt | PermissionState::PromptWithRationale"));
    assert!(NATIVE_NOTIFICATION_SOURCE.contains("NativeNotificationPermissionState::Unavailable"));

    let delivery_start = NATIVE_NOTIFICATION_SOURCE
        .find("fn deliver(")
        .expect("native delivery function");
    let delivery_source = &NATIVE_NOTIFICATION_SOURCE[delivery_start..];
    let system_assignment = delivery_source
        .find("event.delivery = NativeNotificationDelivery::System")
        .expect("system-delivery assignment");
    let bridge_emit = delivery_source
        .find("app.emit(NATIVE_NOTIFICATION_EVENT, event)")
        .expect("in-app bridge fallback");
    assert!(
        bridge_emit > system_assignment,
        "the bridge event must remain outside the granted-system branch so denied, prompt, unavailable, and send failures fall back in-app"
    );
}

#[test]
fn only_a_real_system_banner_response_has_an_activation_path() {
    assert!(NATIVE_NOTIFICATION_SOURCE.contains("handle.wait_for_response"));
    assert!(NATIVE_NOTIFICATION_SOURCE.contains("#[cfg(all(unix, not(target_os = \"macos\")))]"));
    assert!(NATIVE_NOTIFICATION_SOURCE.contains("notification.action(\"default\", \"\")"));
    assert!(NATIVE_NOTIFICATION_SOURCE.contains("response_activates_notification(response)"));
    assert!(NATIVE_NOTIFICATION_SOURCE
        .contains("NotificationResponse::Default | notify_rust::NotificationResponse::Action(_)"));
    assert!(NATIVE_NOTIFICATION_SOURCE.contains("activate_notification_target(&activation_app"));
    assert!(
        !NATIVE_NOTIFICATION_SOURCE.contains(concat!("pending_", "activation")),
        "showing a banner must not pre-arm a later ordinary-focus deep link"
    );
    assert!(
        !MAIN_SOURCE.contains("handle_main_window_focus"),
        "ordinary main-window focus must not consume notification targets"
    );
}

#[test]
fn notification_commands_expose_structured_errors_without_displaying_debug_detail() {
    for command in [
        "cmd_native_notification_request_permission",
        "cmd_native_notification_set_locale",
    ] {
        let start = TAURI_COMMANDS_SOURCE
            .find(&format!("pub fn {command}"))
            .unwrap_or_else(|| panic!("missing Tauri notification command {command}"));
        let signature = &TAURI_COMMANDS_SOURCE[start..TAURI_COMMANDS_SOURCE.len().min(start + 420)];
        assert!(
            signature.contains("r_code_core::UserFacingError"),
            "{command} must return the structured user error type"
        );
    }

    assert!(NATIVE_NOTIFICATION_SOURCE
        .contains("UserFacingError::new(\"notifications.service_unavailable\")"));
    assert!(NATIVE_NOTIFICATION_SOURCE
        .contains("UserFacingError::new(\"notifications.permission_request_failed\")"));

    let error = UserFacingError::new("notifications.permission_request_failed")
        .with_debug_detail("authorization=secret-notification-token");
    assert_eq!(error.to_string(), "notifications.permission_request_failed");
    assert!(!error.to_string().contains("secret-notification-token"));
    assert_eq!(
        NativeLocalizer::for_locale("en-US")
            .text("errors.notifications.permission_request_failed", &[]),
        "R-Code could not request system notification permission. Check the operating system's notification settings and try again."
    );
}
