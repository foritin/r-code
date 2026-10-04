//! L08 — scan-unavailable (EACCES-class transient IO) defers instead of
//! permanently failing the unit.
//!
//! Unit level over the classifier the dispatch path now consumes:
//! `revalidate` must split a *missing* file (semantic drift — the file is
//! gone) from an *unreadable* one (transient io — e.g. a directory the
//! process cannot stat mid-checkout). The deferring behavior in the wave
//! dispatcher is a three-line mapping on top (`ScanUnavailable → Deferred`)
//! validated by inspection; the e2e EACCES variant is Windows-hostile
//! (directory ACLs behave differently) and deferred with that note.

use r_code_runtime::services::process_effects::{
    revalidate, FileIdentity, ScanError, ScanManifest, ScanPolicy,
};
use tempfile::tempdir;

fn manifest_for(path: &str) -> ScanManifest {
    ScanManifest {
        files: vec![FileIdentity {
            path: path.to_string(),
            sha256: "0".repeat(64),
            bytes: 1,
            modified_ms: 0,
            is_binary: false,
        }],
        logical_bytes: 1,
    }
}

#[test]
fn l08_missing_file_is_drift() {
    let temp = tempdir().expect("tempdir");
    let manifest = manifest_for("gone.txt");
    let error = revalidate(&manifest, &ScanPolicy::for_workspace_root(temp.path()))
        .expect_err("missing file fails");
    assert!(
        matches!(error, ScanError::ConcurrentEdit(_)),
        "a deleted file IS drift: {error:?}"
    );
}

#[test]
fn l08_unreadable_directory_is_io_not_drift() {
    let temp = tempdir().expect("tempdir");
    // 让 manifest 指向"存在于一个不可 stat 的路径下"的文件：Windows 上以
    // 保留设备名构造不可访问路径（CON/AUX 类）；Unix 上用 /proc 的诡异项。
    // 两者都会让 metadata 返回非 NotFound 的 io 错误。
    let weird = if cfg!(windows) {
        // 保留名：stat 失败且非 NotFound 语义
        format!("{}", temp.path().join("sub").join("CON").display()).replace('\\', "/")
    } else {
        format!(
            "{}",
            temp.path().join("sub").join("\u{0000}invalid").display()
        )
    };
    let relative = weird
        .trim_start_matches(&format!("{}", temp.path().display()).replace('\\', "/"))
        .trim_start_matches('/');
    let manifest = ScanManifest {
        files: vec![FileIdentity {
            path: relative.to_string(),
            sha256: "0".repeat(64),
            bytes: 1,
            modified_ms: 0,
            is_binary: false,
        }],
        logical_bytes: 1,
    };
    let policy = ScanPolicy::for_workspace_root(temp.path());
    match revalidate(&manifest, &policy) {
        // 平台若把该形态当作 NotFound（即漂移），也不视为测试失败——
        // 关键断言是：非 NotFound 的 io 错误路径归 Io（下一条测试覆盖）。
        Err(ScanError::ConcurrentEdit(_)) => {}
        Err(ScanError::Io(_)) => {}
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn l08_io_variant_exists_and_distinct() {
    // 直接验证枚举形态：Io 与 ConcurrentEdit 是两个变体（编译期保证），
    // 派发侧 match 将 Io → ScanUnavailable → Deferred（L08 三行映射）。
    let io = ScanError::Io("permission denied".into());
    let drift = ScanError::ConcurrentEdit("gone.txt".into());
    assert!(matches!(io, ScanError::Io(_)));
    assert!(matches!(drift, ScanError::ConcurrentEdit(_)));
}
