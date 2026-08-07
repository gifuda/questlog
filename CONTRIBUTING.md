# Contributing to QuestLog

Thanks for your interest in making QuestLog better! This is a small, focused project — every contribution counts.

QuestLog is a **local-first** menu bar board for MCP-compatible coding sessions. Two parts:

- `mcp-server/` — Node.js + TypeScript MCP server (reads/writes JSON files)
- `desktop-app/` — Tauri (Rust + React + Tailwind) menu bar app (displays the board)

---

## Things we need help with

These areas are explicitly open — jump in even without asking:

| Area | Where |
|---|---|
| **Windows / Linux support** | Tauri config, tray code, build CI |
| **Drag-and-drop reordering** | `desktop-app/src/components/KanbanBoard.tsx` |
| **Export / backup** | new command in `desktop-app/src-tauri/src/commands.rs` |
| **i18n (English ↔ Chinese)** | string tables across `desktop-app/src/components/` |
| **Tests** | none yet — see [Testing](#testing) below |

Got a different idea? Open an issue first so we can scope it together.

---

## Setup (15 minutes)

### Prerequisites

- [Node.js](https://nodejs.org) 18+
- [Rust](https://rustup.rs) (stable)
- macOS (Windows/Linux work but are less tested — see open issues)

### Build both parts

```bash
git clone https://github.com/gifuda/questlog.git
cd questlog

# MCP server
cd mcp-server && npm install && npm run build && cd ..

# Desktop app
cd desktop-app && npm install && npx tauri build && cd ..
```

### Run in dev mode

Two terminals:

```bash
# Terminal 1 — desktop app (hot reload)
cd desktop-app && npm run tauri dev

# Terminal 2 — MCP server (watch mode, if you're also touching the server)
cd mcp-server && npm run dev
```

### Register the MCP server with your client

```bash
bash setup-mcp.sh   # registers the server + installs the pre-commit leak guard
```

---

## Workflow

1. **Fork & branch** from `master`:
   ```bash
   git checkout -b feat/short-description
   ```
2. **Make your change.** Keep the diff focused — one feature/fix per PR is easier to review.
3. **Commit.** The pre-commit guard runs automatically (see below).
4. **Push & open a PR** against `master`. Fill in the PR template.

### The pre-commit guard

Every commit triggers `scripts/pre-commit-guard.sh`, which scans staged additions for:

- Hardcoded absolute paths pointing at a specific user's home directory
- Internal IPs and hostnames
- Credentials (API keys, tokens)
- Personal data files (`.todos.json`, `.epics.json`, etc.)

If it blocks a legitimate change (e.g. an example path in docs), use `git commit --no-verify` — but **only** after confirming the flagged content is safe to publish.

---

## Commit messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>: <description>
```

| Type | Use for |
|---|---|
| `feat` | New feature |
| `fix` | Bug fix |
| `docs` | Documentation only |
| `refactor` | Code change that neither fixes a bug nor adds a feature |
| `perf` | Performance improvement |
| `chore` | Build, deps, tooling |
| `test` | Adding or fixing tests |

Examples:

```
feat: add drag-and-drop reordering to kanban columns
fix: tray icon disappears after sleep on macOS 14
docs: clarify setup-mcp.sh for Windows users
```

---

## Code style

- **TypeScript / React**: ESLint + Prettier defaults. Function components with hooks.
- **Rust**: `cargo fmt` + `cargo clippy` clean. Match the existing module structure.
- **No new dependencies** without justification — this is a small app, keep the surface minimal.
- **Local-first, always.** Don't introduce cloud calls, telemetry, or anything that phones home. If your change needs network access, open an issue first to discuss.

---

## Testing

Automated tests cover the **core business algorithms** — progress derivation, dependency blocking, actionable-step detection — on both the TypeScript (MCP server) and Rust (Tauri backend) sides. These are mirrored suites: the same data shapes run through both implementations, so a change that breaks one tends to break the other.

**Run locally:**

```bash
# MCP server (TypeScript, vitest) — run `npm test` to see the current count
cd mcp-server && npm test

# Tauri backend (Rust, cargo test) — run `cargo test` to see the current count
cd desktop-app/src-tauri && cargo test
```

**CI** runs both suites on every push to `master`/`main` and on every PR (see `.github/workflows/ci.yml`).

**What's covered (and what isn't):**

| Covered | Not yet covered |
|---|---|
| MCP server pure logic (`schema`, `epic-store`, `pushTodo` gates) | MCP protocol handlers in `index.ts` |
| Rust pure logic (`epic_progress`, `derive_dependencies`, `actionable_steps`, `build_summary`) | Tauri `#[tauri::command]` wrappers |
| Rust serde round-trips + enum mappings | `discovery.rs` registry CRUD (uses `dirs::home_dir()`) |
| Rust data layer round-trips (via `tempfile`) | Frontend hooks/components (need IPC mock env) |
| | `tray.rs` / `watcher.rs` (GUI-bound) |

**Adding tests:**

- **MCP server**: add a file in `mcp-server/test/` (e.g. `feature.test.ts`). Use the factories in `test/fixtures.ts` to build epic/step data. Run with `npm test:watch` while iterating.
- **Rust**: add a `#[cfg(test)] mod tests` block at the bottom of the source file. For filesystem-touching tests, use `tempfile::tempdir()` to isolate.

---

## Reporting bugs

Use the **Bug report** issue template. Include:

- macOS version / OS
- Commit hash you built from (QuestLog ships source-only for now)
- Steps to reproduce
- Expected vs actual behavior
- Console output (open the app from terminal to capture logs)

---

## Pull requests

- **One concern per PR.** Split unrelated changes into separate PRs.
- **Reference the issue** if your PR closes one (`Closes #123`).
- **Screenshots** for any UI change.
- **Be patient.** This is maintained by a small team — reviews may take a few days.

---

## License

By contributing, you agree your contributions will be licensed under the [MIT License](LICENSE).

---

Questions? Open an issue with the `question` label. Thanks again for helping out! 🙌
