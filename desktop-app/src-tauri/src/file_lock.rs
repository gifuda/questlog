//! Cross-process file locking using O_EXCL atomic lockfile creation.
//!
//! This mirrors the Node MCP server's `file-lock.ts` protocol so that Rust
//! (Tauri UI), Node (MCP), and the future Runner all serialize access to the
//! same JSON data files.
//!
//! Protocol: to lock `foo.json`, atomically create `foo.json.lock` with
//! O_EXCL. If it already exists, check if it's stale (older than 10s); if so,
//! remove and retry. Otherwise wait 50ms and retry, up to the timeout.
//! On success, write `{pid}-{uuid}` into the lock file. On release, only
//! remove if we still own it.
//!
//! The lock is best-effort advisory — it prevents last-writer-wins data loss
//! but assumes cooperating processes. A process that crashes leaves a stale
//! lock that is cleaned up by the next acquirer.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

const RETRY_INTERVAL: Duration = Duration::from_millis(50);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const STALE_LOCK_AGE: Duration = Duration::from_secs(10);

/// RAII guard that releases the lock on drop.
pub struct FileLock {
    lock_path: PathBuf,
    lock_id: String,
    released: bool,
}

impl FileLock {
    /// Acquire a cross-process lock on `data_path` by creating
    /// `data_path.lock` atomically. Blocks until acquired or timeout.
    pub fn acquire(data_path: &Path) -> Result<Self, String> {
        Self::acquire_with_timeout(data_path, DEFAULT_TIMEOUT)
    }

    pub fn acquire_with_timeout(data_path: &Path, timeout: Duration) -> Result<Self, String> {
        let _lock_path = data_path.with_extension(
            data_path
                .extension()
                .map(|e| format!("{}.lock", e.to_string_lossy()))
                .unwrap_or_else(|| "lock".to_string()),
        );
        // Simpler and matches the Node side: just append ".lock" to the full path.
        let lock_path = PathBuf::from(format!("{}.lock", data_path.display()));
        let lock_id = format!("{}-{}", std::process::id(), uuid::Uuid::new_v4());

        let deadline = Instant::now() + timeout;

        loop {
            // Try O_EXCL atomic creation.
            match OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&lock_path)
            {
                Ok(mut f) => {
                    let _ = f.write_all(lock_id.as_bytes());
                    return Ok(FileLock {
                        lock_path,
                        lock_id,
                        released: false,
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Check if stale.
                    if let Ok(meta) = fs::metadata(&lock_path) {
                        if let Ok(modified) = meta.modified() {
                            if let Ok(age) = SystemTime::now().duration_since(modified) {
                                if age > STALE_LOCK_AGE {
                                    let _ = fs::remove_file(&lock_path);
                                    continue; // retry immediately
                                }
                            }
                        }
                    }
                    // Not stale — wait and retry.
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "lock timeout: could not acquire {} within {:?}",
                            lock_path.display(),
                            timeout
                        ));
                    }
                    std::thread::sleep(RETRY_INTERVAL);
                }
                Err(e) => {
                    return Err(format!("lock error: {}", e));
                }
            }
        }
    }

    /// Explicitly release the lock (also done on drop).
    pub fn release(&mut self) {
        if self.released {
            return;
        }
        // Only remove if we still own it.
        if let Ok(content) = fs::read_to_string(&self.lock_path) {
            if content == self.lock_id {
                let _ = fs::remove_file(&self.lock_path);
            }
        }
        self.released = true;
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        self.release();
    }
}

/// Run a closure while holding a cross-process lock on the given file path.
pub fn with_file_lock<F, T>(data_path: &Path, f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String>,
{
    let _lock = FileLock::acquire(data_path)?;
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_acquires_and_releases() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.json");
        let lock_path = PathBuf::from(format!("{}.lock", file.display()));

        {
            let _lock = FileLock::acquire(&file).unwrap();
            assert!(lock_path.exists());
        }
        // Lock file should be removed on drop.
        assert!(!lock_path.exists());
    }

    #[test]
    fn lock_is_reentrant_after_release() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test2.json");

        // Acquire, release, acquire again — should work.
        {
            let _lock = FileLock::acquire(&file).unwrap();
        }
        {
            let _lock = FileLock::acquire(&file).unwrap();
        }
    }

    #[test]
    fn stale_lock_is_cleaned_up() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("stale.json");
        let lock_path = PathBuf::from(format!("{}.lock", file.display()));

        // Create a stale lock (old modification time).
        fs::write(&lock_path, "stale-owner").unwrap();
        // Set modification time to 30 seconds ago.
        let old_time = SystemTime::now() - Duration::from_secs(30);
        let _ =
            filetime::set_file_mtime(&lock_path, filetime::FileTime::from_system_time(old_time));

        // Should acquire despite the stale lock.
        let _lock = FileLock::acquire(&file).unwrap();
    }

    #[test]
    fn with_file_lock_runs_closure() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("closure.json");

        let result = with_file_lock(&file, || Ok(42i32));
        assert_eq!(result.unwrap(), 42);
    }

    // filetime is a dev-dependency for the stale-lock test.
    // We reference it here so the test compiles only when it's available.
}
