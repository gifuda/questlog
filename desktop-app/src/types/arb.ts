/**
 * 套利机会栏 (Arb Board) 类型 — 与 GROS board-export 契约对齐。
 * 契约: docs/套利机会展示栏需求_GROS数据源_20260828.md 第 2 节（2026-08-28 冻结）。
 * 纯只读：这些类型只用于展示，app 不写任何 GROS 数据。
 */

export type ArbBoardStatus = "ok" | "off" | "missing" | "parse_error";

export interface ArbEvidenceCounts {
  support: number;
  oppose: number;
}

/**
 * 侦察线索（契约 v3 A 段，2026-09-05 GROS 侧冻结 + 对表裁定）。
 * 字段以 GROS-02 实际导出为准：verifiable_data / conversation_id / created_at。
 */
export type ArbLeadStatus = "open" | "promoted" | "dismissed";

export interface ArbLead {
  id: string;
  /** 自由探索模式为 null */
  domain: string | null;
  date: string | null;
  phenomenon: string | null;
  cognitive_gap: string | null;
  verifiable_data: string | null;
  /** 实测为中文"高/中/低"（GROS-02 导出），契约初稿的英文枚举不成立 */
  priority: string | null;
  conversation_id: string | null;
  status: ArbLeadStatus;
  /** 账房落库时间 */
  created_at: string | null;
  promoted_hypothesis_id: string | null;
}

/**
 * 自动化控制区当前状态（契约 v3 B 段，已冻结；config-sync 回显，UI 唯一真值）。
 * autonomy 实测为 "on"/"off" 字符串（GROS-02 导出），兼容 boolean。
 */
export interface ArbControl {
  models: string[];
  current_model: string | null;
  autonomy: boolean | string | null;
  leads_per_run: number | null;
  research_quota_per_week: number | null;
  updated_at: string | null;
}

export interface ArbOpenHypothesis {
  /** 8 位短 id（展示用） */
  id: string;
  /** 完整 id（程序用：复制结算命令） */
  hypothesis_id: string;
  title: string;
  status: "open";
  /**
   * 价差 = fair − observed。契约标 number，但实测 GROS 会出现 null
   * （未做公允值估量的假设）——UI 渲染必须防 null，null 排序末位。
   */
  price_diff: number | null;
  /** 年化价差；null = 未填兑现窗口（排序排末位） */
  annualized: number | null;
  horizon_days: number | null;
  age_days: number | null;
  fair_value: number | null;
  observed_value: number | null;
  evidence: Partial<ArbEvidenceCounts> | null;
  falsification_triggers: string[] | null;
  next_action: string;
  /** 完整的 gros settle 命令，复制即可执行 */
  settle_hint: string;
}

export type ArbOutcome = "confirmed" | "falsified" | "expired";

export interface ArbSettledHypothesis {
  id: string;
  hypothesis_id: string;
  title: string;
  outcome: ArbOutcome;
  settled_at: string | null;
  realized_value: number | null;
  price_diff_at_settle: number | null;
  close_reason: string;
}

/**
 * 晨报段（契约 v4 增补 2026-10-08，需求文件第 8 节）。
 * board.json 由 GROS-01 board-export 产出（与 decisions 段同批代码窗口），
 * 该段未产出前整卡置灰。全字段可 null（null 容忍铁律）。
 */
export interface ArbBriefOpportunity {
  title: string | null;
  priority: string | null;
}

export interface ArbBrief {
  /** 轮次日期（YYYY-MM-DD） */
  brief_date: string | null;
  /** 概要一句话 */
  headline: string | null;
  opportunities: ArbBriefOpportunity[] | null;
}

/**
 * 决策项（契约 v4 A 段：decision-queue 档案元素原样透传，采集机不加工）。
 * cmd_approve / cmd_reject 是 GROS 预生成的完整薄壳命令草稿——app 不组装命令，
 * 只做白名单校验后执行；系统事项类无可执行动作则 cmd 为 null（纯展示，无按钮）。
 */
export interface ArbDecisionItem {
  id: string | null;
  item: string | null;
  advice: string | null;
  /** YYYY-MM-DD；null = 无截止 */
  deadline: string | null;
  pending_days: number | null;
  cmd_approve: string | null;
  cmd_reject: string | null;
}

export interface ArbSummary {
  open: number;
  settled: number;
  /** confirmed/(confirmed+falsified)；expired 不计入；null = 无样本 */
  hit_rate: number | null;
  /** 结算偏差均值；null = 无样本 */
  mean_bias: number | null;
  bias_samples: number;
}

/** get_arb_board 命令的返回（含 app 端附加的 status/board_path） */
export interface ArbBoardFile extends ArbSummaryShape {
  status: ArbBoardStatus;
  board_path?: string;
  generated_at?: string;
  error?: string;
}

/** 契约里 board.json 的 summary 与 open/settled 数组 */
interface ArbSummaryShape {
  summary?: ArbSummary;
  open?: ArbOpenHypothesis[];
  settled?: ArbSettledHypothesis[];
  /**
   * 契约 v3 增量段。undefined = GROS-01 增量 #2 未落地（UI 置灰）；
   * [] / {} = 已落地但为空。全字段允许 null（渲染 "—"，不许崩溃）。
   */
  leads?: ArbLead[];
  control?: ArbControl;
  /**
   * 契约 v4 增量段（2026-10-08 晨报卡片）。undefined = GROS 侧未产出（UI
   * 置灰「等待 GROS 侧数据」）；[] / {} = 已产出但为空。全字段允许 null。
   */
  brief?: ArbBrief;
  decisions?: ArbDecisionItem[];
}
