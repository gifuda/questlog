# 受控自动化 S2：ScopeViolation 批准后复用成果续跑

> epic_55f92675 · S2（step_4ef04fe0）· 2026-08-17
> 依赖 S1（已 verified）。目标：把越界从「丢成果后重做」改为「证据可查、人工确认准确路径、新 revision 复用现场续跑」。

## 一、缺口定位（S0/S1 之后剩余的部分）

| 环节 | 现状 | S2 补齐 |
|---|---|---|
| 证据可查 | S1 ✅ scene/ 完整封存（diff+untracked+commits+分类） | — |
| 人工确认准确路径 | S0 ✅ revise_scope 安全收口 + S1 changed-paths.json 分类 | — |
| 新 revision 复用现场续跑 | ❌ 新 execution 全新 worktree 从零跑，executor 重做全部改动（重花 executor 成本，且重做结果可能漂移） | **本步骤** |

S0 已定的预算语义保证新 revision 预算独立（execution 链按 package+链计，新 revision 新链从 0 起）✅。

## 二、修复设计（已吸收 adversarial-auditor 2026-08-17 审查：1 BLOCK + 7 WARN）

### 审查修订
- **B4 采纳（恢复源过滤收紧）**：`find_recoverable_scene` 条件追加 **`prev.base_sha == new.base_sha`**（base 漂移不恢复——patch 失配场景干净降级为全新执行，不浪费恢复尝试）与 **`prev.scene_preserved == Some(true)`**（不完整现场不做恢复源）。选择策略：满足全部条件中取 `updated_at` 最新——安全性由 sealed scope + 第 6 步 scope 校验闭环保证（人批准的新 scope 是唯一放行标准），恢复源只影响工作量不影响安全；不强制 plan_revision 连续（人可能连跳 revision）。
- **W2 部分采纳**：`git_clean` 快照在 fresh_worktree 前采集且用主仓路径系存量缺陷，S2 顺带修正成本高——以 bundle 事件 `scene_recovered`（含 patched/untracked_restored）记录恢复事实替代，存量缺陷记入不做区。
- **W3 采纳**：executor 实际收到的 task string 落盘 bundle `task-prompt.txt`（`manifest.task_prompt_ref`）——审计可见「executor 被告知了什么」，与 sealed objective 区分。
- **W8 采纳（简化版）**：retry/resume child 在 process_execution 开头**继承** parent 的 `scene_recovered_from`（沿 parent_attempt_id 链可回溯完整恢复链）；不建独立 recovery_chain 数组。DR question 在 scene_recovered_from 非空时附「此 attempt 的候选包含来自前次 ScopeViolation 的恢复内容」。
- **W5 采纳**：recover 函数入口断言 scene 目录与 diff-full.patch/changed-paths.json 至少其一存在。
- **W7 采纳**：测试补 base 漂移过滤、scene_preserved 过滤、多次 violation 选最新三类；CLI 零改动边界与 e2e 闭环标注 non-blocking（依赖真实 executor）。
- **W6 采纳**：插入点固化为 fresh_worktree 返回**之后**（worktree_path 已在手才 apply——杜绝错位应用到主仓）。

### 1. 恢复器（attempt_bundle.rs 新增）

```rust
pub struct SceneRecovery { pub patched: bool, pub untracked_restored: usize }
pub fn recover_scene_into_worktree(worktree: &Path, bundle_dir: &Path) -> Result<SceneRecovery, String>
```
- `scene/diff-full.patch` 存在 → `git apply --whitespace=nowarn <abs>`（cwd=worktree）；apply 失败 → Err（base 漂移/冲突）
- `scene/untracked/**` 递归拷回 worktree 同相对路径
- 目录不存在 → Err（调用方判定完整性后才调）

### 2. 可恢复现场检测（execution_store.rs 新增纯函数）

```rust
pub fn find_recoverable_scene(execs: &[Execution], package_id: &str, exclude_id: &str) -> Option<String>
```
条件：同 package、非自身、`outcome == Some("scope_violation")`、终态（Blocked/Cancelled——revise_scope 收口后是 Cancelled，未收口是 Blocked，都允许恢复）、取 `updated_at` 最新。返回前序 execution_id。
（scene 完整性由调用方在恢复前检查 scene 目录存在。）

### 3. Runner 接线（runner.rs process_execution）

插入点：worktree 决定之后、executor 启动之前；条件 `exec.parent_attempt_id.is_none()`（首跑=新 revision 新链；retry/resume 链不重复恢复）：

1. `find_recoverable_scene` 命中且其 bundle `scene/`（diff-full.patch 或 changed-paths.json）存在 → `recover_scene_into_worktree`
2. 成功 → `manifest.scene_recovered_from = Some(prev_id)` + bundle 事件 `scene_recovered` + executor task 附加说明（前次越界改动已就位，在其基础上验证/完善而非重做——objective 本身不动，sealed 契约不变）
3. 失败 → `log::warn` + bundle 事件 `scene_recovery_failed`，**降级为全新执行**（不 block——与今日行为一致）
4. 恢复的改动随后走既有 scope 校验（第 6 步）：若新 sealed scope 仍未涵盖全部路径 → 再次 ScopeViolation（预期闭环，人再修 scope）；涵盖 → 正常 candidate/验收

### 4. 可追溯性

`AttemptManifest.scene_recovered_from: Option<String>`（serde default，读兼容旧 bundle；在 hash 计算前写入）。

### 5. 文案（两端同 commit）

- ScopeViolation DR question 追加：「 approving a revised package automatically restores this scene on its first attempt.」
- revise_scope 收口 detail（commands.rs + decision-actions.ts）同步补一句「新 revision 首跑将自动恢复现场」。

## 三、链路影响图（grep 核实）

```
改动文件                → 消费方
attempt_bundle.rs       → runner.rs（recover 调用）；manifest 反序列化（list_bundles Trace Query——serde default 读兼容）
execution_store.rs      → runner.rs（find_recoverable_scene）；无外部消费者（新增纯函数）
runner.rs               → executor task 字符串（executors/mod.rs trait 参数——String 不变形）
AttemptManifest         → attempt_bundle 自身测试
```

## 四、测试与验收

- **恢复器单测**：fixture repo A（base+改动+untracked）→ scene 封存（复用 S1 测试手法）→ fixture repo B（同 base 全新 clone/worktree）→ recover → 断言 tracked 内容与 untracked 文件逐字节一致
- **apply 冲突降级**：repo B 的 base 文件先改 → recover → Err（断言降级路径事件）——单测 Err 返回即可
- **检测纯函数单测**：造 executions 数组（scope_violation/cancelled/timeout 混合）→ 断言选中最新 scope_violation、排除自身、非同 package 不选
- **回归**：cargo 全绿 + clippy -D warnings + vitest + web build
- 验收（acceptance ⑤）：ScopeViolation 不创建 candidate（现状 ✅）不推进 Step（现状 ✅）；人批准准确路径（revise_scope + 新 revision）后首跑自动恢复成果续跑 ✅

## 五、不做（S2 边界）

- 运行时「批准路径」直接续跑（不改 sealed scope——scope 是 package hash 的一部分，运行时批准=篡改契约；必须走新 revision）
- timeout/cancelled 场景的 scene 恢复（timeout 有自己的 resume 链；cancelled 是人为放弃）
- TaskScope v2 权限分离（S3）/ publish_plan 自动估算（S4）
