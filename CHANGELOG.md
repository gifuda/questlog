# Changelog

All notable changes to QuestLog are tracked here. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), versions follow [SemVer](https://semver.org/).

## [0.1.0] - 2026-08-07

Initial release.

### Added
- **Menu bar ambient board** — tray icon with live active-item count; click opens a timeline of active items across every project
- **Three-column kanban** — Know-how / Todo / Idea with tag filtering, completed archive, soft-delete with undo
- **Plan tracking** — epics with 5-stage steps (pending → designed → coded → verified → observed), dependency chains with auto-blocking, auto-derived progress (never hand-written)
- **Auto progress capture** — Stop hook scans the session end and blocks it if a step was mentioned as done but not logged
- **Board ↔ conversation** — copy a reference (`处理待办: X` / `Handle todo: X`, `继续方案: X` / `Continue plan: X`) and paste it into any MCP-compatible session to resume
- **MCP server** — 10 tools (`push_todo`, `list_todos`, `search_todo`, `update_todo`, `push_epic`, `update_epic`, `push_step`, `update_step`, `list_epics`) for any MCP client (Claude Code, Cursor, ZCode, Codex, OpenCode)
- **Bilingual UI** — Chinese / English, switchable in Settings, persisted, default follows the system language; clipboard protocol text follows the UI language
- **`setup-mcp.sh`** — one-shot registration of the MCP server for all five clients, with absolute node paths and idempotent three-way config merge

### Fixed
- Pre-commit guard promise in READMEs is now fulfilled — `setup-mcp.sh` links `scripts/pre-commit-guard.sh` into `.git/hooks/pre-commit`
- Corrupt data files (`.todos.json` / `.epics.json` / `~/.todo-board.json`) are backed up as `.bak.<ts>` instead of being silently overwritten
- Registry writes are atomic (temp file + rename); temp names are unique per process so concurrent writers cannot clobber each other
- `project_dir` / `project_path` inputs are canonicalized and validated against the workspace/registry — `..` escapes and outside paths are rejected

### Security
- MCP server only reads/writes within the current workspace or registered projects
- Pre-commit guard blocks commits with personal paths, IPs, project names, or credentials
- All data stays local — plain JSON, no database, no cloud, no telemetry
