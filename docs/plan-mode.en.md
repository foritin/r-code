# Plan mode, human confirmation, and enhanced review

This document describes R-Code's Plan workflow and its safety boundaries. It is intended for users and maintainers and reflects the current implementation and tests.

## Workflow

1. Enable Goal from the composer's Add menu and enter it in the main composer. Send persists the Goal and starts the Agent immediately; there is no intermediate “save Goal, then write the first request” state. An ordinary conversation's first task prompt is not mislabeled as a configured Goal, and upgraded conversations default to no explicit Goal. The removable Goal chip exits Goal input, while an active Goal can be edited, stopped/resumed, or deleted above the composer.
2. The native R-Code Agent investigates with read-only workspace tools. Plan mode disables file writes, shell commands, mutating MCP tools, and subagent delegation.
3. When information is missing, the Agent calls `request_user_input` with 1–3 structured questions. Each question offers 2–3 mutually exclusive choices and accepts a separate free-text answer; the user may also skip the complete set.
4. Once the question set is persisted, the current run pauses immediately. The runtime rejects later tool calls from that run and does not start quality review or mark the task review-ready early.
5. Answers are stored atomically with an idempotency key and resume the same task's Plan session. Failed continuations can be retried explicitly.
6. The Agent publishes executable leaf items. They may declare real dependencies and use `section_path` for phases/subphases; the UI and Markdown projection derive labels such as 1, 1.1, and 1.2. Section headings are not executable todos.
7. After user approval, R-Code pins the approved revision, turns its items into todos, and hands implementation to a durable queue. Task mode, the unique queue message, and dispatch state are committed in one SQLite transaction. The queue survives restart, and failed dispatches expose an explicit retry action in the Plan panel.

Plan v1 is orchestrated only by the native R-Code Agent. Codex CLI may still be used as a configured collaborator during normal implementation, but it cannot be the primary Plan runtime because that would bypass the host's pause and idempotency protocol.

The product surface exposes only **Agent** and **Plan** interaction modes. Persisted `ask`, `edit`, and `auto` values are compatibility and execution policies, not three additional user modes; workspace access and the selected main Agent remain orthogonal. Before any write, Agent may use a host tool to enter Plan safely; the host atomically creates the Plan and resumes the same request. Plan returns to Agent implementation only after user approval. Each mode exposes only lifecycle tools valid in that state, so Agent cannot accidentally select `plan_publish`.

## Persistence and file location

SQLite is the source of truth for Plans, questions, answers, items, change attribution, and review decisions. Every Plan receives a random stable ID; model-provided item and question IDs are scoped to their Plan or question set.

The task-scoped Plan workbench derives completed, active, pending, blocked, and failed counts from persisted executable leaves. Presentation-only section headings never affect progress or dependencies. While SQLite still exposes an `in_progress` item, a host-owned continuation gate prevents an ordinary model answer from ending the run and requires `plan_item_update` after acceptance checks. Up to three independent read-only investigation or verification subtasks may run in parallel within the active leaf, but the main Agent must collect them before advancing and remains the single owner of potentially overlapping writes.

R-Code also produces a human-readable Markdown projection in the operating system's application-data directory:

```text
<AppData>/r-code/plans/<plan-id>/plan.md
```

Later revisions of the same Plan atomically replace that Plan's stable projection and cannot overwrite another Plan. A projection failure is recorded without corrupting SQLite state and can be repaired from the UI. Plan documents are never written into the project directory or tracked by project Git.

The user may cancel an unfinished Plan. Cancellation terminates pending questions, unfinished items, and later implementation dispatch, then restores the task's normal workspace mode. It **does not** revert files already written to the workspace. Cancellation fails closed while a main run or enhanced-review rollback is active. Re-entering Plan mode after cancellation creates a new Plan ID and projection.

## Normal and enhanced review

The review panel exposes two deliberately separate views:

- **Normal** uses the current Git working-tree changes as its boundary, follows `.gitignore`, and supports file/line acceptance or rejection. Acceptance only updates R-Code's review ledger; it never runs `git add`, commit, or push automatically.
- **Enhanced** shows only changes attributed to the currently approved Plan, grouped by feature. A user can decide an individual file or a complete feature group. Unrelated Git changes never appear here.

When a task has no corresponding feature Plan, Enhanced review deliberately stays empty and points the user to Normal review. It never guesses that changes from an ordinary task, Shell, MCP, or an external agent belong to a feature.

Enhanced review records before/after snapshots when a trusted write tool completes and binds the event to the host's active feature item. Diffs remain visible while an item is `in_progress` or recoverably `blocked`, but decisions remain disabled until the item becomes terminal (`completed`, `failed`, or `cancelled`). This prevents accepted features from receiving later unreviewed writes. A blocked item pauses Plan writes until the model explicitly resumes it with `plan_item_update(in_progress)`.

Only direct R-Code `edit`, `apply_patch`, `create_file`, and `delete_file` operations receive trusted feature attribution. Writes made through shell, MCP, or external agents cannot be attributed safely and therefore appear only in normal Git review rather than being mislabeled as a feature change.

Multiple features may edit interleaved lines in one file. Rejecting feature A performs a reverse three-way merge using A's before/after snapshots and the current file, so it removes only changes A can prove it owns while preserving later changes from feature B. Conflicts fail closed and leave the file unchanged for manual resolution.

## Concurrency, recovery, and data safety

- Database transactions never span model waits or filesystem I/O.
- Trusted write capture and rejection share a per-path coordinator. Multi-file operations lock canonical paths in sorted order to avoid lock-order inversion.
- Multi-file rejection computes every target first; any conflict prevents all writes.
- A durable rejection journal and rollback blobs are saved before mutation. Partial I/O failures are rolled back, and startup resumes unfinished recovery.
- Workspace paths are canonicalized and checked again at the boundary. Existing-file reads, replacements, and deletes use a workspace-directory capability fixed when the operation starts, and Plan's tracked-write wrapper forwards that capability to its inner tool, so symbolic links cannot escape the workspace.
- Review blobs and ledgers live in AppData. R-Code does not create private Plan or memory metadata in the project directory.
- Permanently deleting a task or forgetting a project releases referenced blobs transactionally and removes only trusted UUID Plan directories. Startup retries orphan cleanup, and database-provided projection paths are never trusted as deletion targets.

## State overview

Plan states progress through `draft`, `awaiting_input`, `ready`, and `executing`, then finish as `completed` or `cancelled`. Approval atomically pins the revision, activates the first feature, and enters `executing`; the persisted `approved` value is retained only for recovery compatibility. Feature items use `proposed`, `pending`, `in_progress`, `blocked`, `completed`, `failed`, and `cancelled`. The UI reads current SQLite state and never infers status from the Markdown projection.
