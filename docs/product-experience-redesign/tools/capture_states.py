from __future__ import annotations

from pathlib import Path
import hashlib
import json
import re
import sys

from playwright.sync_api import Browser, Page, sync_playwright


ROOT = Path(__file__).resolve().parents[1]
HTML = ROOT / "prototype.html"
OUTPUT = ROOT / "images"
SETTLE_MS = 180

VIEWPORTS = [
    (1600, 960),
    (1280, 800),
    (1024, 768),
    (960, 640),
    (740, 800),
    (390, 844),
]

SETTINGS_PANELS = [
    ("providers", "模型服务"),
    ("agents", "Agent 编排"),
    ("subagents", "子代理配置"),
    ("tools", "工具与浏览器"),
    ("knowledge", "知识与指令"),
    ("permissions", "权限"),
    ("security", "隐私与安全"),
    ("appearance", "外观与语言"),
    ("notifications", "通知"),
    ("lifecycle", "启动与关闭"),
    ("updates", "更新"),
    ("diagnostics", "诊断"),
]


class CaptureLog:
    def __init__(self) -> None:
        self.names: list[str] = []

    def shot(
        self,
        page: Page,
        name: str,
        *,
        full_page: bool = False,
        delay_ms: int = SETTLE_MS,
    ) -> None:
        if name in self.names:
            raise AssertionError(f"duplicate screenshot name: {name}")
        settle(page, delay_ms)
        page.screenshot(path=OUTPUT / name, full_page=full_page, caret="hide")
        self.names.append(name)


def must(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def attach_diagnostics(page: Page, errors: list[str]) -> None:
    page.on(
        "console",
        lambda message: errors.append(f"console:{message.type}:{message.text}")
        if message.type in {"warning", "error"}
        else None,
    )
    page.on("pageerror", lambda error: errors.append(f"pageerror:{error}"))
    page.on("requestfailed", lambda request: errors.append(f"requestfailed:{request.url}"))


def settle(page: Page, delay_ms: int = SETTLE_MS) -> None:
    page.evaluate("document.fonts && document.fonts.ready")
    page.evaluate(
        "() => new Promise(resolve => "
        "requestAnimationFrame(() => requestAnimationFrame(resolve)))"
    )
    if delay_ms:
        page.wait_for_timeout(delay_ms)


def load(page: Page, *, startup_settle_ms: int = 90) -> None:
    page.goto(HTML.as_uri(), wait_until="load")
    page.wait_for_function("window.__ready === true")
    settle(page, startup_settle_ms)


def reset_page(page: Page, *, width: int = 1600, height: int = 960) -> None:
    page.set_viewport_size({"width": width, "height": height})
    load(page)


def assert_no_overflow(page: Page, label: str) -> None:
    metrics = page.evaluate(
        """() => ({
          bodyClient: document.body.clientWidth,
          bodyScroll: document.body.scrollWidth,
          htmlClient: document.documentElement.clientWidth,
          htmlScroll: document.documentElement.scrollWidth
        })"""
    )
    if (
        metrics["bodyScroll"] > metrics["bodyClient"] + 1
        or metrics["htmlScroll"] > metrics["htmlClient"] + 1
    ):
        raise AssertionError(f"horizontal overflow at {label}: {metrics}")


def assert_named_visible_controls(page: Page, label: str) -> None:
    unnamed = page.evaluate(
        """() => [...document.querySelectorAll(
          'button, input, select, textarea'
        )].filter(element => {
          const style = getComputedStyle(element);
          return element.getClientRects().length > 0
            && style.visibility !== 'hidden'
            && style.display !== 'none';
        }).map((element, index) => {
          const labelledBy = (element.getAttribute('aria-labelledby') || '')
            .split(/\\s+/)
            .filter(Boolean)
            .map(id => document.getElementById(id)?.textContent || '')
            .join(' ');
          const labels = [...(element.labels || [])]
            .map(item => item.innerText || item.textContent || '')
            .join(' ');
          const intrinsic = element.matches(
            'input[type="button"], input[type="submit"], input[type="reset"]'
          ) ? element.value : element.innerText || '';
          const name = [
            element.getAttribute('aria-label'),
            labelledBy,
            labels,
            intrinsic,
            element.getAttribute('placeholder'),
            element.getAttribute('title')
          ].find(value => value && value.trim());
          return name ? null : {
            index,
            tag: element.tagName.toLowerCase(),
            id: element.id,
            type: element.getAttribute('type') || ''
          };
        }).filter(Boolean)"""
    )
    if unnamed:
        raise AssertionError(f"unnamed visible controls at {label}: {unnamed}")


def assert_in_viewport(page: Page, selector: str, label: str) -> None:
    locator = page.locator(selector)
    must(locator.is_visible(), f"{label} is not visible")
    box = locator.bounding_box()
    viewport = page.viewport_size
    must(box is not None and viewport is not None, f"{label} has no layout box")
    must(box["x"] >= -1 and box["y"] >= -1, f"{label} begins outside viewport: {box}")
    must(
        box["x"] + box["width"] <= viewport["width"] + 1
        and box["y"] + box["height"] <= viewport["height"] + 1,
        f"{label} ends outside viewport: {box}, viewport={viewport}",
    )


def visible_task_keys(page: Page) -> set[str]:
    return set(
        page.locator("[data-task-key]:visible").evaluate_all(
            "elements => elements.map(element => element.dataset.taskKey)"
        )
    )


def select_settings_panel(page: Page, name: str):
    button = page.locator(f'[data-settings-target="{name}"]')
    panel = page.locator(f'[data-settings-panel="{name}"]')
    button.click()
    must(
        button.get_attribute("aria-current") == "page",
        f"settings nav did not select {name}",
    )
    must(panel.is_visible(), f"settings panel did not become visible: {name}")
    return panel


def select_health_scenario(
    page: Page,
    scenario: str,
    *,
    keep_open: bool = False,
) -> None:
    popover = page.locator("#health-popover")
    if not popover.is_visible():
        page.click("#health-trigger")
    button = page.locator(f'[data-health-scenario="{scenario}"]')
    must(button.is_visible(), f"health scenario is not publicly visible: {scenario}")
    button.click()
    must(
        button.get_attribute("aria-pressed") == "true",
        f"health scenario did not activate: {scenario}",
    )
    if not keep_open:
        page.click("#health-close")


def select_run_state(
    page: Page,
    state: str,
    *,
    expanded: bool | None = None,
) -> None:
    page.select_option("#run-state-preview", state)
    must(
        page.input_value("#run-state-preview") == state,
        f"run state did not activate: {state}",
    )
    if expanded is None:
        return
    is_expanded = "expanded" in (
        page.locator("#run-card").get_attribute("class") or ""
    ).split()
    if is_expanded != expanded:
        page.click("#run-toggle")


def ensure_theme(page: Page, theme: str) -> None:
    if page.locator("html").get_attribute("data-theme") != theme:
        page.click("#theme-toggle")
    must(
        page.locator("html").get_attribute("data-theme") == theme,
        f"theme did not activate: {theme}",
    )


def reset_close_preference(page: Page) -> None:
    page.click("#open-settings")
    select_settings_panel(page, "lifecycle")
    page.click("#reset-close-preference")
    page.click("#settings-back")


def trust_and_probe_codex_subagent(
    page: Page,
    shots: CaptureLog | None = None,
) -> dict[str, str]:
    """Recover Codex through visible Runtime controls, then create a receipt.

    Availability and health are deliberately exercised as separate axes. The
    initial trust gate is cleared through the Agent-page Runtime refresh; the
    public health preview is then used to return to ``untested`` before the
    explicit single-source probe. No internal map or row dataset is mutated.
    """

    select_settings_panel(page, "subagents")
    page.select_option("#subagent-source-preview", "codex")
    availability = page.input_value("#subagent-availability-preview")
    initial_health = page.input_value("#subagent-source-preview-state")
    must(
        availability in {"trust_required", "ready"},
        f"unexpected initial Codex availability: {availability}",
    )
    if availability == "trust_required" and shots is not None:
        must(
            initial_health == "untested"
            and page.locator('[data-subagent-test="codex"]').is_disabled(),
            "trust-required Codex did not preserve untested, disabled health",
        )
        shots.shot(page, "128-subagent-trust-required-health-untested-dark.png")

    if availability != "ready":
        select_settings_panel(page, "agents")
        page.click("#test-codex-runtime")
        must(
            "正在验证" in page.locator("#codex-runtime-status").inner_text(),
            "Codex Runtime refresh did not enter checking",
        )
        page.wait_for_function(
            "document.getElementById('codex-runtime-status')"
            ".textContent.includes('ready')"
            " && !document.getElementById('test-codex-runtime').disabled",
            timeout=3000,
        )
        select_settings_panel(page, "subagents")
        page.select_option("#subagent-source-preview", "codex")
        page.wait_for_function(
            "document.getElementById('subagent-availability-preview')"
            ".value === 'ready'",
            timeout=3000,
        )

    # Re-enter an explicit, independently observable health state. Returning
    # to the panel may have started the documented automatic probe already.
    if page.input_value("#subagent-source-preview-state") == "checking":
        page.wait_for_function(
            "document.querySelector('[data-subagent-source=\"codex\"]')"
            ".dataset.receiptState === 'connected'",
            timeout=3000,
        )
    page.select_option("#subagent-source-preview-state", "untested")
    must(
        page.input_value("#subagent-availability-preview") == "ready"
        and page.input_value("#subagent-source-preview-state") == "untested",
        "Codex availability and health axes were not independently observable",
    )
    must(
        not page.locator('[data-subagent-test="codex"]').is_disabled(),
        "ready Codex source did not enable the explicit probe",
    )
    if shots is not None:
        shots.shot(page, "129-subagent-ready-health-untested-dark.png")

    page.click('[data-subagent-test="codex"]')
    must(
        page.input_value("#subagent-source-preview-state") == "checking",
        "Codex health probe did not enter checking",
    )
    if shots is not None:
        shots.shot(
            page,
            "130-subagent-ready-health-checking-dark.png",
            delay_ms=40,
        )
    page.wait_for_function(
        "document.querySelector('[data-subagent-source=\"codex\"]')"
        ".dataset.receiptState === 'connected'",
        timeout=3000,
    )
    must(
        "新回执"
        in page.locator(
            '[data-subagent-source="codex"] .status-inline'
        ).inner_text(),
        "Codex exact source + model receipt was not created",
    )
    if shots is not None:
        shots.shot(page, "131-subagent-ready-health-connected-dark.png")
    return {
        "initial_availability": availability,
        "initial_health": initial_health,
        "final_availability": page.input_value(
            "#subagent-availability-preview"
        ),
        "final_health": page.input_value("#subagent-source-preview-state"),
    }


def verify_startup_health(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    reset_page(page)
    must(
        page.locator(".health-row.checking").count() == 2,
        "startup health must run exactly two checks",
    )
    must(
        page.locator(".health-row.queued").count() == 1,
        "startup health must queue exactly one provider",
    )
    must(
        page.locator("#health-recheck").is_disabled(),
        "manual check must be disabled only while checking",
    )
    page.click("#health-trigger")
    shots.shot(page, "01-launch-provider-checking-dark.png", delay_ms=60)
    page.wait_for_function(
        "document.querySelectorAll('.health-row.checking').length === 0",
        timeout=3000,
    )
    must(
        "3 个连接正常"
        in (page.locator("#health-trigger").get_attribute("aria-label") or ""),
        "startup health did not recover",
    )
    shots.shot(page, "02-launch-provider-recovered-dark.png")

    reset_page(page)
    must(
        page.locator(".health-row.checking").count() == 2
        and page.locator(".health-row.queued").count() == 1,
        "fresh startup did not re-enter the bounded checking state",
    )
    page.click("#health-trigger")
    page.uncheck("#startup-probe-toggle")
    must(
        not page.locator("#health-recheck").is_disabled(),
        "manual health check was disabled after auto-check opt-out",
    )
    opt_out_summary = page.locator("#health-summary").inner_text()
    must(
        "自动检查已关闭" in opt_out_summary
        and "可随时再次检查" in opt_out_summary,
        "auto-check opt-out did not explain that manual recheck remains available",
    )
    page.wait_for_timeout(1400)
    cancelled_rows_copy = page.locator("#health-list").inner_text()
    must(
        page.locator(".health-row.checking").count() == 0
        and page.locator(".health-row.queued").count() == 3
        and "provenance=last-known" in cancelled_rows_copy
        and "未重测" in cancelled_rows_copy
        and "exact-model 回执有效" not in cancelled_rows_copy
        and "3 个连接正常"
        not in (page.locator("#health-trigger").get_attribute("aria-label") or ""),
        "startup opt-out synthesized a successful receipt from checking/queued rows",
    )
    shots.shot(page, "02a-startup-opt-out-no-false-success-dark.png")
    page.click("#health-recheck")
    must(
        page.locator(".health-row.checking").count() == 2,
        "manual health check did not respect concurrency=2",
    )
    must(
        page.locator(".health-row.queued").count() == 1,
        "manual health check did not retain one queued provider",
    )
    shots.shot(
        page,
        "03-provider-manual-check-with-auto-off-dark.png",
        delay_ms=60,
    )
    page.wait_for_function(
        "document.querySelectorAll('.health-row.checking').length === 0",
        timeout=3000,
    )
    shots.shot(page, "04-provider-manual-check-recovered-dark.png")
    page.click("#health-close")
    settle(page, 30)
    must(
        page.evaluate("document.activeElement.id") == "health-trigger",
        "health popover did not restore focus",
    )
    return {
        "checking": 2,
        "queued": 1,
        "startup_opt_out_no_false_success": "passed",
        "manual_when_auto_off": "passed",
    }


def verify_running_and_terminal_states(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    select_health_scenario(page, "recovered")
    select_run_state(page, "running", expanded=False)
    must(
        "status-spinner"
        in (page.locator("#live-status-glyph").get_attribute("class") or ""),
        "running live status has no spinner",
    )
    must(
        page.locator('[data-task-key="experience"] .status-spinner').count() == 1,
        "running task row has no spinner",
    )
    must(
        page.locator('[data-task-key="experience"] .dot').count() == 0,
        "running session regressed to a colored dot marker",
    )
    must(
        page.locator('[data-agent="root"] .agent-state .status-spinner').count()
        == 1,
        "running root agent has no neutral spinner",
    )
    shots.shot(page, "05-workspace-running-dark.png")
    page.click("#run-toggle")
    shots.shot(page, "06-run-expanded-dark.png")
    page.click("#tool-toggle")
    shots.shot(page, "07-run-tools-dark.png")

    page.click('[data-view="历史"]')
    page.click('[data-task-key="runtime"]')
    must(
        "status-mark"
        in (page.locator("#live-status-glyph").get_attribute("class") or ""),
        "terminal live status still uses spinner",
    )
    must(
        page.locator('[data-task-key="runtime"] .status-spinner').count() == 0,
        "terminal task row still uses spinner",
    )
    must(
        page.locator(
            '[data-agent="runtime"] .agent-state .status-spinner'
        ).count()
        == 0,
        "terminal agent still uses spinner",
    )
    shots.shot(page, "08-terminal-state-without-spinner-dark.png")
    page.click('[data-view="进行中"]')
    page.click('[data-task-key="experience"]')
    return {
        "running_spinner": "passed",
        "colored_session_dots": 0,
        "terminal_static_mark": "passed",
    }


def verify_tasks_and_drafts(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    page.click("#new-task")
    must(page.locator("#task-empty").is_visible(), "new-task empty state missing")
    must(
        page.locator("#task-title").inner_text() == "新任务",
        "new-task title missing",
    )
    shots.shot(page, "09-new-task-empty-dark.png")

    new_task_prompt = "为设置搜索增加可恢复的 OCR 深链"
    page.fill("#prompt", new_task_prompt)
    page.press("#prompt", "Enter")
    new_task_row = page.locator('[data-task-key^="new-demo-"]')
    must(
        new_task_row.count() == 1,
        "new task row was not created",
    )
    new_task_key = new_task_row.get_attribute("data-task-key")
    must(bool(new_task_key), "new task row has no stable task key")
    must(
        new_task_row.get_attribute("aria-current") == "page",
        "new task row is not current",
    )
    must(
        new_task_prompt in page.locator("#seed-user-copy").inner_text(),
        "new task content did not render",
    )
    shots.shot(page, "10-new-task-created-dark.png")

    page.fill("#prompt", "新任务草稿：保留 OCR 验收")
    page.click('[data-view="需要我"]')
    page.click('[data-task-key="close"]')
    must(page.input_value("#prompt") == "", "draft leaked into another task")
    page.fill("#prompt", "关闭任务草稿：托盘恢复")
    page.click('[data-view="进行中"]')
    new_task_row.click()
    must(
        page.input_value("#prompt") == "新任务草稿：保留 OCR 验收",
        "new-task draft was not restored",
    )
    page.click('[data-view="需要我"]')
    page.click('[data-task-key="close"]')
    must(
        page.input_value("#prompt") == "关闭任务草稿：托盘恢复",
        "task draft isolation failed",
    )
    shots.shot(page, "11-task-switch-and-draft-isolation-dark.png")

    page.click('[data-view="进行中"]')
    must(
        visible_task_keys(page) == {"experience", "provider", new_task_key},
        "running task filter mismatch",
    )
    shots.shot(page, "12-filter-running-dark.png")
    page.click('[data-view="需要我"]')
    must(
        visible_task_keys(page) == {"close"},
        "attention task filter mismatch",
    )
    shots.shot(page, "13-filter-attention-dark.png")
    page.click('[data-view="历史"]')
    must(
        visible_task_keys(page) == {"runtime"},
        "history task filter mismatch",
    )
    shots.shot(page, "14-filter-history-dark.png")
    page.click('[data-view="进行中"]')
    page.click('[data-task-key="experience"]')
    return {"new_task": "created", "draft_isolation": "passed", "filters": 3}


def verify_selectors_and_project(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    choices = [
        ("#agent-selector", "Codex CLI", "15-selector-agent-dark.png"),
        ("#model-selector", "glm-5.3", "16-selector-model-dark.png"),
        (
            "#policy-selector",
            "低风险自动批准",
            "17-selector-policy-dark.png",
        ),
    ]
    for selector, value, screenshot in choices:
        page.click(selector)
        must(
            page.locator("#selection-popover").is_visible(),
            f"selector did not open: {selector}",
        )
        shots.shot(page, screenshot)
        page.click(f'[data-selector-value="{value}"]')
        must(
            value in page.locator(selector).inner_text(),
            f"selector value did not update: {value}",
        )
        must(
            page.locator("#selection-popover").is_hidden(),
            f"selector did not close: {selector}",
        )

    page.click("#add-project")
    must(
        page.locator("#project-modal.open").is_visible(),
        "project dialog did not open",
    )
    shots.shot(page, "18-project-add-dialog-dark.png")
    page.fill("#project-path", r"D:\project\rust\capture-demo")
    page.click("#project-form button[type=submit]")
    must(
        page.get_by_text("capture-demo", exact=True).count() >= 1,
        "new project was not added to sidebar",
    )
    shots.shot(page, "19-project-added-dark.png")
    return {
        "selectors": ["agent", "model", "policy"],
        "project": "added",
    }


def verify_subagents_and_diff(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    page.click("#tab-agents")
    shots.shot(page, "20-subagent-tree-dark.png")
    page.click("#add-subagent")
    must(
        page.locator('[data-agent^="design-"]').count() == 1,
        "dynamic subagent was not created",
    )
    must(
        page.locator("#metric-agent-count").inner_text() == "4",
        "subagent metric did not increment",
    )
    must(
        page.locator("#agent-detail-view").is_visible(),
        "new subagent detail did not open",
    )
    shots.shot(page, "21-subagent-created-detail-dark.png")
    page.click("#stop-agent")
    row = page.locator('[data-agent^="design-"]')
    must(
        row.locator('.agent-state[data-state="cancelled"]').count() == 1,
        "subagent did not enter cancelled terminal state",
    )
    must(
        "停止" in page.locator("#agent-summary").inner_text(),
        "subagent aggregate did not update",
    )
    shots.shot(page, "22-subagent-stopped-detail-dark.png")
    page.click("#agent-back")
    shots.shot(page, "23-subagent-stopped-tree-dark.png")

    page.click("#tab-changes")
    shots.shot(page, "24-changes-list-dark.png")
    page.click('[data-review-file="prototype.html"]')
    must(page.locator("#review-view").is_visible(), "diff review did not open")
    shots.shot(page, "25-diff-open-dark.png")
    page.click("#mark-reviewed")
    must(
        page.locator("#mark-reviewed").is_disabled(),
        "review action did not reach terminal state",
    )
    must(
        page.locator("#mark-reviewed").inner_text() == "已审阅",
        "review terminal copy missing",
    )
    shots.shot(page, "26-diff-reviewed-dark.png")
    page.click("#review-back")
    must(
        page.locator("#change-list-view").is_visible(),
        "diff did not return to change list",
    )
    page.click("#tab-overview")
    return {"created": 1, "stopped": 1, "diff_review": "passed"}


def verify_provider_credential_recovery(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    select_health_scenario(page, "auth", keep_open=True)
    must(
        page.locator('[data-health-action="fix-auth"]').is_visible(),
        "401 repair action missing",
    )
    shots.shot(page, "27-provider-401-dark.png")
    page.click('[data-health-action="fix-auth"]')
    page.locator("#provider-editor-modal.open").wait_for(state="visible")
    must(
        page.locator("#settings-scene").is_visible(),
        "401 repair did not enter settings",
    )
    must(
        page.locator('[data-settings-panel="providers"]').is_visible(),
        "401 repair did not deep-link to providers",
    )
    settle(page, 30)
    must(
        page.evaluate("document.activeElement.id") == "provider-secret",
        "401 repair did not focus credential field",
    )
    must(
        page.locator("#provider-secret").get_attribute("type") == "password",
        "credential field is not masked",
    )
    shots.shot(page, "28-provider-credential-edit-dark.png")
    page.fill("#provider-secret", "capture-only-demo-secret")
    page.click("#provider-test-editor")
    must(
        "测试中" in page.locator("#provider-test-editor").inner_text(),
        "provider editor did not enter testing state",
    )
    shots.shot(
        page,
        "29-provider-credential-testing-dark.png",
        delay_ms=80,
    )
    page.wait_for_function(
        "document.getElementById('provider-editor-status')"
        ".textContent.includes('连接成功')",
        timeout=3000,
    )
    shots.shot(page, "30-provider-credential-tested-dark.png")
    page.click("#provider-editor-form button[type=submit]")
    page.locator("#provider-editor-modal").wait_for(
        state="hidden", timeout=3000
    )
    must(
        page.locator("#provider-editor-modal").get_attribute("aria-hidden")
        == "true",
        "provider editor did not close after save",
    )
    provider_card_status = page.locator(
        '[data-provider-card-status="deepseek"]'
    )
    provider_card_copy = provider_card_status.inner_text()
    must(
        "exact-model 回执有效" in provider_card_copy
        and "401" not in provider_card_copy
        and provider_card_status.locator(".status-spinner").count() == 0,
        "provider card did not settle on the canonical recovered receipt",
    )
    page.click("#settings-back")
    page.click("#health-trigger")
    health_trigger_label = (
        page.locator("#health-trigger").get_attribute("aria-label") or ""
    )
    must(
        "自动检查已关闭" in health_trigger_label
        and page.locator(
            ".health-row.auth, .health-row.checking, .health-row.queued, "
            ".health-row.timeout, .health-row.offline"
        ).count()
        == 0,
        "provider did not recover after credential save",
    )
    shots.shot(page, "31-provider-recovered-after-save-dark.png")
    page.click("#health-close")
    return {"flow": "401 -> providers -> credential -> save -> recovered"}


def verify_window_lifecycle(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    reset_close_preference(page)
    select_run_state(page, "running", expanded=False)
    page.click("#close-window")
    must(
        page.locator("#close-modal.open").is_visible(),
        "close dialog did not open",
    )
    shots.shot(page, "32-close-choice-dark.png")
    page.check("#remember-choice")
    page.click("#minimize-option")
    must(
        page.locator("#window-state-modal.open").is_visible(),
        "tray state did not open",
    )
    must(
        page.locator("#window-state-spinner").is_visible(),
        "active tray state has no running spinner",
    )
    shots.shot(page, "33-tray-hidden-dark.png")
    page.click("#restore-window")
    must(
        page.locator("#window-state-modal").get_attribute("aria-hidden")
        == "true",
        "tray restore did not return to app",
    )
    shots.shot(page, "34-tray-restored-dark.png")

    page.click("#close-window")
    must(
        page.locator("#close-modal").get_attribute("aria-hidden") == "true"
        and page.locator("#window-state-modal.open").is_visible()
        and "隐藏到托盘" in page.locator("#window-state-title").inner_text(),
        "remembered tray choice did not bypass the close dialog",
    )
    shots.shot(page, "34a-close-remembered-tray-bypass-dark.png")
    page.click("#restore-window")

    page.click("#open-settings")
    select_settings_panel(page, "lifecycle")
    page.click("#reset-close-preference")
    page.click("#settings-back")
    page.click("#close-window")
    must(
        page.locator("#close-modal.open").is_visible(),
        "public close-preference reset did not restore the dialog",
    )
    page.click("#exit-option")
    must(
        "已退出" in page.locator("#window-state-title").inner_text(),
        "exit terminal state missing",
    )
    must(
        page.locator("#window-state-spinner").is_hidden(),
        "exit terminal state still shows spinner",
    )
    must(
        page.locator("#run-card").get_attribute("data-run-state") == "cancelled"
        and page.locator("#live-pill").get_attribute("data-state") == "cancelled"
        and page.locator(".agent-state[data-state='running']").count() == 0
        and page.locator(".status-spinner:visible").count() == 0
        and "Exit ACK"
        in page.locator("#window-state-detail").inner_text(),
        "exit did not cascade cancellation through Run, subagents, tools, and timers",
    )
    shots.shot(page, "35-exit-terminal-without-spinner-dark.png")
    page.click("#restore-window")
    settle(page, 30)
    must(
        page.locator("#window-state-modal").get_attribute("aria-hidden") == "true"
        and page.locator("#run-card").get_attribute("data-run-state")
        == "cancelled"
        and page.locator("#live-pill-copy").inner_text() == "本轮已取消"
        and page.locator('[data-task-key="experience"] .status-spinner').count()
        == 0
        and page.locator(".status-spinner:visible").count() == 0,
        "restart Demo resurrected the exited Run as a running snapshot",
    )
    return {
        "remembered_tray_bypass": "passed",
        "tray_restore": "passed",
        "public_preference_reset": "passed",
        "exit_terminal": "passed",
        "exit_cancel_cascade": "passed",
        "restart_snapshot": "non_running",
    }


def verify_settings_navigation_and_search(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    page.click("#open-settings")
    must(
        page.locator("#settings-scene").is_visible(),
        "settings scene did not open",
    )
    settle(page, 30)
    must(
        page.evaluate("document.activeElement.id") == "settings-back",
        "settings scene did not establish its initial keyboard focus",
    )

    keyboard_panels: list[str] = []
    expected = {name for name, _ in SETTINGS_PANELS}
    tabbable_panels = page.locator("[data-settings-target]").evaluate_all(
        """buttons => buttons.filter(button => {
          const style = getComputedStyle(button);
          return !button.disabled
            && button.tabIndex >= 0
            && button.getClientRects().length > 0
            && style.visibility !== 'hidden'
            && style.display !== 'none';
        }).map(button => button.dataset.settingsTarget)"""
    )
    must(
        set(tabbable_panels) == expected,
        f"settings pages are not all Tab reachable: {tabbable_panels}",
    )
    for name, _ in SETTINGS_PANELS:
        button = page.locator(f'[data-settings-target="{name}"]')
        button.focus()
        must(
            page.evaluate(
                "document.activeElement.dataset.settingsTarget || ''"
            )
            == name,
            f"settings navigation did not accept keyboard focus: {name}",
        )
        page.keyboard.press("Enter")
        must(
            page.locator(f'[data-settings-panel="{name}"]').is_visible(),
            f"settings navigation did not activate by keyboard: {name}",
        )
        keyboard_panels.append(name)

    for index, (name, label) in enumerate(SETTINGS_PANELS, start=1):
        panel = select_settings_panel(page, name)
        must(
            label in panel.locator("h2").inner_text(),
            f"settings heading mismatch: {name}",
        )
        assert_no_overflow(page, f"settings-{name}")
        assert_named_visible_controls(page, f"settings-{name}")
        shots.shot(
            page,
            f"{35 + index:02d}-settings-{index:02d}-{name}-dark.png",
        )

    page.fill("#settings-search", "OCR")
    must(
        page.locator("#settings-search-results").is_visible(),
        "settings search results did not open",
    )
    must(
        "图片理解" in page.locator("#settings-search-results").inner_text(),
        "OCR search result missing",
    )
    shots.shot(page, "48-settings-search-ocr-dark.png")
    page.press("#settings-search", "Tab")
    must(
        page.evaluate("document.activeElement.dataset.searchPanel")
        == "providers",
        "OCR result is not keyboard reachable",
    )
    page.keyboard.press("Enter")
    must(
        page.locator('[data-settings-panel="providers"]').is_visible(),
        "OCR deep-link did not select providers",
    )
    settle(page, 30)
    must(
        page.evaluate("document.activeElement.id")
        == "image-understanding-block",
        "OCR deep-link did not focus target block",
    )
    shots.shot(page, "49-settings-search-ocr-deeplink-dark.png")
    return {
        "mouse_pages": len(SETTINGS_PANELS),
        "keyboard_pages": len(set(keyboard_panels)),
        "ocr_deeplink": "passed",
    }


def verify_settings_state_machines(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    trust_and_probe_codex_subagent(page)
    page.click("#add-subagent-slot")
    must(
        "需要调整" in page.locator("#slot-weight-summary").inner_text(),
        "invalid slot weight state missing",
    )
    must(
        page.locator("#save-subagent-pool").is_disabled(),
        "invalid slot weights did not disable save",
    )
    shots.shot(page, "50-subagent-slots-invalid-dark.png")
    page.locator(".slot-weight").nth(0).fill("50")
    must(
        "权重 100%" in page.locator("#slot-weight-summary").inner_text(),
        "slot weights did not return to 100%",
    )
    must(
        not page.locator("#save-subagent-pool").is_disabled(),
        "valid slot weights did not enable save",
    )
    page.click("#save-subagent-pool")
    shots.shot(page, "51-subagent-slots-saved-dark.png")

    select_settings_panel(page, "tools")
    page.click("#install-browser-runtime")
    must(
        "status-spinner"
        in (
            page.locator("#browser-runtime-status span")
            .first.get_attribute("class")
            or ""
        ),
        "Browser planning state did not start",
    )
    shots.shot(
        page,
        "52-browser-planning-installing-dark.png",
        delay_ms=80,
    )
    page.wait_for_function(
        "document.getElementById('browser-runtime-status')"
        ".textContent.includes('已就绪')",
        timeout=3000,
    )
    must(
        not page.locator("#revoke-browser-grants").is_disabled(),
        "Browser revoke action did not enable",
    )
    shots.shot(page, "53-browser-planning-ready-dark.png")
    page.click("#revoke-browser-grants")
    must(
        page.locator("#revoke-browser-grants").is_disabled(),
        "Browser grants were not revoked",
    )

    select_settings_panel(page, "permissions")
    page.click("#approve-read-group")
    must(
        "已批准"
        in page.locator(
            "#settings-panel-permissions .inline-note"
        ).last.inner_text(),
        "permission group did not reach approved state",
    )
    shots.shot(page, "54-permissions-approved-dark.png")

    select_settings_panel(page, "notifications")
    page.click("#request-notification")
    must(
        "正在" in page.locator("#notification-status").inner_text(),
        "notification permission did not enter checking state",
    )
    shots.shot(
        page,
        "55-notification-permission-checking-dark.png",
        delay_ms=80,
    )
    page.wait_for_function(
        "document.getElementById('notification-status')"
        ".textContent.includes('已授权')",
        timeout=3000,
    )
    shots.shot(page, "56-notification-permission-granted-dark.png")

    select_settings_panel(page, "updates")
    page.click("#check-update")
    must(
        page.locator("#check-update").get_attribute("data-update-state")
        == "checking",
        "update did not enter checking",
    )
    shots.shot(page, "57-update-checking-dark.png", delay_ms=80)
    page.wait_for_function(
        "document.getElementById('check-update').dataset.updateState"
        " === 'available'",
        timeout=3000,
    )
    shots.shot(page, "58-update-available-dark.png")
    page.click("#check-update")
    must(
        page.locator("#check-update").get_attribute("data-update-state")
        == "downloading",
        "update did not enter downloading",
    )
    shots.shot(page, "59-update-downloading-dark.png", delay_ms=80)
    page.wait_for_function(
        "document.getElementById('check-update').dataset.updateState"
        " === 'downloaded'",
        timeout=3000,
    )
    shots.shot(page, "60-update-downloaded-dark.png")
    page.click("#check-update")
    must(
        page.locator("#check-update").get_attribute("data-update-state")
        == "installing",
        "install-and-restart did not enter installing",
    )
    page.wait_for_function(
        "document.getElementById('check-update').dataset.updateState"
        " === 'up_to_date'",
        timeout=3000,
    )
    must(
        page.locator("#check-update").get_attribute("data-update-state")
        == "up_to_date"
        and page.locator("#updater-current-version").inner_text() == "0.8.1",
        "install-and-restart did not automatically restore on version 0.8.1",
    )
    shots.shot(page, "61-update-install-and-restart-up-to-date-dark.png")

    # Exercise the independent install-later branch from a clean public UI
    # state by repeating the visible check/download path. No prototype
    # internals are called or mutated directly.
    reset_page(page)
    page.click("#open-settings")
    select_settings_panel(page, "updates")
    page.click("#check-update")
    page.wait_for_function(
        "document.getElementById('check-update').dataset.updateState"
        " === 'available'",
        timeout=3000,
    )
    page.click("#check-update")
    page.wait_for_function(
        "document.getElementById('check-update').dataset.updateState"
        " === 'downloaded'",
        timeout=3000,
    )
    must(
        page.locator("#check-update").inner_text() == "安装并重启"
        and page.locator("#update-secondary").inner_text()
        == "安装，稍后重启",
        "downloaded update did not expose both public installation choices",
    )
    page.click("#update-secondary")
    must(
        page.locator("#check-update").get_attribute("data-update-state")
        == "installing",
        "install-later did not enter installing",
    )
    page.wait_for_function(
        "document.getElementById('check-update').dataset.updateState"
        " === 'restart_pending'",
        timeout=3000,
    )
    must(
        page.locator("#check-update").inner_text() == "立即重启"
        and page.locator("#updater-current-version").inner_text()
        == "0.8.0-dev",
        "install-later did not preserve the current version while awaiting restart",
    )
    shots.shot(page, "61b-update-install-later-restart-pending-dark.png")
    page.click("#check-update")
    must(
        page.locator("#check-update").get_attribute("data-update-state")
        == "up_to_date"
        and page.locator("#updater-current-version").inner_text() == "0.8.1",
        "restart-now did not complete the install-later restore path on 0.8.1",
    )
    shots.shot(page, "61c-update-install-later-up-to-date-dark.png")

    select_settings_panel(page, "diagnostics")
    page.click("#run-self-check")
    must(
        "自检中" in page.locator("#run-self-check").inner_text(),
        "diagnostics did not enter self-check state",
    )
    page.wait_for_function(
        "document.getElementById('diagnostic-log')"
        ".textContent.includes('self-check passed')",
        timeout=3000,
    )
    page.click("#export-support")
    must(
        page.locator("#support-result").is_visible(),
        "support preview was not generated",
    )
    must(
        "0 个 secret" in page.locator("#support-result").inner_text(),
        "support preview redaction evidence missing",
    )
    shots.shot(page, "62-diagnostics-self-check-and-support-dark.png")
    return {
        "subagent_slots": ["invalid", "saved"],
        "browser": ["installing", "ready", "revoked"],
        "permissions": "approved",
        "notifications": ["checking", "granted"],
        "updates": [
            "checking",
            "available",
            "downloading",
            "downloaded",
            "install_and_restart:installing",
            "install_and_restart:up_to_date:0.8.1",
            "install_later:installing",
            "install_later:restart_pending:0.8.0-dev",
            "restart_now:up_to_date:0.8.1",
        ],
        "diagnostics": ["self-check", "support-preview"],
    }


def verify_settings_capability_depth(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    ensure_theme(page, "dark")

    select_settings_panel(page, "providers")
    page.click("#open-capability-map")
    settle(page, 240)
    must(
        page.locator("#capability-map-modal").get_attribute("aria-hidden")
        == "false",
        "capability preservation map did not open",
    )
    shots.shot(page, "83-settings-capability-map-dark.png")
    page.click("#capability-map-close")

    page.click("#open-provider-catalog")
    shots.shot(page, "84-provider-preset-catalog-dark.png")
    page.click('[data-provider-template="custom"]')
    page.fill("#provider-base-url", "https://${HOST}/v1")
    must(
        "模板变量未替换" in page.locator("#provider-protocol-validation").inner_text(),
        "provider template-variable save gate is missing",
    )
    shots.shot(page, "85-provider-validation-blocked-dark.png")
    page.fill("#provider-base-url", "https://demo.example/v1")
    page.click("#provider-editor-cancel")
    must(
        page.locator("#provider-discard-note").is_visible(),
        "provider unsaved-draft confirmation is missing",
    )
    shots.shot(page, "86-provider-unsaved-confirm-dark.png")
    page.click("#provider-confirm-discard")

    page.select_option("#image-engine", "model")
    page.select_option("#vision-provider", "missing")
    must(
        page.locator("#save-image-engine").is_disabled(),
        "dangling vision provider did not block save",
    )
    shots.shot(page, "87-image-provider-missing-dark.png")
    page.select_option("#vision-provider", "ark")
    page.click("#save-image-engine")

    select_settings_panel(page, "agents")
    if page.locator("#run-budget-details").get_attribute("open") is None:
        page.click("#run-budget-details summary")
    page.locator("#run-guardrails-block").scroll_into_view_if_needed()
    shots.shot(page, "88-agent-run-guardrails-dark.png")
    page.click("#open-codex-settings")
    page.click('[data-codex-tab="preferences"]')
    must(
        page.locator('[data-codex-panel="preferences"]').is_visible(),
        "Codex preferences tab did not open",
    )
    shots.shot(page, "89-codex-runtime-preferences-dark.png")
    page.click("#codex-settings-cancel")

    select_settings_panel(page, "subagents")
    page.locator("#subagent-prompt-block").scroll_into_view_if_needed()
    shots.shot(page, "90-subagent-prompt-and-revision-dark.png")

    select_settings_panel(page, "tools")
    page.click('[data-mcp-toggle="browser-tools"]')
    must(
        page.locator("#mcp-approval-modal").get_attribute("aria-hidden")
        == "false",
        "MCP approval dialog did not open",
    )
    shots.shot(page, "91-mcp-approval-requesting-dark.png")
    page.wait_for_function(
        "!document.getElementById('mcp-approval-confirm').disabled",
        timeout=3000,
    )
    must(
        "Exact launch plan"
        in page.locator("#mcp-approval-launch-plan").inner_text(),
        "MCP exact launch plan is missing",
    )
    shots.shot(page, "92-mcp-launch-ready-dark.png")
    page.click("#mcp-approval-cancel")

    select_settings_panel(page, "knowledge")
    page.select_option("#knowledge-scope", "global")
    page.locator("#memory-review-block").scroll_into_view_if_needed()
    shots.shot(page, "93-knowledge-memory-review-dark.png")
    page.select_option("#knowledge-scope", "project-r-code")
    page.click('[data-knowledge-tab="prompt"]')
    shots.shot(page, "94-knowledge-prompt-append-override-dark.png")
    page.click('[data-knowledge-tab="skills"]')
    shots.shot(page, "95-knowledge-skills-inheritance-dark.png")
    page.click("#manage-skills")
    shots.shot(page, "96-skill-editor-dark.png")
    page.click("#skill-editor-cancel")

    select_settings_panel(page, "permissions")
    page.locator("#codex-permissions-block").scroll_into_view_if_needed()
    shots.shot(page, "97-codex-permission-scope-dark.png")

    select_settings_panel(page, "appearance")
    page.select_option("#companion-mode", "full")
    page.locator("#companion-settings-block").scroll_into_view_if_needed()
    shots.shot(page, "98-companion-complete-settings-dark.png")
    must(
        page.locator("#save-companion").count() == 0,
        "Companion regressed from immediate revision mutations to page save",
    )

    select_settings_panel(page, "diagnostics")
    page.locator("#support-bundle-block").scroll_into_view_if_needed()
    if page.locator("#support-export-controls").is_hidden():
        page.click("#export-support")
    page.click("#choose-support-location")
    must(
        not page.locator("#confirm-support-export").is_disabled(),
        "support bundle export path did not enable export",
    )
    shots.shot(page, "99-support-bundle-export-path-dark.png")

    return {
        "provider": ["catalog", "validation", "unsaved", "model-orphan"],
        "agent": ["guardrails", "codex-preferences"],
        "subagents": ["prompt", "revision"],
        "mcp": ["launch-plan", "ready", "cancelled"],
        "knowledge": ["memory", "prompt", "skills"],
        "companion": "complete",
        "support_bundle": "export-path",
    }


def capture_light_key_pages(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    select_settings_panel(page, "appearance")
    page.click('[data-theme-choice="light"]')
    must(
        page.locator("html").get_attribute("data-theme") == "light",
        "light theme did not activate",
    )
    light_materials = page.evaluate(
        """() => {
          const style = getComputedStyle(document.documentElement);
          const names = [
            '--panel', '--panel-strong', '--surface-content',
            '--surface-sunken', '--canvas-wash', '--scrim'
          ];
          const parse = value => {
            const parts = (value.match(/[\\d.]+/g) || []).map(Number);
            return { value: value.trim(), rgb: parts.slice(0, 3), alpha: parts[3] ?? 1 };
          };
          return Object.fromEntries(names.map(name => [
            name,
            parse(style.getPropertyValue(name))
          ]));
        }"""
    )
    must(
        all(
            min(light_materials[name]["rgb"]) >= 200
            for name in (
                "--panel",
                "--panel-strong",
                "--surface-content",
                "--surface-sunken",
            )
        ),
        f"light surfaces retained a black wash: {light_materials}",
    )
    must(
        sum(light_materials["--canvas-wash"]["rgb"]) >= 90
        and light_materials["--canvas-wash"]["alpha"] <= 0.05
        and sum(light_materials["--scrim"]["rgb"]) >= 90
        and light_materials["--scrim"]["alpha"] <= 0.35,
        f"light overlays use an unintended black wash: {light_materials}",
    )
    shots.shot(page, "63-settings-appearance-light.png")
    select_settings_panel(page, "providers")
    shots.shot(page, "64-settings-providers-light.png")
    page.click("#settings-back")
    shots.shot(page, "65-workspace-light.png")
    page.click("#tab-changes")
    page.click('[data-review-file="README.md"]')
    shots.shot(page, "66-diff-light.png")
    page.click("#review-back")
    page.click("#tab-overview")
    page.click("#theme-toggle")
    must(
        page.locator("html").get_attribute("data-theme") == "dark",
        "dark theme did not restore",
    )
    return {
        "light_pages": 4,
        "black_wash_guard": True,
        "material_tokens": {
            name: record["value"] for name, record in light_materials.items()
        },
    }


def verify_responsive(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    reset_page(page)
    select_health_scenario(page, "recovered")
    for index, (width, height) in enumerate(VIEWPORTS, start=67):
        page.set_viewport_size({"width": width, "height": height})
        settle(page)
        assert_no_overflow(page, f"{width}x{height}")
        assert_named_visible_controls(page, f"{width}x{height}")
        shots.shot(
            page,
            f"{index:02d}-responsive-{width}x{height}-dark.png",
        )
        if width == 1024:
            page.click("#dock-open")
            must(
                page.locator("#dock").get_attribute("aria-hidden") == "false",
                "1024 dock did not open",
            )
            shots.shot(page, "73-responsive-1024-dock-open-dark.png")
            page.keyboard.press("Escape")
            settle(page, 30)
            must(
                page.evaluate("document.activeElement.id") == "dock-open",
                "responsive dock did not restore focus",
            )
        if width in {740, 390}:
            must(
                page.locator("#task-switcher-open").is_visible(),
                f"task navigation entry missing at {width}",
            )
            page.click("#task-switcher-open")
            must(
                page.locator(".sidebar.mobile-open").is_visible(),
                f"task navigation drawer did not open at {width}",
            )
            sequence = 74 if width == 740 else 75
            shots.shot(
                page,
                f"{sequence:02d}-responsive-{width}"
                "-task-navigation-open-dark.png",
            )
            page.keyboard.press("Escape")
            settle(page, 30)
            must(
                page.evaluate("document.activeElement.id")
                == "task-switcher-open",
                f"task drawer focus did not restore at {width}",
            )
    return {
        "viewports": [f"{width}x{height}" for width, height in VIEWPORTS]
    }


def verify_scale_and_reduced_motion(
    browser: Browser,
    errors: list[str],
    shots: CaptureLog,
) -> dict[str, object]:
    scale_context = browser.new_context(
        viewport={"width": 800, "height": 480},
        device_scale_factor=2,
    )
    scale_page = scale_context.new_page()
    attach_diagnostics(scale_page, errors)
    load(scale_page)
    select_health_scenario(scale_page, "recovered")
    assert_no_overflow(scale_page, "800x480@2x")
    assert_named_visible_controls(scale_page, "800x480@2x")
    shots.shot(scale_page, "76-scale-200-workspace-dark.png")
    scale_page.click("#open-settings")
    select_settings_panel(scale_page, "lifecycle")
    assert_in_viewport(
        scale_page,
        "#settings-scene",
        "200% settings scene",
    )
    assert_in_viewport(
        scale_page,
        "#settings-back",
        "200% settings back",
    )
    shots.shot(scale_page, "77-scale-200-settings-dark.png")
    scale_page.click("#settings-back")
    scale_page.click("#health-trigger")
    assert_in_viewport(
        scale_page,
        "#health-popover",
        "200% health popover",
    )
    must(
        scale_page.locator("#health-popover").evaluate(
            "element => element.scrollHeight <= element.clientHeight + 1"
            " || getComputedStyle(element).overflowY === 'auto'"
        ),
        "200% health popover clips content without scrolling",
    )
    shots.shot(scale_page, "78-scale-200-provider-popover-dark.png")
    scale_context.close()

    reduced_context = browser.new_context(
        viewport={"width": 1280, "height": 800},
        reduced_motion="reduce",
    )
    reduced_page = reduced_context.new_page()
    attach_diagnostics(reduced_page, errors)
    load(reduced_page)
    select_health_scenario(reduced_page, "recovered")
    select_run_state(reduced_page, "running", expanded=True)
    spinner_animation = reduced_page.locator(
        "#live-status-glyph"
    ).evaluate("element => getComputedStyle(element).animationName")
    dock_duration = reduced_page.locator("#dock").evaluate(
        "element => getComputedStyle(element).transitionDuration"
    )
    must(
        spinner_animation == "none",
        f"reduced motion left spinner animation active: {spinner_animation}",
    )
    must(
        dock_duration
        in {
            "0s",
            "0.00001s",
            "1e-05s",
            "0.00001s, 0.00001s",
            "1e-05s, 1e-05s",
        },
        f"unexpected reduced-motion dock transition: {dock_duration}",
    )
    shots.shot(
        reduced_page,
        "79-reduced-motion-running-state-dark.png",
    )
    reduced_context.close()
    return {
        "scale": "800x480@2x",
        "reduced_motion": {
            "spinner": spinner_animation,
            "dock": dock_duration,
        },
    }


def capture_completeness_pages(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    reset_page(page)
    select_health_scenario(page, "recovered")

    page.set_viewport_size({"width": 740, "height": 800})
    page.click("#open-settings")
    select_settings_panel(page, "providers")
    settle(page)
    assert_no_overflow(page, "740x800 settings")
    assert_named_visible_controls(page, "740x800 settings")
    shots.shot(page, "80-settings-responsive-740-dark.png")

    page.set_viewport_size({"width": 390, "height": 844})
    settle(page)
    assert_no_overflow(page, "390x844 settings")
    assert_named_visible_controls(page, "390x844 settings")
    shots.shot(page, "81-settings-responsive-390-dark.png")

    page.set_viewport_size({"width": 1280, "height": 800})
    page.click("#theme-toggle")
    must(
        page.locator("html").get_attribute("data-theme") == "light",
        "light theme did not activate for provider editor",
    )
    page.click('[data-provider-edit="deepseek"]')
    page.locator("#provider-editor-modal.open").wait_for(state="visible")
    assert_in_viewport(
        page,
        "#provider-editor-form",
        "light provider editor",
    )
    shots.shot(page, "82-provider-editor-light.png")
    page.click("#provider-editor-cancel")
    page.click("#theme-toggle")
    must(
        page.locator("html").get_attribute("data-theme") == "dark",
        "dark theme did not restore after provider editor capture",
    )
    page.click("#settings-back")
    return {
        "settings_responsive": ["740x800", "390x844"],
        "light_provider_editor": "passed",
    }


def verify_keyboard_contracts(page: Page) -> dict[str, object]:
    reset_page(page)
    select_health_scenario(page, "recovered")

    page.click("#close-window")
    page.locator("#close-cancel").focus()
    page.keyboard.press("Tab")
    must(
        page.evaluate("document.activeElement.id") == "minimize-option",
        "close modal did not wrap focus forward",
    )
    page.locator("#minimize-option").focus()
    page.keyboard.press("Shift+Tab")
    must(
        page.evaluate("document.activeElement.id") == "close-cancel",
        "close modal did not wrap focus backward",
    )
    page.keyboard.press("Escape")
    settle(page, 30)
    must(
        page.evaluate("document.activeElement.id") == "close-window",
        "close modal did not restore focus",
    )

    page.click("#tab-agents")
    page.locator('[data-agent="root"]').focus()
    page.keyboard.press("ArrowDown")
    must(
        page.evaluate("document.activeElement.dataset.agent") == "runtime",
        "agent tree ArrowDown failed",
    )
    page.locator('[data-agent="root"]').focus()
    page.keyboard.press("ArrowLeft")
    must(
        page.locator('[data-agent="root"]').get_attribute("aria-expanded")
        == "false",
        "agent tree ArrowLeft failed",
    )
    page.keyboard.press("ArrowRight")
    must(
        page.locator('[data-agent="root"]').get_attribute("aria-expanded")
        == "true",
        "agent tree ArrowRight failed",
    )
    page.click("#tab-overview")
    page.keyboard.press("ArrowRight")
    must(
        page.locator("#tab-agents").get_attribute("aria-selected") == "true",
        "dock tab ArrowRight failed",
    )

    page.fill("#prompt", "发送快捷键")
    page.press("#prompt", "Enter")
    must(page.input_value("#prompt") == "", "Enter did not send")
    page.fill("#prompt", "排队快捷键")
    page.press("#prompt", "Alt+Enter")
    must(
        page.locator(".user-message.queued").count() >= 1,
        "Alt+Enter did not queue",
    )
    page.fill("#prompt", "换行快捷键")
    page.press("#prompt", "Shift+Enter")
    must(
        "\n" in page.input_value("#prompt"),
        "Shift+Enter did not insert newline",
    )
    return {
        "modal": "passed",
        "tree": "passed",
        "tabs": "passed",
        "composer": ["Enter", "Alt+Enter", "Shift+Enter"],
    }


def verify_settings_load_contract(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    reset_page(page)
    page.click("#open-settings")
    page.click("#settings-load-demo")
    must(
        page.locator("#settings-content").get_attribute("data-load-state")
        == "loading",
        "settings did not enter loading state",
    )

    def panel_lock_state() -> list[dict[str, object]]:
        return page.locator("[data-settings-panel]").evaluate_all(
            """panels => panels.map(panel => ({
              panel: panel.dataset.settingsPanel,
              inert: panel.inert,
              ariaDisabled: panel.getAttribute('aria-disabled')
            }))"""
        )

    loading_locks = panel_lock_state()
    must(
        all(item["inert"] and item["ariaDisabled"] == "true" for item in loading_locks),
        f"settings panels were not inert while loading: {loading_locks}",
    )
    shots.shot(page, "100-settings-load-loading-inert-dark.png", delay_ms=40)
    page.wait_for_function(
        "document.getElementById('settings-content').dataset.loadState"
        " === 'failed-last-good'",
        timeout=3000,
    )
    failed_locks = panel_lock_state()
    must(
        all(item["inert"] and item["ariaDisabled"] == "true" for item in failed_locks),
        f"settings panels were not inert after load failure: {failed_locks}",
    )
    must(
        page.locator("#retry-settings-load").is_visible(),
        "settings retry action is not visible after load failure",
    )
    must(
        page.evaluate("document.activeElement.id") == "retry-settings-load",
        "settings load failure did not focus the retry action",
    )
    for key in ("Tab", "Shift+Tab"):
        page.locator("#retry-settings-load").focus()
        for _ in range(32):
            page.keyboard.press(key)
            must(
                not page.evaluate(
                    "Boolean(document.activeElement.closest('[data-settings-panel]'))"
                ),
                f"{key} entered an inert settings panel after load failure",
            )
    shots.shot(page, "101-settings-load-failed-retry-dark.png")
    provider_name_before = page.locator(
        '[data-provider-card="deepseek"] strong'
    ).inner_text()
    page.click("#settings-load-dismiss")
    must(
        page.locator("#settings-load-banner").is_hidden()
        and page.locator("#settings-content").get_attribute("data-load-state")
        == "failed-last-good",
        "dismissing the load banner did not preserve failed/last-good state",
    )
    must(
        page.locator('[data-settings-panel="providers"]').is_visible()
        and page.locator('[data-provider-card="deepseek"] strong').inner_text()
        == provider_name_before,
        "last-good Provider snapshot was cleared after dismissing the banner",
    )
    dismissed_locks = panel_lock_state()
    must(
        all(
            item["inert"] and item["ariaDisabled"] == "true"
            for item in dismissed_locks
        ),
        "dismissed last-good snapshot became editable before retry",
    )
    shots.shot(page, "101a-settings-load-last-good-readonly-dark.png")
    page.click("#settings-load-demo")
    must(
        page.locator("#settings-content").get_attribute("data-load-state")
        == "loading",
        "settings retry did not re-enter loading state",
    )
    page.wait_for_function(
        "document.getElementById('settings-content').dataset.loadState"
        " === 'failed-last-good'",
        timeout=3000,
    )
    page.click("#retry-settings-load")
    must(
        page.locator("#settings-content").get_attribute("data-load-state")
        == "loading",
        "last-good retry action did not re-enter loading",
    )
    page.wait_for_function(
        "document.getElementById('settings-content').dataset.loadState === 'ready'",
        timeout=3000,
    )
    ready_locks = panel_lock_state()
    must(
        all(not item["inert"] and item["ariaDisabled"] is None for item in ready_locks),
        f"settings panels did not unlock after retry: {ready_locks}",
    )
    must(
        page.evaluate("document.activeElement.id") == "settings-load-demo",
        "settings retry did not restore focus to the load demo trigger",
    )
    shots.shot(page, "102-settings-load-retry-ready-dark.png")

    page.select_option("#settings-lifecycle-demo", "load-empty")
    page.click("#settings-load-demo")
    must(
        page.locator("#settings-content").get_attribute("data-load-state")
        == "loading",
        "no-snapshot lifecycle did not enter loading",
    )
    page.wait_for_function(
        "document.getElementById('settings-content').dataset.loadState"
        " === 'failed-empty'",
        timeout=3000,
    )
    empty_locks = panel_lock_state()
    empty_copy = page.locator("#settings-load-copy").inner_text()
    must(
        all(item["inert"] and item["ariaDisabled"] == "true" for item in empty_locks)
        and page.locator('[data-settings-panel="providers"]').is_hidden()
        and page.locator("#settings-load-dismiss").is_hidden()
        and page.locator("#retry-settings-load").is_visible()
        and "没有 last-good" in empty_copy
        and "不写入默认值" in empty_copy,
        "no-snapshot failure rendered an editable/empty default Settings form",
    )
    shots.shot(page, "102a-settings-load-failed-empty-no-defaults-dark.png")
    page.click("#retry-settings-load")
    page.wait_for_function(
        "document.getElementById('settings-content').dataset.loadState === 'ready'",
        timeout=3000,
    )
    must(
        page.locator('[data-settings-panel="providers"]').is_visible()
        and all(
            not item["inert"] and item["ariaDisabled"] is None
            for item in panel_lock_state()
        ),
        "no-snapshot retry did not restore a real editable Host snapshot",
    )

    select_settings_panel(page, "providers")
    persisted_sync_mode = page.input_value("#provider-sync-mode")
    page.select_option("#settings-lifecycle-demo", "save-rejected")
    page.click("#settings-load-demo")
    rejected_sync_mode = page.input_value("#provider-sync-mode")
    must(
        page.locator("#settings-content").get_attribute("data-load-state")
        == "save-rejected"
        and rejected_sync_mode != persisted_sync_mode
        and "本地草稿仍保留"
        in page.locator("#settings-load-title").inner_text()
        and page.locator("#retry-settings-load").inner_text() == "重试保存"
        and page.locator("#settings-load-dismiss").inner_text()
        == "放弃本地草稿"
        and all(
            not item["inert"] and item["ariaDisabled"] is None
            for item in panel_lock_state()
        ),
        "save rejection did not keep the local draft editable with retry/discard",
    )
    shots.shot(page, "102b-settings-save-rejected-draft-retained-dark.png")
    page.click("#retry-settings-load")
    must(
        page.locator("#settings-content").get_attribute("data-load-state")
        == "ready"
        and page.input_value("#provider-sync-mode") == rejected_sync_mode
        and "saved after retry" in page.input_value("#provider-sync-receipt"),
        "save-rejected retry did not persist the retained local draft",
    )

    persisted_sync_mode = rejected_sync_mode
    page.select_option("#settings-lifecycle-demo", "save-rejected")
    page.click("#settings-load-demo")
    must(
        page.input_value("#provider-sync-mode") != persisted_sync_mode,
        "second save-rejected scenario did not create a distinguishable draft",
    )
    page.click("#settings-load-dismiss")
    must(
        page.locator("#settings-content").get_attribute("data-load-state")
        == "ready"
        and page.input_value("#provider-sync-mode") == persisted_sync_mode,
        "save-rejected discard did not restore the persisted Host value",
    )

    def trigger_settings_revision_conflict() -> tuple[str, str]:
        host_mode = page.input_value("#provider-sync-mode")
        page.select_option("#settings-lifecycle-demo", "revision-conflict")
        page.click("#settings-load-demo")
        local_mode = page.input_value("#provider-sync-mode")
        must(
            page.locator("#settings-content").get_attribute("data-load-state")
            == "revision-conflict"
            and local_mode != host_mode
            and all(
                page.locator(f"#{action}").is_visible()
                for action in (
                    "settings-conflict-discard",
                    "settings-conflict-reapply",
                    "settings-conflict-merge",
                )
            )
            and "本地模型同步草稿仍可见"
            in page.locator("#settings-load-copy").inner_text(),
            "Settings revision conflict did not preserve both draft and Host paths",
        )
        return host_mode, local_mode

    conflict_host_mode, _ = trigger_settings_revision_conflict()
    shots.shot(page, "102c-settings-revision-conflict-three-paths-dark.png")
    page.click("#settings-conflict-discard")
    must(
        page.locator("#settings-content").get_attribute("data-load-state")
        == "ready"
        and page.input_value("#provider-sync-mode") == conflict_host_mode
        and "adopted Host" in page.input_value("#provider-sync-receipt"),
        "Settings conflict discard did not adopt the fresh Host snapshot",
    )

    _, conflict_local_mode = trigger_settings_revision_conflict()
    page.click("#settings-conflict-reapply")
    must(
        page.locator("#settings-content").get_attribute("data-load-state")
        == "ready"
        and page.input_value("#provider-sync-mode") == conflict_local_mode
        and "local draft reapplied" in page.input_value("#provider-sync-receipt"),
        "Settings conflict reapply did not persist the local draft at fresh revision",
    )

    _, conflict_merge_mode = trigger_settings_revision_conflict()
    page.click("#settings-conflict-merge")
    must(
        page.locator("#settings-content").get_attribute("data-load-state")
        == "ready"
        and page.input_value("#provider-sync-mode") == conflict_merge_mode
        and "explicit merge saved" in page.input_value("#provider-sync-receipt"),
        "Settings conflict merge did not save the explicit merged result",
    )
    return {
        "states": [
            "loading",
            "failed-last-good",
            "failed-empty",
            "save-rejected",
            "revision-conflict",
            "ready",
        ],
        "last_good_readonly": True,
        "no_snapshot_no_defaults": True,
        "save_rejected_recovery": ["retry", "discard"],
        "revision_conflict_recovery": ["discard", "reapply", "merge"],
        "banner_dismiss_preserved_state": True,
        "all_panels_inert_during_failure": True,
        "failed_keyboard_isolation": ["Tab", "Shift+Tab"],
        "retry_focus_restored": True,
    }


def verify_provider_and_codex_catalog_contract(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    reset_page(page)
    page.click("#open-settings")
    select_settings_panel(page, "providers")

    page.click('[data-provider-edit="deepseek"]')
    page.locator("#provider-editor-modal.open").wait_for(state="visible")
    vendor_rows = page.locator(
        "#provider-vendor-link-list [data-provider-external-target]"
    )
    must(
        page.locator("#provider-vendor-links").is_visible()
        and vendor_rows.count() == 2,
        "DeepSeek editor did not expose both vendor targets",
    )
    vendor_target = vendor_rows.first.get_attribute(
        "data-provider-external-target"
    )
    vendor_rows.first.click()
    must(
        vendor_target is not None
        and vendor_target
        in page.locator("#provider-vendor-link-status").inner_text(),
        "vendor target action did not show the exact offline destination",
    )
    shots.shot(page, "132-provider-vendor-targets-dark.png")
    page.click("#provider-editor-cancel")

    page.click("#provider-default-failure-demo")
    page.click('[data-provider-default="ark"]')
    must(
        page.locator('[data-provider-card="deepseek"].selected').count() == 1
        and page.locator('[data-provider-default="deepseek"]').is_disabled()
        and not page.locator('[data-provider-default="ark"]').is_disabled()
        and page.locator("#model-selector strong").inner_text()
        == "deepseek-v4-flash"
        and "Host 拒绝默认切换"
        in page.locator('[data-provider-card-status="ark"]').inner_text(),
        "failed Provider default switch corrupted the canonical default snapshot",
    )
    shots.shot(page, "132a-provider-default-reject-rollback-dark.png")

    page.click('[data-provider-default="ark"]')
    must(
        page.locator("[data-provider-card].selected").count() == 1
        and "selected"
        in (
            page.locator('[data-provider-card="ark"]').get_attribute("class")
            or ""
        ).split()
        and page.locator('[data-provider-default="ark"]').is_disabled()
        and page.locator('[data-provider-default="ark"]').inner_text()
        == "已默认"
        and not page.locator('[data-provider-default="deepseek"]').is_disabled()
        and page.locator("#model-selector strong").inner_text() == "glm-5.3",
        "Provider default switch did not update the single canonical default",
    )
    shots.shot(page, "132b-provider-canonical-default-ark-dark.png")

    page.click('[data-provider-edit="ark"]')
    must(
        page.locator("#provider-delete").is_disabled()
        and page.locator("#provider-delete").get_attribute("title")
        == "默认服务必须先切换后才能删除",
        "canonical default Provider did not block delete",
    )
    page.click("#provider-editor-cancel")

    page.click('[data-provider-edit="deepseek"]')
    must(
        not page.locator("#provider-delete").is_disabled(),
        "previous Provider default remained undeletable after canonical switch",
    )
    page.click("#provider-delete")
    must(
        page.locator("#provider-delete-note").is_visible()
        and page.locator("#provider-confirm-delete").is_disabled()
        and page.locator("#provider-open-subagents").is_visible()
        and "子代理" in page.locator("#provider-delete-note").inner_text(),
        "referenced previous Provider default did not block delete",
    )
    shots.shot(page, "132b1-provider-referenced-delete-blocked-dark.png")
    page.click("#provider-open-subagents")
    must(
        page.locator('[data-settings-panel="subagents"]:not([hidden])').count()
        == 1
        and page.locator(
            '[data-slot-id="slot-1"][data-slot-source="DeepSeek"]'
        ).count()
        == 1,
        "Provider delete recovery did not open the referencing subagent slot",
    )
    page.click('[data-slot-id="slot-1"] [data-slot-remove]')
    page.locator(".slot-weight").first.fill("100")
    page.click("#save-subagent-pool")
    subagent_save_receipt = page.locator(
        "#subagent-pool-status"
    ).inner_text()
    must(
        "整池已由 Host 原子替换为 revision 19" in subagent_save_receipt
        and "全部 exact model 回执已复验" in subagent_save_receipt
        and page.locator("#slot-weight-summary").inner_text()
        == "1 个槽位 · 权重 100%",
        "subagent reference removal was not saved before Provider delete",
    )

    select_settings_panel(page, "providers")
    page.click('[data-provider-edit="deepseek"]')
    must(
        not page.locator("#provider-delete").is_disabled(),
        "unreferenced previous Provider default remained undeletable",
    )
    page.click("#provider-delete")
    must(
        page.locator("#provider-delete-note").is_visible()
        and not page.locator("#provider-confirm-delete").is_disabled(),
        "eligible previous Provider default did not expose delete confirmation",
    )
    page.click("#provider-confirm-delete")
    must(
        page.locator('[data-provider-card="deepseek"]').count() == 0
        and page.locator('[data-provider-card="ark"].selected').count() == 1
        and page.locator('[data-provider-default="ark"]').is_disabled()
        and page.locator("#model-selector strong").inner_text() == "glm-5.3",
        "deleting the previous default corrupted the canonical Provider default",
    )
    shots.shot(page, "132c-provider-previous-default-deleted-dark.png")

    page.select_option("#image-engine", "model")
    page.select_option(
        "#vision-state-preview", "provider_deleted"
    )
    must(
        page.input_value("#image-engine") == "model"
        and page.input_value("#vision-provider") == "missing"
        and page.locator("#save-image-engine").is_disabled(),
        "historical deleted Provider value was not preserved as a blocked value",
    )
    must(
        "历史持久值" in page.locator("#vision-validation").inner_text(),
        "historical image Provider recovery explanation is missing",
    )
    shots.shot(page, "133-image-historical-provider-value-dark.png")
    page.select_option("#vision-state-preview", "ready")
    page.click("#save-image-engine")

    select_settings_panel(page, "agents")
    page.click("#open-codex-settings")
    page.click("#codex-browser-login")
    must(
        page.locator("#codex-login-status").get_attribute("data-login-state")
        == "checking",
        "Codex browser login did not start a cancellable process",
    )
    page.click("#codex-browser-login")
    must(
        page.locator("#codex-login-status").get_attribute("data-login-state")
        == "cancelling"
        and "等待底层进程终止 ACK"
        in page.locator("#codex-login-status").inner_text(),
        "Codex cancel did not wait for the Host process-termination ACK",
    )
    page.wait_for_function(
        "document.getElementById('codex-login-status').dataset.loginState"
        " === 'cancelled'",
        timeout=3000,
    )
    must(
        "Host terminate ACK 已确认底层 Codex CLI 登录进程终止"
        in page.locator("#codex-login-status").inner_text(),
        "Codex cancel claimed completion without a process-termination ACK",
    )
    shots.shot(page, "133a-codex-login-cancel-terminate-ack-dark.png")
    page.click('[data-codex-tab="preferences"]')
    must(
        page.input_value("#codex-model-catalog-preview")
        == "login_required"
        and page.locator("#codex-model").is_disabled()
        and page.locator("#codex-effort").is_disabled(),
        "logged-out Codex catalog did not become login_required and inert",
    )
    shots.shot(page, "134-codex-catalog-login-required-dark.png")

    page.click('[data-codex-tab="setup"]')
    page.click("#codex-browser-login")
    must(
        page.locator("#codex-login-status").get_attribute("data-login-state")
        == "checking",
        "Codex login retry did not enter checking",
    )
    page.wait_for_function(
        "document.getElementById('codex-login-status')"
        ".dataset.loginState === 'logged_in'",
        timeout=3000,
    )
    page.click('[data-codex-tab="preferences"]')
    must(
        page.input_value("#codex-model-catalog-preview") == "ready"
        and not page.locator("#codex-model").is_disabled()
        and page.locator("#codex-model option").count() == 3,
        "successful login did not restore the dynamic Codex model catalog",
    )
    shots.shot(page, "135-codex-catalog-ready-dark.png")

    page.select_option("#codex-model", "gpt-5.6-sol")
    page.select_option("#codex-effort", "ultra")
    page.select_option("#codex-model", "gpt-5.6-luna")
    must(
        page.input_value("#codex-effort") == "",
        "incompatible Codex reasoning effort was not cleared",
    )
    must(
        "ultra 不受 gpt-5.6-luna 支持"
        in page.locator(
            '[data-codex-panel="preferences"] .inline-note'
        ).inner_text(),
        "Codex effort cleanup did not explain the incompatible value",
    )
    shots.shot(page, "136-codex-effort-incompatible-cleanup-dark.png")
    page.click("#codex-settings-cancel")
    must(
        page.locator("#codex-discard-note").is_visible(),
        "Codex catalog changes did not participate in the dirty boundary",
    )
    page.click("#codex-confirm-discard")
    return {
        "provider_vendor_targets": 2,
        "provider_default_reject_rollback": "deepseek",
        "provider_canonical_default": "ark",
        "provider_previous_default_deleted": "deepseek",
        "image_historical_value": "provider_deleted",
        "codex_catalog": ["login_required", "ready"],
        "codex_cancel": "host_process_terminate_ack",
        "codex_effort_cleanup": "ultra -> model default",
    }


def verify_subagent_rtk_and_mcp_depth(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    reset_page(page)
    page.click("#open-settings")
    subagent_receipt = trust_and_probe_codex_subagent(page, shots)

    connected_health = page.input_value("#subagent-source-preview-state")
    must(
        connected_health == "connected",
        "Codex health was not connected before availability matrix checks",
    )
    for suffix, health in (("a", "stale"), ("b", "failed")):
        page.select_option(
            "#subagent-source-preview-state", health
        )
        must(
            page.input_value("#subagent-availability-preview") == "ready"
            and not page.locator('[data-subagent-test="codex"]').is_disabled(),
            f"health={health} altered ready availability or disabled recovery",
        )
        shots.shot(page, f"131{suffix}-subagent-ready-health-{health}-dark.png")
    page.select_option(
        "#subagent-source-preview-state", "connected"
    )
    unavailable_states = [
        "needs_configuration",
        "not_installed",
        "login_required",
        "trust_required",
        "unsupported",
    ]
    for index, state in enumerate(unavailable_states, start=137):
        page.select_option(
            "#subagent-availability-preview", state
        )
        must(
            page.input_value("#subagent-source-preview-state")
            == connected_health,
            f"availability={state} overwrote the health receipt",
        )
        must(
            page.locator('[data-subagent-test="codex"]').is_disabled(),
            f"availability={state} left the health probe enabled",
        )
        shots.shot(page, f"{index}-subagent-availability-{state}-dark.png")

    select_settings_panel(page, "tools")
    page.select_option("#rtk-state-preview", "not_installed")
    must(
        page.input_value("#rtk-state-preview") == "not_installed"
        and page.locator("#rtk-version").inner_text() == "—",
        "RTK not-installed state did not render",
    )
    shots.shot(page, "142-rtk-not-installed-dark.png")
    page.click("#toggle-rtk")
    must(
        page.input_value("#rtk-state-preview") == "installing"
        and "校验 artifact" in page.locator("#rtk-version").inner_text(),
        "RTK enable did not enter managed installing",
    )
    shots.shot(page, "143-rtk-managed-installing-dark.png", delay_ms=40)
    page.wait_for_function(
        "document.getElementById('rtk-state-preview').value"
        " === 'managed_ready'",
        timeout=3000,
    )
    must(
        "v0.45.0" in page.locator("#rtk-version").inner_text(),
        "RTK managed install did not pin v0.45.0",
    )
    shots.shot(page, "144-rtk-managed-ready-v045-dark.png")
    page.select_option("#rtk-state-preview", "blocked")
    must(
        page.locator("#rtk-recovery").is_visible()
        and "旧快照" in page.locator("#rtk-state").inner_text(),
        "RTK security-blocked recovery did not preserve the old snapshot",
    )
    shots.shot(page, "145-rtk-security-blocked-rollback-dark.png")

    page.click("#add-mcp")
    must(
        page.locator("#mcp-editor-modal.open").is_visible()
        and page.locator('#mcp-stdio-fields:not([hidden])').is_visible(),
        "MCP stdio editor did not open",
    )
    shots.shot(page, "146-mcp-stdio-config-editor-dark.png")
    page.click('[data-mcp-transport="http"]')
    must(
        page.locator("#mcp-id").is_visible(),
        "MCP HTTP transport hid the transport-independent service ID",
    )
    page.fill("#mcp-id", "capture-http")
    page.fill("#mcp-url", "https://mcp.demo.invalid/v1")
    page.fill("#mcp-header-names", "Authorization\nX-Capture-Token")
    page.click("#mcp-test-editor")
    must(
        "正在校验" in page.locator("#mcp-editor-status").inner_text(),
        "MCP configuration test did not enter checking",
    )
    shots.shot(page, "147-mcp-http-config-testing-dark.png", delay_ms=40)
    page.wait_for_function(
        "document.getElementById('mcp-editor-status')"
        ".textContent.includes('配置测试通过')",
        timeout=3000,
    )
    shots.shot(page, "148-mcp-http-config-tested-dark.png")
    page.click('#mcp-editor-form button[type="submit"]')
    row = page.locator('[data-mcp-row="capture-http"]')
    must(
        row.count() == 1
        and "disabled" in row.locator(".status-inline").inner_text(),
        "new MCP configuration was not saved in the disabled state",
    )
    shots.shot(page, "149-mcp-saved-disabled-before-approval-dark.png")

    row.locator('[data-dynamic-mcp="edit"]').click()
    must(
        page.locator("#mcp-editor-modal.open").is_visible()
        and page.input_value("#mcp-id") == "capture-http"
        and not page.locator("#mcp-id").is_editable()
        and page.locator("#mcp-id").get_attribute("readonly") is not None
        and page.locator("#mcp-id").get_attribute("aria-readonly") == "true"
        and "不可改名" in (page.locator("#mcp-id").get_attribute("title") or ""),
        "existing MCP editor did not keep its canonical ID immutable",
    )
    shots.shot(page, "149a-mcp-existing-id-immutable-dark.png")
    page.click("#mcp-editor-cancel")
    must(
        row.count() == 1 and "disabled" in row.locator(".status-inline").inner_text(),
        "closing the immutable MCP editor mutated or enabled the service",
    )

    row.locator('[data-dynamic-mcp="credentials"]').click()
    page.wait_for_function(
        "document.querySelectorAll('#mcp-credential-list "
        "[data-credential-name]').length === 2",
        timeout=3000,
    )
    auth_row = page.locator(
        '#mcp-credential-list [data-credential-name="Authorization"]'
    )
    token_row = page.locator(
        '#mcp-credential-list [data-credential-name="X-Capture-Token"]'
    )
    auth_row.locator("[data-mcp-credential-input]").fill(
        "capture-demo-authorization"
    )
    token_row.locator("[data-mcp-credential-input]").fill(
        "capture-demo-token"
    )
    page.click("#mcp-credential-failure-demo")
    page.click('#mcp-credential-form button[type="submit"]')
    page.wait_for_function(
        "document.getElementById('mcp-credential-status')"
        ".textContent.includes('1 项成功，1 项失败')",
        timeout=3000,
    )
    must(
        auth_row.get_attribute("data-credential-state") == "error"
        and auth_row.locator("[data-mcp-credential-input]").input_value()
        == "capture-demo-authorization",
        "failed MCP credential replacement did not retain its draft",
    )
    must(
        token_row.get_attribute("data-credential-state") == "configured"
        and token_row.locator("[data-mcp-credential-input]").input_value()
        == "",
        "successful MCP credential replacement was rolled back or echoed",
    )
    shots.shot(page, "150-mcp-credential-partial-failure-dark.png")
    page.click("#mcp-credential-close")

    row.locator('[data-dynamic-mcp="enable"]').click()
    page.wait_for_function(
        "!document.getElementById('mcp-approval-confirm').disabled",
        timeout=3000,
    )
    must(
        "Exact launch plan"
        in page.locator("#mcp-approval-launch-plan").inner_text(),
        "saved MCP did not receive an independent exact launch approval",
    )
    shots.shot(page, "151-mcp-independent-enable-approval-dark.png")
    page.click("#mcp-approval-confirm")
    page.wait_for_function(
        "document.querySelector('[data-mcp-row=\"capture-http\"] "
        ".status-inline').textContent.includes('running')",
        timeout=3000,
    )
    shots.shot(page, "152-mcp-running-after-independent-approval-dark.png")

    initial_market_rows = page.locator("#mcp-market-results [data-market-id]")
    must(initial_market_rows.count() == 2, "Registry initial cursor batch drifted")
    page.click("#mcp-market-next")
    must(
        page.locator("#mcp-market-results [data-market-id]").count() == 4
        and "cursor=cursor-2" in page.locator("#mcp-market-page").inner_text(),
        "Registry load-more did not append the opaque cursor batch",
    )
    shots.shot(page, "153-mcp-registry-cursor-append-dark.png")
    page.select_option("#mcp-market-preview", "stale")
    must(
        "stale_cache" in page.locator("#mcp-market-status").inner_text()
        and page.locator("#mcp-market-results [data-market-id]").count() == 4,
        "Registry stale cache did not preserve the loaded rows",
    )
    shots.shot(page, "154-mcp-registry-stale-cache-dark.png")
    page.select_option("#mcp-market-preview", "error_cache")
    must(
        "refresh_error_with_cache"
        in page.locator("#mcp-market-status").inner_text()
        and page.locator("#mcp-market-results [data-market-id]").count() == 4,
        "Registry refresh error cleared the last-good cache",
    )
    shots.shot(page, "155-mcp-registry-error-with-cache-dark.png")
    return {
        "subagent": subagent_receipt,
        "availability_states": ["ready", *unavailable_states],
        "health_states": [
            "untested",
            "checking",
            "connected",
            "stale",
            "failed",
        ],
        "rtk": ["not_installed", "installing", "managed_ready", "blocked"],
        "mcp": {
            "config": [
                "stdio",
                "http",
                "tested",
                "saved_disabled",
                "existing_id_immutable",
            ],
            "credential_partial_failure": True,
            "independent_enable_approval": True,
            "registry": ["cursor_append", "stale_cache", "error_cache"],
        },
    }


def verify_memory_skills_and_companion_depth(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    reset_page(page)
    page.click("#open-settings")
    select_settings_panel(page, "knowledge")
    page.select_option("#knowledge-scope", "global")

    memory_states = [
        "queued",
        "running",
        "succeeded",
        "failed",
        "interrupted",
        "cancelled",
    ]
    retryable = {"failed", "interrupted"}
    cancellable = {"queued", "running", "failed", "interrupted"}
    for index, state in enumerate(memory_states, start=156):
        page.select_option("#memory-job-preview", state)
        must(
            state in page.locator("#memory-review-job strong").inner_text(),
            f"Memory job did not render state={state}",
        )
        must(
            page.locator("#retry-memory-job").is_disabled()
            == (state not in retryable),
            f"Memory retry affordance drifted for state={state}",
        )
        must(
            page.locator("#cancel-memory-job").is_disabled()
            == (state not in cancellable),
            f"Memory cancel affordance drifted for state={state}",
        )
        shots.shot(page, f"{index}-memory-job-{state}-dark.png")

    page.select_option("#memory-job-preview", "succeeded")
    page.select_option(
        "#memory-overview-poll-preview", "error"
    )
    must(
        "保留 last-good succeeded"
        in page.locator("#memory-review-job-poll").inner_text(),
        "Memory overview polling error did not preserve last-good",
    )
    shots.shot(page, "162-memory-poll-error-last-good-dark.png")
    page.select_option(
        "#memory-overview-poll-preview", "live"
    )
    must(
        "已恢复" in page.locator("#memory-review-job-poll").inner_text(),
        "Memory overview polling did not recover",
    )

    page.fill("#memory-review-interval", "17")
    page.click("#simulate-memory-conflict")
    must(
        page.locator("#memory-conflict-status").is_visible()
        and page.evaluate("document.activeElement.id")
        == "reapply-memory-draft"
        and "本地 base 27" in page.input_value("#memory-version"),
        "Memory CAS conflict did not preserve and focus the local proposal",
    )
    shots.shot(page, "163-memory-conflict-proposal-dark.png")
    page.click("#reapply-memory-draft")
    must(
        page.input_value("#memory-review-interval") == "17"
        and page.input_value("#memory-version") == "revision 29"
        and page.locator("#memory-conflict-status").is_hidden(),
        "Memory conflict reapply did not preserve the local draft",
    )
    shots.shot(page, "164-memory-conflict-reapplied-dark.png")

    page.click('[data-knowledge-tab="skills"]')
    skill_row = page.locator('[data-skill-row="huashu-design"]')
    must(
        skill_row.get_attribute("data-skill-enabled") == "true"
        and "已启用" in skill_row.inner_text(),
        "built-in Skill did not begin with its single enabled state",
    )
    skill_row.locator('[data-skill-toggle="huashu-design"]').click()
    must(
        skill_row.get_attribute("data-skill-enabled") == "false"
        and "已停用" in skill_row.inner_text()
        and "启用"
        == skill_row.locator(
            '[data-skill-toggle="huashu-design"]'
        ).inner_text(),
        "Skill disable did not update the single enabled field",
    )
    shots.shot(page, "165-skill-single-enabled-off-dark.png")
    skill_row.locator('[data-skill-toggle="huashu-design"]').click()
    must(
        skill_row.get_attribute("data-skill-enabled") == "true"
        and "已启用" in skill_row.inner_text(),
        "Skill re-enable did not restore runtime and slash availability",
    )
    shots.shot(page, "166-skill-single-enabled-on-dark.png")

    select_settings_panel(page, "appearance")
    must(
        page.locator("#companion-enabled").is_checked()
        and page.input_value("#companion-mode") == "full"
        and not page.locator("#companion-sound").is_checked()
        and page.input_value("#companion-motion") == "system",
        "Companion production defaults drifted",
    )
    shots.shot(page, "167-companion-production-defaults-dark.png")
    page.click("#companion-orb")
    must(
        page.locator("#companion-task-panel").is_visible()
        and page.locator("#companion-menu").is_hidden(),
        "Companion left click did not open only the task panel",
    )
    shots.shot(page, "168-companion-left-click-task-panel-dark.png")
    page.click("#companion-orb")
    must(
        page.locator("#companion-task-panel").is_hidden(),
        "Companion second left click did not close the task panel",
    )
    page.locator("#companion-orb").click(button="right")
    must(
        page.locator("#companion-menu").is_visible()
        and page.locator("#companion-task-panel").is_hidden()
        and page.locator("#companion-menu [role=menuitem]").count() == 1,
        "Companion right click did not expose the single close action",
    )
    shots.shot(page, "169-companion-right-click-single-menu-dark.png")
    page.click("#companion-menu-close")
    must(
        not page.locator("#companion-enabled").is_checked(),
        "Companion close menu did not persist disabled",
    )
    page.click("#simulate-companion-failure")
    must(
        page.locator("#companion-enabled").is_checked()
        and "正在请求" in page.locator("#companion-status").inner_text(),
        "Companion failure demo did not show the optimistic create phase",
    )
    shots.shot(page, "170-companion-create-checking-dark.png", delay_ms=40)
    page.wait_for_function(
        "document.getElementById('companion-status')"
        ".textContent.includes('启用状态已回滚')",
        timeout=3000,
    )
    must(
        not page.locator("#companion-enabled").is_checked()
        and page.locator("#companion-preview").evaluate(
            "element => getComputedStyle(element).opacity"
        )
        == "0.45",
        "Companion create failure did not roll back to the persisted state",
    )
    shots.shot(page, "171-companion-create-failure-rollback-dark.png")
    return {
        "memory_jobs": memory_states,
        "memory_poll": ["error_keep_last_good", "live_recovered"],
        "memory_conflict": ["proposal", "reapply"],
        "skills_single_enabled": [False, True],
        "companion": {
            "defaults": {
                "enabled": True,
                "mode": "full",
                "sound": False,
                "motion": "system",
            },
            "left_click": "task_panel",
            "right_click_items": 1,
            "create_failure_rollback": True,
        },
    }


def visible_cjk_count(page: Page, selector: str) -> int:
    return len(re.findall(r"[\u3400-\u9fff]", page.locator(selector).inner_text()))


def interface_locale_dom_issues(
    page: Page, selector: str = "body"
) -> dict[str, object]:
    """Inspect all UI copy, including hidden states and accessible attributes.

    Script/style/template source is deliberately excluded: it is prototype
    implementation, not interface copy. Text nodes elsewhere in the selected
    subtree are checked regardless of visibility, as are the four attributes
    translated by the prototype locale contract.
    """

    return page.evaluate(
        r"""selector => {
          const root = document.querySelector(selector);
          if (!root) throw new Error(`locale audit root not found: ${selector}`);
          const missing = '⟦missing en-US copy⟧';
          const result = {
            cjk_count: 0,
            missing_count: 0,
            cjk_samples: [],
            missing_samples: [],
          };
          const describe = element => {
            if (!element) return '<detached>';
            if (element.id) return `#${CSS.escape(element.id)}`;
            const parts = [];
            let current = element;
            while (current && current !== root && current !== document.body) {
              if (current.id) {
                parts.unshift(`#${CSS.escape(current.id)}`);
                break;
              }
              const tag = current.tagName.toLowerCase();
              const sameTagSiblings = current.parentElement
                ? [...current.parentElement.children]
                  .filter(sibling => sibling.tagName === current.tagName)
                : [];
              const suffix = sameTagSiblings.length > 1
                ? `:nth-of-type(${sameTagSiblings.indexOf(current) + 1})`
                : '';
              parts.unshift(`${tag}${suffix}`);
              current = current.parentElement;
            }
            return parts.join(' > ') || root.tagName.toLowerCase();
          };
          const inspect = (value, selector, attribute = null) => {
            if (!value) return;
            const cjk = value.match(/[\u3400-\u9fff]/g) || [];
            const missingCount = value.split(missing).length - 1;
            result.cjk_count += cjk.length;
            result.missing_count += missingCount;
            const compact = value.replace(/\s+/g, ' ').trim().slice(0, 600);
            const sample = {
              selector,
              kind: attribute ? 'attribute' : 'text',
              attribute,
              value: compact,
            };
            if (cjk.length && result.cjk_samples.length < 40) {
              result.cjk_samples.push(sample);
            }
            if (missingCount && result.missing_samples.length < 40) {
              result.missing_samples.push(sample);
            }
          };

          const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
          while (walker.nextNode()) {
            const owner = walker.currentNode.parentElement;
            if (!owner || owner.closest('script, style, template')) continue;
            inspect(walker.currentNode.nodeValue || '', describe(owner));
          }

          const elements = [root, ...root.querySelectorAll('*')]
            .filter(node => node.nodeType === Node.ELEMENT_NODE);
          elements.forEach(element => {
            ['placeholder', 'title', 'aria-label', 'alt'].forEach(attribute => {
              if (element.hasAttribute(attribute)) {
                inspect(
                  element.getAttribute(attribute) || '',
                  describe(element),
                  attribute,
                );
              }
            });
          });
          return result;
        }""",
        selector,
    )


def assert_complete_english_locale(page: Page, label: str) -> dict[str, object]:
    issues = interface_locale_dom_issues(page)
    must(
        issues["cjk_count"] == 0 and issues["missing_count"] == 0,
        f"{label} left untranslated UI copy: {issues}",
    )
    return issues


def verify_latest_production_contracts(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    reset_page(page)
    page.click("#open-settings")
    select_settings_panel(page, "providers")
    page.wait_for_function(
        "!document.getElementById('sync-provider-models').disabled",
        timeout=3000,
    )

    def provider_revision() -> int:
        match = re.search(
            r"revision\s+(\d+)",
            page.input_value("#provider-sync-receipt"),
        )
        must(match is not None, "Provider sync receipt omitted its revision")
        return int(match.group(1))

    initial_revision = provider_revision()
    page.select_option("#provider-sync-scenario", "success")
    page.click("#sync-provider-models")
    must(
        page.locator("#sync-provider-models").is_disabled()
        and "last-good" in page.locator("#provider-sync-status").inner_text(),
        "Provider sync success scenario did not enter checking with last-good",
    )
    shots.shot(page, "172-provider-sync-checking-last-good-dark.png", delay_ms=40)
    page.wait_for_function(
        "document.getElementById('provider-sync-status')"
        ".textContent.includes('已应用')",
        timeout=3000,
    )
    success_revision = provider_revision()
    must(
        success_revision == initial_revision + 1
        and not page.locator(
            '#vision-provider option[value="deepseek"]'
        ).is_disabled(),
        "Provider sync success did not publish one shared-catalog revision",
    )
    shots.shot(page, "173-provider-sync-success-shared-catalog-dark.png")

    page.select_option("#provider-sync-scenario", "failure")
    page.click("#sync-provider-models")
    page.wait_for_function(
        "document.getElementById('provider-sync-status')"
        ".textContent.includes('同步失败')",
        timeout=3000,
    )
    failure_copy = page.locator("#provider-sync-status").inner_text()
    must(
        provider_revision() == success_revision
        and "退避到" in failure_copy
        and "保留 revision" in failure_copy
        and "手动" in failure_copy,
        "Provider sync failure did not preserve last-good with backoff",
    )
    shots.shot(page, "174-provider-sync-failure-backoff-dark.png")

    page.select_option("#provider-sync-scenario", "race")
    page.click("#sync-provider-models")
    page.wait_for_function(
        "document.getElementById('provider-sync-receipt')"
        ".value.includes('stale')",
        timeout=3000,
    )
    must(
        "唯一可应用" in page.locator("#provider-sync-status").inner_text(),
        "Provider race did not identify the replacement request",
    )
    shots.shot(page, "175-provider-sync-race-stale-pending-dark.png")
    page.wait_for_function(
        "document.getElementById('provider-sync-status')"
        ".textContent.includes('stale-result ignored')",
        timeout=3000,
    )
    must(
        provider_revision() == success_revision + 1,
        "Provider race applied more than the replacement response",
    )
    shots.shot(page, "176-provider-sync-race-old-result-ignored-dark.png")

    page.select_option("#image-main-capability", "multimodal")
    page.select_option("#image-attachment-format", "png")
    direct_copy = page.locator("#image-route-status").inner_text()
    must(
        "原始 PNG 直发" in direct_copy and "辅助引擎调用 0 次" in direct_copy,
        "confirmed multimodal image did not route directly",
    )
    shots.shot(page, "177-image-route-direct-multimodal-dark.png")

    page.select_option("#image-main-capability", "text")
    page.select_option("#image-engine", "ocr")
    ocr_copy = page.locator("#image-route-status").inner_text()
    must(
        "用户显式选择 Windows OCR" in ocr_copy and "原图不发送" in ocr_copy,
        "explicit OCR route did not preserve its text-only boundary",
    )
    shots.shot(page, "178-image-route-explicit-ocr-dark.png")

    page.select_option("#image-main-capability", "unknown")
    unknown_ocr_copy = page.locator("#image-route-status").inner_text()
    must(
        "unknown 主模型" in unknown_ocr_copy
        and "用户显式选择 Windows OCR" in unknown_ocr_copy
        and "原图不发送" in unknown_ocr_copy,
        "unknown main-model capability did not honor explicit OCR routing",
    )
    shots.shot(page, "178a-image-route-unknown-explicit-ocr-dark.png")

    page.select_option("#image-engine", "model")
    page.select_option("#vision-provider", "ark")
    page.select_option("#vision-model", "glm-4v-plus")
    page.click("#retry-image-route")
    unknown_vision_copy = page.locator("#image-route-status").inner_text()
    must(
        "unknown 主模型" in unknown_vision_copy
        and "显式 ark/glm-4v-plus 视觉 helper 成功" in unknown_vision_copy
        and "原图不发主模型" in unknown_vision_copy,
        "unknown main-model capability did not use the complete vision helper",
    )
    shots.shot(page, "178b-image-route-unknown-vision-helper-dark.png")

    page.click("#simulate-vision-failure")
    failure_route_copy = page.locator("#image-route-status").inner_text()
    must(
        "整批附件未发送" in failure_route_copy
        and "不会自动 OCR" in failure_route_copy
        and page.input_value("#image-engine") == "model",
        "vision failure silently degraded or lost the original error boundary",
    )
    shots.shot(page, "179-image-vision-failure-no-auto-ocr-dark.png")
    page.select_option("#image-engine", "ocr")
    page.click("#retry-image-route")
    must(
        "用户显式选择 Windows OCR"
        in page.locator("#image-route-status").inner_text(),
        "explicit post-failure OCR retry did not recover",
    )
    shots.shot(page, "180-image-vision-failure-explicit-ocr-retry-dark.png")

    select_settings_panel(page, "agents")
    page.wait_for_function(
        "document.getElementById('codex-setup-preview').value === 'check'",
        timeout=1000,
    )
    must(
        page.locator("#codex-setup-primary").is_disabled(),
        "Codex automatic setup check left its primary action enabled",
    )
    shots.shot(page, "181-codex-setup-auto-check-dark.png", delay_ms=40)
    page.wait_for_function(
        "document.getElementById('codex-setup-preview').value === 'ready'",
        timeout=3000,
    )
    shots.shot(page, "182-codex-setup-ready-dark.png")

    orchestration_expectations = {
        "ready": ("新 Codex 委派可用", "创建时冻结"),
        "provider_unavailable": ("Provider unavailable", "frozen snapshot"),
        "customer_off": ("回退 R-Code", "已创建 Plan 不受影响"),
        "emergency_off": ("已经运行的 Codex 子代理继续完成", "停止产生新的建议"),
        "task_declined": ("新 Codex 委派可用", "本任务不再提示"),
    }
    for index, (state, expected) in enumerate(
        orchestration_expectations.items(), start=183
    ):
        page.select_option("#orchestration-availability-demo", state)
        delegation_copy = page.locator("#delegation-runtime-status").inner_text()
        plan_copy = page.locator("#plan-runtime-status").inner_text()
        must(
            expected[0] in delegation_copy and expected[1] in plan_copy,
            f"orchestration state lost its runtime/frozen semantics: {state}",
        )
        shots.shot(page, f"{index}-orchestration-{state}-dark.png")

    codex_setup_expectations = {
        "install_cli": ("安装 CLI", "install_cli"),
        "login": ("开始登录", "3 分钟"),
        "check": ("检查登录与目录", "旧 CLI"),
        "configure": ("配置协作", "固定 Skill + MCP"),
        "ready": ("重新检查", "ready"),
        "upgrade_failed": ("重试升级检查", "preserved and usable"),
    }
    for state, expected in codex_setup_expectations.items():
        page.select_option("#codex-setup-preview", state)
        must(
            page.locator("#codex-setup-primary").inner_text() == expected[0]
            and expected[1]
            in page.locator("#codex-setup-contract-status").inner_text(),
            f"Codex setup state did not expose its unique next action: {state}",
        )
    shots.shot(page, "188-codex-setup-six-state-contract-dark.png")

    must(
        page.locator('#codex-model option[value="custom"]').count() == 0,
        "Codex custom model was writable without a historical persisted value",
    )
    page.select_option("#codex-historical-custom", "legacy")
    custom_option = page.locator('#codex-model option[value="custom"]')
    must(
        custom_option.count() == 1
        and custom_option.get_attribute("disabled") is not None
        and "只读" in page.locator("#codex-custom-contract-status").inner_text(),
        "historical Codex custom value was not disabled and read-only",
    )
    page.click("#open-codex-settings")
    page.click('[data-codex-tab="preferences"]')
    must(
        page.input_value("#codex-model") != "custom",
        "historical Codex custom value became actively selectable",
    )
    shots.shot(page, "189-codex-historical-custom-readonly-dark.png")
    page.click("#codex-settings-cancel")
    page.select_option("#codex-historical-custom", "none")
    must(
        page.locator('#codex-model option[value="custom"]').count() == 0,
        "Codex historical custom option remained after snapshot removal",
    )

    select_settings_panel(page, "appearance")
    chinese_snapshot = page.locator("#settings-scene").inner_text()
    chinese_count = visible_cjk_count(page, "#settings-scene")
    must(chinese_count > 0, "zh-CN settings contained no visible Chinese")

    # Establish the exact zh-CN labels for the same dynamic states that will
    # be exercised after switching locales, using only public controls.
    page.click("#theme-toggle")
    page.click("#maximize-window")
    settle(page, 40)
    dynamic_chinese_attributes = {
        "theme_toggle": page.locator("#theme-toggle").get_attribute("aria-label"),
        "maximize_window": page.locator("#maximize-window").get_attribute(
            "aria-label"
        ),
    }
    must(
        all(
            value and re.search(r"[\u3400-\u9fff]", value)
            for value in dynamic_chinese_attributes.values()
        ),
        "dynamic zh-CN window/theme labels were not established",
    )
    page.click("#theme-toggle")
    page.click("#maximize-window")
    settle(page, 40)
    must(
        page.locator("#theme-toggle").get_attribute("aria-pressed") == "false"
        and page.locator("#maximize-window").get_attribute("aria-pressed")
        == "false",
        "window/theme controls did not return to their baseline before locale switch",
    )

    page.select_option("#interface-language", "en-US")
    settle(page, 40)
    english_initial_issues = assert_complete_english_locale(
        page, "initial en-US DOM"
    )
    must(
        page.locator("html").get_attribute("lang") == "en-US"
        and visible_cjk_count(page, "#settings-scene") == 0,
        "en-US left visible Chinese text or failed to update html.lang",
    )
    shots.shot(page, "190-interface-language-en-us-zero-visible-cjk-dark.png")

    page.click("#theme-toggle")
    page.click("#maximize-window")
    settle(page, 40)
    english_dynamic_issues = assert_complete_english_locale(
        page, "dynamic en-US DOM"
    )
    dynamic_english_attributes = {
        "theme_toggle": page.locator("#theme-toggle").get_attribute("aria-label"),
        "maximize_window": page.locator("#maximize-window").get_attribute(
            "aria-label"
        ),
    }
    must(
        all(
            value
            and not re.search(r"[\u3400-\u9fff]", value)
            and "⟦missing en-US copy⟧" not in value
            for value in dynamic_english_attributes.values()
        ),
        "dynamic theme/window aria-label rewrites fell back to Chinese or a missing-copy marker",
    )
    shots.shot(page, "190b-interface-language-en-us-dynamic-aria-light.png")

    page.select_option("#interface-language", "zh-CN")
    settle(page, 40)
    restored_count = visible_cjk_count(page, "#settings-scene")
    restored_dom_issues = interface_locale_dom_issues(page)
    must(
        page.locator("html").get_attribute("lang") == "zh-CN"
        and page.locator("#settings-scene").inner_text() == chinese_snapshot
        and restored_count == chinese_count,
        "zh-CN switch-back did not exactly restore visible settings copy",
    )
    must(
        {
            "theme_toggle": page.locator("#theme-toggle").get_attribute(
                "aria-label"
            ),
            "maximize_window": page.locator("#maximize-window").get_attribute(
                "aria-label"
            ),
        }
        == dynamic_chinese_attributes
        and restored_dom_issues["missing_count"] == 0,
        "zh-CN switch-back did not exactly restore dynamic aria-label copy",
    )
    page.click("#theme-toggle")
    page.click("#maximize-window")
    settle(page, 40)
    must(
        page.locator("#theme-toggle").get_attribute("aria-pressed") == "false"
        and page.locator("#maximize-window").get_attribute("aria-pressed")
        == "false",
        "window/theme controls did not return to the dark, restored baseline",
    )
    shots.shot(page, "191-interface-language-zh-cn-restored-dark.png")

    select_settings_panel(page, "tools")
    page.select_option("#shell-detection-scenario", "ready")
    page.wait_for_function(
        "document.getElementById('shell-status-copy')"
        ".textContent.startsWith('ready')",
        timeout=3000,
    )
    must(
        page.input_value("#shell-actual-dialect") == "pwsh"
        and page.input_value("#shell-actual-program").endswith("pwsh.exe"),
        "Shell ready state omitted the actual dialect/program",
    )
    shots.shot(page, "192-shell-ready-actual-program-dark.png")
    page.select_option("#shell-detection-scenario", "error")
    page.wait_for_function(
        "document.getElementById('shell-status-copy')"
        ".textContent.startsWith('error')",
        timeout=3000,
    )
    must(
        page.input_value("#shell-actual-dialect") == "unresolved"
        and page.input_value("#shell-actual-program") == ""
        and "旧 Gateway 快照保持"
        in page.locator("#shell-status-copy").inner_text(),
        "Shell error state hid its failure reason or old-snapshot boundary",
    )
    shots.shot(page, "193-shell-error-old-snapshot-dark.png")

    select_settings_panel(page, "knowledge")
    page.select_option("#knowledge-scope", "global")
    page.click('[data-knowledge-tab="skills"]')
    page.click("#manage-skills")
    page.fill("#skill-name", "foo--bar")
    page.click('#skill-editor-form button[type="submit"]')
    must(
        page.evaluate("document.activeElement.id") == "skill-name"
        and "连续连字符" in page.locator("#skill-editor-status").inner_text(),
        "Skill foo--bar was not rejected at the invocation-name field",
    )
    shots.shot(page, "194-skill-reject-double-hyphen-dark.png")
    page.fill("#skill-name", "capture-contract")
    page.fill("#skill-description", "")
    page.click('#skill-editor-form button[type="submit"]')
    must(
        page.evaluate("document.activeElement.id") == "skill-description"
        and "描述必填" in page.locator("#skill-editor-status").inner_text(),
        "Skill empty description was not rejected at the description field",
    )
    shots.shot(page, "195-skill-reject-empty-description-dark.png")
    page.fill("#skill-description", "Contract validation")
    page.fill("#skill-instructions", "")
    page.click('#skill-editor-form button[type="submit"]')
    must(
        page.evaluate("document.activeElement.id") == "skill-instructions"
        and "指令必填" in page.locator("#skill-editor-status").inner_text(),
        "Skill empty instructions were not rejected at the instructions field",
    )
    shots.shot(page, "196-skill-reject-empty-instructions-dark.png")
    page.fill("#skill-name", "team-rules")
    page.fill("#skill-instructions", "Do not overwrite another scope.")
    page.select_option("#skill-scope", label="全局")
    page.click('#skill-editor-form button[type="submit"]')
    must(
        page.evaluate("document.activeElement.id") == "skill-name"
        and "全局作用域已存在同名 Skill"
        in page.locator("#skill-editor-status").inner_text(),
        "Skill scope-aware duplicate was not rejected",
    )
    shots.shot(page, "197-skill-reject-scope-duplicate-dark.png")
    page.click("#skill-editor-cancel")

    page.select_option("#knowledge-scope", "project-r-code")
    page.click("#simulate-project-removal")
    must(
        page.input_value("#knowledge-scope") == "global"
        and page.locator(
            '#knowledge-scope option[value="project-r-code"]'
        ).count()
        == 0
        and "已从 workspace 移除"
        in page.locator("#memory-reviewer-status").inner_text(),
        "Knowledge project removal did not fall back and reread global scope",
    )
    shots.shot(page, "198-knowledge-project-removal-global-fallback-dark.png")

    select_settings_panel(page, "appearance")
    page.click("#migrate-companion-legacy")
    must(
        page.input_value("#companion-mode") == "minimized"
        and page.input_value("#companion-position-x") == "18"
        and "v2 snapshot" in page.locator("#companion-status").inner_text(),
        "Companion legacy preferences did not migrate to the v2 snapshot",
    )
    shots.shot(page, "199-companion-legacy-to-v2-migration-dark.png")
    companion_snapshot = (
        page.input_value("#companion-mode"),
        page.input_value("#companion-position-x"),
        page.input_value("#companion-position-y"),
    )
    page.click("#simulate-companion-stale")
    must(
        companion_snapshot
        == (
            page.input_value("#companion-mode"),
            page.input_value("#companion-position-x"),
            page.input_value("#companion-position-y"),
        )
        and "stale ignored" in page.locator("#companion-status").inner_text(),
        "Companion accepted an older snapshot/ACK",
    )
    shots.shot(page, "200-companion-stale-revision-ignored-dark.png")
    return {
        "provider_sync": ["success", "failure_backoff", "race_ignored"],
        "image_routing": [
            "direct",
            "explicit_ocr",
            "unknown_explicit_ocr",
            "unknown_complete_vision_helper",
            "vision_failure_no_ocr",
        ],
        "orchestration": list(orchestration_expectations),
        "codex_setup": list(codex_setup_expectations),
        "codex_custom": "historical_readonly_only",
        "locale_visible_cjk": {
            "zh_cn": chinese_count,
            "en_us": 0,
            "zh_cn_restored": restored_count,
        },
        "locale_dom_contract": {
            "initial_en_us": english_initial_issues,
            "dynamic_en_us": english_dynamic_issues,
            "dynamic_en_us_attributes": dynamic_english_attributes,
            "dynamic_zh_cn_restored": dynamic_chinese_attributes,
            "zh_cn_missing_marker_count": restored_dom_issues["missing_count"],
        },
        "shell": ["ready_actual_program", "error_old_snapshot"],
        "skill_validation": [
            "double_hyphen",
            "empty_description",
            "empty_instructions",
            "scope_duplicate",
        ],
        "knowledge_project_removal": "global_reread",
        "companion": ["legacy_to_v2", "stale_ignored"],
    }


def verify_mcp_approval_contract(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    reset_page(page)
    page.click("#open-settings")
    select_settings_panel(page, "tools")
    toggle = page.locator('[data-mcp-toggle="browser-tools"]')
    status = page.locator('[data-mcp-status="browser-tools"]')

    toggle.click()
    settle(page, 30)
    must(
        page.locator("#mcp-approval-modal [role='alertdialog']").is_visible(),
        "MCP approval did not expose an alertdialog",
    )
    must(
        page.evaluate("document.activeElement.id") == "mcp-approval-cancel",
        "MCP approval did not establish initial focus",
    )
    must(
        page.locator("#mcp-approval-confirm").is_disabled(),
        "MCP approval confirm enabled before Host preview was ready",
    )
    shots.shot(page, "103-mcp-approval-requesting-dark.png", delay_ms=40)

    def wait_for_ready() -> None:
        page.wait_for_function(
            "!document.getElementById('mcp-approval-confirm').disabled",
            timeout=3000,
        )

    def assert_hidden_confirm_is_inert(close_method: str) -> None:
        wait_for_ready()
        if close_method == "cancel":
            page.click("#mcp-approval-cancel")
        elif close_method == "escape":
            page.keyboard.press("Escape")
        else:
            page.locator("#mcp-approval-modal").click(
                position={"x": 2, "y": 2}
            )
        settle(page, 30)
        must(
            page.locator("#mcp-approval-modal").get_attribute("aria-hidden")
            == "true",
            f"MCP approval {close_method} did not close the alertdialog",
        )
        must(
            page.evaluate("document.activeElement.dataset.mcpToggle || ''")
            == "browser-tools",
            f"MCP approval {close_method} did not restore trigger focus",
        )
        page.evaluate("document.getElementById('mcp-approval-confirm').click()")
        settle(page, 30)
        must(
            page.input_value("#mcp-runtime-preview") == "disabled"
            and "starting" not in status.inner_text(),
            f"hidden MCP confirm entered starting after {close_method}",
        )

    assert_hidden_confirm_is_inert("cancel")
    for close_method in ("escape", "backdrop"):
        toggle.click()
        assert_hidden_confirm_is_inert(close_method)

    toggle.click()
    wait_for_ready()
    must(
        page.locator("#mcp-approval-retry").is_hidden(),
        "MCP retry remained visible in ready state",
    )
    launch_plan = page.locator("#mcp-approval-launch-plan").inner_text()
    must(
        "https://localhost:8765/mcp" in launch_plan
        and "values: hidden" in launch_plan,
        f"MCP exact HTTP launch plan is incomplete: {launch_plan}",
    )
    shots.shot(page, "104-mcp-approval-ready-dark.png")

    page.select_option("#mcp-approval-preview", "error")
    must(
        page.locator("#mcp-approval-confirm").is_disabled()
        and page.locator("#mcp-approval-retry").is_visible(),
        "MCP Host error did not expose an in-place retry gate",
    )
    must(
        "Host 生成启动方案失败"
        in page.locator("#mcp-approval-status").inner_text(),
        "MCP Host error explanation is missing",
    )
    shots.shot(page, "105-mcp-approval-host-error-dark.png")
    page.click("#mcp-approval-retry")
    must(
        page.locator("#mcp-approval-confirm").is_disabled()
        and "正在重新校验"
        in page.locator("#mcp-approval-status").inner_text(),
        "MCP retry did not re-enter the requesting state",
    )
    shots.shot(page, "106-mcp-approval-retrying-dark.png", delay_ms=40)
    page.wait_for_function(
        "!document.getElementById('mcp-approval-confirm').disabled",
        timeout=3000,
    )
    page.click("#mcp-approval-confirm")
    must(
        "starting" in status.inner_text()
        and page.input_value("#mcp-runtime-preview") == "starting",
        "MCP confirmation did not enter starting",
    )
    shots.shot(page, "107-mcp-starting-dark.png", delay_ms=40)
    page.wait_for_function(
        "document.getElementById('mcp-runtime-preview').value === 'running'",
        timeout=3000,
    )
    must("running" in status.inner_text(), "MCP did not reach running")
    shots.shot(page, "108-mcp-running-dark.png")
    return {
        "dialog_role": "alertdialog",
        "approval": ["cancel", "ready", "error", "retry", "confirm"],
        "runtime": ["starting", "running"],
        "dismissal_guards": ["cancel", "escape", "backdrop"],
        "hidden_confirm_inert": True,
    }


def verify_subagent_prompt_contract(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    reset_page(page)
    page.click("#open-settings")
    select_settings_panel(page, "subagents")
    revision_copy = page.locator("#slot-weight-summary").inner_text()

    trust_and_probe_codex_subagent(page)
    page.click("#add-subagent-slot")
    must(
        page.locator('[data-slot-id="slot-3"]').count() == 1,
        "healthy Codex receipt did not permit adding slot-3",
    )
    shots.shot(page, "108a-subagent-receipt-and-add-dark.png")
    page.click('[data-slot-id="slot-3"] [data-slot-remove]')
    must(
        page.locator('[data-slot-id="slot-3"]').count() == 0
        and not page.locator("#add-subagent-slot").is_disabled(),
        "slot-3 removal did not restore the add affordance",
    )
    page.click("#reload-subagent-pool")
    must(
        page.locator("[data-slot-row]").count() == 2
        and page.locator("#slot-weight-summary").inner_text() == revision_copy,
        "whole-pool reload did not restore the two-slot Host snapshot",
    )
    shots.shot(page, "108b-subagent-remove-and-pool-reload-dark.png")

    page.click('[data-slot-prompt="slot-1"]')
    slot_one_original = page.input_value("#subagent-final-prompt")
    page.click('[data-slot-prompt="slot-2"]')
    slot_two_original = page.input_value("#subagent-final-prompt")
    must(
        slot_one_original != slot_two_original,
        "per-slot Prompt fixtures are not independently distinguishable",
    )

    slot_one_draft = "slot-1 独立草稿 · 仅实现分配文件并返回验证证据。"
    slot_two_draft = "slot-2 独立草稿 · 只读复核状态机与权限边界。"
    page.click('[data-slot-prompt="slot-1"]')
    page.fill("#subagent-final-prompt", slot_one_draft)
    page.click('[data-slot-prompt="slot-2"]')
    must(
        page.input_value("#subagent-final-prompt") == slot_two_original,
        "slot-1 Prompt leaked into slot-2",
    )
    page.fill("#subagent-final-prompt", slot_two_draft)
    page.click('[data-slot-prompt="slot-1"]')
    must(
        page.input_value("#subagent-final-prompt") == slot_one_draft,
        "slot-2 Prompt overwrote slot-1",
    )
    shots.shot(page, "109-subagent-per-slot-drafts-isolated-dark.png")

    page.click("#reload-subagent-pool")
    must(
        page.input_value("#subagent-final-prompt") == slot_one_original,
        "whole-pool reload did not restore slot-1 Host snapshot",
    )
    page.click('[data-slot-prompt="slot-2"]')
    must(
        page.input_value("#subagent-final-prompt") == slot_two_original,
        "whole-pool reload did not restore slot-2 Host snapshot",
    )
    must(
        page.locator("#slot-weight-summary").inner_text() == revision_copy,
        "whole-pool reload unexpectedly changed revision",
    )
    shots.shot(page, "110-subagent-whole-pool-reloaded-dark.png")

    conflict_actions = [
        "discard-subagent-conflict",
        "restore-subagent-prompt-buffer",
        "merge-subagent-conflict",
    ]

    def create_conflict(slot_one_value: str, slot_two_value: str) -> None:
        page.click('[data-slot-prompt="slot-1"]')
        page.fill("#subagent-final-prompt", slot_one_value)
        page.click('[data-slot-prompt="slot-2"]')
        page.fill("#subagent-final-prompt", slot_two_value)
        page.click('[data-slot-prompt="slot-1"]')
        page.click("#simulate-subagent-conflict")
        must(
            page.locator("#simulate-subagent-conflict").get_attribute(
                "aria-pressed"
            )
            == "true",
            "subagent conflict was not armed for the next save",
        )
        page.click("#save-subagent-pool")
        must(
            "CAS conflict" in page.locator("#subagent-pool-status").inner_text()
            and "本地草稿"
            in page.locator("#subagent-prompt-status").inner_text(),
            "subagent revision conflict did not retain both snapshots",
        )
        must(
            page.input_value("#subagent-final-prompt") == slot_one_value,
            "subagent conflict replaced the visible slot-1 local draft",
        )
        page.click('[data-slot-prompt="slot-2"]')
        must(
            page.input_value("#subagent-final-prompt") == slot_two_value,
            "subagent conflict replaced the visible slot-2 local draft",
        )
        page.click('[data-slot-prompt="slot-1"]')
        must(
            all(page.locator(f"#{action}").is_visible() for action in conflict_actions)
            and page.locator("#save-subagent-pool").is_disabled(),
            "subagent conflict did not expose exactly the three recovery paths",
        )

    create_conflict(slot_one_draft, slot_two_draft)
    shots.shot(page, "111-subagent-revision-conflict-local-visible-dark.png")

    page.click("#discard-subagent-conflict")
    must(
        page.input_value("#subagent-final-prompt") == slot_one_original
        and "显式放弃" in page.locator("#subagent-pool-status").inner_text()
        and all(page.locator(f"#{action}").is_hidden() for action in conflict_actions),
        "discard conflict recovery did not adopt the fresh Host snapshot",
    )
    page.click('[data-slot-prompt="slot-2"]')
    must(
        page.input_value("#subagent-final-prompt") == slot_two_original,
        "discard conflict recovery did not restore the Host slot-2 Prompt",
    )
    shots.shot(page, "111a-subagent-conflict-discard-host-dark.png")

    create_conflict(slot_one_draft, slot_two_draft)
    page.click("#restore-subagent-prompt-buffer")
    must(
        page.input_value("#subagent-final-prompt") == slot_one_draft
        and "Host expected revision"
        in page.locator("#subagent-pool-status").inner_text()
        and not page.locator("#save-subagent-pool").is_disabled()
        and all(page.locator(f"#{action}").is_hidden() for action in conflict_actions),
        "reapply conflict recovery did not preserve the local pool at fresh revision",
    )
    page.click('[data-slot-prompt="slot-2"]')
    must(
        page.input_value("#subagent-final-prompt") == slot_two_draft,
        "reapply conflict recovery lost the independent slot-2 draft",
    )
    shots.shot(page, "112-subagent-conflict-reapplied-dark.png")
    page.click("#save-subagent-pool")
    must(
        "Host 原子替换" in page.locator("#subagent-pool-status").inner_text(),
        "reapplied subagent draft could not be saved against the fresh revision",
    )

    slot_one_merge = f"{slot_one_draft} · merge"
    slot_two_merge = f"{slot_two_draft} · merge"
    create_conflict(slot_one_merge, slot_two_merge)
    page.click("#merge-subagent-conflict")
    must(
        page.input_value("#subagent-final-prompt") == slot_one_merge
        and "合并本地草稿与 Host revision"
        in page.locator("#subagent-pool-status").inner_text()
        and "必须再次原子保存"
        in page.locator("#subagent-prompt-status").inner_text()
        and not page.locator("#save-subagent-pool").is_disabled()
        and all(page.locator(f"#{action}").is_hidden() for action in conflict_actions),
        "merge conflict recovery did not expose a saveable merged local draft",
    )
    page.click('[data-slot-prompt="slot-2"]')
    must(
        page.input_value("#subagent-final-prompt") == slot_two_merge,
        "merge conflict recovery lost the local slot-2 Prompt",
    )
    shots.shot(page, "112a-subagent-conflict-merged-dark.png")
    page.click("#save-subagent-pool")
    must(
        "Host 原子替换" in page.locator("#subagent-pool-status").inner_text(),
        "merged subagent draft could not complete the follow-up atomic save",
    )
    return {
        "receipt_add_remove_reload": True,
        "per_slot_isolation": True,
        "whole_pool_reload": True,
        "revision_conflict_local_visible": True,
        "revision_conflict_recovery": ["discard", "reapply", "merge"],
    }


def verify_guide_sheet_contracts(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    reset_page(page)
    page.click("#open-settings")
    cases = [
        ("provider", "providers", "providers-block"),
        ("image", "providers", "image-understanding-block"),
        ("plan", "agents", "delegation-quality-block"),
        ("subagent", "subagents", "subagent-slots-block"),
    ]
    verified: dict[str, list[str]] = {}

    def guide_focus_state() -> dict[str, object]:
        return page.locator("#guide-modal [role='dialog']").evaluate(
            """dialog => {
              const selector = [
                'button:not([disabled])',
                'input:not([disabled])',
                'select:not([disabled])',
                'textarea:not([disabled])',
                'a[href]',
                '[tabindex]:not([tabindex="-1"])',
              ].join(', ');
              const items = [...dialog.querySelectorAll(selector)].filter(
                element => !element.hidden
                  && element.getClientRects().length > 0
                  && element.getAttribute('aria-hidden') !== 'true'
              );
              const describe = element => element?.id
                || element?.dataset.providerVendorDoc
                || (element?.innerText || '').trim().slice(0, 80);
              return {
                count: items.length,
                active_index: items.indexOf(document.activeElement),
                active: describe(document.activeElement),
                first: describe(items[0]),
                last: describe(items[items.length - 1]),
              };
            }"""
        )

    for index, (guide, panel, anchor) in enumerate(cases, start=113):
        select_settings_panel(page, panel)
        trigger = page.locator(f'[data-guide="{guide}"]')
        trigger.scroll_into_view_if_needed()
        trigger.click()
        settle(page, 30)
        must(
            page.locator("#guide-modal [role='dialog']").is_visible(),
            f"{guide} GuideSheet did not open",
        )
        initial_focus = guide_focus_state()
        must(
            initial_focus["count"] >= 2
            and initial_focus["active_index"] == initial_focus["count"] - 1,
            f"{guide} GuideSheet did not focus its actual last focusable: "
            f"{initial_focus}",
        )
        must(
            bool(page.locator("#guide-content").inner_text().strip()),
            f"{guide} GuideSheet has no guidance content",
        )
        page.keyboard.press("Tab")
        forward_focus = guide_focus_state()
        must(
            forward_focus["active_index"] == 0,
            f"{guide} GuideSheet did not wrap last→actual first: "
            f"{forward_focus}",
        )
        page.keyboard.press("Shift+Tab")
        backward_focus = guide_focus_state()
        must(
            backward_focus["active_index"] == backward_focus["count"] - 1,
            f"{guide} GuideSheet did not wrap first→actual last: "
            f"{backward_focus}",
        )
        shots.shot(page, f"{index:03d}-guide-{guide}-dark.png")

        page.keyboard.press("Escape")
        settle(page, 30)
        must(
            page.locator("#guide-modal").get_attribute("aria-hidden") == "true"
            and page.evaluate("document.activeElement.dataset.guide || ''")
            == guide,
            f"{guide} GuideSheet Escape did not close and restore trigger focus",
        )

        trigger.click()
        settle(page, 30)
        page.locator("#guide-modal").click(position={"x": 2, "y": 2})
        settle(page, 30)
        must(
            page.locator("#guide-modal").get_attribute("aria-hidden") == "true"
            and page.evaluate("document.activeElement.dataset.guide || ''")
            == guide,
            f"{guide} GuideSheet backdrop did not close and restore trigger focus",
        )

        trigger.click()
        settle(page, 30)
        page.click("#guide-action")
        settle(page, 30)
        must(
            page.locator("#guide-modal").get_attribute("aria-hidden") == "true",
            f"{guide} GuideSheet action did not close the sheet",
        )
        must(
            page.locator(f'[data-settings-target="{panel}"]').get_attribute(
                "aria-current"
            )
            == "page",
            f"{guide} GuideSheet action selected the wrong settings panel",
        )
        must(
            page.evaluate("document.activeElement.id") == anchor,
            f"{guide} GuideSheet action did not focus {anchor}",
        )
        verified[guide] = [
            "focus-trap",
            "escape",
            "backdrop",
            "action",
            "trigger-focus-restore",
        ]
    return verified


def verify_shell_save_and_detect(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    reset_page(page)
    page.click("#open-settings")
    select_settings_panel(page, "tools")
    page.select_option("#shell-strategy", "override")
    page.fill("#bash-shell-path", "D:\\tools\\git-bash\\bash.exe")
    page.click("#save-shell-settings")
    page.wait_for_function(
        "document.getElementById('shell-status-copy').textContent"
        ".includes('Gateway 已热更新')",
        timeout=3000,
    )
    must(
        page.locator("#shell-status-copy").is_visible(),
        "shell status copy disappeared after save",
    )
    shots.shot(page, "117-shell-saved-status-copy-dark.png")
    page.click("#detect-shell")
    must(
        page.locator("#shell-status-copy").is_visible()
        and "正在检查" in page.locator("#shell-status-copy").inner_text(),
        "shell detect could not run after save",
    )
    page.wait_for_function(
        "document.getElementById('shell-status-copy').textContent"
        ".startsWith('ready · dialect=pwsh · program=')",
        timeout=3000,
    )
    shell_status = page.locator("#shell-status-copy").inner_text()
    must(
        page.locator("#shell-status-copy").is_visible()
        and not page.locator("#detect-shell").is_disabled()
        and shell_status
        == "ready · dialect=pwsh · "
        "program=C:\\Program Files\\PowerShell\\7\\pwsh.exe"
        and page.input_value("#shell-actual-dialect") == "pwsh"
        and page.input_value("#shell-actual-program")
        == "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
        "second shell detection omitted the ready dialect/program receipt",
    )
    shots.shot(page, "118-shell-redetected-after-save-dark.png")
    return {
        "save_status_copy_persisted": True,
        "detect_reusable_after_save": True,
        "second_detect_receipt": shell_status,
    }


def verify_explicit_save_back_warnings(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    reset_page(page)
    cases = [
        ("image", "providers", "图片理解", "image-engine"),
        ("guardrails", "agents", "运行护栏", "budget-tool-rounds"),
        (
            "permission",
            "permissions",
            "Codex 全局权限",
            "codex-permission-mode",
        ),
        ("shell", "tools", "Shell 执行环境", "shell-strategy"),
        (
            "providerSync",
            "providers",
            "模型同步策略",
            "provider-sync-mode",
        ),
        (
            "projectPermission",
            "permissions",
            "项目 Agent 权限",
            "project-read-grouping",
        ),
        (
            "notificationRouting",
            "notifications",
            "后台通知分类",
            "notify-approval",
        ),
        (
            "diagnosticPolicy",
            "diagnostics",
            "诊断写入策略",
            "request-audit-toggle",
        ),
    ]

    def read_domain(domain: str) -> dict[str, object]:
        field_ids = {
            "image": ["image-engine", "vision-provider", "vision-model"],
            "guardrails": ["budget-tool-rounds"],
            "permission": ["codex-permission-mode"],
            "shell": ["shell-strategy", "bash-shell-path"],
            "providerSync": ["provider-sync-mode"],
            "projectPermission": ["project-read-grouping"],
            "notificationRouting": ["notify-approval"],
            "diagnosticPolicy": ["request-audit-toggle"],
        }[domain]
        values: dict[str, object] = {}
        for field_id in field_ids:
            field = page.locator(f"#{field_id}")
            values[field_id] = (
                field.is_checked()
                if field.get_attribute("type") == "checkbox"
                else field.input_value()
            )
        return values

    def mutate_domain(domain: str) -> None:
        if domain == "image":
            page.select_option("#image-engine", "model")
        elif domain == "providerSync":
            if page.locator("#provider-sync-policy").get_attribute("open") is None:
                page.locator("#provider-sync-policy summary").click()
            current = page.input_value("#provider-sync-mode")
            page.select_option(
                "#provider-sync-mode",
                "manual" if current != "manual" else "automatic",
            )
        elif domain == "guardrails":
            if page.locator("#run-budget-details").get_attribute("open") is None:
                page.locator("#run-budget-details summary").click()
            current = int(page.input_value("#budget-tool-rounds"))
            page.fill("#budget-tool-rounds", str(current + 1))
        elif domain == "permission":
            current = page.input_value("#codex-permission-mode")
            page.select_option(
                "#codex-permission-mode",
                "read_only" if current != "read_only" else "request_approval",
            )
        elif domain in {
            "projectPermission",
            "notificationRouting",
            "diagnosticPolicy",
        }:
            field_ids = {
                "projectPermission": "project-read-grouping",
                "notificationRouting": "notify-approval",
                "diagnosticPolicy": "request-audit-toggle",
            }
            checkbox = page.locator(f"#{field_ids[domain]}")
            if checkbox.is_checked():
                checkbox.uncheck()
            else:
                checkbox.check()
        else:
            current = page.input_value("#shell-strategy")
            page.select_option(
                "#shell-strategy",
                "fallback" if current != "fallback" else "auto",
            )

    def save_domain(domain: str) -> None:
        save_ids = {
            "image": "save-image-engine",
            "guardrails": "save-run-budget",
            "permission": "apply-codex-permissions",
            "shell": "save-shell-settings",
            "providerSync": "save-provider-sync",
            "projectPermission": "save-project-permission",
            "notificationRouting": "save-notification-routing",
            "diagnosticPolicy": "save-diagnostic-policy",
        }
        page.click(f"#{save_ids[domain]}")
        if domain == "shell":
            page.wait_for_function(
                "document.getElementById('shell-status-copy').textContent"
                ".includes('Gateway 已热更新')",
                timeout=3000,
            )

    verified: list[str] = []
    for index, (domain, panel, label, continue_focus) in enumerate(
        cases, start=119
    ):
        page.click("#open-settings")
        select_settings_panel(page, panel)
        original = read_domain(domain)
        mutate_domain(domain)
        mutated = read_domain(domain)
        must(mutated != original, f"{label} test mutation did not take effect")

        page.click("#settings-back")
        must(
            page.locator("#settings-unsaved-modal [role='alertdialog']").is_visible(),
            f"{label} dirty Back did not open the shared warning",
        )
        warning_copy = page.locator("#settings-unsaved-list").inner_text()
        must(
            label in warning_copy
            and page.locator("#settings-unsaved-list .settings-table-row").count()
            == 1,
            f"{label} dirty warning listed the wrong domains: {warning_copy}",
        )
        shots.shot(
            page,
            f"{index:03d}-dirty-back-warning-{domain}-dark.png",
        )
        page.click("#settings-unsaved-continue")
        settle(page, 30)
        must(
            page.locator("#settings-unsaved-modal").get_attribute("aria-hidden")
            == "true"
            and page.locator("#settings-scene").is_visible(),
            f"{label} Continue did not keep Settings open",
        )
        must(
            read_domain(domain) == mutated,
            f"{label} Continue did not preserve the draft",
        )
        must(
            page.evaluate("document.activeElement.id") == continue_focus,
            f"{label} Continue did not focus its explicit save surface",
        )

        page.click("#settings-back")
        must(
            page.locator("#settings-unsaved-modal [role='alertdialog']").is_visible(),
            f"{label} draft was lost after Continue",
        )
        page.click("#settings-unsaved-discard")
        settle(page, 30)
        must(
            page.locator("#settings-scene").is_hidden(),
            f"{label} discard did not restore and leave Settings",
        )
        page.click("#open-settings")
        select_settings_panel(page, panel)
        must(
            read_domain(domain) == original,
            f"{label} Discard did not restore the persisted snapshot",
        )
        mutate_domain(domain)
        save_domain(domain)
        page.click("#settings-back")
        settle(page, 30)
        must(
            page.locator("#settings-scene").is_hidden()
            and page.locator("#settings-unsaved-modal").get_attribute(
                "aria-hidden"
            )
            == "true",
            f"{label} Save did not clear the Back dirty gate",
        )
        verified.append(domain)

    page.click("#open-settings")
    select_settings_panel(page, "appearance")
    companion_sound = page.locator("#companion-sound")
    companion_sound_original = companion_sound.is_checked()
    companion_sound.click()
    companion_sound_mutated = companion_sound.is_checked()
    must(
        companion_sound_mutated != companion_sound_original
        and "immediate revision"
        in page.locator("#companion-status").inner_text(),
        "Companion sound did not persist as an immediate revision mutation",
    )
    page.click("#settings-back")
    settle(page, 30)
    must(
        page.locator("#settings-scene").is_hidden()
        and page.locator("#settings-unsaved-modal").get_attribute("aria-hidden")
        == "true",
        "Companion sound incorrectly entered the explicit-save dirty Back gate",
    )
    page.click("#open-settings")
    select_settings_panel(page, "appearance")
    must(
        companion_sound.is_checked() == companion_sound_mutated,
        "Companion sound immediate mutation did not survive Settings reopen",
    )
    companion_sound.click()
    must(
        companion_sound.is_checked() == companion_sound_original,
        "Companion sound could not restore its original immediate value",
    )
    page.click("#settings-back")
    settle(page, 30)
    must(
        page.locator("#settings-scene").is_hidden()
        and page.locator("#settings-unsaved-modal").get_attribute("aria-hidden")
        == "true",
        "Restoring Companion sound incorrectly opened the dirty Back gate",
    )
    return {
        "domains": verified,
        "domain_count": len(verified),
        "shared_alertdialog": True,
        "continue_preserves": True,
        "discard_restores": True,
        "save_clears_dirty": True,
        "companion_sound_is_immediate": True,
        "companion_sound_dirty_back": False,
    }


def verify_knowledge_scope_contract(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    reset_page(page)
    page.click("#open-settings")
    select_settings_panel(page, "knowledge")
    page.click('[data-knowledge-tab="prompt"]')

    def active_mode() -> str:
        return (
            page.locator(
                '[data-choice-group="prompt-mode"][aria-pressed="true"]'
            ).get_attribute("data-choice")
            or ""
        )

    page.select_option("#knowledge-scope", "project-r-code")
    must(active_mode() == "append", "project-r-code did not load append mode")
    project_original = page.input_value("#knowledge-prompt")
    project_subagent_original = page.input_value("#knowledge-subagent-prompt")
    must(
        page.locator("#memory-engine-block").get_attribute("hidden") is not None
        and page.locator("#memory-review-block").get_attribute("hidden") is not None
        and page.locator("#prompt-mode-control").get_attribute("hidden") is None,
        "project knowledge scope did not hide global memory and show Prompt mode",
    )
    must(
        page.locator('[data-memory-item="2"] button').first.is_disabled()
        and "全局继承"
        in page.locator('[data-memory-item="2"] span small').inner_text(),
        "project knowledge scope did not make inherited global memory read-only",
    )

    page.select_option("#knowledge-scope", "project-falib")
    must(
        active_mode() == "override",
        "project-falib did not load its independent override mode",
    )
    page.select_option("#knowledge-scope", "global")
    must(
        page.locator("#prompt-mode-control").get_attribute("hidden") is not None,
        "global knowledge scope exposed the project-only Prompt mode control",
    )
    page.select_option("#knowledge-scope", "project-r-code")
    must(
        active_mode() == "append",
        "project-r-code mode was overwritten by falib/global scope switches",
    )
    page.select_option("#knowledge-scope", "project-falib")
    must(
        active_mode() == "override",
        "project-falib mode was overwritten by another scope",
    )
    page.select_option("#knowledge-scope", "project-r-code")

    project_draft = "project-r-code 独立主 Agent 草稿"
    project_subagent_draft = "project-r-code 独立子代理草稿"
    page.fill("#knowledge-prompt", project_draft)
    page.fill("#knowledge-subagent-prompt", project_subagent_draft)
    page.select_option("#knowledge-scope", "global")
    global_original = page.input_value("#knowledge-prompt")
    global_subagent_original = page.input_value("#knowledge-subagent-prompt")
    must(
        project_draft not in global_original
        and project_subagent_draft not in global_subagent_original,
        "project knowledge drafts leaked into global scope",
    )
    must(
        page.locator("#memory-engine-block").get_attribute("hidden") is None
        and page.locator("#memory-review-block").get_attribute("hidden") is None
        and page.locator("#prompt-mode-control").get_attribute("hidden") is not None,
        "global knowledge scope did not restore global-only memory controls",
    )
    must(
        not page.locator('[data-memory-item="2"] button').first.is_disabled(),
        "global knowledge scope kept its own memory row read-only",
    )
    global_draft = "global 独立主 Agent 草稿"
    global_subagent_draft = "global 独立子代理草稿"
    page.fill("#knowledge-prompt", global_draft)
    page.fill("#knowledge-subagent-prompt", global_subagent_draft)
    shots.shot(page, "124-knowledge-global-scope-dark.png")

    page.select_option("#knowledge-scope", "project-r-code")
    must(
        page.input_value("#knowledge-prompt") == project_draft
        and page.input_value("#knowledge-subagent-prompt")
        == project_subagent_draft,
        "knowledge scope switch did not restore the project draft",
    )
    shots.shot(page, "125-knowledge-project-scope-isolated-dark.png")
    page.click("#reload-knowledge-prompt")
    must(
        page.input_value("#knowledge-prompt") == project_original
        and page.input_value("#knowledge-subagent-prompt")
        == project_subagent_original,
        "project knowledge reload did not restore its Host snapshot",
    )
    page.select_option("#knowledge-scope", "global")
    must(
        page.input_value("#knowledge-prompt") == global_draft
        and page.input_value("#knowledge-subagent-prompt")
        == global_subagent_draft,
        "project reload overwrote the global knowledge draft",
    )
    page.click("#reload-knowledge-prompt")
    must(
        page.input_value("#knowledge-prompt") == global_original
        and page.input_value("#knowledge-subagent-prompt")
        == global_subagent_original,
        "global knowledge reload did not restore its own Host snapshot",
    )
    page.select_option("#knowledge-scope", "project-r-code")

    page.click('[data-choice-group="prompt-mode"][data-choice="override"]')
    must(
        active_mode() == "override"
        and "未保存草稿" in page.locator("#knowledge-prompt-status").inner_text(),
        "Prompt mode change did not mark project-r-code dirty",
    )
    page.click("#save-knowledge")
    must(
        active_mode() == "override"
        and "已同步" in page.locator("#knowledge-prompt-status").inner_text(),
        "Prompt mode save did not persist and clear dirty state",
    )
    page.click('[data-choice-group="prompt-mode"][data-choice="append"]')
    must(
        "未保存草稿" in page.locator("#knowledge-prompt-status").inner_text(),
        "temporary Prompt mode change did not become dirty",
    )
    page.click("#reload-knowledge-prompt")
    must(
        active_mode() == "override"
        and "已同步" in page.locator("#knowledge-prompt-status").inner_text(),
        "Prompt mode reload did not restore persisted override mode",
    )

    page.click('[data-choice-group="prompt-mode"][data-choice="append"]')
    must(
        "未保存草稿" in page.locator("#knowledge-prompt-status").inner_text(),
        "Prompt mode did not become dirty before manual restoration",
    )
    page.click('[data-choice-group="prompt-mode"][data-choice="override"]')
    must(
        "已同步" in page.locator("#knowledge-prompt-status").inner_text(),
        "manually returning to persisted Prompt mode did not clear dirty state",
    )

    removal_main = page.input_value("#knowledge-prompt")
    removal_subagent = page.input_value("#knowledge-subagent-prompt")
    page.click("#remove-project-prompt")
    page.click("#confirm-remove-project-prompt")
    must(
        active_mode() == "override"
        and page.input_value("#knowledge-prompt") == ""
        and page.input_value("#knowledge-subagent-prompt") == "",
        "removing project Prompt did not preserve its Prompt mode",
    )
    page.click("#undo-remove-project-prompt")
    must(
        active_mode() == "override"
        and page.input_value("#knowledge-prompt") == removal_main
        and page.input_value("#knowledge-subagent-prompt") == removal_subagent,
        "undoing project Prompt removal did not restore content and mode",
    )
    must(
        "已同步" in page.locator("#knowledge-prompt-status").inner_text(),
        "undoing project Prompt removal did not clear dirty state",
    )
    shots.shot(page, "125b-knowledge-mode-lifecycle-dark.png")
    return {
        "scopes": ["project-r-code", "project-falib", "global"],
        "draft_isolation": True,
        "scope_reload_isolated": True,
        "inherited_rows_read_only": True,
        "prompt_modes": {
            "project-r-code_initial": "append",
            "project-falib_initial": "override",
            "global_control": "hidden",
            "save_reload": True,
            "remove_undo_preserved": True,
            "manual_return_cleared_dirty": True,
        },
    }


def verify_mobile_touch_targets(
    page: Page, shots: CaptureLog
) -> dict[str, object]:
    reset_page(page, width=390, height=844)
    page.click("#open-settings")
    settings_selector = (
        "#settings-scene button:not([disabled]), "
        "#settings-scene input:not([disabled]):not([type=checkbox])"
        ":not([type=radio]):not([readonly]), "
        "#settings-scene select:not([disabled]), "
        "#settings-scene textarea:not([disabled]), "
        "#settings-scene details > summary"
    )
    failures: list[dict[str, object]] = []
    measured = 0
    minimum_width = float("inf")
    minimum_height = float("inf")

    def measure_controls(selector: str, context: str) -> None:
        nonlocal measured, minimum_width, minimum_height
        targets = page.locator(selector)
        for index in range(targets.count()):
            target = targets.nth(index)
            if not target.is_visible():
                continue
            box = target.bounding_box()
            if box is None:
                continue
            measured += 1
            minimum_width = min(minimum_width, box["width"])
            minimum_height = min(minimum_height, box["height"])
            if box["width"] + 0.01 < 44 or box["height"] + 0.01 < 44:
                identity = target.evaluate(
                    """element => ({
                      panel: element.closest('[data-settings-panel]')?.dataset.settingsPanel || 'shell',
                      tag: element.tagName.toLowerCase(),
                      id: element.id,
                      text: (element.innerText || element.getAttribute('aria-label') || element.placeholder || '').trim().slice(0, 80)
                    })"""
                )
                failures.append({"context": context, **identity, **box})

    def measure_checkbox_labels(selector: str, context: str) -> None:
        nonlocal measured, minimum_width, minimum_height
        checkbox_labels = page.locator(selector)
        for index in range(checkbox_labels.count()):
            label = checkbox_labels.nth(index)
            if not label.is_visible():
                continue
            box = label.bounding_box()
            if box is None:
                continue
            measured += 1
            minimum_width = min(minimum_width, box["width"])
            minimum_height = min(minimum_height, box["height"])
            if box["width"] + 0.01 < 44 or box["height"] + 0.01 < 44:
                failures.append(
                    {
                        "context": context,
                        "panel": context,
                        "tag": "label[checkbox]",
                        "id": label.locator("input").first.get_attribute("id") or "",
                        "text": label.inner_text().strip()[:80],
                        **box,
                    }
                )

    knowledge_tabs_measured: list[str] = []
    for panel, _ in SETTINGS_PANELS:
        select_settings_panel(page, panel)
        details = page.locator(
            f'[data-settings-panel="{panel}"] details.settings-detail'
        )
        for index in range(details.count()):
            detail = details.nth(index)
            if detail.is_visible() and detail.get_attribute("open") is None:
                detail.locator("summary").click()
        assert_no_overflow(page, f"390x844-settings-{panel}")
        measure_controls(settings_selector, panel)
        measure_checkbox_labels(
            "#settings-scene label:has(input[type=checkbox]:not([disabled]))",
            panel,
        )
        if panel == "knowledge":
            for tab in ("prompt", "skills"):
                page.click(f'[data-knowledge-tab="{tab}"]')
                assert_no_overflow(page, f"390x844-knowledge-{tab}")
                measure_controls(settings_selector, f"knowledge-{tab}")
                measure_checkbox_labels(
                    "#settings-scene label:has(input[type=checkbox]:not([disabled]))",
                    f"knowledge-{tab}",
                )
                knowledge_tabs_measured.append(tab)

    select_settings_panel(page, "providers")
    page.click("#open-capability-map")
    settle(page, 240)
    must(
        page.locator("#capability-map-modal [role='dialog']").is_visible(),
        "390px capability coverage modal did not open",
    )
    assert_in_viewport(
        page,
        "#capability-map-modal .dialog",
        "390px capability coverage modal",
    )
    measure_controls(
        "#capability-map-modal button:not([disabled]), "
        "#capability-map-modal input:not([disabled]):not([readonly]), "
        "#capability-map-modal select:not([disabled]), "
        "#capability-map-modal textarea:not([disabled])",
        "capability-modal",
    )
    shots.shot(page, "126a-settings-coverage-modal-390-dark.png")
    page.click("#capability-map-close")
    shots.shot(page, "126-settings-touch-targets-390-dark.png")
    must(measured > 0, "390px touch-target audit measured no controls")
    must(not failures, f"390px touch targets below 44x44: {failures}")
    return {
        "viewport": "390x844",
        "panels": len(SETTINGS_PANELS),
        "targets_measured": measured,
        "minimum_width": round(minimum_width, 2),
        "minimum_height": round(minimum_height, 2),
        "threshold": "44x44",
        "expanded_details": True,
        "knowledge_tabs": ["memory", *knowledge_tabs_measured],
        "modal": "capability-map",
    }


def run_desktop_demo(
    browser: Browser,
    errors: list[str],
    shots: CaptureLog,
) -> dict[str, object]:
    page = browser.new_page(
        viewport={"width": 1600, "height": 960},
        device_scale_factor=1,
    )
    page.set_default_timeout(5000)
    attach_diagnostics(page, errors)
    evidence: dict[str, object] = {}
    evidence["provider_startup"] = verify_startup_health(page, shots)
    evidence["run_semantics"] = verify_running_and_terminal_states(
        page, shots
    )
    evidence["tasks"] = verify_tasks_and_drafts(page, shots)
    evidence["selectors_and_project"] = verify_selectors_and_project(
        page, shots
    )
    evidence["subagents_and_diff"] = verify_subagents_and_diff(page, shots)
    evidence["provider_recovery"] = verify_provider_credential_recovery(
        page, shots
    )
    evidence["window_lifecycle"] = verify_window_lifecycle(page, shots)
    evidence["settings_navigation"] = verify_settings_navigation_and_search(
        page, shots
    )
    evidence["settings_states"] = verify_settings_state_machines(
        page, shots
    )
    evidence["settings_capability_depth"] = verify_settings_capability_depth(
        page, shots
    )
    evidence["light_theme"] = capture_light_key_pages(page, shots)
    evidence["responsive"] = verify_responsive(page, shots)
    evidence["scale_and_motion"] = verify_scale_and_reduced_motion(
        browser, errors, shots
    )
    evidence["completeness_pages"] = capture_completeness_pages(
        page, shots
    )
    evidence["keyboard"] = verify_keyboard_contracts(page)
    evidence["settings_load"] = verify_settings_load_contract(page, shots)
    evidence["provider_and_codex_depth"] = (
        verify_provider_and_codex_catalog_contract(page, shots)
    )
    evidence["subagent_rtk_mcp_depth"] = verify_subagent_rtk_and_mcp_depth(
        page, shots
    )
    evidence["memory_skills_companion_depth"] = (
        verify_memory_skills_and_companion_depth(page, shots)
    )
    evidence["latest_production_contracts"] = (
        verify_latest_production_contracts(page, shots)
    )
    evidence["mcp_approval"] = verify_mcp_approval_contract(page, shots)
    evidence["subagent_prompts"] = verify_subagent_prompt_contract(
        page, shots
    )
    evidence["guide_sheets"] = verify_guide_sheet_contracts(page, shots)
    evidence["shell_save_detect"] = verify_shell_save_and_detect(
        page, shots
    )
    evidence["explicit_save_back_warnings"] = (
        verify_explicit_save_back_warnings(page, shots)
    )
    evidence["knowledge_scope"] = verify_knowledge_scope_contract(
        page, shots
    )
    evidence["mobile_touch_targets"] = verify_mobile_touch_targets(
        page, shots
    )
    page.close()
    return evidence


def main() -> None:
    OUTPUT.mkdir(parents=True, exist_ok=True)
    prototype_sha256 = hashlib.sha256(HTML.read_bytes()).hexdigest()
    manifest_path = OUTPUT / "capture-manifest.json"
    errors: list[str] = []
    shots = CaptureLog()
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        try:
            evidence = run_desktop_demo(browser, errors, shots)
        except Exception:
            failure_page = next(
                (
                    page
                    for context in browser.contexts
                    for page in context.pages
                    if not page.is_closed()
                ),
                None,
            )
            if failure_page is not None:
                try:
                    failure_page.screenshot(
                        path=OUTPUT / "zz-capture-failure.png",
                        caret="hide",
                    )
                except Exception:
                    pass
            raise
        finally:
            browser.close()

    diagnostic_counts = {
        "console_warning": sum(
            item.startswith("console:warning:") for item in errors
        ),
        "console_error": sum(
            item.startswith("console:error:") for item in errors
        ),
        "pageerror": sum(item.startswith("pageerror:") for item in errors),
        "requestfailed": sum(
            item.startswith("requestfailed:") for item in errors
        ),
    }
    if errors:
        raise RuntimeError("Browser diagnostics:\n" + "\n".join(errors))
    must(
        len(shots.names) >= 100,
        f"expected at least 100 generated screenshots, got {len(shots.names)}",
    )
    missing_or_empty = [
        name
        for name in shots.names
        if not (OUTPUT / name).is_file()
        or (OUTPUT / name).stat().st_size == 0
    ]
    must(
        not missing_or_empty,
        f"generated screenshot files are missing or empty: {missing_or_empty}",
    )
    generated = set(shots.names)
    orphan_pngs = sorted(
        path
        for path in OUTPUT.rglob("*.png")
        if path.relative_to(OUTPUT).as_posix() not in generated
    )
    deleted_orphan_pngs = [
        path.relative_to(OUTPUT).as_posix() for path in orphan_pngs
    ]
    for path in orphan_pngs:
        path.unlink()
    must(
        not (OUTPUT / "zz-capture-failure.png").exists(),
        "failure screenshot remained after successful orphan cleanup",
    )
    previous_deleted_orphans: list[str] = []
    if manifest_path.is_file():
        try:
            previous_manifest = json.loads(
                manifest_path.read_text(encoding="utf-8")
            )
            if previous_manifest.get("prototype_sha256") == prototype_sha256:
                previous_deleted_orphans = [
                    str(name)
                    for name in previous_manifest.get(
                        "deleted_orphan_pngs", []
                    )
                ]
        except (OSError, json.JSONDecodeError, TypeError):
            previous_deleted_orphans = []
    deleted_orphan_pngs = sorted(
        set(previous_deleted_orphans) | set(deleted_orphan_pngs)
    )
    manifest = {
        "status": "passed",
        "artifact": str(HTML),
        "prototype_sha256": prototype_sha256,
        "viewports": [
            {"width": width, "height": height} for width, height in VIEWPORTS
        ],
        "evidence": evidence,
        "generated_screenshots": len(shots.names),
        "screenshots": shots.names,
        "deleted_orphan_pngs": deleted_orphan_pngs,
        "diagnostics": diagnostic_counts,
    }
    manifest_path.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    print(json.dumps(manifest, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        print(exc, file=sys.stderr)
        raise
