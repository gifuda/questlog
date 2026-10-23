# 受信验收命令注册表

Runner 验收阶段在隔离 worktree 中执行**受信命令**（command_id → 固定 shell 命令 + cwd），不接受任意 shell 字符串。命令注册表是**硬编码常量**，分布在两处、必须保持同步：

- Node/MCP 侧：`mcp-server/src/task-package.ts` → `ACCEPTANCE_REGISTRY`
- Rust/Runner 侧：`desktop-app/src-tauri/src/runner.rs` → `ACCEPTANCE_REGISTRY`

新增命令 = 人工 code review（两处同步加条目 + 重建 MCP 和 Tauri）。

---

## 命令清单（12 个）

| id | 命令 | 工作目录 | 用途 |
|---|---|---|---|
| `verify:candidate` | `git diff --check HEAD^ HEAD` | `.` | 验证候选 commit 的 patch 干净（无空白错误） |
| `verify:readonly` | `git diff --quiet HEAD` | `.` | 只读任务产物契约：worktree 相对 base 无改动（需配合非空 stdout 报告） |
| **TypeScript（MCP server）** | | | |
| `test:mcp` | `npm test` | `mcp-server` | MCP server 单元测试（vitest） |
| `build:mcp` | `npm run build` | `mcp-server` | MCP server TypeScript 编译 |
| `lint:mcp` | `npx tsc --noEmit` | `mcp-server` | MCP server 类型检查 |
| **Rust（desktop backend）** | | | |
| `cargo:check` | `cargo check` | `desktop-app/src-tauri` | 快速类型/编译检查 |
| `cargo:clippy` | `cargo clippy -- -D warnings` | `desktop-app/src-tauri` | 严格 lint（warning 当 error） |
| `cargo:build` | `cargo build` | `desktop-app/src-tauri` | 编译（不跑测试） |
| `test:desktop` | `cargo test` | `desktop-app/src-tauri` | 确定性 Rust 测试；明确标记的真实 CLI E2E 默认 ignored |
| **前端（desktop web）** | | | |
| `build:web` | `npm run build` | `desktop-app` | 前端生产构建（vite） |
| `test:web` | `npm test` | `desktop-app` | 前端单元测试（vitest） |
| `lint:web` | `npx tsc --noEmit` | `desktop-app` | 前端类型检查 |

---

## 选用决策树

```
改了什么文件？
├─ .rs 文件（desktop-app/src-tauri/）
│   ├─ 文档注释/小改 → cargo:check（首选，最快）
│   ├─ 逻辑改动     → cargo:check + cargo:clippy
│   └─ 新功能/重构  → cargo:check + test:desktop
├─ .ts 文件（mcp-server/src/）
│   ├─ 类型/小改    → lint:mcp（type-check）
│   ├─ 逻辑改动     → lint:mcp + test:mcp
│   └─ 新功能       → lint:mcp + test:mcp + build:mcp
├─ .tsx/.ts 文件（desktop-app/src/）
│   ├─ 类型/小改    → lint:web
│   ├─ 逻辑改动     → lint:web + test:web
│   └─ 新功能       → lint:web + test:web + build:web
└─ 总是可附加 → verify:candidate（验证 patch 干净度）
```

---

## 真实 ZCode E2E

`desktop-app/src-tauri/tests/e2e_bridge.rs` 中两个 live-ZCode 测试默认标记为
`ignored`，普通 `cargo test` 不会启动真实模型或读取用户 CLI 凭据。默认运行
`cargo test --test e2e_bridge` 时应显示 0 executed、2 ignored。

仅在获得本次人工授权、具备相应模型预算，并确认本机 ZCode 与
`~/.zcode/cli/config.json` 已配置后，才逐项显式运行，例如：

```sh
QUESTLOG_LIVE_ZCODE_E2E=1 cargo test --test e2e_bridge bridge_ready_package_to_zcode_execution -- --ignored --exact
QUESTLOG_LIVE_ZCODE_E2E=1 cargo test --test e2e_bridge bridge_readonly_package_no_diff_counts_as_passed -- --ignored --exact
```

显式选择 ignored 测试却缺少 opt-in、ZCode CLI 或当前 `HOME` 下的配置文件时，
测试会明确失败，不会将缺少外部依赖报告为通过。不要复制凭据到隔离环境。

## 常见错误

### Rust 任务用了 `build:mcp` → exit 127

```
stderr: sh: tsc: command not found
```

原因：Rust 任务不碰 TypeScript，却用 `build:mcp`（`npm run build` → `tsc`）。worktree 有 node_modules symlink，但 `cargo:check` 根本不需要它。**用 `cargo:check`。**

### TypeScript 任务用了 `cargo:check` → 编译可能通过但没测到 TS

原因：cargo 只编译 Rust，不碰 TypeScript。TS 改动用 `lint:mcp` 或 `lint:web`。

### 多语言跨改（同时改 .rs 和 .ts）

同时指定两个命令（如 `cargo:check` + `lint:mcp`），验收器逐个跑，任一失败则整体验收失败。
