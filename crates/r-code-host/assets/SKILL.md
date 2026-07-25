# R-Code Terminal Control

You have access to terminal control primitives via the R-Code ControlDoor.

## Available Commands

- `terminal.list` - List available terminals
- `terminal.read <id>` - Read terminal output
- `terminal.send <id> <text>` - Send text to terminal
- `terminal.create <shell> <dir>` - Create new terminal
- `terminal.wait <id>` - Wait for terminal state
- `terminal.kill <id>` - Kill terminal (not available for agents)

## Environment Variables

- `R_CODE_TERM_ID` - Your terminal ID
- `R_CODE_CTL` - Control socket path
- `R_CODE_CTL_TOKEN` - Authentication token
