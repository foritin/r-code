from __future__ import annotations

from pathlib import Path
import json
import re
import sys

from playwright.sync_api import sync_playwright


ROOT = Path(__file__).resolve().parents[1]
HTML = ROOT / "prototype.html"
CJK = re.compile(r"[\u3400-\u9fff]")
MISSING = "⟦missing en-US copy⟧"


def extract_string_literals(line: str) -> list[str]:
    values: list[str] = []
    index = 0
    while index < len(line):
        quote = line[index]
        if quote not in {"'", '"', "`"}:
            index += 1
            continue
        index += 1
        value: list[str] = []
        escaped = False
        while index < len(line):
            char = line[index]
            if escaped:
                value.extend(("\\", char))
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                break
            else:
                value.append(char)
            index += 1
        values.append("".join(value))
        index += 1
    return values


def show_toast_calls(source: str) -> list[tuple[int, str]]:
    calls: list[tuple[int, str]] = []
    cursor = 0
    marker = "showToast("
    while (start := source.find(marker, cursor)) >= 0:
        index = start + len(marker)
        depth = 1
        quote = ""
        escaped = False
        while index < len(source) and depth:
            char = source[index]
            if quote:
                if escaped:
                    escaped = False
                elif char == "\\":
                    escaped = True
                elif char == quote:
                    quote = ""
            elif char in {"'", '"', "`"}:
                quote = char
            elif char == "(":
                depth += 1
            elif char == ")":
                depth -= 1
            index += 1
        calls.append((source.count("\n", 0, start) + 1, source[start:index]))
        cursor = index
    return calls


def split_top_level_arguments(call: str) -> list[str]:
    inner = call[call.find("(") + 1 : call.rfind(")")]
    arguments: list[str] = []
    start = 0
    quote = ""
    escaped = False
    depths = {"(": 0, "[": 0, "{": 0}
    closing = {")": "(", "]": "[", "}": "{"}
    for index, char in enumerate(inner):
        if quote:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = ""
            continue
        if char in {"'", '"', "`"}:
            quote = char
        elif char in depths:
            depths[char] += 1
        elif char in closing:
            depths[closing[char]] = max(0, depths[closing[char]] - 1)
        elif char == "," and not any(depths.values()):
            arguments.append(inner[start:index].strip())
            start = index + 1
    arguments.append(inner[start:].strip())
    return arguments


def toast_fragments(source: str) -> list[dict[str, object]]:
    fragments: list[dict[str, object]] = []
    for line_number, call in show_toast_calls(source):
        arguments = split_top_level_arguments(call)
        if len(arguments) >= 2 and arguments[1] and not CJK.search(arguments[1]):
            continue
        for value in extract_string_literals(call):
            if not CJK.search(value):
                continue
            normalized = re.sub(r"\$\{[^{}]*\}", "⟪value⟫", value)
            fragments.append(
                {"line": line_number, "source": normalized, "raw": value}
            )
    return fragments


def main() -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    source = HTML.read_text(encoding="utf-8")
    fragments = toast_fragments(source)
    errors: list[str] = []
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        page = browser.new_page(viewport={"width": 1600, "height": 960})
        page.on(
            "console",
            lambda message: errors.append(f"console:{message.type}:{message.text}")
            if message.type in {"warning", "error"}
            else None,
        )
        page.on("pageerror", lambda error: errors.append(f"pageerror:{error}"))
        page.on(
            "requestfailed",
            lambda request: errors.append(f"requestfailed:{request.url}"),
        )
        page.goto(HTML.as_uri(), wait_until="load")
        page.wait_for_function("window.__ready === true")
        page.wait_for_function(
            "document.querySelector('#health-trigger')?.getAttribute('aria-label') !== '连接健康：正在后台检查'"
        )
        page.click("#open-settings")
        page.wait_for_selector("#settings-scene", state="visible")
        page.click('[data-settings-target="appearance"]')
        page.wait_for_selector('[data-settings-panel="appearance"]', state="visible")

        snapshot = page.evaluate(
            """() => ({
              text: (() => {
                const rows = [];
                const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
                let node;
                while ((node = walker.nextNode())) {
                  if (!node.parentElement?.closest('script, style, #toast')) rows.push(node.nodeValue);
                }
                return rows;
              })(),
              attrs: [...document.body.querySelectorAll('*')].filter(element => !element.closest('#toast')).flatMap(element =>
                ['placeholder', 'title', 'aria-label', 'alt']
                  .filter(name => element.hasAttribute(name))
                  .map(name => [name, element.getAttribute(name)])
              )
            })"""
        )
        page.select_option("#interface-language", "en-US")
        residual_dom = page.evaluate(
            """missing => {
              const rows = [];
              const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
              let node;
              while ((node = walker.nextNode())) {
                if (node.parentElement?.closest('script, style')) continue;
                const value = node.nodeValue || '';
                if (/[\u3400-\u9fff]/.test(value) || value.includes(missing)) {
                  rows.push({ kind: 'text', value: value.trim(), tag: node.parentElement?.tagName });
                }
              }
              for (const element of document.body.querySelectorAll('*')) {
                for (const name of ['placeholder', 'title', 'aria-label', 'alt']) {
                  const value = element.getAttribute(name) || '';
                  if (/[\u3400-\u9fff]/.test(value) || value.includes(missing)) {
                    rows.push({ kind: name, value, tag: element.tagName, id: element.id });
                  }
                }
              }
              return rows;
            }""",
            MISSING,
        )
        page.evaluate(
            """values => {
              const probe = document.createElement('div');
              probe.id = 'locale-audit-probe';
              probe.hidden = true;
              probe.setAttribute('aria-hidden', 'true');
              values.forEach((value, index) => {
                const row = document.createElement('span');
                row.dataset.index = String(index);
                row.textContent = value;
                probe.append(row);
              });
              document.body.append(probe);
            }""",
            [fragment["source"] for fragment in fragments],
        )
        page.wait_for_timeout(20)
        translated = page.locator("#locale-audit-probe > span").all_text_contents()
        page.locator("#locale-audit-probe").evaluate("element => element.remove()")
        residual_toasts = [
            {**fragment, "translated": result}
            for fragment, result in zip(fragments, translated, strict=True)
            if CJK.search(result) or MISSING in result
        ]

        page.select_option("#interface-language", "zh-CN")
        restored = page.evaluate(
            """() => ({
              text: (() => {
                const rows = [];
                const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
                let node;
                while ((node = walker.nextNode())) {
                  if (!node.parentElement?.closest('script, style, #toast')) rows.push(node.nodeValue);
                }
                return rows;
              })(),
              attrs: [...document.body.querySelectorAll('*')].filter(element => !element.closest('#toast')).flatMap(element =>
                ['placeholder', 'title', 'aria-label', 'alt']
                  .filter(name => element.hasAttribute(name))
                  .map(name => [name, element.getAttribute(name)])
              )
            })"""
        )
        browser.close()

    restoration_diff = {
        "text": [
            {"index": index, "before": before, "after": after}
            for index, (before, after) in enumerate(
                zip(snapshot["text"], restored["text"], strict=False)
            )
            if before != after
        ][:20],
        "attrs": [
            {"index": index, "before": before, "after": after}
            for index, (before, after) in enumerate(
                zip(snapshot["attrs"], restored["attrs"], strict=False)
            )
            if before != after
        ][:20],
        "text_count_before": len(snapshot["text"]),
        "text_count_after": len(restored["text"]),
        "attr_count_before": len(snapshot["attrs"]),
        "attr_count_after": len(restored["attrs"]),
    }

    report = {
        "status": "passed"
        if not residual_dom
        and not residual_toasts
        and snapshot == restored
        and not errors
        else "failed",
        "static_dom_residual_count": len(residual_dom),
        "static_dom_residual": residual_dom,
        "toast_fragment_count": len(fragments),
        "toast_residual_count": len(residual_toasts),
        "toast_residual": residual_toasts,
        "zh_cn_exactly_restored": snapshot == restored,
        "restoration_diff": restoration_diff if snapshot != restored else {},
        "browser_errors": errors,
    }
    print(json.dumps(report, ensure_ascii=False, indent=2))
    if report["status"] != "passed":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
