/**
 * 概述页「近 30 天签到结果」趋势聚合纯模型（VENU-41 风格对齐抽平复用）。
 *
 * 四应用（OfficeAce / LobsterAI / MiniMax / CodeArts）概述页共用同一口径：
 * - 逐账号逐日取当日最终态：任一 success → 成功；否则任一 already → 已领取；
 *   否则任一 noop → 无可领取；否则失败（口径对齐 checkinCalendarModel.daysFromWbRecords
 *   的「任一成功即当日已领，全失败才计失败」——手动重试/调度退避多轮并存时
 *   以当日最好结果计，失败后重试成功不得把该账号整天画成失败）
 * - 同日同账号去重后入桶；noop（无可领取）为中性结果，不入成功/失败桶，
 *   仅保留日点（当日只有 noop 时柱状图为空但不缺日）
 */

/** 概述页近 30 天签到趋势数据点（按日聚合） */
export interface CheckinTrendPoint {
  date: string;
  ok: number;
  already: number;
  failed: number;
  /** 无可领取（CodeArts noop；中性结果不计入三桶，仅供需要时展示） */
  noop: number;
}

export interface CheckinTrendInput {
  date: string;
  user_id: string;
  name?: string;
  status: string;
}

export function aggregateCheckinTrends(records: CheckinTrendInput[]): CheckinTrendPoint[] {
  // date → (账号键 → 当日记录状态集合)
  const byDay = new Map<string, Map<string, string[]>>();
  for (const r of records) {
    if (!r.date) continue;
    const accs = byDay.get(r.date) ?? new Map<string, string[]>();
    const key = r.user_id || r.name || '';
    const arr = accs.get(key) ?? [];
    arr.push(r.status);
    accs.set(key, arr);
    byDay.set(r.date, accs);
  }
  return [...byDay.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([date, accs]) => {
      const p: CheckinTrendPoint = { date, ok: 0, already: 0, failed: 0, noop: 0 };
      for (const statuses of accs.values()) {
        if (statuses.includes('success')) p.ok += 1;
        else if (statuses.includes('already')) p.already += 1;
        else if (statuses.includes('noop')) p.noop += 1;
        else p.failed += 1;
      }
      return p;
    });
}
