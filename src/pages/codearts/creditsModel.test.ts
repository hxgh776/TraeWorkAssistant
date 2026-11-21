import { describe, expect, it } from 'vitest';
import { creditsViewRows, fmtCredits, snapshotRemainTotal } from './creditsModel';
import type { CodeArtsCreditsSnapshot } from '../../types';

/** VENU-39 回归：3.9.3 旧格式快照（本机 codearts_credits_history 实存行形态）
 *  不得让积分看板崩溃或用 0 冒充余额。 */
const LEGACY_ROW = {
  free: 6500.38,
  name: 'hw011008953',
  purchased: 0.0,
  total: 7000.38,
  user_id: 'ca-b0be82d2af31',
} as unknown as CodeArtsCreditsSnapshot['accounts'][number];

const NEW_ROW: CodeArtsCreditsSnapshot['accounts'][number] = {
  id: 'ca-1',
  user_id: 'ca-b0be82d2af31',
  name: 'hw011008953',
  package_name: '标准套餐',
  total: 7000,
  used: 500,
  remain: 6500,
  expiring: 200,
  end_date: '2026-12-31',
};

describe('fmtCredits', () => {
  it('数值正常本地化（真实 0 也显示）', () => {
    expect(fmtCredits(12345)).toBe((12345).toLocaleString());
    expect(fmtCredits(0)).toBe('0');
  });
  it('缺失/非有限数显「—」，不用 0 冒充', () => {
    expect(fmtCredits(undefined)).toBe('—');
    expect(fmtCredits(NaN)).toBe('—');
    expect(fmtCredits(Infinity)).toBe('—');
  });
});

describe('creditsViewRows', () => {
  it('无快照 → 空视图行', () => {
    expect(creditsViewRows(null)).toEqual([]);
  });
  it('新格式行字段完整透传', () => {
    const snap: CodeArtsCreditsSnapshot = { date: '2026-11-21', ts: 1, accounts: [NEW_ROW] };
    const [row] = creditsViewRows(snap);
    expect(row.remain).toBe(6500);
    expect(row.total).toBe(7000);
    expect(row.used).toBe(500);
    expect(row.package_name).toBe('标准套餐');
    expect(row.expiring).toBe(200);
    expect(row.account_id).toBe('ca-1');
  });
  it('旧格式行缺 remain/used/package_name 保持 undefined（不臆测补值），user_id 兜底作 key', () => {
    const snap: CodeArtsCreditsSnapshot = {
      date: '2026-11-17',
      ts: 1794920225690,
      accounts: [LEGACY_ROW],
    };
    const [row] = creditsViewRows(snap);
    expect(row.remain).toBeUndefined();
    expect(row.used).toBeUndefined();
    // 旧格式自带 total（口径不可考）原样透传，渲染层显真实数值；余额/已用显「—」
    expect(row.total).toBe(7000.38);
    expect(row.package_name).toBe('');
    expect(row.account_id).toBe('ca-b0be82d2af31');
    expect(row.name).toBe('hw011008953');
  });
});

describe('snapshotRemainTotal', () => {
  it('新格式快照合计数值型 remain', () => {
    const snap: CodeArtsCreditsSnapshot = {
      date: '2026-11-21',
      ts: 1,
      accounts: [NEW_ROW, { ...NEW_ROW, id: 'ca-2', remain: 500 }],
    };
    expect(snapshotRemainTotal(snap)).toBe(7000);
  });
  it('旧格式快照（无可考 remain）返回 null，趋势留空不冒充 0', () => {
    const snap: CodeArtsCreditsSnapshot = {
      date: '2026-11-17',
      ts: 1794920225690,
      accounts: [LEGACY_ROW],
    };
    expect(snapshotRemainTotal(snap)).toBeNull();
  });
});
