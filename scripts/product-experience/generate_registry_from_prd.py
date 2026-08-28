#!/usr/bin/env python3
r"""从 PRD §11 任务卡机械提取断言注册表（M0-01 实施步骤 2）。

只做确定性提取，不手工维护第二套枚举：
- 42 张任务卡：id、title、milestone、requirement_refs、depends_on、assertions
- 断言：`验收断言:` 段内 `  - \`<ID>\`（<level>）：<text>`，全仓库唯一
- 勾选状态从 §10 `- [x] **<ID>**` 读取，写入 baseline_done
输出 scripts/product-experience/registry.generated.json；同输入必同输出
（无时间戳），供 worklist_gate 的 checkout-stable 要求复用。

运行：python3 scripts/product-experience/generate_registry_from_prd.py [--check]
"""
from __future__ import annotations

import hashlib
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
PRD = ROOT / "docs/product-experience-redesign/r-code-experience-redesign-prd.md"
OUT = ROOT / "scripts/product-experience/registry.generated.json"

CARD_RE = re.compile(r"^### ([A-Z0-9]+-\d+) (.+)$", re.M)
REF_RE = re.compile(r"^- 需求引用：(.*)$", re.M)
DEP_RE = re.compile(r"^- 依赖：(.*)$", re.M)
ASSERT_RE = re.compile(r"^  - `([A-Z0-9-]+\.[A-Za-z0-9]+)`（([\w/]+)）：(.*)$", re.M)
CHECK_RE = re.compile(r"^- \[([ x])\] \*\*([A-Z0-9]+-\d+)\*\*", re.M)
SECTION_SPLIT = "\n## 11. 详细任务卡"
NEXT_SECTION = "\n## 12."


def split_refs(raw: str) -> list[str]:
    parts = [p.strip().rstrip("。") for p in re.split("[、,，;；]", raw) if p.strip()]
    return [p for p in parts if p]


def split_deps(raw: str) -> list[str]:
    s = raw.strip().rstrip("。").strip()
    if not s or s == "无":
        return []
    return [p.strip() for p in re.split("[、,，;；]", s) if p.strip()]


def main() -> int:
    text = PRD.read_text(encoding="utf-8")
    if SECTION_SPLIT not in text:
        print(f"ERROR: {SECTION_SPLIT.strip()!r} not found", file=sys.stderr)
        return 1
    section = text.split(SECTION_SPLIT, 1)[1].split(NEXT_SECTION, 1)[0]

    cards = list(CARD_RE.finditer(section))
    tasks: dict[str, dict] = {}
    assertion_ids: set[str] = set()
    issues: list[str] = []

    for i, m in enumerate(cards):
        tid, title = m.group(1), m.group(2).strip()
        end = cards[i + 1].start() if i + 1 < len(cards) else len(section)
        body = section[m.start():end]
        card_ids = ASSERT_RE.findall(body)
        if not card_ids:
            issues.append(f"{tid}: no assertions parsed")
        for aid, level, desc in card_ids:
            if not aid.startswith(tid + "."):
                issues.append(f"{tid}: foreign assertion id {aid}")
            if aid in assertion_ids:
                issues.append(f"{tid}: duplicate assertion id {aid}")
            assertion_ids.add(aid)
        deps_raw = DEP_RE.search(body)
        refs_raw = REF_RE.search(body)
        milestone = tid.rsplit("-", 1)[0]
        tasks[tid] = {
            "title": title,
            "milestone": milestone,
            "requirement_refs": split_refs(refs_raw.group(1)) if refs_raw else [],
            "depends_on": split_deps(deps_raw.group(1)) if deps_raw else [],
            "assertions": [
                {"id": a[0], "level": a[1], "summary": a[2].strip()} for a in card_ids
            ],
        }

    # 依赖引用的任务必须存在；D0 无依赖
    for tid, t in tasks.items():
        for d in t["depends_on"]:
            if d not in tasks:
                issues.append(f"{tid}: unknown dependency {d}")

    done = {
        c.group(2)
        for c in CHECK_RE.finditer(text[: text.index(SECTION_SPLIT)])
        if c.group(1) == "x"
    }
    unknown_done = sorted(done - set(tasks))
    if unknown_done:
        issues.append(f"§10 checked but no task card: {unknown_done}")

    payload = {
        "schema_version": "product-experience-registry.v1",
        "source_document": str(PRD.relative_to(ROOT)).replace("\\", "/"),
        "source_sha256": hashlib.sha256(PRD.read_bytes()).hexdigest(),
        "task_count": len(tasks),
        "assertion_count": len(assertion_ids),
        "baseline_done": sorted(done),
        "tasks": dict(sorted(tasks.items())),
    }

    if "--check" in sys.argv:
        prev = json.loads(OUT.read_text(encoding="utf-8"))
        prev.pop("source_sha256", None)
        cur = json.loads(json.dumps(payload))
        cur.pop("source_sha256", None)
        if prev != cur:
            print("ERROR: registry.generated.json 与 PRD 不一致，请重新生成", file=sys.stderr)
            return 1
        print(
            f"registry ok: {payload['task_count']} tasks, "
            f"{payload['assertion_count']} assertions, done={len(payload['baseline_done'])}"
        )
        return 0

    OUT.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(
        f"wrote {OUT.relative_to(ROOT)}: {payload['task_count']} tasks, "
        f"{payload['assertion_count']} assertions, baseline_done={sorted(done)}"
    )
    if issues:
        print("\n".join(f"  - {i}" for i in issues), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
