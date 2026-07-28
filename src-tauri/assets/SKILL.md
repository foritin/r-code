# R-Code Collaboration

R-Code can be available as a local MCP server named `r-code`. Use it only when
the user asks you to delegate a focused investigation to R-Code's own Agent.

## Safe delegation flow

1. Work only with a `workspace_path` that the user has already opened in
   R-Code. Do not guess paths or ask R-Code to inspect a different folder.
2. Call `r_code_delegate_readonly` with a concrete `goal`.
3. If the result is still running, call `r_code_wait_for_result` or
   `r_code_task_status` using the returned `task_id`.
4. Use the returned concise result in your answer. Do not treat it as permission
   to write files, run commands, or delegate additional work in that workspace.

## Capability boundary

The R-Code MCP service creates a read-only task. Its Agent can inspect the
already-approved workspace, but cannot edit files, run shell commands, create
terminals, or spawn more agents. It never exposes provider credentials or
Codex credentials.

Use `r_code_cancel_task` only to stop a task that this same MCP session started.
Do not attempt to call undocumented `terminal.*`, `ControlDoor`, or environment
variable interfaces; they are not part of the R-Code MCP contract.
