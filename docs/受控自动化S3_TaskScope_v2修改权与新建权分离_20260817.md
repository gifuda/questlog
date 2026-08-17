# 受控自动化 S3：TaskScope v2 修改权与新建权分离

> epic_55f92675 · S3（step_10e305aa）· 2026-08-17
> 依赖 S2（已 verified）。目标：以 base tree 为事实源分离修改权与新建权；路径/权限为硬门禁，文件数量改 expected/warn/max 三级语义；Runner 先报具体路径违规再判极端数量。

## 一、现状缺口（已核实）

| 编号 | 位置 | 现状 |
|---|---|---|
| S3-1 | `runner_git.rs:568-573` verify_changed_paths_in_scope | **数量检查在路径检查之前**——同时存在越界路径与超量时只报 TooManyFiles，操作者看不到具体违规路径（违反 acceptance ⑥「先报告具体路径违规，再判断极端数量」） |
| S3-2 | `runner_git.rs` classify_path_in_scope | 只有 in/out/forbidden 三类——**不区分「改已有文件」与「建新文件」**：建新文件只要匹配 allowed 即可，无法表达「可改 src/** 但只能新建 docs/*.md」 |
| S3-3 | `execution_store.rs:1050-1052` + `task-package.ts:27-34` | scope 只有 allowed/forbidden/max_changed_files 三字段，无 create 权与数量分级 |
| S3-4 | `runner.rs` scope 校验段 | max_changed_files 是唯一数量闸门——正常施工超配额即熔断（epic 实跑案例：真实必要 5 文件 > max=4 熔断丢现场），max 应只做极端事故最后熔断器 |

## 二、修复设计（已吸收 adversarial-auditor 2026-08-17 审查：2 BLOCK + 6 WARN）

### 审查修订
- **F1 采纳（数量熔断与路径越界的 outcome 分流）**：TooManyFiles 不再与路径越界共用 `scope_violation`——新增 outcome `scope_count_violation`（seal_bundle 映射处与 poll 循环 finish 处按 ScopeViolation 变体分流）。`find_recoverable_scene` 只认 `scope_violation`（路径违规）：**数量熔断不做恢复源**——恢复一个本身超量的现场必然再熔断（max 未变时无解循环）；预算语义上两者同样不计数（不在计数集合即不消耗）。
- **F2 采纳（仓库级 create 模式拦截）**：MCP validate 拦截规则改为「create_paths 中模式为 `"**"` 或以 `"**/"` 开头 → error」——仓库级新建权拒绝；`src/**`、`docs/**` 等子树模式合法（真需宽新建权应显式列子树）。
- **F3 采纳（性能）**：base tree 存在性判定用 `git ls-tree -r --name-only <base_sha>` 一次拉全量入 HashSet，changed 逐条 O(1) 查询——替代逐 path 的 `git cat-file -e`。
- **F5 采纳（时序固化）**：`manifest.scope_size_warning` 与 scene_ref 同批、在 integrity hash 计算前写入。
- **F6 已明确**：expected/warn 为 None 时该级永不触发；仅 max 存在时行为与今日完全等价。
- **F4/F7/F8 采纳**：v1 回退行为=今日行为（方案 §二.4 已述，测试加 v1 fixture 回归）；测试清单补 write_scene_snapshot 分类调用签名变更回归；验收条目与测试映射在 §四 标注。

### 1. TaskScope v2 数据模型（两端同 commit）

```
scope: {
  allowed_paths: ["src/**"],        // 修改权（v1 兼容：缺 create_paths 时兼授新建权）
  create_paths?: ["docs/*.md"],     // 新建权（v2 新增，可选）
  forbidden_paths: [...],
  expected_changed_files?: 6,       // 预期值：超过仅记录
  warn_changed_files?: 10,          // 软告警：超过写 bundle 警告但继续 candidate/验收
  max_changed_files: 40             // 极端熔断（保留 v1 字段名；防全仓格式化/生成器失控）
}
```
- Rust：TaskPackageSummary + RawTaskPackage 加 `create_paths: Vec<String>`（serde default 空）+ `expected/warn_changed_files: Option<u32>`（default None）+ 投影转换
- MCP：TaskScope 接口加三个可选字段；validateTaskPackage 新规则——`create_paths` 含 `"**"` → **error**（禁止自动仓库级新建权）；expected/warn 提供时须满足 `expected ≤ warn ≤ max` → 违反 error

### 2. 判定重构（runner_git.rs）

- 新增 `path_exists_in_base(worktree, base_sha, path) -> bool`（`git cat-file -e <base_sha>:<path>`）——**base tree 是 modify/create 分类的事实源**
- `classify_path_in_scope(path, exists_in_base, rules)` 新分类：
  - `exists_in_base == true`：需匹配 **allowed_paths**（modify 集）——**create_paths 匹配已有文件不算数**（create 不能修改同目录已有文件）；不匹配 → `UnauthorizedModify`
  - `exists_in_base == false`（新文件）：匹配 create_paths **或**（v1 回退）allowed_paths → 过；否则 `UnauthorizedCreate`
  - forbidden / 控制平面检查在前（不变）
- `verify_changed_paths_in_scope` **顺序反转**：先逐路径（具体违规优先报告），全过后才查 `total > max`（熔断）
- rename 双边校验：`get_changed_paths` 的 `git diff --name-only`（不带 -M）把 rename 呈现为「旧路径删除 + 新路径新增」两行——旧路径过 modify 门、新路径过 create 门，天然双边；以代码注释固化该语义
- ScopeViolation 枚举新增 `UnauthorizedModify`/`UnauthorizedCreate` 变体（错误信息分别说明需要 modify/create 权）；`OutOfScope` 保留给 v1 语义兼容路径

### 3. 三级数量语义（runner.rs）

- `scope_size_verdict(total, expected, warn, max) -> SizeVerdict { Ok, OverExpected, OverWarn, OverMax }` 纯函数：
  - `OverMax` → ScopeViolation::TooManyFiles 熔断（S1 scene 保全后）
  - `OverWarn` → **不阻断**：`manifest.scope_size_warning = Some("N files changed exceeds warn threshold W")`（hash 前写入）+ candidate commit trailer `QuestLog-ScopeWarning`——终审与 DR 可见
  - `OverExpected` → 仅 changed-paths.json 记录
- write_scene_snapshot 的 changed-paths.json 增加 `expected/warn/scope_size_warning` 字段（bundle 内可查三级对比）

### 4. 向后兼容

- v1 sealed 包：create_paths 空 → 新建检查回退 allowed_paths（行为与今日一致）；expected/warn None → 只剩 max 闸（行为与今日一致）；package_hash 不变（旧包不受影响），新包 hash 覆盖新字段
- 前端/MCP ExecutionRow 无 scope 消费，无需同步

## 三、链路影响图（grep 核实）

```
改动文件            → 消费方（同步范围）
execution_store.rs  → runner.rs（scope 投影消费）；attempt_bundle mock_pkg 测试
runner_git.rs       → runner.rs verify 调用（L1101，签名变）；classify 消费者（S1 scene 分类）
                      —— write_scene_snapshot 的分类调用需同步传 exists_in_base
runner.rs           → seal_bundle DR 文案（UnauthorizedModify/Create 变体映射）
attempt_bundle.rs   → changed-paths.json 字段扩展 + manifest.scope_size_warning
mcp-server/task-package.ts → validate/prepare 消费；publish-plan-tool 描述（S4 再改编译器）
```

## 四、测试与验收

- **classify v2 单测**（runner_git）：已有文件不在 modify 集 → UnauthorizedModify；create 匹配已有文件不算数；新文件 v1 回退；新文件无 create 权 → UnauthorizedCreate；forbidden 优先
- **顺序反转单测**：越界路径 + 超量同时存在 → 报路径违规非 TooManyFiles
- **scope_size_verdict 单测**：三级四态边界（=expected/=warn/=max）
- **path_exists_in_base 单测**：base 中存在/不存在/untracked
- **MCP validate 单测**（task-package.test.ts 补）：create_paths 含 ** → error；expected > warn → error
- **回归**：cargo 全绿 + clippy + vitest + web build；v1 fixture 包（无新字段）行为不变
- 验收（acceptance ⑥⑦）：forbidden/modify/create 为硬门禁；三级数量语义；先路径后数量；create 不改已有文件；rename 双边；禁自动仓库级 **（MCP 校验）

## 五、不做（S3 边界）

- publish_plan 自动估算 expected/warn/max（编译器）→ S4
- 存量 v1 包的 scope 迁移/重封（读兼容即可）
- Runner 端对 create_paths 的 "**" 拒绝（闸门在 MCP prepare/validate——自动入口；人工手写 JSON 的极端情况由 forbidden/审阅兜底）
