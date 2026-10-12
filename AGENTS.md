# 桌面清单贴 项目规则（项目级 AGENTS.md）

本项目是 QuestLog 受控任务执行系统本体（桌面端 + MCP server + Runner + collector 租户）。通用治理见 `~/.zcode/AGENTS.md`，冲突时全局优先。

## 仓库与工具链

- git 仓库，子智能体的 base/head/diff 输入契约**正常使用**，无需替代协议。
- `mcp-server/`：TypeScript（vitest）；`desktop-app/src-tauri/`：Rust；`desktop-app/` 前端。
- 会话若直接开在 `mcp-server/` 子目录，规则发现会向上命中本文件，无需单独再建。

## 验收命令注册表（硬同步红线）

受信验收命令是**硬编码常量，分布在两处，必须同步**：

- `mcp-server/src/task-package.ts` → `ACCEPTANCE_REGISTRY`
- `desktop-app/src-tauri/src/runner.rs` → `ACCEPTANCE_REGISTRY`

新增/修改命令 = 人工 code review + 两处同步 + 重建 MCP 和 Tauri。命令清单见 `docs/acceptance-commands.md`。任何子智能体不得擅自添加验收命令。

## 子智能体调度要点

- 涉及 Runner/TaskPackage 的施工，验收命令只能从上表选，按全局 AGENTS.md 的语言匹配规则（Rust 用 `cargo:check`/`cargo:clippy`，TS 用 `lint:mcp`/`test:mcp`/`build:mcp`），绝不跨语言错配。
- worktree 治理：`sweep_worktrees` 默认 dry-run，`apply=true` 需用户当轮确认。
- Runner 开关语义：总闸 + 项目开关 + epic auto_execute 三层，包只封存不入队，开关全开才被领取。
