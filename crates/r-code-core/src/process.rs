//! Cross-platform process configuration shared by desktop-side command runners.

use std::process::Command;

/// Prevent a background console process from creating a visible terminal window.
///
/// R-Code captures these commands' output inside its own UI. On Windows, a GUI
/// process must opt out of console creation explicitly or short-lived helpers
/// such as `cmd.exe`, `git.exe`, and `taskkill.exe` flash on the desktop.
#[cfg(windows)]
pub fn hide_background_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_background_console(_command: &mut Command) {}
