//! Small, explicit desktop integrations that should not require broad WebView permissions.
//!
//! Paths are passed as individual process arguments. Nothing in this module is evaluated by a
//! shell, so spaces and shell metacharacters stay data rather than becoming commands.

use std::path::Path;
use std::process::Command;

/// A physical desktop rectangle. Keeping this type independent from Tauri makes the placement
/// policy deterministic and unit-testable on every host platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

const WORKBENCH_TARGET_WIDTH: f64 = 1680.0;
const WORKBENCH_EDGE_MARGIN: f64 = 24.0;

/// Grow a normal window to make room for a docked right-hand workbench.
///
/// The left edge is preserved whenever the monitor has enough room on the right. Only the
/// smallest necessary left shift is applied when it does not. Repeated calls are idempotent.
pub fn workbench_window_rect(
    current: DesktopRect,
    work_area: DesktopRect,
    scale_factor: f64,
) -> Option<DesktopRect> {
    if !scale_factor.is_finite() || scale_factor <= 0.0 || work_area.width == 0 {
        return None;
    }

    let margin = (WORKBENCH_EDGE_MARGIN * scale_factor).round().max(0.0) as i64;
    let work_left = i64::from(work_area.x);
    let work_right = work_left + i64::from(work_area.width);
    let usable_width = (i64::from(work_area.width) - margin.saturating_mul(2)).max(1);
    let desired_width = (WORKBENCH_TARGET_WIDTH * scale_factor).round() as i64;
    let target_width = desired_width
        .min(usable_width)
        .max(i64::from(current.width));

    if target_width <= i64::from(current.width) {
        return None;
    }

    let right_limit = work_right - margin;
    let current_left = i64::from(current.x);
    let target_left = if current_left + target_width <= right_limit {
        current_left
    } else {
        (right_limit - target_width).max(work_left + margin)
    };

    Some(DesktopRect {
        x: target_left.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        y: current.y,
        width: target_width.clamp(1, i64::from(u32::MAX)) as u32,
        height: current.height,
    })
}

/// Reveal a local path in the platform file manager.
pub fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("path does not exist: {}", path.display()));
    }

    reveal_platform(path)
}

#[cfg(target_os = "windows")]
fn reveal_platform(path: &Path) -> Result<(), String> {
    let mut command = Command::new("explorer.exe");
    if path.is_dir() {
        command.arg(path);
    } else {
        // Explorer accepts the selector as its own argument; the path remains a separate OS
        // argument and therefore does not need manual quoting.
        command.arg("/select,").arg(path);
    }
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("cannot open File Explorer: {error}"))
}

#[cfg(target_os = "macos")]
fn reveal_platform(path: &Path) -> Result<(), String> {
    let mut command = Command::new("open");
    if path.is_file() {
        command.arg("-R");
    }
    command
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("cannot open Finder: {error}"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn reveal_platform(path: &Path) -> Result<(), String> {
    // Linux desktops do not expose one reliable cross-DE file-selection protocol. Opening the
    // containing directory is the portable behavior; GIO is preferred and xdg-open is the
    // broadly available fallback.
    let directory = if path.is_dir() {
        path
    } else {
        path.parent()
            .ok_or_else(|| format!("cannot determine parent directory: {}", path.display()))?
    };
    match Command::new("gio").arg("open").arg(directory).spawn() {
        Ok(_) => Ok(()),
        Err(gio_error) => Command::new("xdg-open")
            .arg(directory)
            .spawn()
            .map(|_| ())
            .map_err(|xdg_error| {
                format!("cannot open file manager (gio: {gio_error}; xdg-open: {xdg_error})")
            }),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
fn reveal_platform(path: &Path) -> Result<(), String> {
    Err(format!(
        "revealing paths is not supported on this platform: {}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_to_the_right_when_space_is_available() {
        let current = DesktopRect {
            x: 100,
            y: 80,
            width: 1200,
            height: 800,
        };
        let work = DesktopRect {
            x: 0,
            y: 0,
            width: 2200,
            height: 1200,
        };
        let target = workbench_window_rect(current, work, 1.0).unwrap();
        assert_eq!(target.x, 100);
        assert_eq!(target.width, 1680);
    }

    #[test]
    fn shifts_left_only_when_the_right_edge_is_too_close() {
        let current = DesktopRect {
            x: 600,
            y: 80,
            width: 1200,
            height: 800,
        };
        let work = DesktopRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let target = workbench_window_rect(current, work, 1.0).unwrap();
        assert_eq!(target.x, 216);
        assert_eq!(target.width, 1680);
    }

    #[test]
    fn supports_negative_monitor_coordinates_and_small_displays() {
        let current = DesktopRect {
            x: -1500,
            y: 40,
            width: 1100,
            height: 760,
        };
        let work = DesktopRect {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let target = workbench_window_rect(current, work, 1.0).unwrap();
        assert_eq!(target.x, -1704);
        assert_eq!(target.width, 1680);

        let small = DesktopRect {
            x: 0,
            y: 0,
            width: 1366,
            height: 768,
        };
        let clamped = workbench_window_rect(
            DesktopRect {
                x: 100,
                y: 20,
                width: 960,
                height: 640,
            },
            small,
            1.0,
        )
        .unwrap();
        assert_eq!(clamped.x, 24);
        assert_eq!(clamped.width, 1318);
    }

    #[test]
    fn is_idempotent_after_reaching_target_width() {
        let current = DesktopRect {
            x: 100,
            y: 80,
            width: 1680,
            height: 800,
        };
        let work = DesktopRect {
            x: 0,
            y: 0,
            width: 2200,
            height: 1200,
        };
        assert_eq!(workbench_window_rect(current, work, 1.0), None);
    }
}
