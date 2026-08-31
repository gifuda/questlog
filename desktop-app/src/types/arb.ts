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
}
