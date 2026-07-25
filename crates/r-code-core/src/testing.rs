//! 测试 fixture 工具 -- 提供隔离的测试环境。
//!
//! 使用 `tempdir()` 创建测试隔离环境；每个测试不污染真实用户数据。
//! [doc-15 §4.3]

use std::path::PathBuf;
use tempfile::TempDir;

/// 测试 fixture -- 提供隔离的临时目录和数据库。
pub struct TestFixture {
    /// 临时目录（自动清理）
    pub temp_dir: TempDir,
    /// 数据库路径
    pub db_path: PathBuf,
    /// blobs 目录
    pub blobs_dir: PathBuf,
    /// sessions 目录（JSONL）
    pub sessions_dir: PathBuf,
    /// worktree 目录
    pub worktree_dir: PathBuf,
}

impl TestFixture {
    /// 创建新的测试 fixture。
    pub fn new() -> Self {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let root = temp_dir.path().to_path_buf();

        let db_path = root.join("test.db");
        let blobs_dir = root.join("blobs");
        let sessions_dir = root.join("sessions");
        let worktree_dir = root.join("worktrees");

        Self {
            temp_dir,
            db_path,
            blobs_dir,
            sessions_dir,
            worktree_dir,
        }
    }

    /// 获取根目录路径。
    pub fn root(&self) -> &std::path::Path {
        self.temp_dir.path()
    }

    /// 创建一个项目目录并返回其路径。
    pub fn create_project(&self, name: &str) -> PathBuf {
        let project_dir = self.root().join("projects").join(name);
        std::fs::create_dir_all(&project_dir).expect("failed to create project dir");
        project_dir
    }

    /// 创建一个文件并写入内容。
    pub fn create_file(&self, path: &str, content: &str) -> PathBuf {
        let file_path = self.root().join(path);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).expect("failed to create parent dir");
        }
        std::fs::write(&file_path, content).expect("failed to write file");
        file_path
    }

    /// 获取数据库路径（用于 r-code-store 测试）。
    pub fn db_path(&self) -> &std::path::Path {
        &self.db_path
    }

    /// 获取 blobs 目录路径。
    pub fn blobs_dir_path(&self) -> &std::path::Path {
        &self.blobs_dir
    }
}

impl Default for TestFixture {
    fn default() -> Self {
        Self::new()
    }
}

/// 创建一个大型目录树 fixture（用于性能测试）。
///
/// 生成 `count` 个文件，每个文件包含一些内容。
/// [doc-18 M11-03]
pub fn create_large_tree_fixture(root: &std::path::Path, count: usize) {
    std::fs::create_dir_all(root).expect("failed to create root");
    for i in 0..count {
        let dir = root.join(format!("dir_{}", i / 100));
        std::fs::create_dir_all(&dir).expect("failed to create dir");
        let file_path = dir.join(format!("file_{}.txt", i));
        std::fs::write(&file_path, format!("content {}\n", i)).expect("failed to write file");
    }
}

/// 创建一个大型文本文件 fixture（用于性能测试）。
///
/// 生成约 `size_bytes` 大小的文本文件。
/// [doc-18 M11-03]
pub fn create_large_text_fixture(path: &std::path::Path, size_bytes: usize) {
    use std::io::Write;
    std::fs::create_dir_all(path.parent().unwrap()).expect("failed to create parent");
    let mut file = std::fs::File::create(path).expect("failed to create file");
    let line = b"this is a test line for large file fixture\n";
    let lines = size_bytes / line.len();
    for _ in 0..lines {
        file.write_all(line).expect("failed to write line");
    }
}

/// 性能测试 fixture -- 创建大型目录树。
///
/// 跨多个目录生成 `count` 个文件（每 100 个文件一个目录）。
/// [doc-18 M11-03] 目标：50k 文件，p95 <= 10ms 懒加载
pub fn create_perf_tree(root: &std::path::Path, count: usize) {
    // Create directories with 100 files each
    for i in 0..count {
        let dir = root.join(format!("dir_{}", i / 100));
        std::fs::create_dir_all(&dir).ok();
        let file = dir.join(format!("file_{}.txt", i));
        std::fs::write(&file, format!("line 1 content {}\nline 2\n", i)).ok();
    }
}

/// 性能测试 fixture -- 创建大型文本文件。
///
/// [doc-18 M11-03] 目标：1GiB 文本生成
pub fn create_perf_text_file(path: &std::path::Path, size_bytes: usize) {
    use std::io::Write;
    std::fs::create_dir_all(path.parent().unwrap()).ok();
    let mut file = std::fs::File::create(path).unwrap();
    let line = b"this is a performance test line for benchmarking search and file operations\n";
    let lines = size_bytes / line.len();
    for _ in 0..lines {
        file.write_all(line).unwrap();
    }
}

/// 性能测试结果。
#[derive(Debug, Clone)]
pub struct PerfResult {
    /// 基准名称
    pub name: String,
    /// 迭代次数
    pub iterations: usize,
    /// p50 耗时（微秒）
    pub p50_us: u64,
    /// p95 耗时（微秒）
    pub p95_us: u64,
    /// p99 耗时（微秒）
    pub p99_us: u64,
}

/// 运行性能基准测试。
///
/// 执行 `f` 共 `iterations` 次，统计 p50/p95/p99 耗时（微秒）。
/// `iterations == 0` 时返回全零结果（不 panic）。
pub fn benchmark(name: &str, iterations: usize, mut f: impl FnMut()) -> PerfResult {
    let mut times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = std::time::Instant::now();
        f();
        times.push(start.elapsed().as_micros() as u64);
    }
    if times.is_empty() {
        return PerfResult {
            name: name.to_string(),
            iterations: 0,
            p50_us: 0,
            p95_us: 0,
            p99_us: 0,
        };
    }
    times.sort();
    let p50 = times[times.len() / 2];
    let p95 = times[times.len() * 95 / 100];
    let p99 = times[times.len() * 99 / 100];
    PerfResult {
        name: name.to_string(),
        iterations,
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fixture_creates_dirs() {
        let fixture = TestFixture::new();
        assert!(fixture.root().exists());
    }

    #[test]
    fn test_fixture_create_project() {
        let fixture = TestFixture::new();
        let project = fixture.create_project("test-project");
        assert!(project.exists());
        assert!(project.is_dir());
    }

    #[test]
    fn test_fixture_create_file() {
        let fixture = TestFixture::new();
        let file = fixture.create_file("subdir/test.txt", "hello world");
        assert!(file.exists());
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_fixture_db_path() {
        let fixture = TestFixture::new();
        assert!(fixture.db_path().to_str().unwrap().contains("test.db"));
    }

    #[test]
    fn test_large_tree_fixture() {
        let fixture = TestFixture::new();
        let tree_root = fixture.root().join("large_tree");
        create_large_tree_fixture(&tree_root, 250);
        let file_count = std::fs::read_dir(&tree_root)
            .unwrap()
            .flat_map(|d| std::fs::read_dir(d.unwrap().path()).unwrap())
            .count();
        assert_eq!(file_count, 250);
    }

    // ── 性能 fixture 测试 [doc-18 M11-03] ────────────────────────

    #[test]
    fn test_perf_tree_creates_files() {
        let fixture = TestFixture::new();
        let tree_root = fixture.root().join("perf_tree");
        create_perf_tree(&tree_root, 350);
        let file_count: usize = std::fs::read_dir(&tree_root)
            .unwrap()
            .flat_map(|d| std::fs::read_dir(d.unwrap().path()).unwrap())
            .count();
        assert_eq!(file_count, 350);
    }

    #[test]
    fn test_perf_tree_distributes_across_dirs() {
        let fixture = TestFixture::new();
        let tree_root = fixture.root().join("perf_tree_dirs");
        // 250 文件 / 100 每 dir = 3 个 dir (dir_0, dir_1, dir_2)
        create_perf_tree(&tree_root, 250);
        let dir_count = std::fs::read_dir(&tree_root).unwrap().count();
        assert_eq!(dir_count, 3);
    }

    #[test]
    fn test_perf_tree_zero_count_creates_nothing() {
        let fixture = TestFixture::new();
        let tree_root = fixture.root().join("perf_tree_empty");
        create_perf_tree(&tree_root, 0);
        // 0 文件 -> 循环体不执行，未创建任何目录
        assert!(!tree_root.exists());
    }

    #[test]
    fn test_perf_tree_file_content() {
        let fixture = TestFixture::new();
        let tree_root = fixture.root().join("perf_tree_content");
        create_perf_tree(&tree_root, 1);
        let file_path = tree_root.join("dir_0").join("file_0.txt");
        assert!(file_path.exists());
        let content = std::fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("line 1 content 0"));
        assert!(content.contains("line 2"));
    }

    #[test]
    fn test_perf_text_file_nonzero_size() {
        let fixture = TestFixture::new();
        let path = fixture.root().join("perf_text").join("big.txt");
        create_perf_text_file(&path, 1024);
        let metadata = std::fs::metadata(&path).unwrap();
        // 写入 line.len() 的整数倍，应在 (0, 1024] 范围内
        assert!(metadata.len() > 0 && metadata.len() <= 1024);
    }

    #[test]
    fn test_perf_text_file_creates_parent_dir() {
        let fixture = TestFixture::new();
        let path = fixture.root().join("nested").join("deep").join("big.txt");
        create_perf_text_file(&path, 128);
        assert!(path.exists());
    }

    #[test]
    fn test_perf_result_clone_and_debug() {
        let r = PerfResult {
            name: "bench".to_string(),
            iterations: 10,
            p50_us: 100,
            p95_us: 200,
            p99_us: 300,
        };
        let cloned = r.clone();
        assert_eq!(r.iterations, cloned.iterations);
        assert_eq!(r.p50_us, cloned.p50_us);
        assert_eq!(r.p99_us, cloned.p99_us);
        let debug = format!("{r:?}");
        assert!(debug.contains("bench"));
        assert!(debug.contains("PerfResult"));
    }

    #[test]
    fn test_benchmark_returns_percentiles() {
        let mut counter = 0usize;
        let result = benchmark("counting", 100, || {
            counter += 1;
        });
        assert_eq!(result.name, "counting");
        assert_eq!(result.iterations, 100);
        assert_eq!(counter, 100);
        // 排序后取百分位，索引递增 -> 值非递减
        assert!(result.p50_us <= result.p95_us);
        assert!(result.p95_us <= result.p99_us);
    }

    #[test]
    fn test_benchmark_zero_iterations() {
        let result = benchmark("noop", 0, || {});
        assert_eq!(result.iterations, 0);
        assert_eq!(result.p50_us, 0);
        assert_eq!(result.p95_us, 0);
        assert_eq!(result.p99_us, 0);
    }
}
