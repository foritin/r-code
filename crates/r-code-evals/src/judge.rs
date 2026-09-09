//! Judge 抽象与确定性 Judge（docs/pi-alignment PRD §4.1 R-EVL-02 / M2-02）。
//!
//! [`Judge`] = `scoringFn -> { score ∈ [0,1], rationale }`。失败原因**累积**
//! （rationale 汇总全部 failures，不折叠成单一布尔）；确定性规则优先，LLM
//! Judge 以同签名评分函数注入（[`create_judge`] 不关心函数由谁实现）。

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use sha2::{Digest, Sha256};

use crate::{EvalInput, EvalRunResult};

/// 一次打分结论。
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeVerdict {
    /// [0, 1]；1 = 完全通过（配对统计的 pass 判据 = score >= 1）。
    pub score: f64,
    /// 人可读依据（汇总失败原因）。
    pub rationale: String,
    /// 累积的失败原因（空 = 无失败）。
    pub failures: Vec<String>,
}

impl JudgeVerdict {
    /// 全通过。
    pub fn pass(rationale: impl Into<String>) -> Self {
        Self {
            score: 1.0,
            rationale: rationale.into(),
            failures: Vec::new(),
        }
    }

    /// 从累积失败原因合成结论：每条失败扣一档（n 条失败 → max(0, 1-n/权重)）。
    pub fn from_failures(failures: Vec<String>, total_checks: usize) -> Self {
        let total = total_checks.max(failures.len());
        let score = if total == 0 {
            0.0
        } else {
            (total - failures.len()) as f64 / total as f64
        };
        let rationale = if failures.is_empty() {
            format!("{total}/{total} checks passed")
        } else {
            format!(
                "{}/{} checks passed; failures: {}",
                total - failures.len(),
                total,
                failures.join("; ")
            )
        };
        Self {
            score,
            rationale,
            failures,
        }
    }
}

/// 打分函数入参：输入、结果、（可选）fixture 原貌（改动面对比基准）。
pub struct JudgeInput<'a> {
    pub input: &'a EvalInput,
    pub result: &'a EvalRunResult,
    /// fixture 目录（输入未带 fixture 时为 None）。
    pub fixture: Option<&'a Path>,
}

/// 评分函数（确定性规则或 LLM 评注器共用同一签名——LLM 扩展点）。
pub type ScoringFn = Arc<dyn Fn(&JudgeInput<'_>) -> JudgeVerdict + Send + Sync>;

/// Judge：命名 + 评分函数。
pub struct Judge {
    pub name: String,
    scoring: ScoringFn,
}

/// 构造 Judge（LLM Judge 扩展点 = 传入自实现的 ScoringFn，签名即合同）。
pub fn create_judge(name: impl Into<String>, scoring: ScoringFn) -> Judge {
    Judge {
        name: name.into(),
        scoring,
    }
}

impl Judge {
    /// 打分（每个 Judge 内部确定性可复现）。
    pub fn score(&self, input: &JudgeInput<'_>) -> JudgeVerdict {
        (self.scoring)(input)
    }
}

/// 内置确定性 Judge #1：测试通过率——在工作区运行 `verify.mjs`
/// （plan_eval 金集约定：每个 case 携带冻结验证脚本），exit 0 = 通过。
/// 工作区没有 verify.mjs 视为不可打分（score 0 + 失败原因，不臆造通过）。
pub fn test_pass_judge() -> Judge {
    create_judge(
        "test-pass",
        Arc::new(|input| {
            let mut failures = Vec::new();
            let verify = input.result.workspace.join("verify.mjs");
            if let Err(error) = ensure_regular_file(&verify) {
                failures.push(format!(
                    "invalid verifier script {}: {error}",
                    verify.display()
                ));
            } else {
                let mut command = Command::new("node");
                command
                    .arg("verify.mjs")
                    .current_dir(&input.result.workspace);
                let output = command_output_with_timeout(&mut command, Duration::from_secs(60));
                match output {
                    Ok(finished) if finished.status.success() => {}
                    Ok(finished) => failures.push(format!(
                        "verify.mjs exited with {}: {}",
                        finished.status,
                        String::from_utf8_lossy(&finished.stderr)
                            .chars()
                            .take(300)
                            .collect::<String>()
                    )),
                    Err(error) => failures.push(format!("run node verify.mjs: {error}")),
                }
            }
            // 未收敛的 run 直接判失败（stopReason 非 stop 不得计通过）。
            if !input.result.stop_reason.is_settled() {
                failures.push(format!(
                    "run did not settle: {:?}",
                    input.result.stop_reason
                ));
            }
            JudgeVerdict::from_failures(failures, 2)
        }),
    )
}

/// 内置确定性 Judge #2：改动面——相对 fixture，只允许改声明过的路径
/// （`allowed_changes`）；越界路径逐条累积。
pub fn focus_judge(allowed_changes: Vec<String>) -> Judge {
    create_judge(
        "focus",
        Arc::new(move |input| {
            let mut failures = Vec::new();
            let Some(fixture) = input.fixture else {
                return JudgeVerdict::from_failures(
                    vec!["focus judge requires a fixture baseline".to_string()],
                    1,
                );
            };
            let changed_paths = match changed_paths(fixture, &input.result.workspace) {
                Ok(changed_paths) => changed_paths,
                Err(error) => {
                    return JudgeVerdict::from_failures(
                        vec![format!("unable to compare workspace changes: {error}")],
                        1,
                    );
                }
            };
            for changed in changed_paths {
                if !allowed_changes.iter().any(|allowed| {
                    changed == *allowed || changed.starts_with(&format!("{allowed}/"))
                }) {
                    failures.push(format!("unexpected workspace change: {changed}"));
                }
            }
            let total = failures.len() + 1;
            JudgeVerdict::from_failures(failures, total)
        }),
    )
}

/// 内置确定性 Judge #3：测试文件完整性——fixture 里的测试文件（verify.mjs /
/// *.test.* / tests/ 目录）在结果工作区必须原样存在（不被删除或改写）。
pub fn integrity_judge() -> Judge {
    create_judge(
        "integrity",
        Arc::new(|input| {
            let mut failures = Vec::new();
            let Some(fixture) = input.fixture else {
                return JudgeVerdict::from_failures(
                    vec!["integrity judge requires a fixture baseline".to_string()],
                    1,
                );
            };
            let is_test_file = |path: &Path| -> bool {
                let text = path.to_string_lossy().replace('\\', "/");
                text.ends_with("verify.mjs")
                    || text.contains(".test.")
                    || text.starts_with("tests/")
                    || text.contains("/tests/")
            };
            let mut checked = 0usize;
            let protected_files = match test_files(fixture, &is_test_file) {
                Ok(files) => files,
                Err(error) => {
                    return JudgeVerdict::from_failures(
                        vec![format!("unable to enumerate protected test files: {error}")],
                        1,
                    );
                }
            };
            for relative in protected_files {
                checked += 1;
                let before = read_regular_file(&fixture.join(&relative));
                let after = read_regular_file(&input.result.workspace.join(&relative));
                match (before, after) {
                    (Ok(before), Ok(after)) if before == after => {}
                    (Ok(_), Ok(_)) => {
                        failures.push(format!("test file modified: {relative}"));
                    }
                    (Err(_), _) => {
                        failures.push(format!("test file unreadable in fixture: {relative}"));
                    }
                    (Ok(_), Err(_)) => {
                        failures.push(format!("test file missing or not regular: {relative}"));
                    }
                }
            }
            if checked == 0 {
                failures.push("fixture contains no test files to protect".to_string());
            }
            JudgeVerdict::from_failures(failures, checked.max(1))
        }),
    )
}

fn ensure_regular_file(path: &Path) -> Result<(), String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| format!("read metadata: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err("expected a regular file without symbolic links".to_string());
    }
    Ok(())
}

fn read_regular_file(path: &Path) -> Result<Vec<u8>, String> {
    ensure_regular_file(path)?;
    std::fs::read(path).map_err(|error| format!("read file: {error}"))
}

/// 相对 fixture 的改动路径集合（相对路径，`/` 分隔；仅在结果侧存在的文件也算改动）。
fn changed_paths(fixture: &Path, workspace: &Path) -> Result<Vec<String>, String> {
    let before = snapshot_tree(fixture)?;
    let after = snapshot_tree(workspace)?;
    let mut changed = Vec::new();
    for (relative, digest) in &after {
        match before.get(relative) {
            Some(before_digest) if before_digest == digest => {}
            _ => changed.push(relative.clone()),
        }
    }
    for relative in before.keys() {
        if !after.contains_key(relative) {
            changed.push(relative.clone());
        }
    }
    Ok(changed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotEntry {
    Directory,
    File([u8; 32]),
    Symlink(PathBuf),
}

/// 对目录做确定性、失败即报错的快照。目录符号链接只记录链接本身，绝不跟随。
fn snapshot_tree(root: &Path) -> Result<BTreeMap<String, SnapshotEntry>, String> {
    fn visit(
        dir: &Path,
        prefix: &str,
        snapshot: &mut BTreeMap<String, SnapshotEntry>,
    ) -> Result<(), String> {
        let entries = std::fs::read_dir(dir)
            .map_err(|error| format!("read directory {}: {error}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!("read directory entry under {}: {error}", dir.display())
            })?;
            let name = entry.file_name().into_string().map_err(|name| {
                format!(
                    "path under {} is not valid UTF-8: {}",
                    dir.display(),
                    name.to_string_lossy()
                )
            })?;
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("read metadata for {}: {error}", path.display()))?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                let target = std::fs::read_link(&path)
                    .map_err(|error| format!("read symlink {}: {error}", path.display()))?;
                snapshot.insert(relative, SnapshotEntry::Symlink(target));
            } else if file_type.is_dir() {
                snapshot.insert(relative.clone(), SnapshotEntry::Directory);
                visit(&path, &relative, snapshot)?;
            } else if file_type.is_file() {
                let mut file = std::fs::File::open(&path)
                    .map_err(|error| format!("open file {}: {error}", path.display()))?;
                let mut hasher = Sha256::new();
                let mut buffer = [0_u8; 64 * 1024];
                loop {
                    let read = file
                        .read(&mut buffer)
                        .map_err(|error| format!("read file {}: {error}", path.display()))?;
                    if read == 0 {
                        break;
                    }
                    hasher.update(&buffer[..read]);
                }
                snapshot.insert(relative, SnapshotEntry::File(hasher.finalize().into()));
            } else {
                return Err(format!("unsupported filesystem entry: {}", path.display()));
            }
        }
        Ok(())
    }

    let metadata = std::fs::symlink_metadata(root)
        .map_err(|error| format!("read metadata for {}: {error}", root.display()))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "snapshot root is not a directory: {}",
            root.display()
        ));
    }
    let mut snapshot = BTreeMap::new();
    visit(root, "", &mut snapshot)?;
    Ok(snapshot)
}

/// fixture 内的测试文件相对路径集合。
fn test_files(fixture: &Path, is_test_file: &dyn Fn(&Path) -> bool) -> Result<Vec<String>, String> {
    fn visit(
        dir: &Path,
        prefix: &str,
        is_test_file: &dyn Fn(&Path) -> bool,
        out: &mut Vec<String>,
    ) -> Result<(), String> {
        let entries = std::fs::read_dir(dir)
            .map_err(|error| format!("read directory {}: {error}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!("read directory entry under {}: {error}", dir.display())
            })?;
            let path = entry.path();
            let name = entry.file_name().into_string().map_err(|name| {
                format!(
                    "path under {} is not valid UTF-8: {}",
                    dir.display(),
                    name.to_string_lossy()
                )
            })?;
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("read metadata for {}: {error}", path.display()))?;
            if metadata.file_type().is_symlink() {
                if is_test_file(Path::new(&relative)) {
                    out.push(relative);
                }
            } else if metadata.file_type().is_dir() {
                visit(&path, &relative, is_test_file, out)?;
            } else if is_test_file(Path::new(&relative)) {
                out.push(relative);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    visit(fixture, "", is_test_file, &mut out)?;
    Ok(out)
}

fn command_output_with_timeout(command: &mut Command, timeout: Duration) -> Result<Output, String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to spawn process: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture process stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to capture process stderr".to_string())?;
    let stdout_reader = spawn_pipe_drain(stdout);
    let stderr_reader = spawn_pipe_drain(stderr);
    let started = Instant::now();

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let kill_error = terminate_process_tree(&mut child).err();
                let _ = child.wait();
                stdout_reader.abandon();
                stderr_reader.abandon();
                return Err(match kill_error {
                    Some(error) => format!(
                        "timed out after {} ms and could not terminate process: {error}",
                        timeout.as_millis()
                    ),
                    None => format!("timed out after {} ms", timeout.as_millis()),
                });
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                stdout_reader.abandon();
                stderr_reader.abandon();
                return Err(format!("failed while waiting for process: {error}"));
            }
        }
    };

    finish_output(status, stdout_reader, stderr_reader)
}

#[cfg(windows)]
fn terminate_process_tree(child: &mut Child) -> Result<(), String> {
    let result = Command::new("taskkill.exe")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match result {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => child.kill().map_err(|error| {
            format!("taskkill exited with {status}; direct termination also failed: {error}")
        }),
        Err(taskkill_error) => child.kill().map_err(|kill_error| {
            format!(
                "failed to start taskkill ({taskkill_error}); direct termination also failed: {kill_error}"
            )
        }),
    }
}

#[cfg(unix)]
fn terminate_process_tree(child: &mut Child) -> Result<(), String> {
    let process_group = i32::try_from(child.id())
        .map_err(|_| format!("child process id {} exceeds i32", child.id()))?;
    // SAFETY: the child was spawned into a fresh process group whose id is its pid. A negative
    // pid targets that group only, so the harness and unrelated processes cannot be signalled.
    if unsafe { libc::kill(-process_group, libc::SIGKILL) } == 0 {
        Ok(())
    } else {
        let group_error = std::io::Error::last_os_error();
        child.kill().map_err(|kill_error| {
            format!(
                "failed to kill verifier process group ({group_error}); direct termination also failed: {kill_error}"
            )
        })
    }
}

/// 进程退出后管道读线程的收尾宽限：`verify.mjs` 的后台后代仍持有管道写端时，
/// 只等这么久即带已读到的部分输出返回（对齐 gateway 侧 `DRAIN_GRACE` 语义）。
const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(5);

/// 一根由后台线程排空的输出管道。
///
/// 字节是边读边写进共享缓冲的（不是 `read_to_end` 一次性带回），所以即使
/// 「进程已退出、继承管道写端的后台后代还没退」，收尾也能在宽限期后带着
/// 部分输出返回，而不是永久阻塞在 join 上。
struct PipeDrain {
    buffer: Arc<Mutex<Vec<u8>>>,
    handle: thread::JoinHandle<Result<(), std::io::Error>>,
}

fn spawn_pipe_drain(mut pipe: impl Read + Send + 'static) -> PipeDrain {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let shared = Arc::clone(&buffer);
    let handle = thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => return Ok(()),
                Err(error) => return Err(error),
                Ok(n) => shared
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .extend_from_slice(&chunk[..n]),
            }
        }
    });
    PipeDrain { buffer, handle }
}

impl PipeDrain {
    /// 等读线程在 [`PIPE_DRAIN_GRACE`] 内自然收尾；超时则分离线程、返回已读
    /// 到的部分输出（截断用 eprintln 标注，不写进捕获的输出本身）。
    fn finish(self, what: &str) -> Result<Vec<u8>, String> {
        let deadline = Instant::now() + PIPE_DRAIN_GRACE;
        let PipeDrain { buffer, handle } = self;
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                eprintln!(
                    "judge: process {what} pipe still open after {} ms (background descendant holds it); returning truncated output",
                    PIPE_DRAIN_GRACE.as_millis()
                );
                return Ok(take_buffer(&buffer));
            }
            thread::sleep(Duration::from_millis(10));
        }
        match handle.join() {
            Ok(Ok(())) => Ok(take_buffer(&buffer)),
            Ok(Err(error)) => Err(format!("read process {what}: {error}")),
            Err(_) => Err(format!("{what} reader thread panicked")),
        }
    }

    /// 超时/终止路径：输出已不需要，直接分离读线程（进程树被终止、管道
    /// 写端关闭后线程会自行退出）。
    fn abandon(self) {
        drop(self.handle);
    }
}

fn take_buffer(buffer: &Mutex<Vec<u8>>) -> Vec<u8> {
    std::mem::take(
        &mut *buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}

fn finish_output(
    status: ExitStatus,
    stdout_reader: PipeDrain,
    stderr_reader: PipeDrain,
) -> Result<Output, String> {
    let stdout = stdout_reader.finish("stdout")?;
    let stderr = stderr_reader.finish("stderr")?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EvalRunResult, EvalStopReason, EvalTimings};
    use std::path::PathBuf;

    fn result_with(workspace: &std::path::Path, settled: bool) -> EvalRunResult {
        EvalRunResult {
            harness: "r-code".to_string(),
            input_id: "case".to_string(),
            output: String::new(),
            usage_json: None,
            timings: EvalTimings { wall_ms: 10 },
            events: Vec::new(),
            stop_reason: if settled {
                EvalStopReason::Settled
            } else {
                EvalStopReason::NotSettled("budget".to_string())
            },
            workspace: workspace.to_path_buf(),
        }
    }

    /// M2-02.A1：score ∈ [0,1] + rationale + 失败累积（不折叠单布尔）。
    #[test]
    fn verdicts_accumulate_failures() {
        let no_fail = JudgeVerdict::from_failures(vec![], 3);
        assert_eq!(no_fail.score, 1.0);
        assert!(no_fail.rationale.contains("3/3"));
        let two_fails =
            JudgeVerdict::from_failures(vec!["a broke".to_string(), "b broke".to_string()], 3);
        assert!(
            (two_fails.score - 1.0 / 3.0).abs() < 1e-9,
            "score={}",
            two_fails.score
        );
        assert!(two_fails.rationale.contains("a broke"));
        assert!(
            two_fails.rationale.contains("b broke"),
            "failures accumulate"
        );
        assert_eq!(two_fails.failures.len(), 2);
        // 越界钳制。
        let over = JudgeVerdict::from_failures(vec!["x".to_string()], 0);
        assert_eq!(over.score, 0.0);
    }

    /// M2-02.A2：TestPassJudge 确定性可复现——同一工作区两次打分逐字节一致。
    #[test]
    fn test_pass_judge_is_deterministic() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("verify.mjs"), "process.exit(0);\n").unwrap();
        let input = EvalInput::new("case", "do");
        let result = result_with(workspace.path(), true);
        let judge = test_pass_judge();
        let first = judge.score(&JudgeInput {
            input: &input,
            result: &result,
            fixture: None,
        });
        let second = judge.score(&JudgeInput {
            input: &input,
            result: &result,
            fixture: None,
        });
        assert_eq!(first, second);
        assert_eq!(first.score, 1.0, "passing verify + settled => 1.0");
        // 失败腿：verify 失败 + 未收敛 = 两条失败原因累积。
        std::fs::write(workspace.path().join("verify.mjs"), "process.exit(1);\n").unwrap();
        let failed_result = result_with(workspace.path(), false);
        let verdict = judge.score(&JudgeInput {
            input: &input,
            result: &failed_result,
            fixture: None,
        });
        assert_eq!(verdict.failures.len(), 2);
        assert!(verdict.score < 1.0);
    }

    /// M2-02.A2（续）：FocusJudge——越界改动逐条累积，允许清单内不罚。
    #[test]
    fn focus_judge_flags_out_of_scope_changes() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join("keep.txt"), "base").unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("keep.txt"), "base").unwrap();
        std::fs::create_dir_all(workspace.path().join("src")).unwrap();
        std::fs::write(workspace.path().join("src").join("lib.ts"), "new").unwrap();
        std::fs::write(workspace.path().join("stray.txt"), "oops").unwrap();

        let input = EvalInput::new("case", "do");
        let result = result_with(workspace.path(), true);
        let judge = focus_judge(vec!["src".to_string()]);
        let verdict = judge.score(&JudgeInput {
            input: &input,
            result: &result,
            fixture: Some(fixture.path()),
        });
        assert_eq!(
            verdict.failures,
            vec!["unexpected workspace change: stray.txt".to_string()]
        );
        assert!(verdict.score < 1.0);
    }

    #[test]
    fn focus_judge_detects_same_length_content_changes() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join("same-size.txt"), "before").unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("same-size.txt"), "after!").unwrap();

        let input = EvalInput::new("case", "do");
        let verdict = focus_judge(Vec::new()).score(&JudgeInput {
            input: &input,
            result: &result_with(workspace.path(), true),
            fixture: Some(fixture.path()),
        });

        assert_eq!(
            verdict.failures,
            vec!["unexpected workspace change: same-size.txt".to_string()]
        );
    }

    #[test]
    fn command_timeout_terminates_hung_verifier_process_tree() {
        let workspace = tempfile::tempdir().unwrap();
        let marker = workspace.path().join("descendant-survived.txt");
        let descendant = format!(
            "setTimeout(() => require('node:fs').writeFileSync({}, 'alive'), 800);",
            serde_json::to_string(&marker.to_string_lossy()).unwrap(),
        );
        let parent = format!(
            "require('node:child_process').spawn(process.execPath, ['-e', {}], {{ stdio: 'ignore' }}); setInterval(() => {{}}, 60_000);",
            serde_json::to_string(&descendant).unwrap(),
        );
        std::fs::write(workspace.path().join("hang.cjs"), parent).unwrap();
        let mut command = Command::new("node");
        command.arg("hang.cjs").current_dir(workspace.path());

        let started = Instant::now();
        let error = command_output_with_timeout(&mut command, Duration::from_millis(100))
            .expect_err("hung verifier must time out");

        assert!(error.contains("timed out after 100 ms"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_secs(1));
        assert!(
            !marker.exists(),
            "verifier descendant survived the process-tree timeout"
        );
    }

    /// 进程退出后，后台后代持有管道写端时：收尾只等 PIPE_DRAIN_GRACE 宽限，
    /// 随后带部分输出返回——不阻塞在 join 上（对齐 gateway DRAIN_GRACE 语义）。
    #[test]
    fn pipe_drain_returns_partial_output_when_descendant_holds_pipe() {
        let workspace = tempfile::tempdir().unwrap();
        // 后台后代继承父进程的 stdout 并长睡：父进程退出后写端仍被持有。
        let descendant = "setTimeout(() => {}, 15_000);";
        let parent = format!(
            "require('node:child_process').spawn(process.execPath, ['-e', {}], {{ stdio: ['ignore', 'inherit', 'ignore'] }}); console.log('parent done');",
            serde_json::to_string(descendant).unwrap(),
        );
        std::fs::write(workspace.path().join("hold-pipe.cjs"), parent).unwrap();
        let mut command = Command::new("node");
        command.arg("hold-pipe.cjs").current_dir(workspace.path());

        let started = Instant::now();
        let output = command_output_with_timeout(&mut command, Duration::from_secs(60))
            .expect("parent exits; pipe drain must return within the grace window");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("parent done"), "partial stdout: {stdout}");
        assert!(
            started.elapsed() >= PIPE_DRAIN_GRACE,
            "held pipe must consume the full grace window"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "must not block until the verifier timeout"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_pass_judge_rejects_symlink_verifier() {
        use std::os::unix::fs::symlink;

        let external = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(external.path(), "process.exit(0);\n").unwrap();
        let workspace = tempfile::tempdir().unwrap();
        symlink(external.path(), workspace.path().join("verify.mjs")).unwrap();
        let input = EvalInput::new("case", "do");

        let verdict = test_pass_judge().score(&JudgeInput {
            input: &input,
            result: &result_with(workspace.path(), true),
            fixture: None,
        });

        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("invalid verifier script")),
            "{:?}",
            verdict.failures,
        );
    }

    /// M2-02.A2（续）：IntegrityJudge——测试文件被删/被改判失败，原样保留通过。
    #[test]
    fn integrity_judge_protects_test_files() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(fixture.path().join("tests")).unwrap();
        std::fs::write(fixture.path().join("tests").join("a.test.ts"), "1").unwrap();
        std::fs::write(fixture.path().join("verify.mjs"), "ok").unwrap();
        std::fs::write(fixture.path().join("note.md"), "x").unwrap();

        // 完整保留：通过。
        let intact = tempfile::tempdir().unwrap();
        for relative in ["tests/a.test.ts", "verify.mjs"] {
            let target = intact.path().join(relative);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(fixture.path().join(relative), &target).unwrap();
        }
        let input = EvalInput::new("case", "do");
        let judge = integrity_judge();
        let verdict = judge.score(&JudgeInput {
            input: &input,
            result: &result_with(intact.path(), true),
            fixture: Some(fixture.path()),
        });
        assert_eq!(verdict.score, 1.0, "{:?}", verdict.failures);

        // 改写测试文件：失败且指名道姓。
        std::fs::write(intact.path().join("verify.mjs"), "tampered").unwrap();
        let verdict = judge.score(&JudgeInput {
            input: &input,
            result: &result_with(intact.path(), true),
            fixture: Some(fixture.path()),
        });
        assert!(verdict
            .failures
            .iter()
            .any(|failure| failure.contains("verify.mjs")));
    }

    /// M2-02.A3：LLM Judge 扩展点——create_judge 接受任意同签名函数。
    #[test]
    fn llm_judge_extension_point_signature() {
        let judge = create_judge(
            "llm",
            Arc::new(|_input: &JudgeInput<'_>| JudgeVerdict {
                score: 0.75,
                rationale: "model-graded".to_string(),
                failures: vec!["partial".to_string()],
            }),
        );
        let input = EvalInput::new("case", "do");
        let workspace = PathBuf::from(".");
        let verdict = judge.score(&JudgeInput {
            input: &input,
            result: &result_with(&workspace, true),
            fixture: None,
        });
        assert_eq!(verdict.score, 0.75);
        assert_eq!(judge.name, "llm");
    }
}
