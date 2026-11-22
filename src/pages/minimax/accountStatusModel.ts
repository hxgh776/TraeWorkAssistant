/**
 * MiniMax 概述「需重新授权」判定纯模型（VENU-41 F1 抽出以便单测）。
 *
 * 与每日签到页 MiniTokenBadge 同口径：needs_relogin 标记，或 token 有明确到期时刻
 * 且已过（token_expires_at_ms > 0 且 <= now）的账号均计入——消除
 * 「概述 0 / 签到页红」的两页矛盾。token 到期时刻未知（0/缺省）不计入：
 * 设备码链可自动刷新，未知 ≠ 失效，不得误报。
 */

/** 概述页判定所需的账号字段子集（MinimaxAccountView 的结构化切片） */
export interface MiniMaxExpiryLike {
  needs_relogin: boolean;
  /** STS/access token 过期时刻（Unix 毫秒；0/缺省 = 未知） */
  token_expires_at_ms?: number | null;
}

/** 凭证链失效判定：needs_relogin 标记优先，其次 token 已过期 */
export function isExpiredAccount(a: MiniMaxExpiryLike, nowMs = Date.now()): boolean {
  if (a.needs_relogin) return true;
  return !!a.token_expires_at_ms && a.token_expires_at_ms > 0 && a.token_expires_at_ms <= nowMs;
}
