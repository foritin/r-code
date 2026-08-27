#!/usr/bin/env python3
"""Binary gate for the Settings zero-loss capability baseline.

This gate validates design/plan traceability only. It deliberately does not
mark production capabilities implemented or verified; implementation evidence
belongs to the single PRD worklist and its future unified verification harness.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from collections import Counter, defaultdict
from html.parser import HTMLParser
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
BASELINE = ROOT / "settings-capability-baseline.json"
PROTOTYPE = ROOT / "prototype.html"
PRD = ROOT / "r-code-experience-redesign-prd.md"
FREEZE = ROOT / "r-code-experience-redesign-freeze.yaml"
REPORT = ROOT / "settings-capability-gate.json"
COVERAGE = ROOT / "settings-capability-coverage.md"
BASELINE_SCHEMA_VERSION = "settings-capability-baseline.v2"

REQUIRED_TOP = {
    "schema_version",
    "baseline_revision",
    "authority_note",
    "discovery",
    "allowed_dispositions",
    "retirement_policy",
    "classification_policy",
    "source_inventories",
    "source_manifest",
    "group_defaults",
    "trace_defaults",
    "disposition_trace_extensions",
    "contract_dimensions",
    "compatibility_contracts",
    "inventory_items",
    "capabilities",
    "expected_counts",
}
REQUIRED_GROUP = {
    "legacy_pane",
    "target_page",
    "scope",
    "authority",
    "state_contract",
    "failure_policy",
}
REQUIRED_CAPABILITY = {
    "capability_id",
    "classification",
    "group",
    "kind",
    "title",
    "apply_mode",
    "disposition",
    "target_anchor",
}
REQUIRED_INVENTORY_ITEM = {
    "inventory_item_id",
    "classification",
    "capability_id",
    "group",
    "kind",
    "description",
    "disposition",
    "provenance",
    "contract",
    "target",
    "trace",
}
REQUIRED_PROVENANCE = {
    "audit_method",
    "source_ids",
    "source_manifest_pinned",
}
REQUIRED_PRODUCTION_PROVENANCE = {"source_evidence"}
REQUIRED_TRACE = {
    "materialized",
    "requirement_refs",
    "task_ids",
    "assertion_ids",
    "required_profiles",
}
REQUIRED_TARGET = {"prototype_anchor", "planned_product_target_id"}
REQUIRED_CONTRACT = {
    "semantics",
    "source_contract",
    "scope",
    "authority",
    "state_contract",
    "positive_contract",
    "failure_policy",
    "disabled_contract",
    "operation_failure_mode",
    "apply_mode",
    "default_contract",
    "value_domain_contract",
    "persistence_contract",
    "ipc_host_contract",
    "permission_contract",
    "visibility_contract",
    "side_effect_contract",
}
REQUIRED_MIGRATION_CONTRACT = {
    "contract_id",
    "capability_id",
    "kind",
    "implementation_stage",
    "source_state",
    "target_state",
    "identifier_maps",
    "value_mapping",
    "unknown_field_policy",
    "migration_id",
    "idempotent",
    "failure_policy",
    "downgrade_policy",
    "rollback_policy",
    "roundtrip_assertion_ids",
}
REQUIRED_MERGE_CONTRACT = {
    "contract_id",
    "capability_id",
    "kind",
    "implementation_stage",
    "source_entrypoints",
    "target_authority",
    "preserved_dimensions",
    "recovery_paths",
    "value_mapping",
    "failure_policy",
    "required_assertion_ids",
}
ALLOWED_KINDS = {
    "field",
    "action",
    "read_only_status",
    "navigation",
    "compound_flow",
    "validation",
}
ALLOWED_APPLY = {
    "not_applicable",
    "transient",
    "immediate",
    "next_use",
    "next_connection",
    "next_run",
    "next_session",
    "next_restart",
}
ALLOWED_OPERATION_FAILURE_MODES = {
    "not_applicable",
    "single_operation",
    "atomic_transaction",
    "per_operation",
}
ALLOWED_SOURCE_ROLES = {
    "production_ui",
    "production_frontend_domain",
    "production_ipc",
    "production_contract",
    "production_state",
    "production_host",
    "production_handler_registry",
    "production_persistence",
}
CONTRACT_OBJECT_DIMENSIONS = (
    "source_contract",
    "default_contract",
    "value_domain_contract",
    "persistence_contract",
    "ipc_host_contract",
    "permission_contract",
    "visibility_contract",
    "side_effect_contract",
    "positive_contract",
    "disabled_contract",
)
SOURCE_EVIDENCE_ROLES = {
    "authority",
    "positive",
    "failure",
    "disabled",
    "atomicity",
}
REQUIRED_SOURCE_EVIDENCE_ROLES = {"authority", "positive", "failure"}
META_STATES = {
    "uninitialized",
    "loading",
    "ready",
    "stale_last_good",
    "retrying",
    "clean",
    "dirty",
    "saving",
    "conflict",
    "success",
    "failed",
    "error",
    "disabled",
}
PLACEHOLDER_PATTERNS = (
    re.compile(r"defined by the pinned production sources", re.IGNORECASE),
    re.compile(r"summari[sz]ed by semantics", re.IGNORECASE),
    re.compile(r"follows? the pinned", re.IGNORECASE),
    re.compile(r"follows? semantics", re.IGNORECASE),
    re.compile(r"M5 must execute", re.IGNORECASE),
    re.compile(r"source set", re.IGNORECASE),
)
PER_OPERATION_SCOPE = re.compile(
    r"partial|per[- ]operation|each (field|operation|request)|逐(?:项|字段|操作)|部分",
    re.IGNORECASE,
)
PER_OPERATION_SUCCESS = re.compile(
    r"success|succeed|appl(?:y|ied)|commit|成功|已应用|已保存",
    re.IGNORECASE,
)
PER_OPERATION_FAILURE = re.compile(
    r"fail|error|reject|失败|错误|拒绝",
    re.IGNORECASE,
)
MEANINGFUL_LOCATOR = re.compile(r"[A-Za-z_][A-Za-z0-9_]{2,}")
ALLOWED_CLASSIFICATIONS = {
    "production_existing",
    "new_requirement",
    "planned_demo",
}
PRODUCTION_DISPOSITIONS = {
    "preserve",
    "merge",
    "migrate",
    "explicitly_retired",
}
NON_PRODUCTION_DISPOSITIONS = {
    "new_requirement": "add",
    "planned_demo": "demo",
}
REQUIRED_TRACE_ASSERTIONS = {
    "D0-01.A6",
    "D0-01.A7",
    "M2-03.A7",
    "M2-03.A8",
    "M5-02.A7",
    "M5-02.A8",
    "M5-02.A9",
    "M5-03.A4",
}
REQUIRED_MIGRATION_ASSERTIONS = {"M2-03.A9", "M5-03.A5"}
CAPABILITY_ID = re.compile(r"^SET-[A-Z]+-[0-9]{3}$")
INVENTORY_ID = re.compile(r"^INV-SET-[A-Z]+-[0-9]{3}$")
SOURCE_ID = re.compile(r"^SRC-[A-Z0-9-]+$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
PRODUCT_TARGET = re.compile(r"^settings\.[a-z0-9_-]+\.set-[a-z0-9-]+$")
DATA_ANCHOR = re.compile(
    r'^\[([A-Za-z_:][-A-Za-z0-9_:.]*)="([^"]+)"\]$'
)
COVERAGE_BINDING = re.compile(
    r"^<!-- generated_from_settings_capability_baseline: ([0-9a-f]{64}) -->$",
    re.MULTILINE,
)


def digest_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def normalized_source_digest(payload: bytes) -> str:
    """Hash UTF-8 source text independently of checkout newline policy."""

    text = payload.decode("utf-8")
    normalized = text.replace("\r\n", "\n").replace("\r", "\n")
    return digest_bytes(normalized.encode("utf-8"))


def canonical_digest(value: Any) -> str:
    payload = json.dumps(
        value, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    return digest_bytes(payload)


def write_if_changed(path: Path, value: str) -> None:
    if path.is_file() and path.read_text(encoding="utf-8") == value:
        return
    path.write_text(value, encoding="utf-8")


class AnchorCollector(HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.ids: Counter[str] = Counter()
        self.attrs: list[dict[str, str]] = []

    def handle_starttag(
        self, tag: str, attrs: list[tuple[str, str | None]]
    ) -> None:
        values = {name: value or "" for name, value in attrs}
        self.attrs.append(values)
        if values.get("id"):
            self.ids[values["id"]] += 1


def anchor_count(anchor: str, collector: AnchorCollector) -> int:
    if anchor.startswith("#"):
        return collector.ids[anchor[1:]]
    match = DATA_ANCHOR.fullmatch(anchor)
    if not match:
        return 0
    name, expected = match.groups()
    return sum(1 for attrs in collector.attrs if attrs.get(name) == expected)


def bind_coverage(text: str, baseline_digest: str) -> str:
    binding = f"<!-- generated_from_settings_capability_baseline: {baseline_digest} -->"
    if COVERAGE_BINDING.search(text):
        return COVERAGE_BINDING.sub(binding, text, count=1)
    lines = text.splitlines()
    insert_at = 1 if lines else 0
    lines.insert(insert_at, "")
    lines.insert(insert_at + 1, binding)
    return "\n".join(lines).rstrip() + "\n"


def current_git_head(repo: Path) -> str | None:
    try:
        completed = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=repo,
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return None
    value = completed.stdout.strip().lower()
    return value if re.fullmatch(r"[0-9a-f]{40}", value) else None


def git_commit_is_ancestor(repo: Path, ancestor: str, descendant: str) -> bool | None:
    try:
        completed = subprocess.run(
            ["git", "merge-base", "--is-ancestor", ancestor, descendant],
            cwd=repo,
            check=False,
            capture_output=True,
            text=True,
        )
    except OSError:
        return None
    if completed.returncode == 0:
        return True
    if completed.returncode == 1:
        return False
    return None


def is_repo_relative(path: str) -> bool:
    candidate = Path(path)
    return bool(path) and not candidate.is_absolute() and ".." not in candidate.parts


def main() -> int:
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true", help="validate without writing")
    mode.add_argument(
        "--update-report",
        action="store_true",
        help="update the coverage binding and gate report",
    )
    args = parser.parse_args()

    issues: list[dict[str, str]] = []

    def issue(code: str, message: str) -> None:
        issues.append({"code": code, "message": message})

    for path in (BASELINE, PROTOTYPE, PRD, FREEZE, COVERAGE):
        if not path.is_file():
            issue("missing_file", str(path))
    if issues:
        result = {"status": "failed", "issues": issues}
        if args.update_report:
            write_if_changed(REPORT, json.dumps(result, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 1

    baseline_bytes = BASELINE.read_bytes()
    try:
        baseline_text = baseline_bytes.decode("utf-8")
    except UnicodeDecodeError as exc:
        result = {
            "status": "failed",
            "issues": [{
                "code": "invalid_utf8",
                "message": f"{BASELINE}: byte {exc.start}: {exc.reason}",
            }],
        }
        if args.update_report:
            write_if_changed(REPORT, json.dumps(result, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 1
    try:
        baseline = json.loads(baseline_text)
    except json.JSONDecodeError as exc:
        result = {
            "status": "failed",
            "issues": [{
                "code": "invalid_json",
                "message": f"{BASELINE}:{exc.lineno}:{exc.colno}: {exc.msg}",
            }],
        }
        if args.update_report:
            write_if_changed(REPORT, json.dumps(result, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 1

    missing_top = sorted(REQUIRED_TOP - set(baseline))
    if missing_top:
        issue("missing_top_fields", ", ".join(missing_top))
    if baseline.get("schema_version") != BASELINE_SCHEMA_VERSION:
        issue(
            "invalid_schema_version",
            f"expected={BASELINE_SCHEMA_VERSION!r} "
            f"actual={baseline.get('schema_version')!r}",
        )

    repo = ROOT.parents[1]
    repo_resolved = repo.resolve()
    prototype_text = PROTOTYPE.read_text(encoding="utf-8")
    prd_text = PRD.read_text(encoding="utf-8")
    freeze_text = FREEZE.read_text(encoding="utf-8")
    coverage_text = COVERAGE.read_text(encoding="utf-8")
    collector = AnchorCollector()
    collector.feed(prototype_text)

    duplicate_html_ids = sorted(
        item for item, count in collector.ids.items() if count != 1
    )
    if duplicate_html_ids:
        issue("duplicate_html_ids", ", ".join(duplicate_html_ids))

    baseline_digest = normalized_source_digest(baseline_bytes)
    baseline_normative_digest = canonical_digest(baseline)

    revision = baseline.get("baseline_revision", {})
    if not isinstance(revision, dict):
        issue("invalid_baseline_revision", repr(revision))
        revision = {}
    for field in ("branch", "head", "worktree", "snapshot_policy"):
        value = revision.get(field)
        if not isinstance(value, str) or not value.strip():
            issue("invalid_baseline_revision_field", field)
    git_head = current_git_head(repo)
    if git_head is None:
        issue("git_head_unavailable", str(repo))
    else:
        expected_head = revision.get("head")
        exact_revision = expected_head == git_head
        descendant_revision = (
            revision.get("worktree") == "dirty-source-snapshot"
            and isinstance(expected_head, str)
            and re.fullmatch(r"[0-9a-f]{40}", expected_head) is not None
            and git_commit_is_ancestor(repo, expected_head, git_head) is True
        )
        if not exact_revision and not descendant_revision:
            issue(
                "baseline_revision_stale",
                f"expected={expected_head} actual={git_head}",
            )

    manifest = baseline.get("source_manifest", [])
    manifest_by_id: dict[str, dict[str, Any]] = {}
    manifest_paths: Counter[str] = Counter()
    stale_source_files: list[str] = []
    symbol_resolution_failures: list[str] = []
    valid_source_ids: set[str] = set()
    source_text_by_id: dict[str, str] = {}
    if not isinstance(manifest, list) or not manifest:
        issue("invalid_source_manifest", "source_manifest must be a non-empty list")
        manifest = []
    for index, entry in enumerate(manifest):
        label = f"source_manifest[{index}]"
        if not isinstance(entry, dict):
            issue("invalid_source_manifest_entry", label)
            continue
        missing = {
            "source_id",
            "path",
            "sha256",
            "role",
            "symbol_locators",
        } - set(entry)
        if missing:
            issue(
                "invalid_source_manifest_entry",
                f"{label}: missing {', '.join(sorted(missing))}",
            )
            continue
        source_id = entry.get("source_id")
        source_path = entry.get("path")
        expected_sha = entry.get("sha256")
        source_role = entry.get("role")
        if not isinstance(source_id, str) or not SOURCE_ID.fullmatch(source_id):
            issue("invalid_source_id", f"{label}: {source_id!r}")
            continue
        if source_id in manifest_by_id:
            issue("duplicate_source_id", source_id)
            continue
        manifest_by_id[source_id] = entry
        if source_role not in ALLOWED_SOURCE_ROLES:
            issue("invalid_source_role", f"{source_id}: {source_role!r}")
            continue
        if not isinstance(source_path, str) or not is_repo_relative(source_path):
            issue("invalid_source_path", f"{source_id}: {source_path!r}")
            continue
        manifest_paths[source_path] += 1
        if not isinstance(expected_sha, str) or not SHA256.fullmatch(expected_sha):
            issue("invalid_source_sha256", f"{source_id}: {expected_sha!r}")
            continue
        full_path = (repo / source_path).resolve()
        try:
            full_path.relative_to(repo_resolved)
        except ValueError:
            issue("source_path_escape", f"{source_id}: {source_path}")
            continue
        if not full_path.is_file():
            issue("missing_source_file", f"{source_id}: {source_path}")
            continue
        source_bytes = full_path.read_bytes()
        try:
            actual_sha = normalized_source_digest(source_bytes)
        except UnicodeDecodeError as exc:
            issue(
                "invalid_source_encoding",
                f"{source_id}: {source_path}: byte {exc.start}: {exc.reason}",
            )
            continue
        if actual_sha != expected_sha:
            stale_source_files.append(
                f"{source_id}:{source_path}:expected={expected_sha}:actual={actual_sha}"
            )
            issue(
                "stale_source_snapshot",
                f"{source_id}: expected={expected_sha} actual={actual_sha}",
            )
            continue
        symbol_locators = entry.get("symbol_locators", [])
        source_symbols_valid = True
        if (
            not isinstance(symbol_locators, list)
            or not symbol_locators
            or not all(
                isinstance(locator, str)
                and len(locator.strip()) >= 8
                and MEANINGFUL_LOCATOR.search(locator) is not None
                for locator in symbol_locators
            )
            or len(symbol_locators) != len(set(symbol_locators))
        ):
            failure = (
                f"{source_id}: symbol_locators must be unique, non-empty, "
                "meaningful strings of at least 8 characters"
            )
            symbol_resolution_failures.append(failure)
            issue("symbol_resolution_failure", failure)
            source_symbols_valid = False
        else:
            source_text = source_bytes.decode("utf-8", errors="replace")
            source_text_by_id[source_id] = source_text
            for locator in symbol_locators:
                locator_count = source_text.count(locator)
                if locator_count != 1:
                    failure = f"{source_id}: count={locator_count}: {locator}"
                    symbol_resolution_failures.append(failure)
                    issue("symbol_resolution_failure", failure)
                    source_symbols_valid = False
        if source_symbols_valid:
            valid_source_ids.add(source_id)

    duplicate_source_paths = sorted(
        path for path, count in manifest_paths.items() if count != 1
    )
    if duplicate_source_paths:
        issue("duplicate_source_paths", ", ".join(duplicate_source_paths))
    source_manifest_digest = canonical_digest(manifest)

    groups = baseline.get("group_defaults", {})
    inventories = baseline.get("source_inventories", {})
    if not isinstance(groups, dict) or not groups:
        issue("invalid_group_defaults", repr(groups))
        groups = {}
    if not isinstance(inventories, dict) or not inventories:
        issue("invalid_source_inventories", repr(inventories))
        inventories = {}
    if set(groups) != set(inventories):
        issue(
            "group_inventory_mismatch",
            f"groups={sorted(groups)} inventories={sorted(inventories)}",
        )

    referenced_source_ids: set[str] = set()
    for group_name, source_ids in inventories.items():
        if not isinstance(source_ids, list) or not source_ids:
            issue("empty_source_inventory", group_name)
            continue
        if not all(
            isinstance(source_id, str) and SOURCE_ID.fullmatch(source_id)
            for source_id in source_ids
        ):
            issue(
                "invalid_source_inventory",
                f"{group_name}: source IDs must be valid strings",
            )
            continue
        duplicates = sorted(
            source_id for source_id, count in Counter(source_ids).items() if count != 1
        )
        if duplicates:
            issue("duplicate_group_source", f"{group_name}: {', '.join(duplicates)}")
        for source_id in source_ids:
            if not isinstance(source_id, str) or source_id not in manifest_by_id:
                issue("unknown_group_source", f"{group_name}: {source_id!r}")
                continue
            referenced_source_ids.add(source_id)
    unused_manifest_sources = sorted(set(manifest_by_id) - referenced_source_ids)
    if unused_manifest_sources:
        issue("unused_manifest_sources", ", ".join(unused_manifest_sources))

    for group_name, group in groups.items():
        if not isinstance(group, dict):
            issue("invalid_group", f"{group_name}: expected object")
            continue
        missing = REQUIRED_GROUP - set(group)
        if missing:
            issue("invalid_group", f"{group_name}: missing {', '.join(sorted(missing))}")
        if not isinstance(group.get("state_contract"), list) or not group.get(
            "state_contract"
        ):
            issue("empty_state_contract", group_name)

    trace_defaults = baseline.get("trace_defaults", {})
    trace_extensions = baseline.get("disposition_trace_extensions", {})
    for field in ("requirement_refs", "task_ids", "assertion_ids", "required_profiles"):
        if not isinstance(trace_defaults.get(field), list) or not trace_defaults.get(field):
            issue("missing_trace_default", field)
    if not isinstance(trace_extensions, dict):
        issue("invalid_trace_extensions", repr(trace_extensions))
        trace_extensions = {}
    missing_common = sorted(
        REQUIRED_TRACE_ASSERTIONS - set(trace_defaults.get("assertion_ids", []))
    )
    if missing_common:
        issue("missing_required_trace_assertions", ", ".join(missing_common))
    missing_migration = sorted(
        REQUIRED_MIGRATION_ASSERTIONS - set(trace_extensions.get("migrate", []))
    )
    if missing_migration:
        issue("missing_migration_trace_assertions", ", ".join(missing_migration))
    declared_contract_dimensions = baseline.get("contract_dimensions", [])
    if (
        not isinstance(declared_contract_dimensions, list)
        or len(declared_contract_dimensions) != len(REQUIRED_CONTRACT)
        or set(declared_contract_dimensions) != REQUIRED_CONTRACT
    ):
        issue(
            "contract_dimension_mismatch",
            f"declared={declared_contract_dimensions!r} "
            f"required={sorted(REQUIRED_CONTRACT)}",
        )

    capabilities = baseline.get("capabilities", [])
    if not isinstance(capabilities, list):
        issue("invalid_capabilities", "capabilities must be a list")
        capabilities = []
    capability_ids = [
        item.get("capability_id", "") if isinstance(item, dict) else ""
        for item in capabilities
    ]
    for capability_id, count in Counter(capability_ids).items():
        if not capability_id or count != 1:
            issue("duplicate_capability_id", capability_id or "<missing>")

    allowed_dispositions = set(baseline.get("allowed_dispositions", []))
    cap_by_id: dict[str, dict[str, Any]] = {}
    for index, capability in enumerate(capabilities):
        label = (
            capability.get("capability_id")
            if isinstance(capability, dict)
            else f"index:{index}"
        )
        if not isinstance(capability, dict):
            issue("invalid_capability", f"{label}: expected object")
            continue
        missing = REQUIRED_CAPABILITY - set(capability)
        if missing:
            issue(
                "invalid_capability",
                f"{label}: missing {', '.join(sorted(missing))}",
            )
            continue
        capability_id = capability["capability_id"]
        if not CAPABILITY_ID.fullmatch(capability_id):
            issue("invalid_capability_id", capability_id)
        else:
            cap_by_id[capability_id] = capability
        if capability["classification"] not in ALLOWED_CLASSIFICATIONS:
            issue(
                "invalid_capability_classification",
                f"{label}: {capability['classification']}",
            )
        if capability["group"] not in groups:
            issue("unknown_group", f"{label}: {capability['group']}")
        if capability["kind"] not in ALLOWED_KINDS:
            issue("invalid_kind", f"{label}: {capability['kind']}")
        if capability["apply_mode"] not in ALLOWED_APPLY:
            issue("invalid_apply_mode", f"{label}: {capability['apply_mode']}")
        if capability["disposition"] not in allowed_dispositions:
            issue("invalid_disposition", f"{label}: {capability['disposition']}")
        classification = capability["classification"]
        disposition = capability["disposition"]
        if (
            classification == "production_existing"
            and disposition not in PRODUCTION_DISPOSITIONS
        ):
            issue(
                "invalid_production_disposition",
                f"{label}: {disposition}",
            )
        expected_non_production = NON_PRODUCTION_DISPOSITIONS.get(classification)
        if (
            expected_non_production is not None
            and disposition != expected_non_production
        ):
            issue(
                "invalid_non_production_disposition",
                f"{label}: expected={expected_non_production} actual={disposition}",
            )

    compatibility_contracts = baseline.get("compatibility_contracts", {})
    if not isinstance(compatibility_contracts, dict):
        issue("invalid_compatibility_contracts", repr(compatibility_contracts))
        compatibility_contracts = {}
    used_compatibility_contracts: set[str] = set()
    invalid_merges: list[str] = []
    invalid_migrations: list[str] = []
    unauthorized_retirements: list[str] = []

    for capability_id, capability in cap_by_id.items():
        disposition = capability.get("disposition")
        if disposition not in {"merge", "migrate"}:
            if capability.get("compatibility_contract_id"):
                issue("unexpected_compatibility_contract", capability_id)
            if disposition == "explicitly_retired":
                retirement_ref = capability.get("retirement_requirement_ref", "")
                if not retirement_ref or retirement_ref == "R-SET-07":
                    unauthorized_retirements.append(capability_id)
            continue
        contract_id = capability.get("compatibility_contract_id")
        if not isinstance(contract_id, str) or not contract_id:
            message = f"{capability_id}: missing compatibility_contract_id"
            issue("missing_compatibility_contract", message)
            (invalid_merges if disposition == "merge" else invalid_migrations).append(
                message
            )
            continue
        contract = compatibility_contracts.get(contract_id)
        if not isinstance(contract, dict):
            message = f"{capability_id}: unknown {contract_id}"
            issue("missing_compatibility_contract", message)
            (invalid_merges if disposition == "merge" else invalid_migrations).append(
                message
            )
            continue
        used_compatibility_contracts.add(contract_id)
        required = (
            REQUIRED_MERGE_CONTRACT if disposition == "merge"
            else REQUIRED_MIGRATION_CONTRACT
        )
        missing = required - set(contract)
        if missing:
            message = (
                f"{capability_id}: {contract_id} missing "
                f"{', '.join(sorted(missing))}"
            )
            issue("invalid_compatibility_contract", message)
            (invalid_merges if disposition == "merge" else invalid_migrations).append(
                message
            )
            continue
        if (
            contract.get("contract_id") != contract_id
            or contract.get("capability_id") != capability_id
            or contract.get("kind") != disposition
        ):
            message = f"{capability_id}: identity mismatch in {contract_id}"
            issue("invalid_compatibility_identity", message)
            (invalid_merges if disposition == "merge" else invalid_migrations).append(
                message
            )
        if disposition == "migrate":
            identifier_maps = contract.get("identifier_maps")
            map_types = {"routes", "deep_links", "config_keys", "enums", "ipc"}
            if not isinstance(identifier_maps, dict) or set(identifier_maps) != map_types:
                message = f"{capability_id}: invalid identifier_maps"
                issue("invalid_migration_identifier_maps", message)
                invalid_migrations.append(message)
            elif not all(isinstance(identifier_maps[name], list) for name in map_types):
                message = f"{capability_id}: identifier map values must be lists"
                issue("invalid_migration_identifier_maps", message)
                invalid_migrations.append(message)
            elif not any(identifier_maps.values()):
                message = f"{capability_id}: migration has no identifier mapping"
                issue("empty_migration_identifier_maps", message)
                invalid_migrations.append(message)
            if contract.get("idempotent") is not True:
                message = f"{capability_id}: idempotent must be true"
                issue("non_idempotent_migration", message)
                invalid_migrations.append(message)
            missing_roundtrip = sorted(
                REQUIRED_MIGRATION_ASSERTIONS
                - set(contract.get("roundtrip_assertion_ids", []))
            )
            if missing_roundtrip:
                message = (
                    f"{capability_id}: missing round-trip assertions "
                    f"{', '.join(missing_roundtrip)}"
                )
                issue("missing_migration_assertions", message)
                invalid_migrations.append(message)
        else:
            for field in (
                "source_entrypoints",
                "preserved_dimensions",
                "recovery_paths",
                "required_assertion_ids",
            ):
                if not isinstance(contract.get(field), list) or not contract.get(field):
                    message = f"{capability_id}: empty merge field {field}"
                    issue("invalid_merge_contract", message)
                    invalid_merges.append(message)

    unused_compatibility = sorted(
        set(compatibility_contracts) - used_compatibility_contracts
    )
    if unused_compatibility:
        issue("unused_compatibility_contracts", ", ".join(unused_compatibility))

    inventory_items = baseline.get("inventory_items", [])
    if not isinstance(inventory_items, list):
        issue("invalid_inventory_items", "inventory_items must be a list")
        inventory_items = []
    inventory_ids = [
        item.get("inventory_item_id", "") if isinstance(item, dict) else ""
        for item in inventory_items
    ]
    for inventory_id, count in Counter(inventory_ids).items():
        if not inventory_id or count != 1:
            issue("duplicate_inventory_item_id", inventory_id or "<missing>")

    classification_policy = baseline.get("classification_policy", {})
    lower_bound_classification = classification_policy.get("lower_bound")
    if lower_bound_classification != "production_existing":
        issue("invalid_lower_bound_classification", repr(lower_bound_classification))
    non_lower_bound = classification_policy.get("non_lower_bound", [])
    if set(non_lower_bound) != {"new_requirement", "planned_demo"}:
        issue("invalid_non_lower_bound_classification", repr(non_lower_bound))

    production_items: list[dict[str, Any]] = []
    inventory_classifications: Counter[str] = Counter()
    inventory_dispositions: Counter[str] = Counter()
    inventory_group_counts: Counter[str] = Counter()
    mappings: defaultdict[str, list[str]] = defaultdict(list)
    unmapped_inventory_items: list[str] = []
    empty_provenance: list[str] = []
    non_production_basis_failures: list[str] = []
    missing_targets: list[str] = []
    missing_assertions: list[str] = []
    prototype_only_evidence: list[str] = []
    missing_item_source_evidence: list[str] = []
    invalid_item_source_evidence: list[str] = []
    incomplete_contract_dimensions: dict[str, list[str]] = {}
    placeholder_contracts: dict[str, list[str]] = {}
    group_contract_placeholders: list[str] = []
    meta_state_only_contracts: list[str] = []
    invalid_apply_modes: list[str] = []
    invalid_operation_failure_modes: list[str] = []
    unproven_atomicity_claims: list[str] = []
    traceability: list[dict[str, Any]] = []
    source_audited_count = 0
    production_source_audited_count = 0
    item_source_evidenced_count = 0
    production_item_source_evidenced_count = 0
    contract_complete_count = 0
    production_contract_complete_count = 0
    prototype_mapped_count = 0
    planned_product_target_count = 0

    for index, item in enumerate(inventory_items):
        label = (
            item.get("inventory_item_id")
            if isinstance(item, dict)
            else f"index:{index}"
        )
        item_invalid = False

        def item_issue(code: str, message: str) -> None:
            nonlocal item_invalid
            item_invalid = True
            issue(code, f"{label}: {message}")

        if not isinstance(item, dict):
            item_issue("invalid_inventory_item", "expected object")
            continue
        missing = REQUIRED_INVENTORY_ITEM - set(item)
        if missing:
            item_issue(
                "invalid_inventory_item",
                f"missing {', '.join(sorted(missing))}",
            )
            continue
        inventory_id = item["inventory_item_id"]
        if not INVENTORY_ID.fullmatch(inventory_id):
            item_issue("invalid_inventory_item_id", inventory_id)
        classification = item["classification"]
        if classification not in ALLOWED_CLASSIFICATIONS:
            item_issue("invalid_inventory_classification", repr(classification))
        if isinstance(classification, str):
            inventory_classifications[classification] += 1
        if isinstance(item.get("disposition"), str):
            inventory_dispositions[item["disposition"]] += 1
        if isinstance(item.get("group"), str):
            inventory_group_counts[item["group"]] += 1
        if classification == lower_bound_classification:
            production_items.append(item)

        capability_id = item["capability_id"]
        capability = cap_by_id.get(capability_id)
        if capability is None:
            unmapped_inventory_items.append(inventory_id)
            item_issue("unmapped_inventory_item", f"unknown capability {capability_id!r}")
            continue
        mappings[capability_id].append(inventory_id)
        for field in ("classification", "group", "kind", "disposition"):
            if item.get(field) != capability.get(field):
                item_issue(
                    "inventory_capability_mismatch",
                    f"{field}: inventory={item.get(field)!r} "
                    f"capability={capability.get(field)!r}",
                )

        provenance = item.get("provenance")
        provenance_valid = True
        if not isinstance(provenance, dict):
            provenance_valid = False
            empty_provenance.append(inventory_id)
            item_issue("invalid_provenance", "expected object")
            provenance = {}
        missing_provenance = REQUIRED_PROVENANCE - set(provenance)
        if missing_provenance:
            provenance_valid = False
            item_issue(
                "invalid_provenance",
                f"missing {', '.join(sorted(missing_provenance))}",
            )
        source_ids = provenance.get("source_ids", [])
        if (
            not isinstance(source_ids, list)
            or not source_ids
            or not all(
                isinstance(source_id, str) and SOURCE_ID.fullmatch(source_id)
                for source_id in source_ids
            )
        ):
            provenance_valid = False
            if inventory_id not in empty_provenance:
                empty_provenance.append(inventory_id)
            item_issue(
                "empty_provenance",
                "source_ids must be a non-empty list of valid SourceIDs",
            )
            source_ids = []
        if provenance.get("source_manifest_pinned") is not True:
            provenance_valid = False
            item_issue("unpinned_provenance", "source_manifest_pinned must be true")
        audit_method = provenance.get("audit_method")
        if classification == lower_bound_classification:
            if audit_method != "read_only_source_review":
                provenance_valid = False
                item_issue(
                    "invalid_production_audit_method",
                    repr(audit_method),
                )
        else:
            basis = provenance.get("classification_basis")
            if audit_method != "read_only_source_absence_review":
                provenance_valid = False
                non_production_basis_failures.append(inventory_id)
                item_issue(
                    "invalid_non_production_audit_method",
                    repr(audit_method),
                )
            if not isinstance(basis, str) or not basis.strip():
                provenance_valid = False
                non_production_basis_failures.append(inventory_id)
                item_issue(
                    "missing_non_production_classification_basis",
                    "classification_basis must explain the pinned-source absence boundary",
                )
        allowed_group_sources = set(inventories.get(item["group"], []))
        for source_id in source_ids:
            if source_id not in manifest_by_id:
                provenance_valid = False
                item_issue("unknown_provenance_source", str(source_id))
            elif source_id not in allowed_group_sources:
                provenance_valid = False
                item_issue("cross_group_provenance_source", str(source_id))
            elif source_id not in valid_source_ids:
                provenance_valid = False
        if len(source_ids) != len(set(source_ids)):
            provenance_valid = False
            item_issue("duplicate_provenance_source", repr(source_ids))

        source_evidence_valid = classification != lower_bound_classification
        source_evidence_roles: set[str] = set()
        source_evidence = provenance.get("source_evidence")
        if classification == lower_bound_classification:
            missing_production_provenance = (
                REQUIRED_PRODUCTION_PROVENANCE - set(provenance)
            )
            if missing_production_provenance:
                provenance_valid = False
                missing_item_source_evidence.append(inventory_id)
                item_issue(
                    "missing_item_source_evidence",
                    "production_existing requires item-level source_evidence",
                )
                source_evidence = []
            elif not isinstance(source_evidence, list) or not source_evidence:
                provenance_valid = False
                missing_item_source_evidence.append(inventory_id)
                item_issue(
                    "missing_item_source_evidence",
                    "source_evidence must be a non-empty list",
                )
                source_evidence = []
            else:
                source_evidence_valid = True
                for evidence_index, evidence in enumerate(source_evidence):
                    evidence_label = f"source_evidence[{evidence_index}]"
                    if not isinstance(evidence, dict):
                        source_evidence_valid = False
                        invalid_item_source_evidence.append(inventory_id)
                        item_issue(
                            "invalid_item_source_evidence",
                            f"{evidence_label}: expected object",
                        )
                        continue
                    evidence_source_id = evidence.get("source_id")
                    locator = evidence.get("symbol_locator")
                    roles = evidence.get("roles")
                    if (
                        not isinstance(evidence_source_id, str)
                        or evidence_source_id not in source_ids
                    ):
                        source_evidence_valid = False
                        invalid_item_source_evidence.append(inventory_id)
                        item_issue(
                            "invalid_item_source_evidence",
                            f"{evidence_label}: source_id must be in provenance.source_ids",
                        )
                    elif evidence_source_id not in valid_source_ids:
                        source_evidence_valid = False
                        invalid_item_source_evidence.append(inventory_id)
                        item_issue(
                            "invalid_item_source_evidence",
                            f"{evidence_label}: source_id is not a resolved manifest source",
                        )
                    if (
                        not isinstance(locator, str)
                        or len(locator.strip()) < 8
                        or MEANINGFUL_LOCATOR.search(locator) is None
                    ):
                        source_evidence_valid = False
                        invalid_item_source_evidence.append(inventory_id)
                        item_issue(
                            "invalid_item_source_evidence",
                            f"{evidence_label}: symbol_locator must be meaningful and at least 8 characters",
                        )
                    elif evidence_source_id in source_text_by_id:
                        locator_count = source_text_by_id[evidence_source_id].count(
                            locator
                        )
                        if locator_count != 1:
                            source_evidence_valid = False
                            invalid_item_source_evidence.append(inventory_id)
                            item_issue(
                                "unresolved_item_symbol_locator",
                                f"{evidence_source_id}: count={locator_count}: {locator}",
                            )
                    else:
                        source_evidence_valid = False
                        invalid_item_source_evidence.append(inventory_id)
                        item_issue(
                            "unresolved_item_symbol_locator",
                            f"{evidence_source_id}: source text unavailable",
                        )
                    if (
                        not isinstance(roles, list)
                        or not roles
                        or not all(
                            isinstance(role, str) and role in SOURCE_EVIDENCE_ROLES
                            for role in roles
                        )
                    ):
                        source_evidence_valid = False
                        invalid_item_source_evidence.append(inventory_id)
                        item_issue(
                            "invalid_item_source_evidence",
                            f"{evidence_label}: invalid roles {roles!r}",
                        )
                    else:
                        source_evidence_roles.update(roles)
                missing_roles = sorted(
                    REQUIRED_SOURCE_EVIDENCE_ROLES - source_evidence_roles
                )
                if missing_roles:
                    source_evidence_valid = False
                    invalid_item_source_evidence.append(inventory_id)
                    item_issue(
                        "incomplete_item_source_evidence",
                        f"missing roles {', '.join(missing_roles)}",
                    )
            if source_evidence_valid and provenance_valid:
                item_source_evidenced_count += 1
                production_item_source_evidenced_count += 1

        contract_invalid = False

        def contract_issue(code: str, message: str) -> None:
            nonlocal contract_invalid
            contract_invalid = True
            item_issue(code, message)

        contract = item.get("contract")
        if not isinstance(contract, dict):
            contract_issue("invalid_item_contract", "expected object")
            contract = {}
        missing_contract = sorted(REQUIRED_CONTRACT - set(contract))
        if missing_contract:
            incomplete_contract_dimensions[inventory_id] = missing_contract
            contract_issue(
                "incomplete_contract_dimensions",
                f"missing {', '.join(missing_contract)}",
            )
        group = groups.get(item["group"], {})
        expected_values = {
            "semantics": capability.get("title"),
            "apply_mode": capability.get("apply_mode"),
        }
        for field, expected_value in expected_values.items():
            if contract.get(field) != expected_value:
                contract_issue(
                    "contract_resolution_mismatch",
                    f"{field}: actual={contract.get(field)!r} "
                    f"expected={expected_value!r}",
                )
        for field in ("scope", "authority", "failure_policy"):
            value = contract.get(field)
            if not isinstance(value, str) or not value.strip():
                contract_issue("invalid_item_contract", f"{field}: missing value")

        apply_mode = contract.get("apply_mode")
        if apply_mode not in ALLOWED_APPLY:
            invalid_apply_modes.append(
                f"{inventory_id}:{apply_mode!r}"
            )
            contract_issue("invalid_apply_mode", repr(apply_mode))
        if (
            item.get("kind") in {"navigation", "read_only_status"}
            and apply_mode not in {"transient", "not_applicable"}
        ):
            invalid_apply_modes.append(
                f"{inventory_id}:{item.get('kind')}:{apply_mode}"
            )
            contract_issue(
                "imprecise_apply_mode",
                f"{item.get('kind')} must use transient or not_applicable, not {apply_mode!r}",
            )

        states = contract.get("state_contract")
        if (
            not isinstance(states, list)
            or not states
            or not all(isinstance(state, str) and state.strip() for state in states)
        ):
            contract_issue(
                "invalid_item_contract",
                "state_contract: expected non-empty string list",
            )
        else:
            normalized_states = {state.strip().lower() for state in states}
            if normalized_states.issubset(META_STATES):
                meta_state_only_contracts.append(inventory_id)
                contract_issue(
                    "meta_state_only_contract",
                    "shared loading/dirty/conflict states cannot replace domain states",
                )

        group_fields = ("authority", "state_contract", "failure_policy")
        if group and all(contract.get(field) == group.get(field) for field in group_fields):
            group_contract_placeholders.append(inventory_id)
            contract_issue(
                "group_contract_placeholder",
                "item contract copies every group scope/authority/state/failure field",
            )

        placeholder_fields: list[str] = []
        for field, value in contract.items():
            detail = value.get("detail") if isinstance(value, dict) else value
            if not isinstance(detail, str):
                continue
            if any(pattern.search(detail) for pattern in PLACEHOLDER_PATTERNS):
                placeholder_fields.append(field)
        if placeholder_fields:
            placeholder_contracts[inventory_id] = sorted(placeholder_fields)
            contract_issue(
                "placeholder_contract_text",
                ", ".join(sorted(placeholder_fields)),
            )

        for field in CONTRACT_OBJECT_DIMENSIONS:
            dimension = contract.get(field)
            if not isinstance(dimension, dict):
                contract_issue(
                    "invalid_contract_dimension",
                    f"{field}: expected object",
                )
                continue
            if dimension.get("status") not in {
                "source_defined", "not_applicable", "explicit"
            }:
                contract_issue(
                    "invalid_contract_dimension",
                    f"{field}: invalid status {dimension.get('status')!r}",
                )
            detail = dimension.get("detail")
            if not isinstance(detail, str) or not detail.strip():
                contract_issue(
                    "invalid_contract_dimension",
                    f"{field}: missing detail",
                )

        if (
            classification == lower_bound_classification
            and isinstance(contract.get("disabled_contract"), dict)
            and contract["disabled_contract"].get("status") != "not_applicable"
            and "disabled" not in source_evidence_roles
        ):
            contract_issue(
                "missing_disabled_source_evidence",
                "disabled behavior requires an item-level disabled evidence role",
            )

        operation_failure_mode = contract.get("operation_failure_mode")
        if operation_failure_mode not in ALLOWED_OPERATION_FAILURE_MODES:
            invalid_operation_failure_modes.append(
                f"{inventory_id}:{operation_failure_mode!r}"
            )
            contract_issue(
                "invalid_operation_failure_mode",
                repr(operation_failure_mode),
            )
        elif (
            item.get("kind") == "compound_flow"
            and operation_failure_mode not in {"atomic_transaction", "per_operation"}
        ):
            invalid_operation_failure_modes.append(inventory_id)
            contract_issue(
                "invalid_operation_failure_mode",
                "compound_flow must declare atomic_transaction or per_operation",
            )
        elif operation_failure_mode == "per_operation":
            failure_policy = contract.get("failure_policy", "")
            if (
                not isinstance(failure_policy, str)
                or not PER_OPERATION_SCOPE.search(failure_policy)
                or not PER_OPERATION_SUCCESS.search(failure_policy)
                or not PER_OPERATION_FAILURE.search(failure_policy)
            ):
                invalid_operation_failure_modes.append(inventory_id)
                contract_issue(
                    "missing_partial_failure_contract",
                    "per_operation must explicitly identify the operation scope and both successful and failed outcomes",
                )
        elif operation_failure_mode == "atomic_transaction":
            if classification == lower_bound_classification and "atomicity" not in source_evidence_roles:
                unproven_atomicity_claims.append(inventory_id)
                contract_issue(
                    "unproven_atomicity_claim",
                    "atomic_transaction requires item-level atomicity source evidence",
                )

        failure_policy = contract.get("failure_policy", "")
        if (
            isinstance(failure_policy, str)
            and re.search(
                r"never partially apply|reject the whole|rollback (?:all|the entire|the whole)|整体回滚|全部旧值.*不变|整(?:体|批).*拒绝",
                failure_policy,
                re.IGNORECASE,
            )
            and operation_failure_mode != "atomic_transaction"
        ):
            unproven_atomicity_claims.append(inventory_id)
            contract_issue(
                "unproven_atomicity_claim",
                "whole-operation rollback claim lacks atomic_transaction evidence",
            )

        if not contract_invalid:
            contract_complete_count += 1
            if classification == lower_bound_classification:
                production_contract_complete_count += 1

        target = item.get("target")
        if not isinstance(target, dict):
            item_issue("invalid_inventory_target", "expected object")
            target = {}
        missing_target_fields = REQUIRED_TARGET - set(target)
        if missing_target_fields:
            item_issue(
                "invalid_inventory_target",
                f"missing {', '.join(sorted(missing_target_fields))}",
            )
        prototype_anchor = target.get("prototype_anchor", "")
        if prototype_anchor != capability.get("target_anchor"):
            item_issue(
                "prototype_anchor_mismatch",
                f"inventory={prototype_anchor!r} "
                f"capability={capability.get('target_anchor')!r}",
            )
        target_count = (
            anchor_count(prototype_anchor, collector)
            if isinstance(prototype_anchor, str)
            else 0
        )
        if target_count != 1:
            missing_targets.append(
                f"{capability_id}:{prototype_anchor} (count={target_count})"
            )
            item_issue(
                "missing_or_ambiguous_prototype_target",
                f"{prototype_anchor!r} count={target_count}",
            )
        else:
            prototype_mapped_count += 1
        planned_target = target.get("planned_product_target_id", "")
        if not isinstance(planned_target, str) or not PRODUCT_TARGET.fullmatch(
            planned_target
        ):
            missing_targets.append(
                f"{capability_id}:planned_product_target={planned_target!r}"
            )
            item_issue("invalid_planned_product_target", repr(planned_target))
        else:
            planned_product_target_count += 1

        trace = item.get("trace")
        if not isinstance(trace, dict):
            item_issue("invalid_materialized_trace", "expected object")
            trace = {}
        missing_trace_fields = REQUIRED_TRACE - set(trace)
        if missing_trace_fields:
            item_issue(
                "invalid_materialized_trace",
                f"missing {', '.join(sorted(missing_trace_fields))}",
            )
        if trace.get("materialized") is not True:
            item_issue("invalid_materialized_trace", "materialized must be true")
        trace_lists_valid = True
        for field in (
            "requirement_refs",
            "task_ids",
            "assertion_ids",
            "required_profiles",
        ):
            values = trace.get(field)
            if (
                not isinstance(values, list)
                or not values
                or not all(isinstance(value, str) and value.strip() for value in values)
            ):
                trace_lists_valid = False
                item_issue(
                    "invalid_materialized_trace",
                    f"{field}: expected non-empty string list",
                )
        actual_assertions = set(trace.get("assertion_ids", []))
        if classification == lower_bound_classification:
            expected_assertions = set(trace_defaults.get("assertion_ids", []))
            expected_assertions.update(
                trace_extensions.get(capability.get("disposition"), [])
            )
            if actual_assertions != expected_assertions:
                delta = sorted(expected_assertions - actual_assertions)
                extra = sorted(actual_assertions - expected_assertions)
                missing_assertions.extend(
                    f"{capability_id}:{assertion}" for assertion in delta
                )
                item_issue(
                    "trace_assertion_mismatch",
                    f"missing={delta} extra={extra}",
                )
        for requirement in trace.get("requirement_refs", []):
            if requirement not in prd_text:
                item_issue("missing_requirement", requirement)
        for task_id in trace.get("task_ids", []):
            if f"### {task_id} " not in prd_text:
                item_issue("missing_task", task_id)
        for assertion in trace.get("assertion_ids", []):
            if f"`{assertion}`" not in prd_text:
                missing_assertions.append(f"{capability_id}:{assertion}")
                item_issue("missing_assertion", assertion)
        if trace_lists_valid:
            actual_profiles = set(trace.get("required_profiles", []))
            default_profiles = set(trace_defaults.get("required_profiles", []))
            profiles_valid = (
                actual_profiles == default_profiles
                if classification == lower_bound_classification
                else actual_profiles.issubset(default_profiles)
            )
            if not profiles_valid:
                item_issue(
                    "trace_profile_mismatch",
                    repr(trace.get("required_profiles")),
                )

        traceability.append({
            "inventory_item_id": inventory_id,
            "capability_id": capability_id,
            "classification": classification,
            "source_ids": source_ids,
            "item_source_evidence_count": (
                len(source_evidence) if isinstance(source_evidence, list) else 0
            ),
            "contract_complete": not contract_invalid,
            "planned_product_target_id": planned_target,
            "requirement_refs": trace.get("requirement_refs", []),
            "task_ids": trace.get("task_ids", []),
            "assertion_ids": trace.get("assertion_ids", []),
        })
        if not provenance_valid and target_count == 1:
            prototype_only_evidence.append(capability_id)
        if not item_invalid:
            source_audited_count += 1
            if classification == lower_bound_classification:
                production_source_audited_count += 1

    production_capability_ids = {
        capability_id
        for capability_id, capability in cap_by_id.items()
        if capability.get("classification") == lower_bound_classification
    }
    all_capability_ids = set(cap_by_id)
    duplicate_mappings = sorted(
        f"{capability_id}:{','.join(ids)}"
        for capability_id, ids in mappings.items()
        if len(ids) != 1
    )
    for duplicate in duplicate_mappings:
        issue("duplicate_inventory_mapping", duplicate)
    orphan_capabilities = sorted(
        capability_id
        for capability_id in all_capability_ids
        if len(mappings.get(capability_id, [])) != 1
    )
    if orphan_capabilities:
        issue("orphan_capabilities", ", ".join(orphan_capabilities))
    mapped_capability_ids = {
        capability_id
        for capability_id in production_capability_ids
        if len(mappings.get(capability_id, [])) == 1
    }
    mapped_count = len(mapped_capability_ids)
    inventory_mapped_count = sum(
        1 for capability_id in all_capability_ids if len(mappings.get(capability_id, [])) == 1
    )
    dispositions = Counter(
        cap_by_id[capability_id]["disposition"]
        for capability_id in mapped_capability_ids
    )
    group_counts = Counter(
        cap_by_id[capability_id]["group"]
        for capability_id in mapped_capability_ids
    )

    expected = baseline.get("expected_counts", {})
    if len(inventory_items) != expected.get("inventory_items"):
        issue(
            "inventory_count_mismatch",
            f"actual={len(inventory_items)} expected={expected.get('inventory_items')}",
        )
    for classification in sorted(ALLOWED_CLASSIFICATIONS):
        actual = inventory_classifications.get(classification, 0)
        if actual != expected.get(classification):
            issue(
                "classification_count_mismatch",
                f"{classification}: actual={actual} expected={expected.get(classification)}",
            )
    if len(production_items) != expected.get("production_existing"):
        issue(
            "capability_count_mismatch",
            f"production_inventory={len(production_items)} "
            f"expected={expected.get('production_existing')}",
        )
    if len(production_capability_ids) != expected.get("production_existing"):
        issue(
            "production_capability_count_mismatch",
            f"actual={len(production_capability_ids)} "
            f"expected={expected.get('production_existing')}",
        )
    if len(capabilities) != expected.get("inventory_items"):
        issue(
            "total_capability_count_mismatch",
            f"actual={len(capabilities)} expected={expected.get('inventory_items')}",
        )
    for disposition in PRODUCTION_DISPOSITIONS:
        if dispositions.get(disposition, 0) != expected.get(disposition):
            issue(
                "disposition_count_mismatch",
                f"{disposition}: actual={dispositions.get(disposition, 0)} "
                f"expected={expected.get(disposition)}",
            )
    expected_groups = expected.get("by_group", {})
    for group_name in sorted(set(groups) | set(expected_groups)):
        if group_counts.get(group_name, 0) != expected_groups.get(group_name):
            issue(
                "group_count_mismatch",
                f"{group_name}: actual={group_counts.get(group_name, 0)} "
                f"expected={expected_groups.get(group_name)}",
            )
    expected_inventory_groups = expected.get("inventory_by_group", {})
    for group_name in sorted(set(groups) | set(expected_inventory_groups)):
        if inventory_group_counts.get(group_name, 0) != expected_inventory_groups.get(
            group_name
        ):
            issue(
                "inventory_group_count_mismatch",
                f"{group_name}: actual={inventory_group_counts.get(group_name, 0)} "
                f"expected={expected_inventory_groups.get(group_name)}",
            )
    if len(unmapped_inventory_items) != expected.get("unmapped_inventory_items"):
        issue(
            "unmapped_inventory_count_mismatch",
            f"actual={len(unmapped_inventory_items)} "
            f"expected={expected.get('unmapped_inventory_items')}",
        )
    if baseline.get("discovery", {}).get("secret_value_hits") != expected.get(
        "secret_value_hits"
    ):
        issue(
            "secret_value_count_mismatch",
            f"discovery={baseline.get('discovery', {}).get('secret_value_hits')} "
            f"expected={expected.get('secret_value_hits')}",
        )

    expected_coverage_text = bind_coverage(
        coverage_text, baseline_normative_digest
    )
    if args.update_report:
        write_if_changed(COVERAGE, expected_coverage_text)
        coverage_text = expected_coverage_text
    elif coverage_text != expected_coverage_text:
        issue("coverage_binding_stale", baseline_normative_digest)

    baseline_rel = "docs/product-experience-redesign/settings-capability-baseline.json"
    if baseline_rel not in freeze_text:
        issue("baseline_missing_from_freeze", baseline_rel)
    if (
        f"settings_capability_baseline: {baseline_normative_digest}"
        not in freeze_text
    ):
        issue("baseline_digest_missing_from_freeze", baseline_normative_digest)

    source_snapshot_blockers = {
        "invalid_baseline_revision",
        "invalid_baseline_revision_field",
        "git_head_unavailable",
        "baseline_revision_stale",
        "invalid_source_manifest",
        "invalid_source_manifest_entry",
        "invalid_source_id",
        "invalid_source_role",
        "duplicate_source_id",
        "invalid_source_path",
        "invalid_source_sha256",
        "source_path_escape",
        "missing_source_file",
        "stale_source_snapshot",
        "symbol_resolution_failure",
        "duplicate_source_paths",
        "invalid_group_defaults",
        "invalid_source_inventories",
        "invalid_source_inventory",
        "group_inventory_mismatch",
        "empty_source_inventory",
        "duplicate_group_source",
        "unknown_group_source",
        "unused_manifest_sources",
    }
    source_snapshot_verified = (
        len(manifest) == len(manifest_by_id) == len(valid_source_ids)
        and set(referenced_source_ids) == set(manifest_by_id)
        and not any(
            item["code"] in source_snapshot_blockers for item in issues
        )
    )
    source_inventory_proof_passed = (
        source_snapshot_verified
        and source_audited_count == len(inventory_items)
        and not symbol_resolution_failures
        and not unmapped_inventory_items
        and not duplicate_mappings
        and not orphan_capabilities
        and not empty_provenance
        and not non_production_basis_failures
        and not missing_item_source_evidence
        and not invalid_item_source_evidence
        and not incomplete_contract_dimensions
        and not placeholder_contracts
        and not group_contract_placeholders
        and not meta_state_only_contracts
        and not invalid_apply_modes
        and not invalid_operation_failure_modes
        and not unproven_atomicity_claims
    )
    source_inventory_proof = {
        "status": "passed" if source_inventory_proof_passed else "failed",
        "source_snapshot_verified": source_snapshot_verified,
        "inventory_items": len(inventory_items),
        "item_source_evidenced_count": item_source_evidenced_count,
        "contract_complete_count": contract_complete_count,
        "symbol_resolution_failures": sorted(set(symbol_resolution_failures)),
        "missing_item_source_evidence": sorted(
            set(missing_item_source_evidence)
        ),
        "invalid_item_source_evidence": sorted(
            set(invalid_item_source_evidence)
        ),
        "incomplete_contract_dimensions": dict(
            sorted(incomplete_contract_dimensions.items())
        ),
        "placeholder_contracts": dict(sorted(placeholder_contracts.items())),
        "group_contract_placeholders": sorted(
            set(group_contract_placeholders)
        ),
        "meta_state_only_contracts": sorted(
            set(meta_state_only_contracts)
        ),
        "invalid_apply_modes": sorted(set(invalid_apply_modes)),
        "invalid_operation_failure_modes": sorted(
            set(invalid_operation_failure_modes)
        ),
        "unproven_atomicity_claims": sorted(
            set(unproven_atomicity_claims)
        ),
        "unmapped": unmapped_inventory_items,
        "duplicate_mapping": duplicate_mappings,
        "orphan_capabilities": orphan_capabilities,
        "empty_provenance": sorted(set(empty_provenance)),
        "non_production_basis_failures": sorted(
            set(non_production_basis_failures)
        ),
        "provenance_counts": {
            "production_existing": inventory_classifications.get(
                "production_existing", 0
            ),
            "new_requirement": inventory_classifications.get("new_requirement", 0),
            "planned_demo": inventory_classifications.get("planned_demo", 0),
        },
        "disposition_counts": dict(sorted(inventory_dispositions.items())),
    }

    inventory_digest = canonical_digest(inventory_items)
    production_inventory_digest = canonical_digest(production_items)
    report = {
        "schema_version": "settings-capability-gate.v3",
        "status": "passed" if not issues else "failed",
        "mode": "update_report" if args.update_report else "check",
        "stage": "source_audit_and_design_traceability",
        "source_manifest": {
            "revision": revision,
            "manifest_count": len(manifest),
            "manifest_digest": source_manifest_digest,
            "resolved_source_count": len(valid_source_ids),
            "declared_symbol_locator_count": sum(
                len(entry.get("symbol_locators", []))
                for entry in manifest
                if isinstance(entry, dict)
                and isinstance(entry.get("symbol_locators"), list)
            ),
            "stale_source_files": stale_source_files,
            "unused_source_ids": unused_manifest_sources,
        },
        "source_inventory_proof": source_inventory_proof,
        "settings_capabilities": {
            "baseline_digest": baseline_digest,
            "baseline_normative_digest": baseline_normative_digest,
            "inventory_digest": inventory_digest,
            "production_inventory_digest": production_inventory_digest,
            "inventory_count": len(inventory_items),
            "production_existing_count": len(production_items),
            "new_requirement_count": inventory_classifications.get(
                "new_requirement", 0
            ),
            "planned_demo_count": inventory_classifications.get("planned_demo", 0),
            "source_audited_count": source_audited_count,
            "production_source_audited_count": production_source_audited_count,
            "item_source_evidenced_count": item_source_evidenced_count,
            "production_item_source_evidenced_count": (
                production_item_source_evidenced_count
            ),
            "contract_complete_count": contract_complete_count,
            "production_contract_complete_count": (
                production_contract_complete_count
            ),
            "baseline_count": len(production_capability_ids),
            "mapped_count": mapped_count,
            "inventory_mapped_count": inventory_mapped_count,
            "prototype_mapped_count": prototype_mapped_count,
            "planned_product_target_count": planned_product_target_count,
            "verified_count": 0,
            "verified_count_note": (
                "Production verification intentionally remains zero until M5 "
                "executes SettingsScene -> IPC -> Host -> persistence evidence."
            ),
            "dispositions": {
                name: dispositions.get(name, 0)
                for name in baseline.get("allowed_dispositions", [])
            },
            "by_group": dict(sorted(group_counts.items())),
            "inventory_by_group": dict(sorted(inventory_group_counts.items())),
            "inventory_dispositions": dict(sorted(inventory_dispositions.items())),
            "unmapped_inventory_items": unmapped_inventory_items,
            "duplicate_mappings": duplicate_mappings,
            "orphan_capabilities": orphan_capabilities,
            "missing_targets": missing_targets,
            "missing_assertions": sorted(set(missing_assertions)),
            "prototype_only_evidence": sorted(set(prototype_only_evidence)),
            "invalid_merges": sorted(set(invalid_merges)),
            "invalid_migrations": sorted(set(invalid_migrations)),
            "unauthorized_retirements": unauthorized_retirements,
        },
        "coverage_binding": {
            "baseline_normative_digest": baseline_normative_digest,
            "coverage_digest": digest_bytes(coverage_text.encode("utf-8")),
        },
        "traceability": traceability,
        "issue_summary": dict(
            sorted(Counter(item["code"] for item in issues).items())
        ),
        "issues": issues,
    }
    if args.update_report:
        write_if_changed(
            REPORT,
            json.dumps(report, ensure_ascii=False, indent=2) + "\n",
        )
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0 if not issues else 1

if __name__ == "__main__":
    sys.exit(main())
