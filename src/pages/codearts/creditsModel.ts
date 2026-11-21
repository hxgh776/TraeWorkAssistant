import type { CodeArtsCreditsInfo, CodeArtsCreditsSnapshot } from '../../types';

/** CodeArts 积分看板视图模型（VENU-39 兼容修复）。
 *  旧 3.9.3 快照行 accounts 元素形如 {free, purchased, total, name, user_id}，
 *  没有 remain / used / package_name / expiring 字段；total ≠ free + purchased
 *  （口径不同），旧格式数值不可考——如实显「—」/趋势留空，延续
 *  「无积分套餐不用 0 冒充」口径，不做臆测映射。 */

/** 数值安全格式化：缺失/非有限数（旧格式行）显「—」，真实 0 照常显示 */
export function fmtCredits(v: number | undefined): string {
  return typeof v === 'number' && Number.isFinite(v) ? v.toLocaleString() : '—';
}

/** 最新快照 → 账号余额视图行（旧格式行缺字段保持 undefined，不臆测补值） */
export function creditsViewRows(latest: CodeArtsCreditsSnapshot | null): CodeArtsCreditsInfo[] {
  return (latest?.accounts ?? []).map((a) => ({
    account_id: a.id ?? a.user_id,
    user_id: a.user_id,
    name: a.name,
    package_name: a.package_name || '',
    is_credit_package: true,
    total: a.total,
    used: a.used,
    remain: a.remain,
    expiring: a.expiring ?? 0,
    bonus_remain: a.bonus_remain,
    end_date: a.end_date || '',
    fetched_at: latest ? new Date(latest.ts).toLocaleString() : '',
  }));
}

/** 快照当日总余额：仅合计数值型 remain；整行无可考数值（旧格式）返回 null，
 *  趋势图留空不冒充 0。 */
export function snapshotRemainTotal(snap: CodeArtsCreditsSnapshot): number | null {
  let sum = 0;
  let known = false;
  for (const a of snap.accounts) {
    if (typeof a.remain === 'number' && Number.isFinite(a.remain)) {
      sum += a.remain;
      known = true;
    }
  }
  return known ? sum : null;
}
