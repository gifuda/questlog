//! Read-only, pinned projection of the mini task-maintenance desktop feed.
//!
//! This module never enables Runner behavior. Any source, process, transport,
//! freshness, or schema failure is represented as an explicit UNKNOWN payload.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub const HELPER_PATH: &str =
    "/Users/power/Library/Application Support/HomeSupervision/access/task_maintenance_desktop.py";
pub const HELPER_SHA256: &str = "5137e1dd7caad965f5d40c99dbb37cdc444687e5db9c447b2c6ea2150f3f3f35";
const PYTHON: &str = "/usr/bin/python3";
const MAX_HELPER_BYTES: usize = 2 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 16 * 1024;
const HELPER_TIMEOUT: Duration = Duration::from_secs(5);
const PYTHON_EXEC: &str =
    "import sys; exec(compile(sys.stdin.buffer.read(), '<pinned-task-maintenance>', 'exec'))";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema_version: u64,
    observed_at: String,
    source: String,
    state: String,
    reason: String,
    independent_nas_ack: String,
    assurance: String,
    automatic_resume: bool,
    execution_allowed: bool,
    source_as_of: Value,
    #[serde(deserialize_with = "deserialize_nullable_summary")]
    summary: Option<Summary>,
    #[serde(deserialize_with = "deserialize_nullable_coverage")]
    coverage: Option<Coverage>,
    links: Vec<Link>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Link {
    execution_id: String,
    attempt_no: u64,
    case_id: Value,
    case_episode: Value,
    case_revision: Value,
    status: String,
    reason: String,
    decision_request_id: Value,
    recovery_candidate: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Summary {
    execution_count: u64,
    linked_count: u64,
    candidate_count: u64,
    malformed_annotation_count: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Coverage {
    projected_links: u64,
    total_links: u64,
    truncated_links: u64,
}

fn deserialize_nullable_summary<'de, D>(deserializer: D) -> Result<Option<Summary>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<Summary>::deserialize(deserializer)
}

fn deserialize_nullable_coverage<'de, D>(deserializer: D) -> Result<Option<Coverage>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<Coverage>::deserialize(deserializer)
}

pub fn unknown() -> Value {
    json!({
        "schema_version": 1,
        "observed_at": Utc::now().to_rfc3339(),
        "source": "mini-task-maintenance",
        "state": "UNKNOWN",
        "reason": "source_unavailable_or_stale",
        "independent_nas_ack": "NOT_PROVIDED",
        "assurance": "operator_asserted",
        "automatic_resume": false,
        "execution_allowed": false,
        "source_as_of": null,
        "summary": null,
        "coverage": null,
        "links": []
    })
}

pub fn get() -> Value {
    observe_from(Path::new(HELPER_PATH), HELPER_SHA256, HELPER_TIMEOUT)
}

fn observe_from(path: &Path, expected_sha256: &str, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    let source = match read_pinned_source(path) {
        Ok(bytes) if sha256(&bytes) == expected_sha256 => bytes,
        _ => return unknown(),
    };
    if Instant::now() >= deadline {
        return unknown();
    }
    let output = match run_pinned_source(source, deadline) {
        Some(bytes) => bytes,
        None => return unknown(),
    };
    validate_snapshot(&output, Utc::now()).unwrap_or_else(|_| unknown())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Open each path component relative to an already-open directory with
/// O_NOFOLLOW. The helper bytes are then the exact bytes hashed and executed.
fn read_pinned_source(path: &Path) -> Result<Vec<u8>, ()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(());
    }
    let root = CString::new("/").map_err(|_| ())?;
    let mut directory = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if directory < 0 {
        return Err(());
    }
    let components: Vec<_> = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.as_bytes().to_vec()),
            Component::RootDir => None,
            _ => None,
        })
        .collect();
    if components.is_empty() {
        unsafe {
            libc::close(directory);
        }
        return Err(());
    }
    for component in &components[..components.len() - 1] {
        let name = CString::new(component.as_slice()).map_err(|_| ())?;
        let child = unsafe {
            libc::openat(
                directory,
                name.as_ptr(),
                libc::O_RDONLY
                    | libc::O_DIRECTORY
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
            )
        };
        if child < 0 {
            unsafe {
                libc::close(directory);
            }
            return Err(());
        }
        if !safe_directory_fd(child) {
            unsafe {
                libc::close(child);
                libc::close(directory);
            }
            return Err(());
        }
        unsafe {
            libc::close(directory);
        }
        directory = child;
    }
    let name = CString::new(components.last().ok_or(())?.as_slice()).map_err(|_| ())?;
    let fd = unsafe {
        libc::openat(
            directory,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    unsafe {
        libc::close(directory);
    }
    if fd < 0 {
        return Err(());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let before = file.metadata().map_err(|_| ())?;
    if !before.is_file()
        || before.uid() != 501
        || before.mode() & 0o7777 != 0o600
        || before.len() > MAX_HELPER_BYTES as u64
    {
        return Err(());
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take((MAX_HELPER_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    let after = file.metadata().map_err(|_| ())?;
    if bytes.len() != before.len() as usize
        || bytes.len() > MAX_HELPER_BYTES
        || (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
        ) != (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
        )
    {
        return Err(());
    }
    Ok(bytes)
}

fn safe_directory_fd(fd: libc::c_int) -> bool {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut stat) } != 0 {
        return false;
    }
    let owner_ok = stat.st_uid == 0 || stat.st_uid == 501;
    let directory = (stat.st_mode & libc::S_IFMT) == libc::S_IFDIR;
    let writable_by_others = stat.st_mode & 0o022 != 0;
    let sticky = stat.st_mode & libc::S_ISVTX != 0;
    owner_ok && directory && (!writable_by_others || sticky)
}

fn run_pinned_source(source: Vec<u8>, deadline: Instant) -> Option<Vec<u8>> {
    let mut child = Command::new(PYTHON)
        .arg("-B")
        .arg("-I")
        .arg("-c")
        .arg(PYTHON_EXEC)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .env("PYTHONNOUSERSITE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take()?;
    let writer = thread::spawn(move || {
        let mut stdin = stdin;
        let result = stdin.write_all(&source);
        drop(stdin);
        result
    });
    let out_reader = thread::spawn(move || drain_bounded(stdout));
    let err_reader = thread::spawn(move || drain_bounded(stderr));
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = writer.join();
                let stdout = out_reader.join().ok()?.ok()?;
                let stderr = err_reader.join().ok()?.ok()?;
                if !status.success() || stdout.len() + stderr.len() > MAX_OUTPUT_BYTES {
                    return None;
                }
                return Some(stdout);
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    let _ = writer.join();
    let _ = out_reader.join();
    let _ = err_reader.join();
    None
}

fn drain_bounded<R: Read>(mut reader: R) -> Result<Vec<u8>, ()> {
    let mut output = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut failed = false;
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(size) => {
                let remaining = MAX_OUTPUT_BYTES
                    .saturating_add(1)
                    .saturating_sub(output.len());
                output.extend_from_slice(&chunk[..size.min(remaining)]);
            }
            Err(_) => {
                failed = true;
                break;
            }
        }
    }
    if failed {
        Err(())
    } else {
        Ok(output)
    }
}

pub(crate) fn validate_snapshot(raw: &[u8], now: DateTime<Utc>) -> Result<Value, ()> {
    if raw.len() > MAX_OUTPUT_BYTES {
        return Err(());
    }
    let snapshot: Snapshot = serde_json::from_slice(raw).map_err(|_| ())?;
    if snapshot.schema_version != 1
        || snapshot.source != "mini-task-maintenance"
        || snapshot.independent_nas_ack != "NOT_PROVIDED"
        || snapshot.assurance != "operator_asserted"
        || snapshot.automatic_resume
        || snapshot.execution_allowed
        || snapshot.links.len() > 4
    {
        return Err(());
    }
    let observed_at = DateTime::parse_from_rfc3339(&snapshot.observed_at)
        .map_err(|_| ())?
        .with_timezone(&Utc);
    let observed_age = now.signed_duration_since(observed_at).num_milliseconds();
    if !(0..=180_000).contains(&observed_age) {
        return Err(());
    }
    match snapshot.state.as_str() {
        "UNKNOWN" => {
            if snapshot.reason != "source_unavailable_or_stale"
                || !snapshot.source_as_of.is_null()
                || snapshot.summary.is_some()
                || snapshot.coverage.is_some()
                || !snapshot.links.is_empty()
            {
                return Err(());
            }
        }
        "observed" => {
            if snapshot.reason != "readonly_projection" {
                return Err(());
            }
            let source_as_of: String =
                serde_json::from_value(snapshot.source_as_of.clone()).map_err(|_| ())?;
            let source_as_of = DateTime::parse_from_rfc3339(&source_as_of)
                .map_err(|_| ())?
                .with_timezone(&Utc);
            let age = now.signed_duration_since(source_as_of).num_milliseconds();
            if source_as_of > observed_at || !(0..=180_000).contains(&age) {
                return Err(());
            }
            validate_summary(&snapshot)?;
        }
        _ => return Err(()),
    }
    serde_json::from_slice(raw).map_err(|_| ())
}

fn validate_summary(snapshot: &Snapshot) -> Result<(), ()> {
    let summary = snapshot.summary.as_ref().ok_or(())?;
    let coverage = snapshot.coverage.as_ref().ok_or(())?;
    if summary.execution_count > 4096
        || summary.linked_count > 4096
        || summary.candidate_count > 4096
        || summary.malformed_annotation_count > 1024
        || summary.candidate_count > summary.linked_count
        || summary.linked_count > summary.execution_count
        || coverage.projected_links != snapshot.links.len() as u64
        || coverage.total_links > 8192
        || coverage.truncated_links > 8192
        || coverage.total_links < coverage.projected_links
        || coverage.truncated_links != coverage.total_links - coverage.projected_links
        || summary.linked_count > coverage.total_links
    {
        return Err(());
    }
    let mut shown_linked = 0u64;
    let mut shown_candidates = 0u64;
    for link in &snapshot.links {
        if !valid_uuid_id(&link.execution_id, "exec_")
            || link.attempt_no == 0
            || link.attempt_no > u32::MAX as u64
            || !valid_reason(&link.reason)
            || !matches!(
                link.status.as_str(),
                "unlinked" | "operator_asserted" | "retracted" | "stale" | "superseded" | "UNKNOWN"
            )
            || !valid_nullable_case(&link.case_id)
            || !valid_nullable_positive_u64(&link.case_episode)
            || !valid_nullable_positive_u64(&link.case_revision)
        {
            return Err(());
        }
        let has_decision = valid_uuid_value(&link.decision_request_id, "dec_");
        if !link.decision_request_id.is_null() && !has_decision {
            return Err(());
        }
        if link.status == "operator_asserted" {
            shown_linked += 1;
        }
        if link.recovery_candidate {
            if link.status != "operator_asserted"
                || link.case_id.is_null()
                || link.case_episode.is_null()
                || link.case_revision.is_null()
                || !has_decision
            {
                return Err(());
            }
            shown_candidates += 1;
        }
    }
    if shown_linked > summary.linked_count || shown_candidates > summary.candidate_count {
        return Err(());
    }
    Ok(())
}

fn valid_uuid_id(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .map(|uuid| {
            let parts: Vec<_> = uuid.split('-').collect();
            parts.len() == 5
                && [8, 4, 4, 4, 12].iter().zip(parts.iter()).all(|(n, part)| {
                    part.len() == *n
                        && part
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                })
                && parts[2]
                    .as_bytes()
                    .first()
                    .map(|b| (b'1'..=b'8').contains(b))
                    .unwrap_or(false)
                && parts[3]
                    .as_bytes()
                    .first()
                    .map(|b| (b'8'..=b'b').contains(b))
                    .unwrap_or(false)
        })
        .unwrap_or(false)
}

fn valid_uuid_value(value: &Value, prefix: &str) -> bool {
    value
        .as_str()
        .map(|id| valid_uuid_id(id, prefix))
        .unwrap_or(false)
}

fn valid_nullable_positive_u64(value: &Value) -> bool {
    value.is_null() || value.as_u64().map(|n| n > 0).unwrap_or(false)
}

fn valid_nullable_case(value: &Value) -> bool {
    value.is_null()
        || value
            .as_str()
            .map(|case| {
                !case.is_empty()
                    && case.len() <= 256
                    && case
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b":._,-".contains(&b))
            })
            .unwrap_or(false)
}

fn valid_reason(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn sample(now: DateTime<Utc>) -> Value {
        json!({
            "schema_version": 1,
            "observed_at": now.to_rfc3339(),
            "source": "mini-task-maintenance",
            "state": "observed",
            "reason": "readonly_projection",
            "independent_nas_ack": "NOT_PROVIDED",
            "assurance": "operator_asserted",
            "automatic_resume": false,
            "execution_allowed": false,
            "source_as_of": now.to_rfc3339(),
            "summary": {"execution_count": 1, "linked_count": 1, "candidate_count": 1,
                "malformed_annotation_count": 0},
            "coverage": {"projected_links": 1, "total_links": 1, "truncated_links": 0},
            "links": [{"execution_id": "exec_11111111-1111-4111-8111-111111111111",
                "attempt_no": 1, "case_id": "infrastructure::mini::evidence_gap",
                "case_episode": 1, "case_revision": 4, "status": "operator_asserted",
                "reason": "review_required", "decision_request_id": "dec_22222222-2222-4222-8222-222222222222",
                "recovery_candidate": true}]
        })
    }

    fn serialized_sample(now: DateTime<Utc>) -> Vec<u8> {
        serde_json::to_vec(&sample(now)).unwrap()
    }

    #[test]
    fn accepts_exact_observed_and_unknown_shapes() {
        let now = Utc::now();
        assert_eq!(
            validate_snapshot(&serialized_sample(now), now).unwrap()["state"],
            "observed"
        );
        let unknown_wire = serde_json::to_vec(&unknown()).unwrap();
        assert_eq!(
            validate_snapshot(&unknown_wire, Utc::now()).unwrap()["state"],
            "UNKNOWN"
        );
    }

    #[test]
    fn malformed_schema_flags_ids_freshness_and_coverage_become_unknown() {
        let now = Utc::now();
        let valid = sample(now);
        let mut cases = Vec::new();
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove("reason");
        cases.push(missing);
        let mut extra = valid.clone();
        extra["unrecognized"] = json!(true);
        cases.push(extra);
        let mut bad_flag = valid.clone();
        bad_flag["execution_allowed"] = json!(true);
        cases.push(bad_flag);
        let mut bad_resume = valid.clone();
        bad_resume["automatic_resume"] = json!(true);
        cases.push(bad_resume);
        let mut future_observation = valid.clone();
        future_observation["observed_at"] =
            json!((now + chrono::Duration::seconds(1)).to_rfc3339());
        cases.push(future_observation);
        let mut expired_observation = valid.clone();
        expired_observation["observed_at"] =
            json!((now - chrono::Duration::seconds(181)).to_rfc3339());
        cases.push(expired_observation);
        let mut future = valid.clone();
        future["source_as_of"] = json!((now + chrono::Duration::seconds(1)).to_rfc3339());
        cases.push(future);
        let mut stale = valid.clone();
        stale["source_as_of"] = json!((now - chrono::Duration::seconds(181)).to_rfc3339());
        cases.push(stale);
        let mut bad_coverage = valid.clone();
        bad_coverage["coverage"]["truncated_links"] = json!(1);
        cases.push(bad_coverage);
        let mut bad_id = valid.clone();
        bad_id["links"][0]["execution_id"] = json!("exec_invalid");
        cases.push(bad_id);
        for case in cases {
            assert!(validate_snapshot(&serde_json::to_vec(&case).unwrap(), now).is_err());
        }
        assert!(validate_snapshot(b"{bad json", now).is_err());
        let duplicate = String::from_utf8(serialized_sample(now)).unwrap().replace(
            "\"execution_count\":1,",
            "\"execution_count\":1,\"execution_count\":1,",
        );
        assert!(validate_snapshot(duplicate.as_bytes(), now).is_err());
        assert!(validate_snapshot(&vec![b' '; MAX_OUTPUT_BYTES + 1], now).is_err());
    }

    #[test]
    fn missing_helper_and_wrong_pin_return_standard_unknown() {
        let temp = tempfile::tempdir().unwrap();
        let absent = temp.path().join("absent.py");
        let missing = observe_from(&absent, "0".repeat(64).as_str(), Duration::from_secs(1));
        assert_eq!(missing["state"], "UNKNOWN");
        let helper = temp.path().join("helper.py");
        std::fs::write(&helper, b"print('{}')\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o600)).unwrap();
        let wrong_pin = observe_from(&helper, "0".repeat(64).as_str(), Duration::from_secs(1));
        assert_eq!(wrong_pin["state"], "UNKNOWN");
    }

    #[test]
    fn helper_process_timeout_and_oversized_output_return_unknown() {
        let timeout = run_pinned_source(
            b"import time; time.sleep(30)\n".to_vec(),
            Instant::now() + Duration::from_millis(80),
        );
        assert!(timeout.is_none());
        let oversized = run_pinned_source(
            b"print('x' * 20000)\n".to_vec(),
            Instant::now() + Duration::from_secs(2),
        );
        assert!(oversized.is_none());
    }
}
