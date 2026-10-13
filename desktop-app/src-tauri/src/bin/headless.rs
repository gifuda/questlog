//! QuestLog headless runtime (S3): Runner + nightly tick + minimal API,
//! without the Tauri GUI.
//!
//! Deployment target is m5p via launchd (S4); on M4 it doubles as the
//! smoke-test surface (`QUESTLOG_RUNTIME=<tmpdir>` isolates everything
//! except read-only registry discovery). All engine modules are shared
//! with the GUI build — this entry point only decides which threads and
//! surfaces to bring up.
//!
//! Switch semantics are identical to the GUI: the Runner thread only
//! starts when global_enabled AND at least one project switch is on; the
//! todo tick runs unconditionally (audit B-3) and gates its own actions.

use std::sync::OnceLock;

fn main() {
    init_logger();
    let args: Vec<String> = std::env::args().collect();
    let serve = args.iter().any(|a| a == "--serve");
    // --bind may appear multiple times (S5): one listener thread per address
    // (e.g. loopback + tailnet interface). At least one bind, default loopback.
    let mut binds: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--bind" {
            if let Some(v) = args.get(i + 1) {
                binds.push(v.clone());
            }
        }
        i += 1;
    }
    if binds.is_empty() {
        binds.push("127.0.0.1:5889".to_string());
    }

    app_lib::data_root::init();
    if app_lib::data_root::is_remote() {
        // The headless process is the AUTHORITY side. A remote-mirror marker
        // here means this machine is a read-only mirror client — running an
        // engine against the mirror would be a category error.
        eprintln!(
            "questlog-headless: remote mirror marker present — this machine is a \
             read-only mirror client; refusing to start the engine"
        );
        std::process::exit(2);
    }

    // Same gating as lib.rs GUI setup: switches off = shadow/idle.
    if app_lib::runner_config::is_globally_enabled()
        && app_lib::runner_config::any_project_enabled()
    {
        log::info!("Runner: auto-starting (project(s) enabled)");
        app_lib::runner::ensure_running();
    } else {
        log::info!("Runner: staying off (global switch off or no projects enabled)");
    }

    // Audit B-3 semantics preserved: tick runs unconditionally and cheaply
    // idles with no schedule configured; every action re-checks its gates.
    app_lib::todo_sweep::start_tick_thread();
    log::info!(
        "questlog-headless up (pid {}, api {})",
        std::process::id(),
        if serve {
            binds.iter().map(|b| format!("http://{b}")).collect::<Vec<_>>().join(", ")
        } else {
            "off".to_string()
        }
    );

    if serve {
        let mut handles = Vec::new();
        for b in binds {
            handles.push(std::thread::spawn(move || {
                if let Err(e) = app_lib::api_server::serve(&b) {
                    eprintln!("questlog-headless: {e}");
                    std::process::exit(1);
                }
            }));
        }
        for h in handles {
            let _ = h.join();
        }
    } else {
        loop {
            std::thread::park();
        }
    }
}

/// Minimal stdout logger — the GUI uses tauri-plugin-log, which the
/// headless binary cannot host. Info level keeps the launchd log lean.
fn init_logger() {
    struct StdoutLogger;
    impl log::Log for StdoutLogger {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Info
        }
        fn log(&self, record: &log::Record) {
            if self.enabled(record.metadata()) {
                println!("[{}] {}", record.level(), record.args());
            }
        }
        fn flush(&self) {}
    }
    static LOGGER: StdoutLogger = StdoutLogger;
    static INIT: OnceLock<()> = OnceLock::new();
    let _ = INIT.set(());
    let _ = log::set_boxed_logger(Box::new(StdoutLogger));
    log::set_max_level(log::LevelFilter::Info);
    let _ = LOGGER;
}
