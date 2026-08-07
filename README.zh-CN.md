<div align="center">

<img src="docs/logo.png" alt="QuestLog" width="96" />

# QuestLog

**你的 MCP 编码会话，终于有了持久化看板。**

**兼容任何 MCP 客户端**——Claude Code、Codex、OpenCode、Cursor 等。捕获决策、待办、方案，让它们跨会话、跨项目、跨时间留存下来。

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Platform](https://img.shields.io/badge/platform-macOS-lightgrey.svg)
![Local-first](https://img.shields.io/badge/数据-100%25本地-success.svg)

</div>

---

## 为什么需要

编码会话是临时的。昨天做的决策、发现的 bug、规划的方案——会话一关就没了。QuestLog 让它们**持久化**，常驻菜单栏，一眼可见。

```
「帮我记一下」  →  AI 写入 .todos.json  →  菜单栏刷新
                        ↑                          │
                        └─── 下次会话读回 ─────────┘
```

## 核心功能

| | |
|---|---|
| **菜单栏环境式看板** | 托盘图标带实时计数。点击 → 所有项目活跃条目时间线。三秒看清全局。 |
| **三列看板** | 经验（决策）· 待办（工作）· 想法（思路）。标签筛选、已完成归档、撤销删除。 |
| **方案追踪** | 跨周方案 + 5 阶段步骤 + 依赖链 + 自动派生进度条。一个对话一个方案——拆解，不要堆砌。 |
| **自动进度捕捉** | Stop hook 在会话结束时扫描，若 Claude 说了「完成」却没登记则阻止结束。永不丢进度。 |
| **看板 ↔ 对话** | 点任意条目的 📋 → 粘贴到任意会话 → Claude 找到它（可跨项目）并接续。 |
| **100% 本地** | 纯 JSON 文件，**无数据库、无云端、无遥测**。 |

## 安装

QuestLog 目前**仅支持从源码构建**。我们不提供预编译 `.dmg`，因为未签名的
macOS 应用在 Sequoia 及以上系统会触发"已损坏，请移到废纸篓"的提示，体验
比一次性构建更差。

**前置条件：** [Node.js](https://nodejs.org) 20.19+（Vite 8 要求 ≥20.19）和 [Rust](https://rustup.rs)。

```bash
git clone https://github.com/gifuda/questlog.git
cd questlog
cd mcp-server && npm install && npm run build && cd ..
cd desktop-app && npm install && npx tauri build && cd ..
bash setup-mcp.sh   # 自动检测已装的 MCP 客户端，注册服务器，软链 pre-commit 防泄露守卫
```

构建产物在 `desktop-app/src-tauri/target/release/bundle/`。打开任意 MCP 兼容
的编码会话（Claude Code、Codex、OpenCode、Cursor、ZCode…），说 **「帮我记一下」** ——搞定。

`setup-mcp.sh` 会自动检测已装的客户端，把服务器写进各自的用户级配置。要指定子集，用 flag 覆盖：

```bash
bash setup-mcp.sh --client claude,cursor   # 只注册这两个
bash setup-mcp.sh --all                    # 强制尝试全部 5 个，忽略检测结果
bash setup-mcp.sh --help                   # 完整用法
```

想手动配置？各客户端的配置文件及 `todo-board` 条目所在位置：

| 客户端 | 配置文件 | Key 路径 |
|---|---|---|
| Claude Code | `~/.claude.json`（由 `claude mcp add` 管理） | `mcpServers.todo-board` |
| Cursor | `~/.cursor/mcp.json` | `mcpServers.todo-board` |
| ZCode | `~/.zcode/cli/config.json` | `mcp.servers.todo-board` |
| Codex | `~/.codex/config.toml` | `[mcp_servers.todo-board]` |
| OpenCode | `~/.config/opencode/config.json` | `mcp.todo-board` |

所有条目形态相同：`{"command": "<绝对路径>/node", "args": ["<绝对路径>/mcp-server/dist/index.js"]}`。**让脚本生成绝对路径，别手写**——含空格或中文的路径手写会断。

## 用法

### 捕获

| 你说 | Claude 做 |
|------|----------|
| 「帮我记一下」 | `push_todo` —— 自动判类型和项目 |
| 「这是个 idea」 | 显式指定类型 |
| 「创建方案：P0 重构认证」 | `push_epic` —— 启动追踪方案 |

三种类型：**📘 经验**（已验证的决策）· **✅ 待办**（可执行的工作）· **💡 想法**（值得探索的思路）。

### 接续

| 你想要 | 怎么做 |
|--------|--------|
| 处理某个已知待办 | 说「处理 X 那个待办」—— Claude 会话开始时已加载看板 |
| 跨项目查找 | 说「找一下 X」—— `search_todo` 扫描所有项目 |
| 接续方案 | 点方案卡的 📋 → 粘贴引用（`继续方案: X / Continue plan: X` + 进度）→ Claude 调 `list_epics` 继续 |

> 复制的引用跟随界面语言：`处理待办: X` / `Handle todo: X`、`继续方案: X` / `Continue plan: X`。MCP server 两种前缀都能识别。

### 追踪方案

```
P0  重构认证                                    56%  🔵
├── S1  审计现有流程                ████████░░    50%  ◑ 已实施        ← 可做
├── S2  设计新 schema               ███░░░░░░░    25%  ◐ 已设计
└── S3  迁移用户 (依赖 S2)          ░░░░░░░░░░     0%  ☐ 未启动        🔗
```

- **一个对话一个顶级方案** —— 拆成子方案或步骤，不要平行堆砌 epic
- **进度是派生的** —— 从不手写；子方案 = 步骤均值，顶级方案 = 子方案均值
- **依赖自动阻塞** —— `depends_on` 未满足 → 🔴 blocked；依赖完成 → 自动释放
- **可做步骤高亮** —— 所有前置已观察 = 现在就能做（多个 = 可并行）

## MCP 工具

| 工具 | 用途 |
|------|------|
| `push_todo` / `list_todos` / `search_todo` / `update_todo` | 条目增删改查 + 跨项目搜索 |
| `push_epic` / `update_epic` | 方案生命周期（draft → active → ... → done） |
| `push_step` / `update_step` | 步骤增删 + 5 阶段进度（pending → designed → coded → verified → observed） |
| `list_epics` | 列出方案+进度+可做标记——用于接续 |

## Stop hook（自动进度捕捉）

Claude 有时在你说了「S2 完成了」之后忘了调 `update_step`。Stop hook 兜底：

1. 扫描会话中提到完成的步骤
2. 检查是否调过 `update_step`
3. 没有 → **阻止会话结束**，直到 Claude 补登

加入 `~/.claude/settings.json`：

```json
{
  "hooks": {
    "Stop": [{
      "matcher": "",
      "hooks": [{
        "type": "command",
        "command": "node /绝对路径/questlog/mcp-server/hooks/capture-progress.mjs",
        "timeout": 15
      }]
    }]
  }
}
```

## 架构

```
任意 MCP 客户端 ──MCP──▶ Node.js 服务器 ──▶ .todos.json + .epics.json
(Claude Code,                │                      │
 Codex, Cursor,          Stop hook               fs.watch
 OpenCode, ZCode…)           │                      ▼
                             ▼               Tauri (Rust + React)
                      扫描对话                  ├── 菜单栏弹窗
                      漏登则阻止                ├── 看板
                                                └── 方案看板
```

**技术栈：** Tauri v2 · React · Tailwind v4 · TypeScript · notify crate

## 隐私

- 数据全在本地 JSON 文件 —— 无数据库、无云端、无遥测
- MCP 服务器只读写你项目目录内的文件
- Pre-commit 守卫拦截含个人路径、内网 IP、项目名、凭证的提交

## 故障排查

| 问题 | 修复 |
|------|------|
| 菜单栏图标不显示 | 重启应用；活动监视器搜「QuestLog」 |
| MCP 工具不可用 | 重跑 `bash setup-mcp.sh`——汇总表显示各客户端状态。按上面表格里的配置文件路径核对条目是否写入。 |
| 条目不显示 | 检查 `~/.todo-board.json` 是否含该项目；重开应用 |
| Stop hook 不触发 | 确认绝对路径 + `node` 在 PATH |
| Pre-commit 守卫误报 | 文档示例用 `git commit --no-verify` |

## 贡献

Fork → 分支 → PR。Pre-commit 守卫自动运行。需要帮忙：Windows/Linux、拖拽排序、导出/备份、i18n。

## 许可证

[MIT](LICENSE)

---

[English](README.md) · [报告 Bug](../../issues) · [提需求](../../issues)
