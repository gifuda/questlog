//! Cross-process file locking using O_EXCL atomic lockfile creation.
//!
//! This mirrors the Node MCP server's `file-lock.ts` protocol so that Rust
//! (Tauri UI), Node (MCP), and the Runner all serialize access to the same
//! JSON data files.
//!
//! # Owner-safe protocol (v2)
//!
//! The lock file carries the holder's identity:
//! `{"v":2,"pid":<u32>,"token":"<uuid>","created_ms":<u64>}`.
//! A lock left behind by a crashed process is only reclaimed when the owner
//! is PROVABLY dead (`kill(pid, 0)` → ESRCH). Age alone never justifies
//! stealing a lock — a live-but-slow holder (paused process, long GC, laptop
//! sleep) must not be robbed mid-critical-section.
//!
//! Reclaim rules on an existing lock file:
//! - owner pid known + dead        → remove and retry (safe: holder is gone)
//! - owner pid known + alive       → wait; timeout surfaces "held by live pid"
//! - empty file (0 bytes) + stale  → remove and retry (creator crashed between
//!                                   O_EXCL create and writing its identity;
//!                                   no owner can ever release it)
//! - non-empty, unparseable        → NEVER auto-remove; timeout returns a
//!                                   "blocked" error for manual cleanup
//!
//! Legacy compatibility: locks written by the previous protocol as
//! `"{pid}-{uuid}"` are still readable — the pid prefix is extracted and the
//! same liveness rules apply.
//!
//! Known limitation: pid reuse can make a dead holder look alive (conservative
//! block, surfaced by the timeout error) — never a silent steal.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

const RETRY_INTERVAL: Duration = Duration::from_millis(50);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// Only used for the EMPTY-lock escape hatch (creator crashed before writing
/// its identity). Age never justifies stealing a lock with a known owner.
const STALE_LOCK_AGE: Duration = Duration::from_secs(10);

/// RAII guard that releases the lock on drop.
#[derive(Debug)]
pub struct FileLock {
    lock_path: PathBuf,
    /// Exact content we wrote — release only removes the file if it still
    /// matches (someone else may have reclaimed and re-created it meanwhile).
    lock_content: String,
    released: bool,
}

/// Extract the holder pid from lock file content.
/// Understands both the v2 JSON form and the legacy `"{pid}-{uuid}"` form.
/// Returns None for empty or unparseable content (identity unknown).
fn parse_owner_pid(content: &str) -> Option<i32> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        let pid = v.get("pid")?.as_i64()?;
        if pid <= 0 || pid > i32::MAX as i64 {
            return None;
        }
        return Some(pid as i32);
    }
    // Legacy "pid-uuid": leading decimal integer before the first '-'.
    if let Some(dash) = trimmed.find('-') {
        if let Ok(pid) = trimmed[..dash].parse::<i32>() {
            if pid > 0 {
                return Some(pid);
            }
        }
    }
    None
}

/// Probe whether a process is alive via `kill(pid, 0)`.
/// - Ok(0)            → alive
/// - Err(EPERM)       → exists but owned by another user → treat as alive
/// - Err(ESRCH)       → no such process → dead (the only "provably dead")
/// - other errors     → cannot determine → treat as alive (conservative)
fn owner_alive(pid: i32) -> bool {
    // Safety: kill(2) with signal 0 is a pure liveness probe.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    !matches!(errno, libc::ESRCH)
}

impl FileLock {
    /// Acquire a cross-process lock on `data_path` by creating
    /// `data_path.lock` atomically. Blocks until acquired or timeout.
    pub fn acquire(data_path: &Path) -> Result<Self, String> {
        Self::acquire_with_timeout(data_path, DEFAULT_TIMEOUT)
    }

    pub fn acquire_with_timeout(data_path: &Path, timeout: Duration) -> Result<Self, String> {
        let lock_path = PathBuf::from(format!("{}.lock", data_path.display()));
        let lock_content = serde_json::json!({
            "v": 2,
            "pid": std::process::id(),
            "token": uuid::Uuid::new_v4().to_string(),
            "created_ms": SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        })
        .to_string();

        let deadline = Instant::now() + timeout;

        loop {
            match OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&lock_path)
            {
                Ok(mut f) => {
                    let _ = f.write_all(lock_content.as_bytes());
                    return Ok(FileLock {
                        lock_path,
                        lock_content,
                        released: false,
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let content = fs::read_to_string(&lock_path).unwrap_or_default();
                    match parse_owner_pid(&content) {
                        Some(pid) if !owner_alive(pid) => {
                            // Owner is provably dead — safe to reclaim.
                            let _ = fs::remove_file(&lock_path);
                            continue; // retry immediately
                        }
                        Some(_pid) => {
                            // Live holder (or undeterminable probe) — wait.
                        }
                        None if content.trim().is_empty() => {
                            // Creator crashed between create and writing its
                            // identity. Only this case may fall back to the
                            // age heuristic: no owner can ever release it.
                            if let Ok(meta) = fs::metadata(&lock_path) {
                                if let Ok(modified) = meta.modified() {
                                    if let Ok(age) =
                                        SystemTime::now().duration_since(modified)
                                    {
                                        if age > STALE_LOCK_AGE {
                                            let _ = fs::remove_file(&lock_path);
                                            continue;
                                        }
                                    }
                                }
                            }
                        }
                        None => {
                            // Non-empty, unparseable — identity unknown. Never
                            // auto-remove; surface a blocked error at timeout.
                            if Instant::now() >= deadline {
                                return Err(format!(
                                    "lock blocked: {} is unreadable and no owner can be \
                                     proven dead — manual cleanup required",
                                    lock_path.display()
                                ));
                            }
                            std::thread::sleep(RETRY_INTERVAL);
                            continue;
                        }
                    }
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "lock timeout: {} held by live owner (pid {:?})",
                            lock_path.display(),
                            parse_owner_pid(&content)
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
        // Only remove if we still own it (content matches what we wrote).
        if let Ok(content) = fs::read_to_string(&self.lock_path) {
            if content.trim() == self.lock_content.trim() {
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

        {
            let _lock = FileLock::acquire(&file).unwrap();
        }
        {
            let _lock = FileLock::acquire(&file).unwrap();
        }
    }

    #[test]
    fn with_file_lock_runs_closure() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("closure.json");

        let result = with_file_lock(&file, || Ok(42i32));
        assert_eq!(result.unwrap(), 42);
    }

    /// Spawn a short-lived child and wait for it to exit, yielding a pid that
    /// is PROVABLY dead (recently exited, unlikely to be reused within the
    /// test's lifetime).
    fn dead_pid() -> i32 {
        let mut child = std::process::Command::new("sleep")
            .arg("0")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i32;
        child.wait().unwrap();
        pid
    }

    #[test]
    fn v2_lock_with_dead_owner_is_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("dead-v2.json");
        let lock_path = PathBuf::from(format!("{}.lock", file.display()));

        let pid = dead_pid();
        let content = serde_json::json!({"v":2,"pid":pid,"token":"t","created_ms":0}).to_string();
        fs::write(&lock_path, content).unwrap();

        let _lock = FileLock::acquire(&file).unwrap();
    }

    #[test]
    fn legacy_lock_with_dead_owner_is_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("dead-legacy.json");
        let lock_path = PathBuf::from(format!("{}.lock", file.display()));

        let pid = dead_pid();
        fs::write(&lock_path, format!("{pid}-some-uuid")).unwrap();

        let _lock = FileLock::acquire(&file).unwrap();
    }

    #[test]
    fn v2_lock_with_live_owner_blocks_until_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("live.json");
        let lock_path = PathBuf::from(format!("{}.lock", file.display()));

        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let content =
            serde_json::json!({"v":2,"pid":child.id(),"token":"t","created_ms":0}).to_string();
        fs::write(&lock_path, content).unwrap();

        let err = FileLock::acquire_with_timeout(&file, Duration::from_millis(300))
            .expect_err("live owner must block acquisition");
        assert!(err.contains("live owner"), "got: {err}");
        // The lock must still be there — age-based stealing is gone.
        assert!(lock_path.exists());

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn empty_stale_lock_is_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("empty.json");
        let lock_path = PathBuf::from(format!("{}.lock", file.display()));

        fs::write(&lock_path, "").unwrap();
        let old_time = SystemTime::now() - Duration::from_secs(30);
        let _ = filetime::set_file_mtime(
            &lock_path,
            filetime::FileTime::from_system_time(old_time),
        );

        let _lock = FileLock::acquire(&file).unwrap();
    }

    #[test]
    fn unparseable_lock_never_reclaimed_even_when_old() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("garbage.json");
        let lock_path = PathBuf::from(format!("{}.lock", file.display()));

        fs::write(&lock_path, "garbage-not-a-pid").unwrap();
        let old_time = SystemTime::now() - Duration::from_secs(30);
        let _ = filetime::set_file_mtime(
            &lock_path,
            filetime::FileTime::from_system_time(old_time),
        );

        let err = FileLock::acquire_with_timeout(&file, Duration::from_millis(300))
            .expect_err("unparseable lock must block, not be stolen");
        assert!(err.contains("blocked"), "got: {err}");
        assert!(lock_path.exists(), "unparseable lock must survive");
    }
}
