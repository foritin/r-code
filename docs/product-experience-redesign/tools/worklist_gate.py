#!/usr/bin/env python3
"""Deterministic integrity gate for the product-experience AI worklist.

The script intentionally uses only the Python standard library. It validates the
one-checklist/one-card contract, requirement and dependency references, required
task-card fields, unique assertion IDs, and writes reproducible SHA-256 digests.
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import importlib.util
import io
import json
import re
import sys
import tempfile
from pathlib import Path
from typing import Iterable


WORKLIST_ID = "product-experience-gap-closure"
TASK_RE = r"(?:D0|M[0-9])-[0-9]{2}"
REQ_RE = r"R-[A-Z0-9]+-[0-9]{2}"
NORM_START = "<!-- AI_WORKLIST_NORMATIVE_START -->"
NORM_END = "<!-- AI_WORKLIST_NORMATIVE_END -->"
CONTRACT_START = "<!-- AI_WORKLIST_CONTRACT_START -->"
CONTRACT_END = "<!-- AI_WORKLIST_CONTRACT_END -->"
REQUIRED_FIELDS = (
    "结果",
    "需求引用",
    "依赖",
    "前置事实",
    "固定约束",
    "决策空间",
    "产物",
    "实施步骤",
    "验收断言",
    "验证",
    "证据",
    "失败处理",
)

SETTINGS_REPORT_SCHEMAS = {
    "settings-capability-gate.v1",
    "settings-capability-gate.v2",
    "settings-capability-gate.v3",
}


def between(text: str, start: str, end: str, issues: list[dict]) -> str:
    if text.count(start) != 1 or text.count(end) != 1:
        issues.append(
            {
                "severity": "blocking",
                "code": "marker_count",
                "message": f"expected exactly one {start} and {end}",
            }
        )
        return ""
    start_at = text.index(start) + len(start)
    end_at = text.index(end, start_at)
    return text[start_at:end_at]


def normalize(value: str, *, worklist: bool) -> str:
    value = value.replace("\r\n", "\n").replace("\r", "\n")
    lines = [line.rstrip() for line in value.split("\n")]
    if worklist:
        # Checkbox state is deliberately volatile; task IDs and wording are not.
        lines = [re.sub(r"^- \[[xX ]\]", "- [ ]", line) for line in lines]
        # Checklist evidence destinations are filled as tasks complete.
        lines = [
            re.sub(r"(证据：)(?!待生成).*$", r"\1<volatile>", line)
            if re.match(rf"^- \[ \] \*\*{TASK_RE}\*\*", line)
            else line
            for line in lines
        ]
    return "\n".join(lines).strip() + "\n"


def digest(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def canonical_json(value: object) -> str:
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    )


def write_if_changed(path: Path, value: str) -> None:
    if path.is_file() and path.read_text(encoding="utf-8") == value:
        return
    path.write_text(value, encoding="utf-8")


def add_issue(issues: list[dict], code: str, message: str, severity: str = "blocking") -> None:
    issues.append({"severity": severity, "code": code, "message": message})


def duplicates(values: Iterable[str]) -> list[str]:
    seen: set[str] = set()
    dupes: set[str] = set()
    for value in values:
        if value in seen:
            dupes.add(value)
        seen.add(value)
    return sorted(dupes)


def dependency_cycles(graph: dict[str, list[str]]) -> list[str]:
    visiting: set[str] = set()
    visited: set[str] = set()
    cycles: list[str] = []

    def visit(node: str, stack: list[str]) -> None:
        if node in visited:
            return
        if node in visiting:
            start = stack.index(node) if node in stack else 0
            cycles.append(" -> ".join(stack[start:] + [node]))
            return
        visiting.add(node)
        stack.append(node)
        for dependency in graph.get(node, []):
            visit(dependency, stack)
        stack.pop()
        visiting.remove(node)
        visited.add(node)

    for task in graph:
        visit(task, [])
    return sorted(set(cycles))


def semantic_proof_status(settings_report: dict) -> str:
    """Return whether a source-derived Settings inventory has been proved."""

    proof = settings_report.get("source_inventory_proof")
    if not isinstance(proof, dict):
        return "pending"
    provenance = proof.get("provenance_counts")
    required_zeroes = (
        "symbol_resolution_failures",
        "unmapped",
        "duplicate_mapping",
        "orphan_capabilities",
        "empty_provenance",
    )

    def is_zero(value: object) -> bool:
        return value == 0 or value == [] or value == {}

    valid = (
        proof.get("status") == "passed"
        and proof.get("source_snapshot_verified") is True
        and isinstance(proof.get("inventory_items"), int)
        and proof["inventory_items"] > 0
        and all(name in proof and is_zero(proof[name]) for name in required_zeroes)
        and isinstance(provenance, dict)
        and isinstance(provenance.get("production_existing"), int)
        and provenance["production_existing"] > 0
        and isinstance(provenance.get("new_requirement"), int)
        and provenance["new_requirement"] >= 0
        and isinstance(provenance.get("planned_demo"), int)
        and provenance["planned_demo"] >= 0
    )
    return "passed" if valid else "failed"


def run_settings_validator(
    validator_path: Path,
    freeze_override: Path,
    expected_baseline_digest: str,
    validator_rel: str,
    issues: list[dict],
) -> tuple[dict, dict]:
    """Execute the Settings validator in-process without allowing report writes."""

    validator_digest = (
        digest(validator_path.read_text(encoding="utf-8"))
        if validator_path.is_file()
        else "missing"
    )
    exit_code = 1
    if not validator_path.is_file():
        add_issue(issues, "missing_settings_validator", validator_rel)
        report = {"status": "failed", "issues": [{"code": "missing_validator"}]}
    else:
        spec = importlib.util.spec_from_file_location(
            "product_experience_settings_capability_gate",
            validator_path,
        )
        if spec is None or spec.loader is None:
            add_issue(issues, "settings_validator_import", validator_rel)
            report = {"status": "failed", "issues": [{"code": "import_failed"}]}
        else:
            module = importlib.util.module_from_spec(spec)
            try:
                spec.loader.exec_module(module)
                module.FREEZE = freeze_override
                # The combined gate is read-only with respect to the standalone
                # Settings report, including in --update-freeze mode.
                module.write_if_changed = lambda *_args, **_kwargs: None
                captured = io.StringIO()
                original_argv = sys.argv[:]
                try:
                    sys.argv = [str(validator_path), "--check"]
                    with contextlib.redirect_stdout(captured):
                        exit_code = module.main()
                finally:
                    sys.argv = original_argv
                report = json.loads(captured.getvalue().strip())
            except Exception as exc:  # pragma: no cover - surfaced as gate data
                add_issue(
                    issues,
                    "settings_validator_execution",
                    f"{type(exc).__name__}: {exc}",
                )
                report = {
                    "status": "failed",
                    "issues": [{"code": "validator_exception"}],
                }
            else:
                if not isinstance(exit_code, int):
                    add_issue(
                        issues,
                        "settings_validator_exit_code",
                        f"expected int, found {type(exit_code).__name__}",
                    )
                if report.get("schema_version") not in SETTINGS_REPORT_SCHEMAS:
                    add_issue(
                        issues,
                        "settings_validator_schema",
                        str(report.get("schema_version")),
                    )
                status_passed = report.get("status") == "passed"
                if (exit_code == 0) != status_passed:
                    add_issue(
                        issues,
                        "settings_validator_status_mismatch",
                        f"exit={exit_code}; status={report.get('status')}",
                    )
                if not status_passed:
                    add_issue(
                        issues,
                        "settings_validator_failed",
                        canonical_json(report.get("issues", [])),
                    )

    capabilities = report.get("settings_capabilities", {})
    if not isinstance(capabilities, dict):
        capabilities = {}
    observed_baseline_digest = capabilities.get("baseline_normative_digest")
    if observed_baseline_digest != expected_baseline_digest:
        add_issue(
            issues,
            "settings_validator_baseline_digest",
            f"expected={expected_baseline_digest}; observed={observed_baseline_digest}",
        )
    metadata = {
        "executed_live": True,
        "validator": validator_rel,
        "validator_digest": validator_digest,
        "report_schema": report.get("schema_version", "missing"),
        "report_status": report.get("status", "failed"),
        "report_digest": digest(canonical_json(report)),
        "baseline_count": capabilities.get("baseline_count", 0),
        "mapped_count": capabilities.get("mapped_count", 0),
        "verified_count": capabilities.get("verified_count", 0),
        "d0_semantic_proof": semantic_proof_status(report),
    }
    return report, metadata


def yaml_text(
    document: str,
    baseline: str,
    normative_digest: str,
    prd_normative_digest: str,
    baseline_digest: str,
    worklist_digest: str,
    task_count: int,
    passed: bool,
    blocking: int,
    major: int,
    report_path: str,
    settings_validation: dict | None = None,
) -> str:
    status = "frozen" if passed else "stale"
    passed_yaml = "true" if passed else "false"
    settings_block = ""
    if settings_validation is not None:
        executed_live = "true" if settings_validation["executed_live"] else "false"
        settings_block = f"""settings_validation:
  executed_live: {executed_live}
  validator: {settings_validation["validator"]}
  validator_digest: {settings_validation["validator_digest"]}
  report_schema: {settings_validation["report_schema"]}
  report_status: {settings_validation["report_status"]}
  report_digest: {settings_validation["report_digest"]}
  baseline_count: {settings_validation["baseline_count"]}
  mapped_count: {settings_validation["mapped_count"]}
  verified_count: {settings_validation["verified_count"]}
  d0_semantic_proof: {settings_validation["d0_semantic_proof"]}

"""
    return f"""schema_version: ai-worklist-freeze.v1
skill_contract:
  name: prd-to-ai-worklist
  version: 1.1.0

status: {status}
source_document: {document}

normative_input:
  files:
    - {document}
    - {baseline}
  refs:
    - {document}#1-背景目标终态与非目标
    - {document}#2-已冻结产品与架构决策
    - {document}#3-仓库事实基线
    - {document}#4-机器合同
    - {document}#5-产品流程与状态矩阵
    - {document}#6-平台延续边界
    - {document}#7-质量性能与安全门禁
  digest_algorithm: sha256
  normalization: ai-worklist-markers-v1 + canonical-json-v1
  component_digests:
    prd_normative: {prd_normative_digest}
    settings_capability_baseline: {baseline_digest}
  digest: {normative_digest}

{settings_block}worklist:
  refs:
    - {document}#8-verification-harness
    - {document}#9-依赖-dag-与并行协议
    - {document}#10-主-checklist唯一状态源
    - {document}#11-详细任务卡
    - {document}#12-连续执行与恢复状态机
    - {document}#13-证据追踪与完成协议
    - {document}#14-风险兼容发布与外部放行
  task_count: {task_count}
  required_task_count: {task_count}
  digest_algorithm: sha256
  normalization: ai-worklist-contract-markers-v1
  digest: {worklist_digest}

completion_gate:
  passed: {passed_yaml}
  blocking_issues: {blocking}
  major_issues: {major}
  report_path: {report_path}

material_change_triggers:
  - normative_requirement_changed
  - repository_fact_invalidates_task
  - blocking_or_major_gate_failure
  - state_or_evidence_contradiction
  - explicit_scope_change

volatile_fields_excluded:
  - checkbox_state
  - progress_counts
  - current_task
  - evidence_paths
  - run_metadata
"""


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="validate and return a binary exit code")
    parser.add_argument("--update-freeze", action="store_true", help="write freeze YAML and gate JSON")
    args = parser.parse_args()

    repo = Path(__file__).resolve().parents[3]
    document_path = repo / "docs/product-experience-redesign/r-code-experience-redesign-prd.md"
    baseline_path = repo / "docs/product-experience-redesign/settings-capability-baseline.json"
    freeze_path = repo / "docs/product-experience-redesign/r-code-experience-redesign-freeze.yaml"
    report_path = repo / "docs/product-experience-redesign/worklist-gate.json"
    settings_validator_path = repo / "docs/product-experience-redesign/tools/settings_capability_gate.py"
    document_rel = document_path.relative_to(repo).as_posix()
    baseline_rel = baseline_path.relative_to(repo).as_posix()
    freeze_rel = freeze_path.relative_to(repo).as_posix()
    report_rel = report_path.relative_to(repo).as_posix()
    settings_validator_rel = settings_validator_path.relative_to(repo).as_posix()

    text = document_path.read_text(encoding="utf-8")
    issues: list[dict] = []
    normative = between(text, NORM_START, NORM_END, issues)
    contract = between(text, CONTRACT_START, CONTRACT_END, issues)
    normalized_prd = normalize(normative, worklist=False)
    prd_normative_digest = digest(normalized_prd)
    baseline_normalized = ""
    if not baseline_path.is_file():
        add_issue(issues, "missing_capability_baseline", baseline_rel)
    else:
        try:
            baseline_value = json.loads(baseline_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as exc:
            add_issue(
                issues,
                "invalid_capability_baseline",
                f"{baseline_rel}:{exc.lineno}:{exc.colno}: {exc.msg}",
            )
        else:
            if baseline_value.get("schema_version") not in {
                "settings-capability-baseline.v1",
                "settings-capability-baseline.v2",
            }:
                add_issue(
                    issues,
                    "invalid_capability_baseline_schema",
                    str(baseline_value.get("schema_version")),
                )
            baseline_normalized = canonical_json(baseline_value)
    baseline_digest = digest(baseline_normalized)
    normative_digest = digest(
        "prd_normative:\n"
        + normalized_prd
        + "settings_capability_baseline:\n"
        + baseline_normalized
        + "\n"
    )
    worklist_digest = digest(normalize(contract, worklist=True))

    requirement_ids = re.findall(rf"\*\*({REQ_RE})（(?:MUST|SHOULD)）\*\*", normative)
    requirement_set = set(requirement_ids)
    if duplicates(requirement_ids):
        add_issue(issues, "duplicate_requirements", ", ".join(duplicates(requirement_ids)))

    checklist_ids = re.findall(rf"^- \[[xX ]\] \*\*({TASK_RE})\*\*", contract, re.MULTILINE)
    card_matches = list(re.finditer(rf"^### ({TASK_RE}) .+$", contract, re.MULTILINE))
    card_ids = [match.group(1) for match in card_matches]
    assertion_ids = re.findall(rf"`(({TASK_RE})\.A[0-9]+)`", contract)
    flat_assertions = [item[0] for item in assertion_ids]

    for label, values in (("checklist", checklist_ids), ("task_cards", card_ids), ("assertions", flat_assertions)):
        dupes = duplicates(values)
        if dupes:
            add_issue(issues, f"duplicate_{label}", ", ".join(dupes))

    if len(checklist_ids) != 42:
        add_issue(issues, "task_count", f"expected 42 checklist tasks, found {len(checklist_ids)}")
    if set(checklist_ids) != set(card_ids):
        missing_cards = sorted(set(checklist_ids) - set(card_ids))
        extra_cards = sorted(set(card_ids) - set(checklist_ids))
        add_issue(issues, "task_card_bijection", f"missing={missing_cards}; extra={extra_cards}")

    cards: dict[str, str] = {}
    graph: dict[str, list[str]] = {}
    referenced_requirements: set[str] = set()
    for index, match in enumerate(card_matches):
        task_id = match.group(1)
        end = card_matches[index + 1].start() if index + 1 < len(card_matches) else len(contract)
        body = contract[match.end():end]
        cards[task_id] = body
        for field in REQUIRED_FIELDS:
            if not re.search(rf"^- {re.escape(field)}：", body, re.MULTILINE):
                add_issue(issues, "missing_task_field", f"{task_id} missing {field}")
        task_assertions = re.findall(rf"`({re.escape(task_id)}\.A[0-9]+)`", body)
        if not task_assertions:
            add_issue(issues, "missing_assertion", f"{task_id} has no assertion")
        foreign_assertions = [
            value for value in re.findall(rf"`({TASK_RE}\.A[0-9]+)`", body)
            if not value.startswith(task_id + ".")
        ]
        if foreign_assertions:
            add_issue(issues, "foreign_assertion", f"{task_id}: {foreign_assertions}")
        requirement_line = re.search(r"^- 需求引用：(.*)$", body, re.MULTILINE)
        if requirement_line:
            refs = set(re.findall(REQ_RE, requirement_line.group(1)))
            referenced_requirements.update(refs)
            unknown = sorted(refs - requirement_set)
            if unknown:
                add_issue(issues, "unknown_requirement", f"{task_id}: {unknown}")
        dependency_line = re.search(r"^- 依赖：(.*)$", body, re.MULTILINE)
        dependencies = re.findall(TASK_RE, dependency_line.group(1)) if dependency_line else []
        graph[task_id] = dependencies

    unknown_dependencies = sorted(
        {dependency for dependencies in graph.values() for dependency in dependencies if dependency not in set(card_ids)}
    )
    if unknown_dependencies:
        add_issue(issues, "unknown_dependencies", ", ".join(unknown_dependencies))
    for cycle in dependency_cycles(graph):
        add_issue(issues, "dependency_cycle", cycle)

    uncovered = sorted(requirement_set - referenced_requirements)
    if uncovered:
        add_issue(issues, "uncovered_requirements", ", ".join(uncovered), severity="major")
    if CONTRACT_END not in text or not text.rstrip().endswith(CONTRACT_END):
        add_issue(issues, "contract_end", "AI_WORKLIST_CONTRACT_END must be the final non-whitespace content")

    if args.update_freeze:
        with tempfile.TemporaryDirectory(prefix="r-code-worklist-freeze-") as temp_dir:
            candidate_freeze_path = Path(temp_dir) / freeze_path.name
            candidate_freeze_path.write_text(
                yaml_text(
                    document_rel,
                    baseline_rel,
                    normative_digest,
                    prd_normative_digest,
                    baseline_digest,
                    worklist_digest,
                    len(checklist_ids),
                    True,
                    0,
                    0,
                    report_rel,
                ),
                encoding="utf-8",
            )
            settings_report, settings_validation = run_settings_validator(
                settings_validator_path,
                candidate_freeze_path,
                baseline_digest,
                settings_validator_rel,
                issues,
            )
    else:
        settings_report, settings_validation = run_settings_validator(
            settings_validator_path,
            freeze_path,
            baseline_digest,
            settings_validator_rel,
            issues,
        )

    d0_completed = bool(
        re.search(r"^- \[[xX]\] \*\*D0-01\*\*", contract, re.MULTILINE)
    )
    if d0_completed and settings_validation["d0_semantic_proof"] != "passed":
        add_issue(
            issues,
            "d0_semantic_proof",
            "D0-01 cannot be complete until the live Settings validator emits a passing source_inventory_proof with non-empty production provenance",
        )

    if args.check and not args.update_freeze:
        if not freeze_path.is_file():
            add_issue(issues, "missing_freeze", freeze_rel)
        else:
            freeze_text = freeze_path.read_text(encoding="utf-8")
            normative_match = re.search(
                r"^normative_input:\s*$([\s\S]*?)^worklist:\s*$",
                freeze_text,
                re.MULTILINE,
            )
            worklist_match = re.search(
                r"^worklist:\s*$([\s\S]*?)^completion_gate:\s*$",
                freeze_text,
                re.MULTILINE,
            )
            settings_match = re.search(
                r"^settings_validation:\s*$([\s\S]*?)^worklist:\s*$",
                freeze_text,
                re.MULTILINE,
            )
            normative_freeze = normative_match.group(1) if normative_match else ""
            worklist_freeze = worklist_match.group(1) if worklist_match else ""
            settings_freeze = settings_match.group(1) if settings_match else ""
            freeze_checks = {
                "freeze_status": "status: frozen" in freeze_text,
                "freeze_baseline_input": f"- {baseline_rel}" in normative_freeze,
                "freeze_prd_component_digest": f"prd_normative: {prd_normative_digest}" in normative_freeze,
                "freeze_baseline_component_digest": f"settings_capability_baseline: {baseline_digest}" in normative_freeze,
                "freeze_normative_digest": f"digest: {normative_digest}" in normative_freeze,
                "freeze_worklist_digest": f"digest: {worklist_digest}" in worklist_freeze,
                "freeze_settings_executed_live": "executed_live: true" in settings_freeze,
                "freeze_settings_validator": f"validator: {settings_validator_rel}" in settings_freeze,
                "freeze_settings_validator_digest": f"validator_digest: {settings_validation['validator_digest']}" in settings_freeze,
                "freeze_settings_report_schema": f"report_schema: {settings_validation['report_schema']}" in settings_freeze,
                "freeze_settings_report_status": f"report_status: {settings_validation['report_status']}" in settings_freeze,
                "freeze_settings_report_digest": f"report_digest: {settings_validation['report_digest']}" in settings_freeze,
                "freeze_settings_baseline_count": f"baseline_count: {settings_validation['baseline_count']}" in settings_freeze,
                "freeze_settings_mapped_count": f"mapped_count: {settings_validation['mapped_count']}" in settings_freeze,
                "freeze_settings_verified_count": f"verified_count: {settings_validation['verified_count']}" in settings_freeze,
                "freeze_d0_semantic_proof": f"d0_semantic_proof: {settings_validation['d0_semantic_proof']}" in settings_freeze,
            }
            for code, valid in freeze_checks.items():
                if not valid:
                    add_issue(issues, code, "freeze is stale or incomplete", severity="major")

    blocking = sum(issue["severity"] == "blocking" for issue in issues)
    major = sum(issue["severity"] == "major" for issue in issues)
    passed = blocking == 0 and major == 0
    report = {
        "schema_version": "ai-worklist-gate.v1",
        "mode": "update_freeze" if args.update_freeze else "check",
        "worklist_id": WORKLIST_ID,
        "document": document_rel,
        "freeze": freeze_rel,
        "passed": passed,
        "counts": {
            "requirements": len(requirement_set),
            "checklist_tasks": len(checklist_ids),
            "task_cards": len(card_ids),
            "assertions": len(flat_assertions),
        },
        "digests": {
            "normative": normative_digest,
            "prd_normative": prd_normative_digest,
            "settings_capability_baseline": baseline_digest,
            "worklist": worklist_digest,
        },
        "settings_validation": settings_validation,
        "issues": issues,
    }

    if args.update_freeze:
        write_if_changed(
            report_path,
            json.dumps(report, ensure_ascii=False, indent=2) + "\n",
        )
        write_if_changed(
            freeze_path,
            yaml_text(
                document_rel,
                baseline_rel,
                normative_digest,
                prd_normative_digest,
                baseline_digest,
                worklist_digest,
                len(checklist_ids),
                passed,
                blocking,
                major,
                report_rel,
                settings_validation,
            ),
        )
    else:
        print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
