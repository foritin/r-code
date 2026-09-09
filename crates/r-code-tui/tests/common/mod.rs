use std::io::Read;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

pub fn run_current_test_in_pty(
    test_name: &str,
    child_env: &str,
    size: PtySize,
    marker: &str,
    timeout: Duration,
) -> Result<String, String> {
    let pair = native_pty_system()
        .openpty(size)
        .map_err(|error| format!("open PTY: {error}"))?;
    let executable = std::env::current_exe()
        .map_err(|error| format!("locate current test executable: {error}"))?;
    let mut command = CommandBuilder::new(executable);
    command.args(["--exact", test_name, "--nocapture"]);
    command.env(child_env, "1");
    command.env("RUST_BACKTRACE", "0");
    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| format!("spawn PTY child: {error}"))?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| format!("clone PTY reader: {error}"))?;
    let reader_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).map(|_| bytes)
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                drop(pair.master);
                let output = reader_thread
                    .join()
                    .ok()
                    .and_then(Result::ok)
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .unwrap_or_default();
                return Err(format!(
                    "PTY child timed out after {} ms; output: {output:?}",
                    timeout.as_millis()
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                drop(pair.master);
                let _ = reader_thread.join();
                return Err(format!("wait for PTY child: {error}"));
            }
        }
    };
    drop(pair.master);
    let bytes = reader_thread
        .join()
        .map_err(|_| "PTY reader thread panicked".to_string())?
        .map_err(|error| format!("read PTY output: {error}"))?;
    let output = String::from_utf8_lossy(&bytes).into_owned();
    if !status.success() {
        return Err(format!(
            "PTY child exited with code {}; output: {output:?}",
            status.exit_code()
        ));
    }
    if !output.contains(marker) {
        return Err(format!(
            "PTY child exited before marker {marker:?}; output: {output:?}"
        ));
    }
    Ok(output)
}
