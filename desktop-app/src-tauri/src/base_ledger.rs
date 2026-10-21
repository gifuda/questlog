//! base_ledger — agent-base 外部执行记账薄封装（PROTOCOL v1.5.3 §10.1
//! QuestLog 式：`slot` / `extern-start` / `extern-finish`）。
//!
//! 边界：
//! - 纯 std::process::Command，在 Runner 进程内调用 agent-ctl；**绝不进
//!   seatbelt**（seatbelt 只包 executor 子进程，executor_sandbox.rs）。
//! - 所有错误只返回 `Err(String)` 给调用方 `log::warn!`，不 panic、不阻断
//!   主流程；调用方按 mode 决定 defer/放行。
//! - 绝不传 prompt 原文：只传 `prompt_sha256`（hex64）+ 短标签。
//! - 退出码契约（§10.0）：slot **0**=放行 / **3**=defer（stdout 首行=原因，
//!   可含 `retry_after=<N>s`）/ **1**=账本不可达 / **2**=用法非法。0/3 之外
//!   一律映射 `Unavailable`（fail-closed；用法拼写错误的排障表现是「闸门
//!   不可用」而非显式用法错误——预检已记录该取舍）。
//! - extern-start 成功输出形如 `task 254 accepted (running, external)`：
//!   首 token 是字面 `task`，行 id 在其后的 token——解析取**首个可解析为
//!   i64 的 token**。
//! - `slot` 子命令**无 `--tenant` 旗标**（§10.1 实测）；extern-start/finish
//!   携带 `--tenant`。
//! - mode=Off 时三个入口零 spawn：slot 直接 Ok、start 返回 Err("off") 由
//!   调用方忽略、finish 直接 Ok(())。

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// 配置（mode 三档/tenant/agent_ctl_path/call_timeout）落在 runner_config.rs；
// 这里 pub use 保持 `crate::base_ledger::LedgerMode` 等路径可用。
pub use crate::runner_config::{BaseLedgerConfig, LedgerMode};

// ───────────────────────── 判定与数据结构 ─────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotVerdict {
    Ok,
    Defer(String),
    Unavailable(String),
}

pub struct ExternStart<'a> {
    /// QuestLog execution_id（作底座行 ref；底座幂等键 sha256('external|T|ref')）
    pub r#ref: &'a str,
    /// 『epic 前缀/step 前缀 pkg#N att{n}』——短标签（≤120），不含 prompt
    pub label: &'a str,
    pub workspace: &'a str,
    /// = effective_timeout_secs（runner.rs）
    pub hard_cap: u64,
    pub attempt: u32,
    pub resume: bool,
    /// 复用项目既有 sha2（Cargo.toml sha2 = "0.10"），由调用方算好传入（hex64）
    pub prompt_sha256: &'a str,
}

pub struct ExternFinish<'a> {
    pub ok: bool,
    pub exit_code: i32,
    pub duration_secs: u64,
    pub timed_out: bool,
    pub partial: bool,
    pub session: Option<&'a str>,
    pub error: Option<&'a str>,
    pub raw: Option<&'a str>,
    pub result: Option<&'a str>,
}

// ───────────────────────── 公共 API ─────────────────────────

/// 风控闸门判定（只读，不落行）。`agent-ctl slot --harness H`（无 --tenant）。
pub fn slot(harness: &str, cfg: &BaseLedgerConfig) -> SlotVerdict {
    if cfg.mode == LedgerMode::Off {
        return SlotVerdict::Ok;
    }
    match run_ctl(cfg, &["slot", "--harness", harness]) {
        Ok(o) => parse_slot_output(o.status, &o.stdout, &o.stderr),
        Err(e) => SlotVerdict::Unavailable(e),
    }
}

/// 落底座外部行，返回行 id（stdout 首个可解析为 i64 的 token）。
/// mode=Off 返回 Err("off") 由调用方忽略。
pub fn start(cfg: &BaseLedgerConfig, s: &ExternStart<'_>) -> Result<i64, String> {
    if cfg.mode == LedgerMode::Off {
        return Err("off".into());
    }
    let hard_cap = s.hard_cap.to_string();
    let attempt = s.attempt.to_string();
    let mut args: Vec<&str> = vec![
        "extern-start",
        "--tenant", &cfg.tenant,
        "--harness", "zcode",
        "--ref", s.r#ref,
        "--label", s.label,
        "--workspace", s.workspace,
        "--hard-cap", &hard_cap,
        "--attempt", &attempt,
        "--prompt-sha256", s.prompt_sha256,
    ];
    // §10.1：--resume 为纯开关（true=传，false=不传）
    if s.resume {
        args.push("--resume");
    }
    let o = run_ctl(cfg, &args)?;
    if o.status != Some(0) {
        return Err(format!(
            "extern-start exit {:?}: {}",
            o.status,
            first_line(&o.stderr).or_else(|| first_line(&o.stdout)).unwrap_or_default()
        ));
    }
    parse_first_id_token(&o.stdout)
        .ok_or_else(|| format!("extern-start: no id in stdout {:?}", o.stdout))
}

/// 回填外部行终态（一律 result_ready，ok 表成败）。id 为**位置参数**。
/// 底座二进制实测绘 PROTOCOL §10.1 有漂移（doc: extern-finish/--ok 开关式；
/// 实机: extern-end/--ok true|false）：本封装以实机为准——
/// 接受集 = --ok/--exit-code/--duration/--partial/--error；
/// --timed-out/--session 被实机拒收（timed_out 语义按 §10.1 原文折进
/// error="timed out"；session 无法上送，留在本地 bundle）。
pub fn finish(cfg: &BaseLedgerConfig, id: i64, f: &ExternFinish<'_>) -> Result<(), String> {
    if cfg.mode == LedgerMode::Off {
        return Ok(());
    }
    let id_s = id.to_string();
    let xc = f.exit_code.to_string();
    let dur = f.duration_secs.to_string();
    // §10.1：--error ≤400 字符，截断由租户侧（本封装）做。先落到与 args 同
    // 生命周期的绑定，再以 &str 引用入参（避免引用短于 Vec 借期）。
    let error_arg: Option<String> = match (f.error, f.timed_out) {
        (Some(e), _) => Some(e.chars().take(400).collect()),
        (None, true) => Some("timed out".to_string()),
        (None, false) => None,
    };
    let ok_s = if f.ok { "true" } else { "false" };
    let pa_s = if f.partial { "true" } else { "false" };
    let mut args: Vec<&str> = vec![
        "extern-end",
        &id_s,
        "--ok", ok_s,
        "--exit-code", &xc,
        "--duration", &dur,
        "--partial", pa_s,
    ];
    if let Some(e) = error_arg.as_deref() {
        args.extend(["--error", e]);
    }
    let o = run_ctl(cfg, &args)?;
    if o.status != Some(0) {
        return Err(format!(
            "extern-end exit {:?}: {}",
            o.status,
            first_line(&o.stderr).or_else(|| first_line(&o.stdout)).unwrap_or_default()
        ));
    }
    Ok(())
}

// ───────────────────────── 解析（纯函数，便于单测） ─────────────────────────

pub fn parse_slot_output(status: Option<i32>, stdout: &str, stderr: &str) -> SlotVerdict {
    match status {
        Some(0) => SlotVerdict::Ok,
        Some(3) => SlotVerdict::Defer(first_line(stdout).unwrap_or_else(|| "defer".to_string())),
        Some(c) => SlotVerdict::Unavailable(format!(
            "exit {c}: {}",
            first_line(stderr).or_else(|| first_line(stdout)).unwrap_or_default()
        )),
        None => SlotVerdict::Unavailable("killed by signal / timeout".to_string()),
    }
}

/// extern-start 成功输出实测形如 `task 254 accepted (running, external)`——
/// 首 token 是字面 `task`，行 id 在其后的 token。取**首个**可解析为 i64 的
/// token（容忍 `4711 queued external` 这类 id 在首位的老形态）。
pub fn parse_first_id_token(stdout: &str) -> Option<i64> {
    stdout
        .split_whitespace()
        .find_map(|t| t.trim_end_matches(',').parse::<i64>().ok())
}

fn first_line(s: &str) -> Option<String> {
    s.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

// ───────────────────────── 进程调用（try_wait 轮询，超时 kill） ─────────────────────────

struct CtlOutput {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run_ctl(cfg: &BaseLedgerConfig, args: &[&str]) -> Result<CtlOutput, String> {
    let mut child = Command::new(&cfg.agent_ctl_path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", cfg.agent_ctl_path.display()))?;

    // 读线程：避免子进程写满管道阻塞（agent-ctl 输出极小，仍防御）
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_string(&mut s);
        }
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_string(&mut s);
        }
        s
    });

    let deadline = Instant::now() + Duration::from_secs(cfg.call_timeout_secs.max(1));
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    log::warn!(
                        "base_ledger: agent-ctl {:?} timed out after {}s",
                        args.first(),
                        cfg.call_timeout_secs
                    );
                    return Err(format!("agent-ctl timeout {}s", cfg.call_timeout_secs));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                log::warn!("base_ledger: try_wait error: {e}");
                return Err(format!("try_wait: {e}"));
            }
        }
    };
    let stdout = t_out.join().unwrap_or_default();
    let stderr = t_err.join().unwrap_or_default();
    Ok(CtlOutput { status, stdout, stderr })
}

// ───────────────────────── 单测（施工单 §4 步骤 5 四组） ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_parsing() {
        assert_eq!(parse_slot_output(Some(0), "ok\n", ""), SlotVerdict::Ok);
        // 实测 defer 首行形态（retry_after 带单位）
        assert_eq!(
            parse_slot_output(
                Some(3),
                "defer blackout harness=zcode blackout window end=0935 retry_after=2293s\n",
                ""
            ),
            SlotVerdict::Defer(
                "defer blackout harness=zcode blackout window end=0935 retry_after=2293s".into()
            )
        );
        match parse_slot_output(Some(1), "乱输出 garbage", "") {
            SlotVerdict::Unavailable(m) => assert!(m.starts_with("exit 1:"), "{m}"),
            other => panic!("expected Unavailable, got {other:?}"),
        }
        match parse_slot_output(None, "", "") {
            SlotVerdict::Unavailable(_) => {}
            other => panic!("expected Unavailable on signal, got {other:?}"),
        }
        // extern-start 实测输出：`task 254 accepted (running, external)`
        assert_eq!(parse_first_id_token("task 254 accepted (running, external)\n"), Some(254));
        // 容忍 id 在首位的老形态
        assert_eq!(parse_first_id_token("4711\n"), Some(4711));
        assert_eq!(parse_first_id_token("4711 queued external\n"), Some(4711));
        assert_eq!(parse_first_id_token("error: nope"), None);
    }

    #[test]
    fn mode_off_zero_spawn() {
        // agent_ctl_path 指向会留痕的假脚本；mode=off 时三个入口都不得执行它
        let dir = std::env::temp_dir().join(format!("bl-off-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("called");
        let script = dir.join("fake-agent-ctl");
        std::fs::write(&script, format!("#!/bin/sh\ntouch '{}'\necho 1\n", marker.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cfg = BaseLedgerConfig {
            mode: LedgerMode::Off,
            agent_ctl_path: script.clone(),
            ..Default::default()
        };
        assert_eq!(slot("zcode", &cfg), SlotVerdict::Ok);
        let st = ExternStart {
            r#ref: "exec_x",
            label: "l",
            workspace: "/tmp",
            hard_cap: 900,
            attempt: 1,
            resume: false,
            prompt_sha256: "abc",
        };
        assert!(start(&cfg, &st).is_err());
        let fin = ExternFinish {
            ok: true,
            exit_code: 0,
            duration_secs: 1,
            timed_out: false,
            partial: false,
            session: None,
            error: None,
            raw: None,
            result: None,
        };
        assert!(finish(&cfg, 1, &fin).is_ok());
        assert!(!marker.exists(), "mode=off must not spawn agent-ctl");

        // 对照：advisory 档确实会执行假脚本并解析首 token id
        let cfg2 = BaseLedgerConfig {
            mode: LedgerMode::Advisory,
            agent_ctl_path: script,
            ..Default::default()
        };
        assert_eq!(start(&cfg2, &st), Ok(1));
        assert!(marker.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_backward_compat_roundtrip() {
        use crate::runner_config::RunnerRuntimeConfig;
        // 缺字段 → None（零行为变化）
        let c: RunnerRuntimeConfig =
            serde_json::from_str(r#"{"version":2,"global_enabled":true,"revision":89}"#).unwrap();
        assert!(c.base_ledger.is_none());
        // 仅 mode → 其余缺省
        let c: RunnerRuntimeConfig = serde_json::from_str(
            r#"{"version":2,"global_enabled":true,"base_ledger":{"mode":"advisory"},"revision":90}"#,
        )
        .unwrap();
        let bl = c.base_ledger.unwrap();
        assert_eq!(bl.mode, LedgerMode::Advisory);
        assert_eq!(bl.tenant, "questlog");
        assert_eq!(bl.call_timeout_secs, 8);
        assert_eq!(bl.agent_ctl_path, BaseLedgerConfig::default().agent_ctl_path);
        // 空对象 → mode=off
        let c: RunnerRuntimeConfig =
            serde_json::from_str(r#"{"version":2,"global_enabled":true,"base_ledger":{}}"#).unwrap();
        assert_eq!(c.base_ledger.unwrap().mode, LedgerMode::Off);
        // roundtrip
        let full = BaseLedgerConfig {
            mode: LedgerMode::Required,
            ..Default::default()
        };
        let s = serde_json::to_string(&full).unwrap();
        let back: BaseLedgerConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back, full);
    }

    #[test]
    fn call_timeout_kills_hung_ctl() {
        let dir = std::env::temp_dir().join(format!("bl-hang-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("hang-agent-ctl");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cfg = BaseLedgerConfig {
            mode: LedgerMode::Required,
            agent_ctl_path: script,
            call_timeout_secs: 1,
            ..Default::default()
        };
        let t0 = Instant::now();
        match slot("zcode", &cfg) {
            SlotVerdict::Unavailable(m) => assert!(m.contains("timeout"), "{m}"),
            other => panic!("expected Unavailable(timeout), got {other:?}"),
        }
        assert!(t0.elapsed() < Duration::from_secs(5), "must not wait for the hung child");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
