//! Cross-profile workspace and Git-metadata locks.
//!
//! Locks are keyed by the canonical directory identity and stored in a
//! user-level location, so different runtime profiles (dev/prod) writing
//! the same physical workspace collide on the same lock — exactly the
//! protection wanted. Git metadata mutations additionally serialize by the
//! repository's `common_dir`. A newly free lock never proves an orphan
//! stopped: callers pair locks with durable barriers (T24).

use std::io;
use std::path::{Path, PathBuf};

/// An acquired OS lock handle. Dropping releases.
pub struct WorkspaceLock {
    path: PathBuf,
    _handle: LockHandle,
}

enum LockHandle {
    #[cfg(unix)]
    Flocked(std::fs::File),
    #[cfg(windows)]
    NoShare(#[allow(dead_code)] std::fs::File),
}

impl WorkspaceLock {
    /// The user-level lock directory for canonical workspace identities.
    fn lock_root() -> PathBuf {
        std::env::temp_dir().join("r-code-workspace-locks")
    }

    fn compute_lock_path(kind: &str, identity: &str) -> PathBuf {
        let digest = crate::services::artifacts::sha256_hex(identity.as_bytes());
        Self::lock_root().join(format!("{kind}-{digest}.lock"))
    }

    fn open_exclusive(path: &Path) -> io::Result<LockHandle> {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .open(path)?;
            let fd = file.as_raw_fd();
            let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(LockHandle::Flocked(file))
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            match std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .share_mode(0)
                .open(path)
            {
                Ok(file) => Ok(LockHandle::NoShare(file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let file = std::fs::OpenOptions::new()
                        .write(true)
                        .share_mode(0)
                        .open(path)?;
                    Ok(LockHandle::NoShare(file))
                }
                Err(error) => Err(error),
            }
        }
    }

    /// Acquire the write lock for a canonical workspace directory.
    pub fn acquire(canonical_root: &Path) -> Result<Self, io::Error> {
        let root =
            std::fs::canonicalize(canonical_root).unwrap_or_else(|_| canonical_root.to_path_buf());
        let identity = root.to_string_lossy().to_lowercase();
        let path = Self::compute_lock_path("ws", &identity);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let handle = Self::open_exclusive(&path)?;
        Ok(Self {
            path,
            _handle: handle,
        })
    }

    /// Acquire the Git-metadata lock for a repository common dir.
    pub fn acquire_common_dir(common_dir: &Path) -> Result<Self, io::Error> {
        let identity = common_dir.to_string_lossy().to_lowercase();
        let path = Self::compute_lock_path("git", &identity);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let handle = Self::open_exclusive(&path)?;
        Ok(Self {
            path,
            _handle: handle,
        })
    }

    pub fn lock_path(&self) -> &Path {
        &self.path
    }
}
