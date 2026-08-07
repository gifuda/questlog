# Security Policy

## Supported versions

QuestLog is early-stage. Security fixes target the latest release only.

| Version | Supported |
|---|---|
| latest `master` | ✅ |
| tagged releases | ✅ |
| older branches | ❌ |

## Reporting a vulnerability

**Please do not open a public GitHub issue for security problems.**

Instead, report privately using one of these methods:

1. **GitHub Security Advisories** (preferred): go to the [Security tab](../../security/advisories/new) of this repo and create a private advisory. This lets us discuss and patch collaboratively without public exposure.
2. **Email**: send details to the maintainer via GitHub's contact path. Put `[QuestLog security]` in the subject line.

Please include:

- A description of the issue and its potential impact
- Steps to reproduce (code, config, or a minimal example)
- Affected version (commit hash or release tag)
- Suggested fix, if you have one

## Response timeline

| Step | Target |
|---|---|
| Acknowledge receipt | within 72 hours |
| Initial assessment | within 7 days |
| Fix or mitigation | within 30 days (severity-dependent) |
| Public disclosure | after a fix is released, coordinated with you |

We'll keep you informed at each step. We appreciate responsible disclosure and will credit reporters in the advisory unless you prefer to remain anonymous.

## Scope

QuestLog is **100% local**: data lives in JSON files on the user's machine, and the MCP server only reads/writes within project directories. There is no cloud, database, or telemetry.

In scope:

- Anything that exfiltrates local data (`.todos.json`, `.epics.json`) outside the user's machine
- Path traversal / arbitrary file read/write via the MCP tools
- Code execution through the desktop app (Tauri command injection, etc.)
- The pre-commit guard being bypassable in a way that leaks personal info

Out of scope:

- Vulnerabilities in dependencies not reachable from QuestLog's code paths — report those upstream
- Social engineering or phishing
- Issues requiring physical access to an unlocked machine

## Built-in protections

QuestLog ships with a pre-commit guard (`scripts/pre-commit-guard.sh`) that blocks commits containing personal paths, IPs, hostnames, credentials, or data files. It is installed by `setup-mcp.sh`. See [CONTRIBUTING.md](CONTRIBUTING.md) for details.

Thank you for helping keep QuestLog and its users safe.
