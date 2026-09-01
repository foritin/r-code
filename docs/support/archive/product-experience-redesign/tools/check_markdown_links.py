#!/usr/bin/env python3
"""Check repository-local Markdown links after documentation moves."""

from __future__ import annotations

import json
import re
from pathlib import Path
from urllib.parse import unquote


LINK = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")


def main() -> int:
    repo = Path(__file__).resolve().parents[3]
    documents = sorted((repo / "docs").rglob("*.md"))
    documents.extend(
        path
        for name in (
            "README.md",
            "README.zh-CN.md",
            "SUPPORT.md",
            "PRIVACY.md",
            "SECURITY.md",
            "CONTRIBUTING.md",
        )
        if (path := repo / name).exists()
    )
    broken: list[dict[str, object]] = []
    checked = 0
    for document in documents:
        for line_number, line in enumerate(document.read_text(encoding="utf-8").splitlines(), 1):
            for match in LINK.finditer(line):
                raw = match.group(1).strip()
                target = raw[1:-1] if raw.startswith("<") and raw.endswith(">") else raw
                target = target.split(maxsplit=1)[0]
                if target.startswith(("http://", "https://", "mailto:", "#", "data:")):
                    continue
                target = unquote(target.split("#", 1)[0].split("?", 1)[0])
                if not target or any(token in target for token in ("*", "${", "<", ">")):
                    continue
                checked += 1
                resolved = (document.parent / target).resolve()
                if not resolved.exists():
                    broken.append(
                        {
                            "file": document.relative_to(repo).as_posix(),
                            "line": line_number,
                            "target": raw,
                            "resolved": str(resolved),
                        }
                    )
    report = {"schema_version": "markdown-link-check.v1", "checked": checked, "broken": broken}
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 1 if broken else 0


if __name__ == "__main__":
    raise SystemExit(main())
