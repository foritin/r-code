# R-Code v1 Safety Boundary — Operator Runbook

This runbook covers the safety boundary of the shipped product: what the
helper binaries are, what SafeDisabled means for an installation, how to
read the activation readiness, and what to do when a workspace lands in
quarantine. The release policy (P32) enforces this automatically; this
document is what a human operator reads when something needs judgement.

## Prerequisites: the helper binaries

Every installation ships two sidecars beside the daemon:

- `r-code-process-guardian` — the process-tree guardian that owns launch
  gating and tree containment.
- `r-code-safety-probe` — the deny-probe binary the platform safety
  report executes to prove (or refuse) sandbox capabilities.

The daemon resolves these ONLY from its own installed directory (or an
explicit `--helper-dir` override): it never searches `PATH`. A helper that
is missing, truncated, or carries a non-native executable magic fails
verification and the capability that needs it stays **SafeDisabled** — the
product refuses to fall back to running anything unsandboxed.

If an antivirus quarantines a helper, the installation is expected to run
with that capability hidden until the helper is restored; do not "fix" this
by placing a substitute binary in the directory — the digest and magic
checks will treat it as tampered.

## What SafeDisabled means

**SafeDisabled is a shipping posture, not a failure.** When the platform
safety report cannot prove a sandbox Activated (no probes passed, helper
unavailable, or the platform has no backend this release), the product:

- hides the capabilities that would need it (the O-GATE parallel path,
  required Checks, interactive Processes, and the Shell tool are
  undiscoverable in the UI — not merely disabled);
- keeps read-only workflows fully functional;
- publishes the exact readiness on the daemon's boot log:
  `r-code-service: activation readiness: recovered=... granted=[...] verdict=...`.

macOS in this release ships SafeDisabled for write execution by design;
the write surfaces are hidden. A future release may advertise write targets
on platforms that prove **Activated** — the release policy refuses to ship
an advertised-write target whose probes did not pass. There is no
configuration flag that overrides this: SafeDisabled never counts as
Activated.

## Quarantine: when a workspace is fenced

A workspace (or lease) lands in **quarantine** when a process tree could
not be PROVEN dead — a crash mid-effect, a termination whose proof did
not arrive, or an incomplete process-effect operation found at the next
boot. Quarantine is durable: it survives daemon restarts, and it blocks
writes (reads stay available for diagnosis).

The recovery path:

1. Read the diagnostics: `safety.quarantine.get` (read-only) shows the
   trees, their reasons, and their proof status.
2. Confirm the processes are actually gone (the OS process list is the
   truth; the daemon only records what it proved).
3. Only then use `safety.quarantine.retry` with an explicit actor and
   session — it requires a changed-boot or proof-backed reason and is
   fully audited. Never delete the store rows by hand.

## Downgrade policy

A downgrade to an older release that predates a helper or a safety-report
format must go through a fresh boot: the older daemon re-evaluates the
platform report under its own material, refuses what it cannot verify, and
quarantines any incomplete effects it finds before accepting writes. Do
not downgrade by copying a newer store file over an older installation —
the store schema migration is one-way, and a rejected migration leaves the
older daemon refusing to start rather than running unverified.
