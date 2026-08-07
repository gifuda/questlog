<div align="center">

<img src="docs/logo.png" alt="QuestLog" width="96" />

# QuestLog

**A persistent board for your MCP-compatible coding sessions.**

**Works with any MCP client** — Claude Code, Codex, OpenCode, Cursor, and more. Capture decisions, todos, and plans from any session; they survive across conversations, projects, and time.

![CI](https://github.com/gifuda/questlog/actions/workflows/ci.yml/badge.svg)
![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Platform](https://img.shields.io/badge/platform-macOS-lightgrey.svg)
![Local-first](https://img.shields.io/badge/data-100%25_local-success.svg)

</div>

---

## Why

Coding sessions are ephemeral. The decision you made yesterday, the bug you spotted, the refactor you planned — all gone when the session closes. QuestLog makes them **persistent**, in a menu bar app that's always one glance away.

```
"Add this to the board"  →  AI pushes to .todos.json  →  Menu bar updates
                              ↑                                │
                              └──── next session reads back ───┘
```

## Core features

| | |
|---|---|
| **Menu bar ambient board** | Tray icon with live count. Click → timeline of all active items across every project. Three seconds to see what's open. |
| **Three-column kanban** | Know-how (decisions) · Todo (work) · Idea (thoughts). Tag filtering, completed archive, soft-delete with undo. |
| **Plan tracking** | Multi-week epics with 5-stage steps, dependency chains, and auto-derived progress bars. One plan per conversation — decompose, don't pile up. |
| **Auto progress capture** | A Stop hook scans the session end, blocks it if Claude mentioned "done" but didn't log it. Never lose a stage advancement. |
| **Board ↔ conversation** | Click 📋 on any item → paste into any session → Claude finds it (even cross-project) and resumes. |
| **100% local** | Plain JSON files. **No database, no cloud, no telemetry.** |

## Install

QuestLog ships **from source only** for now. We don't distribute a pre-built
`.dmg` because unsigned macOS apps trigger Gatekeeper's "damaged, move to
Trash" prompt on Sequoia+, which is a worse first impression than a one-time
build.

**Prerequisites:** [Node.js](https://nodejs.org) 20.19+ (Vite 8 requires ≥20.19) and [Rust](https://rustup.rs).

```bash
git clone https://github.com/gifuda/questlog.git
cd questlog
cd mcp-server && npm install && npm run build && cd ..
cd desktop-app && npm install && npx tauri build && cd ..
bash setup-mcp.sh   # auto-detects installed MCP clients, registers the server, links the pre-commit leak guard
```

The built app lands in `desktop-app/src-tauri/target/release/bundle/`. Open
any MCP-compatible coding session (Claude Code, Codex, OpenCode, Cursor,
ZCode…), say **"add this to the board"** — done.

`setup-mcp.sh` auto-detects which clients are installed and writes the server into each one's user-level config. To target specific clients, override with flags:

```bash
bash setup-mcp.sh --client claude,cursor   # only these
bash setup-mcp.sh --all                    # attempt all 5, ignore detection
bash setup-mcp.sh --help                   # full usage
```

Prefer manual config? Each client's file and where the `todo-board` entry lives:

| Client | Config file | Key path |
|---|---|---|
| Claude Code | `~/.claude.json` (managed by `claude mcp add`) | `mcpServers.todo-board` |
| Cursor | `~/.cursor/mcp.json` | `mcpServers.todo-board` |
| ZCode | `~/.zcode/cli/config.json` | `mcp.servers.todo-board` |
| Codex | `~/.codex/config.toml` | `[mcp_servers.todo-board]` |
| OpenCode | `~/.config/opencode/config.json` | `mcp.todo-board` |

All entries share the same shape: `{"command": "<abs>/node", "args": ["<abs>/mcp-server/dist/index.js"]}`. **Let the script generate the absolute paths rather than hand-writing them** — paths with spaces or unicode break when written by hand.

## Usage

### Capture

| You say | Claude does |
|---|---|
| "Add this to the board" | `push_todo` — auto-detects type and project |
| "This is an idea" | Explicit type override |
| "Create a plan: P0 refactor auth" | `push_epic` — starts a tracked plan |

Three types: **📘 Know-how** (validated experience) · **✅ Todo** (actionable work) · **💡 Idea** (worth exploring).

### Resume

| You want | How |
|---|---|
| Work on a known todo | Say "work on the X todo" — Claude already loaded the board at session start |
| Find it cross-project | Say "find the X item" — `search_todo` scans all projects |
| Resume a plan | Click 📋 on the plan card → paste the reference (`继续方案: X / Continue plan: X` + progress) → Claude calls `list_epics` and continues |

> Copied references follow the app's UI language: `处理待办: X` / `Handle todo: X`, `继续方案: X` / `Continue plan: X`. The MCP server recognizes both prefixes.

### Track plans

```
P0  Refactor auth                              56%  🔵
├── S1  Audit current flow         ████████░░    50%  ◑ coded        ← 可做
├── S2  Design new schema          ███░░░░░░░    25%  ◐ designed
└── S3  Migrate users (depends S2) ░░░░░░░░░░     0%  ☐ pending      🔗
```

- **One top-level plan per conversation** — decompose into child plans or steps, don't create parallel epics
- **Progress is derived** — never hand-written; child plan = mean of steps, top plan = mean of children
- **Dependencies auto-block** — `depends_on` unmet → 🔴 blocked; dependency done → auto-released
- **Actionable steps highlighted** — all prerequisites observed = can work on it now (multiple = parallelizable)

## MCP tools

| Tool | Purpose |
|---|---|
| `push_todo` / `list_todos` / `search_todo` / `update_todo` | Item CRUD + cross-project search |
| `push_epic` / `update_epic` | Plan lifecycle (draft → active → ... → done) |
| `push_step` / `update_step` | Step CRUD + 5-stage progress (pending → designed → coded → verified → observed) |
| `list_epics` | List plans with progress + actionable markers — for resuming |

## Stop hook (auto progress capture)

Claude sometimes forgets to call `update_step` after you say "S2 is done." The Stop hook catches it:

1. Scans the session for steps mentioned as done
2. Checks if `update_step` was called for them
3. If not → **blocks session end** until Claude logs the progress

Add to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "Stop": [{
      "matcher": "",
      "hooks": [{
        "type": "command",
        "command": "node /absolute/path/to/questlog/mcp-server/hooks/capture-progress.mjs",
        "timeout": 15
      }]
    }]
  }
}
```

## Architecture

```
Any MCP client ──MCP──▶ Node.js server ──▶ .todos.json + .epics.json
(Claude Code,                │                    │
 Codex, Cursor,          Stop hook            fs.watch
 OpenCode, ZCode…)           │                    ▼
                             ▼             Tauri (Rust + React)
                      scans transcript        ├── Menu bar popup
                      blocks if unlogged      ├── Kanban board
                                              └── Plan board
```

**Stack:** Tauri v2 · React · Tailwind v4 · TypeScript · notify crate

## Privacy

- All data in local JSON files — no database, no cloud, no telemetry
- MCP server only reads/writes within your project directories
- Pre-commit guard blocks commits with personal paths, IPs, project names, or credentials

## Troubleshooting

| Problem | Fix |
|---|---|
| Menu bar icon missing | Restart app; check Activity Monitor for "QuestLog" |
| MCP tools unavailable | Re-run `bash setup-mcp.sh` — the summary table shows each client's status. Verify the entry in your client's config file (see the table above). |
| Items not showing | Check `~/.todo-board.json` lists the project; reopen app |
| Stop hook not firing | Verify absolute path + `node` on PATH |
| Pre-commit guard false positive | `git commit --no-verify` for doc examples |

## Contributing

Fork → branch → PR. The pre-commit guard runs automatically. Areas needing help: Windows/Linux, drag-and-drop, export/backup, i18n.

## License

[MIT](LICENSE)

---

[中文文档](README.zh-CN.md) · [Report Bug](../../issues) · [Request Feature](../../issues)
