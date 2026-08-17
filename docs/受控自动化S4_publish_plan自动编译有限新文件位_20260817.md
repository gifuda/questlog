# 受控自动化 S4：publish_plan 自动编译有限新文件位

> epic_55f92675 · S4（step_e11656c8）· 2026-08-17
> 依赖 S3（已 verified）。目标：对话只提交完备 PlanSpec；发布门自动生成有限新文件位，并结合实现、测试、类型同步和注册面估算 expected/warn/max——普通方案无需手填 max，批量重构才显式覆盖。

## 一、缺口（S3 之后显现）

| 编号 | 位置 | 现状 |
|---|---|---|
| S4-1 | `task-package-ops.ts:181` | `max_changed_files: input.max_changed_files ?? 10`——小额配额默认 10，S3 已把它定位为最后熔断器：**默认 10 会拦正常施工**（S3 后 max 才是唯一硬数量闸，默认值必须升位） |
| S4-2 | PlanSpec steps | 无 create_paths/expected/warn——S3 收紧新建权后，executor 正常补测试文件（allowed=["src/**"] 时新建 `tests/foo_test.rs`）会撞 UnauthorizedCreate——**门禁与正常流程脱节** |
| S4-3 | plan-publisher.ts | 无估算逻辑——tiers 全靠手填，而 epic 裁决「新包默认由编译器生成 expected/warn/max」 |

## 二、修复设计（已吸收 adversarial-auditor 2026-08-17 审查：5 BLOCK + 5 WARN，全部处置）

### 审查修订
- **F1 采纳（根级精确文件兜底）**：模式 dirname 为空（根级文件如 README.md）→ **跳过镜像位**（不生成 `./`、`**` 等仓库级形态）；根前缀第一段含通配符或为 `.` 的模式同样跳过。
- **F2 采纳（根前缀规则与防爆）**：根前缀 = 模式按 `/` 切分的首段（首段含 `*` 或为 `.` → 跳过）；根集合去重后**每根最多 1 测试位 + 1 文档位、总测试位 ≤ 4**；docs-only 步骤（全部根前缀为 docs）跳过测试位。
- **F3/F8 采纳（单调性钳制 + 第四级序）**：估算顺序固化为 max（显式??公式）→ expected=min(expected, max) → warn=min(warn, max) → 若 warn<expected 则 warn=expected——编译产物恒满足 expected ≤ warn ≤ max；`validatePackageContent` 补 **expected ≤ max** 检查（error，S3 缺席的第四级序）；summary 提示 max 默认下限 40 的熔断器语义。
- **F4 采纳（参数链四段全列）**：PlanStepSpec + stepSchema + pushStepWithPackage input + PrepareInput + prepare scope 组装，同 commit 打通。
- **F5 采纳（overlap 含创建位）**：`scopesMayOverlap` 的候选集改为 allowed ∪ create 的两两交叉——创建权共享同一子树的无依赖并行步骤也判 step_scope_conflict（预期收紧；编译镜像会让同子树步骤需显式 depends_on 串行化）。
- **F8 采纳（编译器触发边界）**：编译/估算只在 publishPlan 主路径执行；pushStepWithPackage 被单独调用时不编译（input 缺省走旧 default 10）。
- **F6 采纳（结构化 summary）**：published 结果的 structuredContent 暴露 `step_summaries[]`（step_id + tiers + create_paths + compile 说明）。
- **F7 采纳（测试边界）**：补显式 create 跳过共存、显式 max 钳制、根级文件跳过、多根去重、`*` 模式边界、docs-only、expected>max 拒绝七类用例。

### 1. 有限新文件位编译器（plan-publisher.ts）

`compileCreatePaths(step): string[]`——确定性规则，从 PlanSpec 可得面推导：
- **实现位镜像**：对每个 `allowed_paths` 模式——精确文件（无通配）→ 同目录子树 `<dirname>/**`；含通配 → 原样（可改的子树即可建的子树，不构成扩权）
- **测试位**（从 `acceptance_command_ids` 推断）：cargo 系（`cargo:*`/`test:desktop`）→ 各实现位根前缀下 `**/tests/**` 与 Rust 同目录单测位；TS 系（`test:mcp`/`test:web`/`build:mcp`/`lint:mcp`）→ 根前缀下 `**/*.test.ts`/`**/*.test.tsx`。根前缀取实现位模式的首段目录（无 `/` 的模式跳过）
- **文档位**：`<根前缀>/docs/**`（存在时）
- 全程过滤仓库级模式（裸 `**` 或 `**/` 前缀——S3 校验会拒）；显式 `create_paths` 透传时**跳过编译**（人工判断优先）

### 2. 三级数量估算器

`estimateScopeTiers(step): { expected, warn, max }`：
- `base = 具体路径数 × 1 + 通配模式数 × 4`（一个通配面典型触达实现+测试+类型+邻近 4 文件）
- 验收面：`min(acceptance_command_ids 数, 3)`（验收配套）
- 类型同步面：allowed 含源码模式（`.rs`/`.ts`/`.tsx`/`.py`）→ +2
- `expected = base + 验收 + 类型`；`warn = ceil(expected × 1.5) + 2`；`max = step.max_changed_files（显式覆盖，批量重构用） ?? max(warn × 2, 40)`（默认下限 40——最后熔断器定位，正常施工不触碰）
- 显式 `expected/warn_changed_files` 透传优先；**显式 max 与编译 warn 冲突时**（max < warn）以 max 为准并降 warn=max（validate 的单调序在 seal 侧兜底）

### 3. 接线与可见性

- `PlanStepSpec` + stepSchema 加 `create_paths?`/`expected_changed_files?`/`warn_changed_files?`（可选；描述注明「无需手填，发布门自动编译估算；批量重构才显式覆盖 max」）
- `pushStepWithPackage` → `prepare` 参数链打通（task-package-ops.ts input 类型 + scope 组装）
- published 结果 summary 附每步编译出的 tiers + create_paths 数（审计可解释、可复查）
- `max_changed_files ?? 10` 默认改为承接估算值（estimate 产物）

### 4. 向后兼容

- 旧调用（无新字段）：走编译器（行为=自动 tiers，比旧默认 10 更符合 S3 语义）；既有显式 max 调用不变（覆盖生效）
- Runner/scene/S2 链路零改动（S3 已消费这些字段）

## 三、链路影响图

```
改动文件                    → 消费方
plan-publisher.ts           → publish-plan-tool.ts（PlanSpec 类型源）
task-package-ops.ts         → prepare 的 scope 组装（Rust 端反序列化已支持 S3 字段）
publish-plan-tool.ts        → MCP schema（可选字段，zod optional 不破坏旧输入）
```

## 四、测试与验收

- **编译器单测**：精确文件→同目录位；通配镜像；cargo/ts 验收位推断；仓库级过滤；显式 create_paths 跳过
- **估算器单测**：tiers 公式边界（纯精确/纯通配/混合/显式覆盖/显式 max< 编译 warn 的钳制）
- **publish 集成**（plan-publisher.test.ts 补）：发布后包的 scope 含编译 create_paths 与 tiers；summary 含估算说明
- **回归**：vitest 全绿 + cargo（Runner 端读新包字段路径已由 S3 覆盖）
- 验收（acceptance ⑧）：实现/测试/Schema类型/注册/fixture/新文件位六面估算；普通方案无需手填 max；批量重构显式覆盖

## 五、不做（S4 边界）

- 「注册面」的深度推断（nodes/allowlist 三处同步无法从 PlanSpec 确定性推导——计入 expected 保守缓冲，注释说明）
- 发布时读仓库实际文件数辅助估算（保持纯函数、可单测；不引入 IO）
