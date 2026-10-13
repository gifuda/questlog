# S5 施工规格 · M4 客户端化（executor 执行单）

> 主模型已定稿设计。本单为机械施工：按规格实现，跑门禁，不改规格外行为。
> 红线：不动 seatbelt/seal/observed/kill switch；不动既有锁协议；全部新增走既有模块边界。

## 目标形态

远程模式（`data_root::is_remote()` 且 marker 带 `api_base`）下：GUI 读面走 m5p HTTP API；写面（todo/epic/step 编辑、开关、DR、retry/control）走 API POST；本地模式行为零变化。m5p 侧 api_server 增补对应端点（薄包 commands 的 `_impl` 纯函数）。

## 1. data_root.rs：RemoteConfig 增字段

```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RemoteConfig {
    pub prefix_from: String,
    pub mirror_root: PathBuf,
    /// S5: m5p API 基址（如 "http://powermac-mini:5889"）。缺省=无 API（桥接期纯镜像只读）。
    #[serde(default)]
    pub api_base: Option<String>,
}
pub fn api_base() -> Option<&'static str> {
    cfg().and_then(|c| c.api_base.as_deref())
}
```
（cfg() 已存在。测试夹具补 `api_base: None`。）

## 2. 新文件 src/remote_client.rs（全文照抄）

```rust
//! Minimal HTTP client for the m5p authority API (S5 remote mode).
//! std-only, mirrors api_server's protocol; every call has a short timeout
//! so an unreachable authority degrades into a visible error, never a hang.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(8);

pub fn enabled() -> bool {
    crate::data_root::is_remote() && crate::data_root::api_base().is_some()
}

fn base() -> Result<String, String> {
    crate::data_root::api_base()
        .map(|b| b.to_string())
        .ok_or_else(|| "remote mode has no api_base configured".to_string())
}

/// Percent-encode a query value (UTF-8 → %XX; unreserved chars pass).
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn request(method: &str, path_and_query: &str, body: Option<&serde_json::Value>) -> Result<serde_json::Value, String> {
    let base = base()?;
    let url = base.trim_end_matches('/');
    let (host_port, _scheme) = match url.strip_prefix("http://") {
        Some(rest) => (rest, ()),
        None => return Err(format!("unsupported api_base (need http://): {url}")),
    };
    let payload = body.map(|v| serde_json::to_vec(v)).transpose().map_err(|e| format!("serialize: {e}"))?;
    let mut stream = TcpStream::connect(host_port).map_err(|e| format!("api connect {host_port}: {e}"))?;
    stream.set_read_timeout(Some(TIMEOUT)).ok();
    stream.set_write_timeout(Some(TIMEOUT)).ok();
    let mut req = format!(
        "{method} {path_and_query} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n"
    );
    if let Some(p) = &payload {
        req.push_str("Content-Type: application/json\r\n");
        req.push_str(&format!("Content-Length: {}\r\n", p.len()));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).map_err(|e| format!("api write: {e}"))?;
    if let Some(p) = &payload {
        stream.write_all(p).map_err(|e| format!("api write body: {e}"))?;
    }
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|e| format!("api read: {e}"))?;
    let text = String::from_utf8_lossy(&raw);
    let body_start = text.find("\r\n\r\n").ok_or("api response: no header end")?;
    let status_line = text.lines().next().unwrap_or_default();
    let status: u16 = status_line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let resp_body = &text[body_start + 4..];
    let value: serde_json::Value = serde_json::from_str(resp_body.trim_start()).map_err(|e| format!("api parse: {e}"))?;
    if status >= 400 {
        return Err(format!("api {status}: {}", value.get("error").and_then(|x| x.as_str()).unwrap_or("?")));
    }
    Ok(value)
}

pub fn get_json(path_and_query: &str) -> Result<serde_json::Value, String> {
    request("GET", path_and_query, None)
}

pub fn post_json(path_and_query: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    request("POST", path_and_query, Some(body))
}

/// Deserialize an API payload into the local command's return type.
pub fn get_into<T: serde::de::DeserializeOwned>(path_and_query: &str) -> Result<T, String> {
    let v = get_json(path_and_query)?;
    serde_json::from_value(v).map_err(|e| format!("api shape mismatch: {e}"))
}
```

lib.rs：`pub mod remote_client;`（按字母序插入 pub mod 列表）。

## 3. headless 多地址绑定（bin/headless.rs）

`--bind` 允许多次出现。改造 main：收集 `Vec<String>` binds（至少一个，默认 127.0.0.1:5889）；serve 分支改为每个 bind `std::thread::spawn` 调 `api_server::serve(&b)`（serve 内部已阻塞 accept；多 listener 各占一线程）。无 serve 时保持 park 循环。

## 4. api_server.rs 增补路由（m5p 侧）

先在 commands.rs 完成 §5 的 `_impl` 重构，然后加路由（全部走 `parse_body` + 显式取参 + `map_store`）：

| 路由 | 实现要点 |
|---|---|
| GET /api/trash | `map_store(crate::commands::get_trash())`（纯函数直接包） |
| GET /api/all-active | `map_store(crate::commands::get_all_active_impl(&crate::discovery::discover_projects()))` |
| GET /api/epics-global | `map_store(crate::commands::get_epics_global_impl(&discover_projects()))` |
| GET /api/project-items?project= | project_allowed 门 + `crate::commands::get_project_items_impl(&discover(), p)` |
| GET /api/epic-auto-execute?epic_id=&project= | `map_store(crate::commands::get_epic_auto_execute(epic_id, project))`（纯函数直接包） |
| POST /api/epic-auto-execute | body {epic_id, project_path, auto_execute:bool?} → commands::set_epic_auto_execute |
| GET /api/runner-project-status?project= | `map_store(crate::commands::get_runner_project_status(p))`（纯） |
| GET /api/executor-options | `map_store(crate::commands::get_executor_options())`（纯） |
| GET /api/todo-counts | 遍历 discover_projects()，read_todos 计 Active+Todo 数 → `{"counts":[{"name","path","active"}],"total":N}` |
| POST /api/todos/add | body 全字段镜像 commands::add_todo 参数（project_path, item_type, title, content?, suggestion?, tags?, priority?）→ `add_todo_impl(&discover(), …)` |
| POST /api/todos/update | 镜像 update_todo 参数（project_path,id,status?,content?,tags?,priority?,title?,item_type?,processing?,auto_process?,auto_ack?）→ `update_todo_impl(&discover(), …)` |
| POST /api/epics/add | add_epic_impl（project_path,title,description?,priority?,depends_on?,parent_id?,acceptance?） |
| POST /api/epics/update | update_epic_impl（按实际签名镜像全部 Option 字段） |
| POST /api/epics/delete | delete_epic_impl(project_path, id) |
| POST /api/steps/add | add_step_impl（按实际签名） |
| POST /api/steps/update | update_step_impl（id,title?,description?,stage?,order?,depends_on?,todo_ref? + project_path） |
| POST /api/steps/delete | delete_step_impl(project_path, id) |

取参 helper（api_server 内）：`fn s<'a>(v:&'a serde_json::Value,k:&str)->Option<&'a str>`、`fn os(v,k)->Option<String>`、`fn ob(v,k)->Option<bool>`、`fn ovec(v,k)->Option<Vec<String>>`、必填缺失返回 `err(400, format!("body needs '{k}'"))`。

## 5. commands.rs `_impl` 重构（8 个 State 命令）

模式（以 add_todo 为例）：
```rust
#[tauri::command]
pub fn add_todo(project_path: String, state: State<AppState>, item_type: String, /* …原参数 */) -> Result<TodoItem, String> {
    let projects = state.projects.lock().map_err(|e| format!("state: {e}"))?.clone();
    add_todo_impl(&projects, project_path, item_type, /* …透传 */)
}

/// Plain core: projects = registry allowlist（API 路由传 discover_projects()）。
pub fn add_todo_impl(projects: &[(String, std::path::PathBuf)], project_path: String, /* …原参数去 State */) -> Result<TodoItem, String> {
    // 原 body 原样搬入，validate_project_path 改为对 projects 列表校验：
    let path = validate_project_path_list(&project_path, projects)?;
    …
}
```
- 新增 `fn validate_project_path_list(p:&str, projects:&[(String,PathBuf)]) -> Result<PathBuf,String>`：把现有 `validate_project_path`（:33-48）的名单比对逻辑抽到这里；原 `validate_project_path(&p,&state)` 改为锁 state 后调 list 版（保持旧签名给未重构命令用——本单内全部 State 变更命令都重构后，旧 helper 若无调用者则删除）。
- 同法重构：update_todo / add_todo / get_project_items / add_epic / update_epic / delete_epic / add_step / update_step / delete_step / get_todo_status / get_all_active / get_epics_global / get_todos / get_projects / get_fleet_health（读命令也一并 impl 化，供 API 路由与 GUI 远程分支复用；get_projects 注意它返回 Vec<serde_json::Value> 且 body 可能含附加字段——纯搬运）。
- **搬 body 时逐行保持原语义，不改任何锁/写/派生逻辑。**

## 6. commands.rs 远程分支（GUI 侧）

模式（读）：
```rust
#[tauri::command]
pub fn get_todos(project_path: String, state: State<AppState>) -> Result<TodosFile, String> {
    if crate::remote_client::enabled() {
        return crate::remote_client::get_into(&format!("/api/todos?project={}", crate::remote_client::urlencode(&project_path)));
    }
    …原逻辑…
}
```
写（POST）：`post_json("/api/todos/update", &json!({"project_path":…,"id":…, …仅 Some 字段}))` 后按需 `get_into` 或忽略返回。

需加远程分支的命令全集（读）：get_projects（GET /api/projects，注意响应形状 `{"projects":[{name,path}]}` → 映射回原返回形状 Vec<Value>，用 `v["projects"]` 反序列化 `[{"name","path"}]` 数组成 Vec<(String,PathBuf)> 再按原 get_projects 的 JSON 组装——**直接调 get_projects_impl 于映射后列表**，保证形状一致）、get_todos、get_all_active（GET /api/all-active）、get_project_items、get_epics（GET /api/epics?project=）、get_epics_global、get_trash、get_executions、get_current_execution、get_executor_options、get_fleet_health、get_todo_status（GET /api/todo-status?project=，None 时不带参）、get_runner_runtime_status（GET /api/runner/status）、get_runner_project_status、get_epic_auto_execute（query 两参）。
写：update_todo、add_todo、add_epic、update_epic、delete_epic、add_step、update_step、delete_step、set_epic_auto_execute（POST /api/epic-auto-execute）。（开关/DR/retry/control 五个在 S3/S4 已有 API 端点，同样补远程分支：set_runner_global_enabled→POST /api/runner/global-enabled {enabled} 回传 bool、set_runner_project_enabled→/api/runner/project-enabled、resolve_decision_request→/api/decision-requests/resolve {id,resolution}、retry_execution→/api/executions/retry {id} 返回 String、cancel_execution/pause_execution→/api/executions/control {id,pause:false|true}。）
**不加远程分支**：get_arb_board/set_arb_board/submit_arb_control/run_arb_decision（arb 腿留本地）、show_main_window/quit_app/diag_report_error/undo_remove_project/restore_project/permanently_delete_project/reorder_projects/remove_project（窗口控制与罕见注册表管理——远程模式照旧本地执行；注册表管理命令在镜像模式本就被 guard 拒绝，报错可见即可）。

## 7. tray.rs 远程分支（假绿修复）

徽标循环内：
```rust
let (total, ok) = if crate::remote_client::enabled() {
    match crate::remote_client::get_json("/api/todo-counts") {
        Ok(v) => (v.get("total").and_then(|t| t.as_u64()).unwrap_or(0) as usize, true),
        Err(_) => (0, false),
    }
} else { …原逻辑，ok=true… };
```
徽标设置处：`!ok` → 置 badge 字符串 `"!"`（表达不可用，不显示 0）。找不到 tray 的分支保持原样。

## 8. watcher.rs 远程跳板

`start_watching` 开头：若 `crate::remote_client::enabled()`（API 模式，无镜像可看）→ 只保留 arb board 父目录 watch（若配置存在），跳过 projects/registry/runtime watch，直接进事件循环（或提前 return 前仍 spawn 空循环——保持函数结构最小改动：用 if 包住三段 watch 注册，arb 段保留）。

## 9. 前端（desktop-app/src/）

- `hooks/useExecutions.ts`：`useRefreshFallback(refresh)`（import 并在 hook 体内调用，同 useStore 用法；若该文件已有 interval 则替换）。
- `hooks/useFleetHealth.ts`：同上。
- DR 收件箱数据钩子（搜 `get_decision_requests` 的 invoke 所在 hook/组件，通常是 useExecutions 或专用 hook）：同上。
- `hooks/useEpics.ts`（若存在）：同上。
- `components/TimelinePanel.tsx:181`：`invoke("navigate_to_project", …)` → `invoke("show_main_window").catch(() => {})`（该命令已注册；修复静默死链）。

## 10. 门禁（executor 必跑并附输出尾行）

1. `cargo check --all-targets`（0 error）
2. `cargo clippy --all-targets`（新增代码零 warning；存量 14 条不计）
3. `cargo test --lib`（全绿；若 _impl 重构动了被测函数，测试照跑不许改断言——只允许改调用形参）
4. `cd desktop-app && npm test`（vitest 全绿）
5. `cargo build --release --bin headless` 成功

## 纪律

- 不改 api_server 既有路由行为；不动 data_root 既有 guard/map 语义（只加 api_base 字段）
- _impl 搬运逐行保持原语义；不做"顺手优化"
- 所有新 pub fn 带 1-2 行 doc 注释说明用途
- 完成后 `git status` 干净度自查：只应触及 本单列出文件
