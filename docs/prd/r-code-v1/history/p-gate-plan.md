# R-Code v1 unified execution — P-GATE implementation tranche

This tranche implements the dependency-complete foundation required before any
write-capable planning or parallel execution is activated. It preserves the
existing Harness/API v1 rename and editable default prompt work, then freezes
provider, prompt, workspace, permissions and plan identity into durable run
snapshots. It finishes with a read-only PRD planning and exact-revision approval
vertical slice. M-GATE and O-GATE remain disabled until their later tranches.

The authoritative product constraints are the user-approved R-Code v1 plan in
this task: daemon-owned settings, current-checkout execution, no automatic Git
writes, fail-closed capabilities, exact PlanRevision approval, CAS-safe recovery
and no public v2 identifiers.
