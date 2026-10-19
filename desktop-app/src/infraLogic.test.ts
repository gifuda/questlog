/**
 * InfraBoard 纯逻辑测试 — 夹具以 2026-10-19 实读的 snapshot-local.json
 * 形状为准（queue 仅 dead_letter、providers 四家、health 9 项 + 4 个
 * _detail、recent 含 secs:null）。只测纯函数，不做组件渲染测试
 * （项目无 @testing-library，不引新依赖）。
 */
import { describe, expect, it } from "vitest";
import {
  DSH_DAILY_GATE,
  STALE_MS,
  aggUsage14d,
  dshPct,
  fmtSecs,
  healthItems,
  nasTone,
  parseSnapshot,
  providerList,
  queueEntries,
  quotaWarn,
  stalenessState,
  toneOf,
} from "./infraLogic";

/** 真实 snapshot-local.json 形状的正常夹具（关键字段原样，其余从简）。 */
function realShapedFixture() {
  return {
    snapshot: {
      base: {
        queue: { dead_letter: 15 }, // 实况仅此一键（契约自注「键动态」）
        recent: [
          { id: 38, secs: null, status: "dead_letter", harness: "light" },
          { id: 37, secs: null, status: "dead_letter", harness: "selftest-nosuch-[selftest 14536]" },
          { id: 36, secs: null, status: "dead_letter", harness: "dsh" },
        ],
        dsh_today: 2,
        usage_14d: [
          { ok: 3, day: "2026-10-19", fail: 0, secs: 265, in_tok: 47852, harness: "codex" },
          { ok: 15, day: "2026-10-19", fail: 3, secs: 476, in_tok: 0, harness: "zcode" },
        ],
        usage_today: [
          { tasks: 3, harness: "codex", input_tokens: 47852 },
          { tasks: 1, harness: "?", input_tokens: 0 },
        ],
        health: {
          checked_at: "2026-10-19 16:52:21",
          status: "ok",
          pg: "ok",
          worker: "ok",
          queue: "ok",
          queue_detail: "0 待处理 / 15 死信",
          codex_auth: "ok",
          proxy: "ok",
          proxy_detail: "127.0.0.1:7892 alive",
          zcode_env: "ok",
          zcode_live: "ok",
          zcode_live_detail: "端到端实活通过",
          dsh_budget: "ok",
          dsh_budget_detail: "2/50 次",
          egress_ip: "ok",
          egress_ip_detail: "154.40.137.230",
        },
      },
      nas_frame_age_secs: 113,
      generated_at: "2026-10-19T14:06:55Z",
    },
    llm_quota: {
      updated_at: 1792418849,
      updated_local: "22:07",
      // 契约 v1 未记载的额外键（qingtie/lifeos/ztasks）——视图须忽略不展示
      qingtie: { valid: true },
      lifeos: { valid: true },
      ztasks: { valid: true },
      providers: [
        {
          id: "codex",
          name: "Codex plus",
          state: "live",
          five_hour: { remaining_pct: 98, reset_at: 1792433915, reset_local: "周二 02:18" },
          weekly: { remaining_pct: 74, reset_at: 1792875502, reset_local: "周日 04:58" },
          error: null,
          as_of: 1792418848,
        },
        {
          id: "glm",
          name: "GLM",
          state: "live",
          five_hour: { remaining_pct: 91, reset_at: 1792433074, reset_local: "周二 02:04" },
          weekly: { remaining_pct: 84, reset_at: 1793002225, reset_local: "周一 16:10" },
          error: null,
          as_of: 1792418848,
        },
        {
          id: "kimi",
          name: "Kimi",
          state: "live",
          five_hour: { remaining_pct: 100, reset_at: 1792428365, reset_local: "周二 00:46" },
          weekly: { remaining_pct: 100, reset_at: 1792716365, reset_local: "周五 08:46" },
          error: null,
          as_of: 1792418848,
        },
        {
          id: "minimax",
          name: "MiniMax",
          state: "live",
          five_hour: { remaining_pct: 92, reset_at: 1792425600, reset_local: "周二 00:00" },
          weekly: { remaining_pct: 98, reset_at: 1792944000, reset_local: "周一 00:00" },
          error: null,
          as_of: 1792418848,
        },
      ],
    },
    fetched_at: "2026-10-19T14:07:37Z",
  };
}

// ── ① 真实形状正常夹具 ───────────────────────────────────────

describe("healthItems（① 真实形状 + ④ warn 项）", () => {
  it("枚举出 9 项：跳过 checked_at/status 与 *_detail，detail 正确配对", () => {
    const items = healthItems(realShapedFixture().snapshot.base.health);
    expect(items.map((i) => i.key)).toEqual([
      "pg",
      "worker",
      "queue",
      "codex_auth",
      "proxy",
      "zcode_env",
      "zcode_live",
      "dsh_budget",
      "egress_ip",
    ]);
    const byKey = Object.fromEntries(items.map((i) => [i.key, i]));
    expect(byKey.queue.detail).toBe("0 待处理 / 15 死信");
    expect(byKey.zcode_live.detail).toBe("端到端实活通过");
    expect(byKey.egress_ip.detail).toBe("154.40.137.230");
    // 无 _detail 配对的项 → detail 为 null（title 仍显示 key/value，不炸）
    expect(byKey.worker.detail).toBeNull();
    expect(byKey.pg.detail).toBeNull();
  });

  it("④ warn 项：detail 进 title 语义（detail 配对成功）且 tone 为琥珀", () => {
    const items = healthItems({ proxy: "warn", proxy_detail: "127.0.0.1:7892 dead" });
    expect(items).toEqual([{ key: "proxy", value: "warn", detail: "127.0.0.1:7892 dead" }]);
    expect(toneOf("warn")).toBe("amber");
    expect(toneOf("proxy")).toBe("muted"); // 未知值宁灰不误报
  });

  it("空/非对象输入 → 空数组不炸", () => {
    expect(healthItems(undefined)).toEqual([]);
    expect(healthItems(null)).toEqual([]);
    expect(healthItems({})).toEqual([]);
  });
});

describe("toneOf（④ ok/warn/fail → 绿黄红）", () => {
  it("ok→success，warn/warning→amber，fail/error/down→danger，未知→muted", () => {
    expect(toneOf("ok")).toBe("success");
    expect(toneOf("OK")).toBe("success");
    expect(toneOf("warn")).toBe("amber");
    expect(toneOf("warning")).toBe("amber");
    expect(toneOf("fail")).toBe("danger");
    expect(toneOf("error")).toBe("danger");
    expect(toneOf("down")).toBe("danger");
    expect(toneOf("whatever-new-state")).toBe("muted");
    expect(toneOf(null)).toBe("muted");
    expect(toneOf(undefined)).toBe("muted");
  });

  it("非字符串真值（数字/布尔/对象/数组）→ muted 不抛 TypeError（对抗审查：schema 漂移防线）", () => {
    // 复现原缺陷的输入形态：123 ?? "" 不接住真值，.trim 即 TypeError
    expect(toneOf(123)).toBe("muted");
    expect(toneOf(0)).toBe("muted");
    expect(toneOf(true)).toBe("muted");
    expect(toneOf({ trim: () => "ok" })).toBe("muted");
    expect(toneOf(["ok"])).toBe("muted");
  });
});

describe("queueEntries（① 动态键不被写死假设）", () => {
  it("实况仅 dead_letter → 恰好一枚举，无 queued/running 假设项", () => {
    expect(queueEntries(realShapedFixture().snapshot.base.queue)).toEqual([
      { key: "dead_letter", value: 15 },
    ]);
  });

  it("键集合变动（v2 出现 queued/running）自动跟上；非数键跳过", () => {
    expect(
      queueEntries({ queued: 3, running: 1, dead_letter: 0, junk: "x" as unknown as number }),
    ).toEqual([
      { key: "queued", value: 3 },
      { key: "running", value: 1 },
      { key: "dead_letter", value: 0 },
    ]);
    expect(queueEntries(undefined)).toEqual([]);
    expect(queueEntries(null)).toEqual([]);
  });
});

describe("fmtSecs（① recent[].secs 可 null）", () => {
  it("null/undefined → —，正常秒数带单位", () => {
    expect(fmtSecs(null)).toBe("—");
    expect(fmtSecs(undefined)).toBe("—");
    expect(fmtSecs(265)).toBe("265s");
  });
});

// ── ③ F5 新鲜度（15 分钟阈值） ───────────────────────────────

describe("stalenessState（③ + F5 红线）", () => {
  const NOW = Date.parse("2026-10-19T14:07:37Z");

  it("刚写入（fetched_at=now）→ fresh", () => {
    expect(stalenessState("2026-10-19T14:07:37Z", NOW)).toBe("fresh");
  });

  it("超 15 分钟 → stale", () => {
    const old = new Date(NOW - STALE_MS - 1).toISOString();
    expect(stalenessState(old, NOW)).toBe("stale");
  });

  it("恰 15 分钟整（未超）→ fresh（边界：严格大于才 stale）", () => {
    const edge = new Date(NOW - STALE_MS).toISOString();
    expect(stalenessState(edge, NOW)).toBe("fresh");
  });

  it("缺失/不可解析 → unknown（同样按未就绪处理）", () => {
    expect(stalenessState(null, NOW)).toBe("unknown");
    expect(stalenessState(undefined, NOW)).toBe("unknown");
    expect(stalenessState("not-a-date", NOW)).toBe("unknown");
    expect(stalenessState("", NOW)).toBe("unknown");
  });
});

describe("parseSnapshot（② 前端侧防御解析）", () => {
  it("合法 JSON 对象 → 解析结果", () => {
    const file = parseSnapshot(JSON.stringify(realShapedFixture()));
    expect(file?.fetched_at).toBe("2026-10-19T14:07:37Z");
    expect(file?.llm_quota?.providers?.length).toBe(4);
  });

  it("损坏 JSON / 非对象（数组、数字）/ 空输入 → null，不抛", () => {
    expect(parseSnapshot("not json {")).toBeNull();
    expect(parseSnapshot("[1,2]")).toBeNull();
    expect(parseSnapshot("42")).toBeNull();
    expect(parseSnapshot(null)).toBeNull();
    expect(parseSnapshot(undefined)).toBeNull();
    expect(parseSnapshot("")).toBeNull();
  });
});

// ── ⑤ F1 额度告警边界 ────────────────────────────────────────

describe("providerList（对抗审查：providers 元素形态守卫）", () => {
  it("null/数字/字符串/数组元素剔除，仅保留完整对象（坏元素不连坐整卡）", () => {
    expect(
      providerList([null, 123, "codex", [1], { id: "glm", name: "GLM" }]),
    ).toEqual([{ id: "glm", name: "GLM" }]);
  });

  it("非数组输入 → 空列表；实况四家全部保留", () => {
    expect(providerList(null)).toEqual([]);
    expect(providerList(undefined)).toEqual([]);
    expect(providerList({})).toEqual([]);
    expect(providerList("codex")).toEqual([]);
    const four = realShapedFixture().llm_quota.providers;
    expect(providerList(four)).toHaveLength(4);
    expect(providerList(four).map((p) => p.id)).toEqual(["codex", "glm", "kimi", "minimax"]);
  });
});

describe("quotaWarn（⑤ <20% 边界）", () => {
  it("19 → 告警；20 → 不告警（严格小于）", () => {
    expect(quotaWarn(19)).toBe(true);
    expect(quotaWarn(19.9)).toBe(true);
    expect(quotaWarn(20)).toBe(false);
    expect(quotaWarn(100)).toBe(false);
    expect(quotaWarn(0)).toBe(true);
  });

  it("null/NaN/非数 → 不告警（无数据不算低额度）", () => {
    expect(quotaWarn(null)).toBe(false);
    expect(quotaWarn(undefined)).toBe(false);
    expect(quotaWarn(Number.NaN)).toBe(false);
  });
});

// ── F2 dsh 水位 ──────────────────────────────────────────────

describe("dshPct", () => {
  it("实况 2/50 → 4%；超闸钳 100；非数 → null", () => {
    expect(dshPct(2)).toBe(4);
    expect(dshPct(50)).toBe(100);
    expect(dshPct(55)).toBe(100); // 钳上限
    expect(dshPct(-3)).toBe(0); // 钳下限
    expect(dshPct(null)).toBeNull();
    expect(dshPct(undefined, DSH_DAILY_GATE)).toBeNull();
  });
});

// ── ⑥ F8 前置：NAS 帧龄 ──────────────────────────────────────

describe("nasTone（⑥ null→红 / >600→黄 / ≤600→绿）", () => {
  it("边界分明：601 黄、600 绿、null 红", () => {
    expect(nasTone(null)).toBe("danger");
    expect(nasTone(undefined)).toBe("danger");
    expect(nasTone(601)).toBe("amber");
    expect(nasTone(600)).toBe("success");
    expect(nasTone(113)).toBe("success"); // 实况值
    expect(nasTone(Number.NaN)).toBe("danger");
  });
});

// ── ⑦ 14 天用量稀疏聚合 ──────────────────────────────────────

describe("aggUsage14d（⑦ 稀疏天不补零不炸）", () => {
  it("实况两天同日跨 harness → 合并为一条（ok/fail/secs/in_tok 各自求和）", () => {
    const days = aggUsage14d(realShapedFixture().snapshot.base.usage_14d);
    expect(days).toEqual([
      { day: "2026-10-19", ok: 18, fail: 3, secs: 741, in_tok: 47852 },
    ]);
  });

  it("稀疏：仅出现过的天有记录，不补零填充其他 13 天", () => {
    const days = aggUsage14d([
      { day: "2026-10-17", ok: 1, fail: 0, secs: 10, in_tok: 5, harness: "zcode" },
      { day: "2026-10-19", ok: 2, fail: 1, secs: 20, in_tok: 6, harness: "codex" },
    ]);
    expect(days.length).toBe(2);
    expect(days.map((d) => d.day)).toEqual(["2026-10-17", "2026-10-19"]); // 升序
    expect(days[0]).toEqual({ day: "2026-10-17", ok: 1, fail: 0, secs: 10, in_tok: 5 });
  });

  it("day 缺失/非字符串的行跳过，缺数字段按 0 计，不抛", () => {
    expect(
      aggUsage14d([
        { ok: 1, secs: 5 }, // 无 day
        { day: "", ok: 1 }, // 空 day
        { day: 42 as unknown as string }, // day 非字符串
        { day: "2026-10-19", ok: 1, fail: 2 }, // secs/in_tok 缺 → 按 0
      ]),
    ).toEqual([{ day: "2026-10-19", ok: 1, fail: 2, secs: 0, in_tok: 0 }]);
    expect(aggUsage14d(undefined)).toEqual([]);
    expect(aggUsage14d(null)).toEqual([]);
  });
});
