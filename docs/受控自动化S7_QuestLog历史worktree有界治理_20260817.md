# 受控自动化 S7：QuestLog 历史 worktree 有界治理

> epic_55f92675 · S7（step_a7e4a661）· 2026-08-17
> 目标（acceptance ⑫）：历史 worktree 先 dry-run 分类；仅回收可证明 terminal、无未决 DR/重试/会话且超过 TTL 的 QuestLog 自有目录；unknown 保留；实际存量批量删除需人工确认。

## 一、现状盘点（2026-08-17 实测）

| 项 | 实测 |
|---|---|
| `/tmp/questlog-worktrees/` | 仅 1 个空目录 `questlog/`（无 worktree 实体） |
| git 分支 `questlog/*` | **21 个**（create_worktree 的 branch_name 残留） |
| `/Users/abo/questlog-controlled-execution` | 1 个 codex 开发 worktree（git worktree list 注册项，531cd8e）——**非 Runner 自有，unknown 保留** |
| epic 登记的「约 130 个」 | 与本机现状不符——历史残留应已被清理；本工具以实测为准做有界治理 |

## 二、设计（已吸收 adversarial-auditor 2026-08-17 审查：4 BLOCK + 3 WARN，全部处置）

### 审查修订
- **B1 采纳（未合并 candidate 保护）**：分类新增 `keep_unmerged_candidate`——关联 execution 的 `candidate_sha` 存在且**非 HEAD 祖先**（`git merge-base --is-ancestor <sha> HEAD` 不通过）→ 保留（`EffectiveBase::PredecessorCandidate` 血缘链不断）。
- **B2 采纳（apply 三重防护）**：① apply 前读 runner-config，`global_enabled=true` → **拒绝执行**（提示先关总闸急停）；② 持 `executions.json` 文件锁（10s 超时对齐 Rust）**锁内重读重分类**后执行；③ 目录删除用 `git worktree remove --force`（git 元数据一致），非裸 rm。
- **B3 采纳（项目显式化）**：工具必收 `project_dir`（沿用 resolveCwd 校验模式），全部 git 操作 cwd=该项目的 repo root；不跨项目批处理。
- **B4 采纳（双层路径）**：worktree 叶子 = `/tmp/questlog-worktrees/questlog/exec_<uuid>/`（branch_name 已含 `questlog/` 前缀形成双层）；遍历 glob `/tmp/questlog-worktrees/questlog/exec_*/`。
- **W1 采纳（TTL 组合子）**：`lastActivity = max(目录 mtime, 分支 tip commit date)`，**两者都旧**才可回收。
- **W2/W3 采纳**：apply 幂等验收（二次调用零增量、分支消失）；锁超时 10s。
- 审查自证伪项（node_modules symlink 悬挂）确认为 PASS 无需处理。

### 数据源与命名空间
- **目录**：`/tmp/questlog-worktrees/**`（QuestLog Runner 自有命名空间）
- **分支**：`questlog/*` 前缀（create_worktree 配套创建）
- 两者之外（如 questlog-controlled-execution、用户自建 worktree）一律 **unknown → keep**

### 分类规则（纯函数，可单测）
- `reclaimable`：QuestLog 自有 **且** 无活跃引用（executions.json 中无 queued/running/candidate/verifying 项的 worktree_path/branch 指向它）**且** 无证据保留现场（无 scene_preserved=false 的 execution 关联）**且** 超 TTL（目录 mtime，默认 7 天；分支按其 tip commit date）
- `keep_active`：被活跃 execution 引用
- `keep_evidence`：scene_preserved=false 的保留现场
- `keep_fresh`：TTL 内
- `unknown`：命名空间外/判定失败（fail-open 保留）

### 行为
- **默认 dry-run**：输出分类清单（每项：目标、判定、原因、引用 execution id），不改任何状态
- `apply: true` 才执行：目录 remove + `git worktree prune` + `git branch -D`（仅 reclaimable）——**实际批量删除仍需人工确认**（工具描述明示：apply 前先 dry-run 审阅）
- 失败单项：记入结果继续其余（不中止），输出逐项结果

### 实现位置
- `mcp-server/src/worktree-sweep.ts`：分类纯函数 + sweep 实现
- `mcp-server/src/index.ts`：注册 `sweep_worktrees` 工具
- 测试 `mcp-server/test/worktree-sweep.test.ts`：分类规则五态 + TTL 边界 + 活跃引用排除 + apply 幂等

## 二·五、本机实测 dry-run 结果（2026-08-17，工具实跑，零副作用）

```
mode: dry-run | ttl: 7d
targets: 133
  keep_fresh:              122  （7 天内有活动——含今日测试运行刷新的 mtime）
  keep_unmerged_candidate:  11  （含未合并 candidate_sha——B1 防线保住血缘链）
  reclaimable:               0
  keep_active/keep_evidence: 0
  unknown:                   0  （codex 开发 worktree 不在 questlog/exec_* 命名，未入 targets ✓）
```

**关键发现**：①「约 130 个」历史证据确认（此前 `ls` 只见一层——叶子在 `<base>/questlog/exec_*/` 双层之下，审查 B4 命中）；② **11 个未合并 candidate 分支若无 B1 防线将被 apply 删除**（断 PredecessorCandidate 链）——对抗审查的最大价值实证；③ 当前无可回收项（全部在 TTL 内或受未合并保护）——**本步骤交付 dry-run 工具与分类防线，实际回收待 TTL 过期后由人工审阅 apply**（acceptance ⑫ 约定：批量删除需人工确认）。

## 三、链路影响图

```
worktree-sweep.ts（新） → index.ts 注册（无既有消费者）；读 executions.json（只读）+ git（prune/branch -D 仅 apply）
```

## 四、验收

- dry-run 零副作用（测试断言无删除调用）
- 分类五态全覆盖单测；unknown fail-open
- 本机实测 dry-run 清单落盘（方案附录/会话记录）
- 实际删除留人工（不自动 apply）

## 五、不做

- Rust Runner 内的清理逻辑改动（poll 已有终态清理 + S1 保留闸）
- codex/用户自建 worktree 的回收（unknown）
- 「130 个」历史数字的追溯（以实测为准）
