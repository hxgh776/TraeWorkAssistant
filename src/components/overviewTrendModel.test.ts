import { describe, expect, it } from 'vitest';
import { aggregateCheckinTrends } from './overviewTrendModel';

describe('aggregateCheckinTrends', () => {
  it('同日同账号多轮并存取当日最好结果（失败后重试成功 → 成功）', () => {
    const pts = aggregateCheckinTrends([
      { date: '2026-11-22', user_id: 'u1', name: 'a', status: 'fail' },
      { date: '2026-11-22', user_id: 'u1', name: 'a', status: 'success' },
    ]);
    expect(pts).toEqual([{ date: '2026-11-22', ok: 1, already: 0, failed: 0, noop: 0 }]);
  });

  it('已领取优先于失败；仅 noop 的账号不入三桶但日点保留', () => {
    const pts = aggregateCheckinTrends([
      { date: '2026-11-22', user_id: 'u1', status: 'fail' },
      { date: '2026-11-22', user_id: 'u1', status: 'already' },
      { date: '2026-11-21', user_id: 'u1', status: 'noop' },
    ]);
    expect(pts).toHaveLength(2);
    expect(pts[0]).toEqual({ date: '2026-11-21', ok: 0, already: 0, failed: 0, noop: 1 });
    expect(pts[1]).toEqual({ date: '2026-11-22', ok: 0, already: 1, failed: 0, noop: 0 });
  });

  it('同日多账号分桶计数；无 user_id 回落 name 作账号键', () => {
    const pts = aggregateCheckinTrends([
      { date: '2026-11-22', user_id: 'u1', status: 'success' },
      { date: '2026-11-22', user_id: 'u2', status: 'already' },
      { date: '2026-11-22', user_id: '', name: 'legacy', status: 'fail' },
      { date: '2026-11-22', user_id: '', name: 'legacy', status: 'noop' },
    ]);
    expect(pts).toEqual([{ date: '2026-11-22', ok: 1, already: 1, failed: 0, noop: 1 }]);
  });

  it('按日期升序输出；空输入返回空数组', () => {
    const pts = aggregateCheckinTrends([
      { date: '2026-11-20', user_id: 'u1', status: 'success' },
      { date: '2026-11-18', user_id: 'u1', status: 'fail' },
    ]);
    expect(pts.map((p) => p.date)).toEqual(['2026-11-18', '2026-11-20']);
    expect(aggregateCheckinTrends([])).toEqual([]);
  });
});
