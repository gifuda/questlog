# WP-GX1 · QuestLog 租户入租（待办租户）

> 提交：经理机会话 2026-09-17 · 状态：待总控审查适配 → executor 施工
> 契约：见本目录《租户入租契约-QuestLog.md》
> 方案源：经理机仓库 `docs/待办租户_采集机凌晨自动处理方案_审阅修订版_20260916.md`（§16.8 消息映射/入租流程/zcode 现状）

## 目标

QuestLog 桌面清单贴（经理机权威待办）作为平台第 5 个租户 `questlog` 接入：夜间窗口（03:00–06:00 +08:00）把经理机 outbox 中 opt-in 的只读研究待办提交平台队列执行，产出报告并回执经理机对账。经理机侧管线（G1 数据契约与安全写回 / G2 标准包生产 / G3 只读策略与报告验证 / G4 调度与回执状态机）已实现、测试并提交（Rust 224 + TS 260 全绿）。

## 本包含什么（全部为本目录新文件，零平台既有文件修改）

| 文件 | 落位（executor 施工） | 说明 |
|---|---|---|
| `questlog.py` | `collector_node/tenants/questlog.py` | 纯函数适配器：投递消息校验 + TaskSpec 构造 |
| `questlog_dispatch.py` | `scripts/questlog_dispatch.py` | 派发器（allowlist 名 `questlog-dispatch`）：拉 outbox → 校验 → 快照 → submit（幂等键=task_key）→ 终态回执 → 归档 |
| `questlog_research.py` | `scripts/questlog_research.py` | 执行器（allowlist 名 `questlog-research`）：只读信封 → zcode headless / shadow 桩 → 报告落盘 |
| `task_spec_questlog_research.json` | `examples/` | TaskSpec 草稿 |
| `test_questlog_adapter.py` | `tests/` | 适配器+桩模式测试（manager 侧已裸跑 10/10 通过） |

## executor 施工步骤（建议）

1. `tenants/questlog.py` + `scripts/questlog_{dispatch,research}.py` + `examples/` + `tests/` 按上表落位（脚本须随发布走，白名单条目按 current/ 稳定软链形态登记——WP-FIXDR 规则）。
2. `script_allowlist.json` 加两条：`questlog-dispatch` / `questlog-research`（tenant 约束在 TaskSpec 层）。
3. `NodeManifest.tenants` 登记 `"questlog"`。
4. scheduler 注册 `questlog-dispatch` schedule：`interval_seconds=900`（派发自查 19:00–21:00 UTC 窗口；回执半程恒跑——脚本内已实现）。
5. `pytest` 全绿 → `tools/release.py create` → verify → current 切换 → 守护 kickstart（标准发布流程）。
6. 登记数据集备份：`CollectorData/tenants/questlog/` 进 backup-catalog。

## 决策清单（请总控裁定）

- **D1 zcode 执行方式（建议 A 先行）**：
  - A. shadow 桩先行：`QUESTLOG_RESEARCH_STUB=1`（经 TaskSpec 参数或环境注入）打通「outbox→TaskSpec→执行→报告→回执→经理机对账」全链，产出固定 NEEDS_REVIEW 报告；
  - B. zcode headless 直上：需先解决手册 F-2（本机 PATH 无 zcode 二进制；ZCode.app 内 `zcode.cjs` 可经 wrapper + QUESTLOG_ZCODE_BIN 指定）+ flag 白名单按真实 help 核验扩列。建议作为独立后续包。
- **D2 调度形态**：平台 scheduler interval 900s（本包方案）vs 每日固定槽。派发窗口语义要求窗口内多次尝试（SSH 失败重试），建议 interval。
- **D3 环境注入**：QUESTLOG_RESEARCH_STUB / QUESTLOG_ZCODE_BIN 的注入面（TaskSpec parameters vs 脚本侧配置文件 `tenants/questlog.env`）——请按平台惯例裁定。
- **D4 跨机 SSH 面**：契约第三节申请的薄壳（manager 只读 + 两处写）是否需在台账单独登记。

## 验收标准（shadow 期）

1. 经理机投一条 fixture 投递消息（合法 task_key）→ 派发器拉取 → 平台任务 succeeded → 报告落 `tenants/questlog/reports/` → 回执写回经理机 inbox → 经理机 G4 对账把对应待办置 needs_review（stub 报告的正确分类）→ outbox 条目归档。
2. 同 task_key 重投递被幂等吸收（submit_replayed 事件，无二次执行）。
3. 非法投递消息（坏 task_key/超界 timeout）被跳过并留 stderr 记录，不产生任务。
4. 其他租户（ymos/taopuli/life-os/commerce）零感知：无目录交叉、无队列挤占（max_workers 共享席位内）。
5. 全程无 shell 注入面：task_key 已验证 64-hex，objective 只经 scp 文件传输。

## 回滚

撤销 schedule 注册 + allowlist 两条目 + NodeManifest 条目（走发布流程）；`tenants/questlog/` 数据目录保留（含报告与账本证据，只增不删）。经理机侧：`runner-config.json` 删 `todo_schedule` 即停扫描（已实现）。

## UNKNOWN（诚实声明）

- `questlog_dispatch.py` 对平台 API（`db.connect` / `queue.submit` / 任务表查询列名）按 2026-09-17 只读核对编写；executor 施工时以 current release 实际签名为准微调。
- zcode headless 的真实 flag 集（`--prompt <file>` / `--cwd` / `--mode`）未在采集机实测——D1-B 施工项。
- 回执 `timed_out` 判定 v1 用「failed 状态」近似；平台 attempts 表有精确 exit_code 后可细化（executor 施工时核对）。
