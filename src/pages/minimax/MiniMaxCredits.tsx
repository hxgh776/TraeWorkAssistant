import { useCallback, useEffect, useMemo, useState } from 'react';
import { Bar, BarChart, CartesianGrid, Line, LineChart, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts';
import { RefreshCw } from 'lucide-react';
import PageHeader from '../../components/PageHeader';
import { Badge, EmptyState, Spinner, StatCard } from '../../components/ui';
import { api } from '../../lib/tauri';
import { useAppStore } from '../../store';
import type { MinimaxCreditsInfo, MinimaxCreditsSnapshot, MinimaxRegion } from '../../types';

/** MiniMax · 积分看板页（VENU-36 重建）。
 *  余额来自 get_membership_info 快照（free/purchased 拆分，禁止硬编码签到金额）；
 *  「近 30 天获得」来自签到结果 reward 合计；上游失败如实报错，不用 0 冒充。 */

const REGION_LABEL: Record<MinimaxRegion, string> = {
  minimax: '国内区',
  'minimax-global': '国际区',
};

export default function MiniMaxCredits() {
  const pushToast = useAppStore((s) => s.pushToast);
  const [accounts, setAccounts] = useState<MinimaxCreditsInfo[]>([]);
  const [snapshots, setSnapshots] = useState<MinimaxCreditsSnapshot[]>([]);
  const [checkinRewards, setCheckinRewards] = useState<{ date: string; earned: number }[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [snaps, results] = await Promise.all([api.minimax.creditsHistory(30), api.minimax.checkinResults(30)]);
      setSnapshots(snaps);
      // 近 30 天每日签到获得（reward 合计；同 uid 同日取最后一次为最终态口径）
      const byDate = new Map<string, Map<string, number>>();
      for (const r of results) {
        if (r.status !== 'success' || typeof r.reward !== 'number' || r.reward <= 0) continue;
        const perUid = byDate.get(r.date) ?? new Map<string, number>();
        perUid.set(r.user_id, r.reward);
        byDate.set(r.date, perUid);
      }
      const earned = [...byDate.entries()]
        .map(([date, perUid]) => ({ date, earned: [...perUid.values()].reduce((s, v) => s + v, 0) }))
        .sort((a, b) => a.date.localeCompare(b.date));
      setCheckinRewards(earned);
      // 最新快照的账号余额 → accounts 视图（余额列展示）
      const latest = snaps.length > 0 ? snaps[snaps.length - 1] : null;
      const rows: MinimaxCreditsInfo[] = (latest?.accounts ?? []).map((a) => ({
        account_id: a.id,
        user_id: a.user_id,
        name: a.name,
        region: a.region,
        plan_name: a.plan_name || '',
        free: a.free,
        purchased: a.purchased,
        total: a.total,
        expires_at_ms: 0,
        fetched_at: latest ? new Date(latest.ts).toLocaleString() : '',
      }));
      setAccounts(rows);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const refresh = async () => {
    setRefreshing(true);
    try {
      const rows = await api.minimax.creditsFetch();
      const failed = rows.filter((r) => r.error);
      if (failed.length > 0) {
        pushToast('error', `${failed.length} 个账号查询失败：${failed.map((r) => `${r.name || r.user_id}（${r.error}）`).join('；')}`);
      } else {
        pushToast('success', `已刷新 ${rows.length} 个账号余额`);
      }
      await load();
    } catch (e) {
      pushToast('error', String(e));
    } finally {
      setRefreshing(false);
    }
  };

  // 趋势数据（快照 per-date 总余额 + 签到获得双线）
  const trend = useMemo(() => {
    const balance = new Map<string, number>();
    for (const s of snapshots) {
      const total = s.accounts.reduce((sum, a) => sum + (a.total ?? 0), 0);
      balance.set(s.date, total);
    }
    const earned = new Map(checkinRewards.map((r) => [r.date, r.earned]));
    const dates = [...new Set([...balance.keys(), ...earned.keys()])].sort();
    return dates.map((d) => ({
      date: d.slice(5),
      balance: balance.get(d) ?? null,
      earned: earned.get(d) ?? 0,
    }));
  }, [snapshots, checkinRewards]);

  const totalBalance = accounts.reduce((s, a) => s + (a.total ?? 0), 0);
  const totalFree = accounts.reduce((s, a) => s + (a.free ?? 0), 0);
  const totalPurchased = accounts.reduce((s, a) => s + (a.purchased ?? 0), 0);
  const earned30 = checkinRewards.reduce((s, r) => s + r.earned, 0);
  const today = new Date().toISOString().slice(0, 10);
  const earnedToday = checkinRewards.find((r) => r.date === today)?.earned ?? 0;

  return (
    <div>
      <PageHeader
        title="MiniMax 积分看板"
        desc="余额取自 get_membership_info 快照（免费/购买拆分）；近 30 天获得来自签到记录"
        actions={
          <button
            onClick={() => void refresh()}
            disabled={refreshing}
            className="flex h-9 items-center gap-1.5 rounded-lg bg-zinc-900 px-4 text-sm font-medium text-white transition hover:bg-zinc-800 disabled:opacity-40 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-zinc-200"
          >
            {refreshing ? <Spinner className="h-4 w-4" /> : <RefreshCw size={15} />}
            刷新余额
          </button>
        }
      />

      {error && (
        <div className="mb-4 rounded-lg border border-rose-200 bg-rose-50 px-4 py-2.5 text-sm text-rose-600 dark:border-rose-900 dark:bg-rose-950 dark:text-rose-300">
          {error}
        </div>
      )}

      {loading ? (
        <div className="flex justify-center py-12"><Spinner className="h-5 w-5" /></div>
      ) : (
        <>
          <div className="mb-4 grid grid-cols-2 gap-3 lg:grid-cols-5">
            <StatCard label="总余额" value={accounts.length > 0 ? totalBalance.toLocaleString() : '—'} tone="brand" />
            <StatCard label="免费/活动积分" value={accounts.length > 0 ? totalFree.toLocaleString() : '—'} />
            <StatCard label="购买积分" value={accounts.length > 0 ? totalPurchased.toLocaleString() : '—'} />
            <StatCard label="今日签到获得" value={earnedToday > 0 ? `+${earnedToday}` : '0'} tone="green" />
            <StatCard label="近 30 天获得" value={earned30 > 0 ? `+${earned30.toLocaleString()}` : '0'} tone="green" />
          </div>

          {/* 账号余额明细 */}
          {accounts.length === 0 ? (
            <div className="mb-4">
              <EmptyState title="暂无余额数据" hint="添加账号后点「刷新余额」拉取实时积分（上游失败会报错，不用 0 冒充）。" />
            </div>
          ) : (
            <div className="mb-4 overflow-hidden rounded-lg border border-slate-200 bg-white dark:border-zinc-800 dark:bg-zinc-900">
              <div className="border-b border-slate-100 px-4 py-3 text-sm font-medium dark:border-zinc-800">账号余额</div>
              <div className="divide-y divide-slate-100 text-sm dark:divide-zinc-800">
                {accounts.map((a) => (
                  <div key={`${a.region}-${a.account_id}`} className="flex flex-wrap items-center gap-2 px-4 py-2.5">
                    <Badge tone={a.region === 'minimax' ? 'blue' : 'violet'}>{REGION_LABEL[a.region] || a.region}</Badge>
                    <span className="font-medium text-slate-800 dark:text-zinc-100">{a.name || a.user_id}</span>
                    {a.plan_name && <Badge tone="slate">{a.plan_name}</Badge>}
                    <span className="ml-auto text-slate-600 dark:text-zinc-300">
                      免费 <b>{a.free.toLocaleString()}</b> · 购买 <b>{a.purchased.toLocaleString()}</b> · 合计 <b>{a.total.toLocaleString()}</b>
                    </span>
                  </div>
                ))}
              </div>
            </div>
          )}

          {/* 近 30 天趋势 */}
          <div className="grid gap-4 lg:grid-cols-2">
            <div className="rounded-lg border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
              <h2 className="mb-3 text-sm font-medium">余额趋势（近 30 天快照）</h2>
              {trend.filter((t) => t.balance != null).length === 0 ? (
                <EmptyState title="暂无快照" hint="积分数据同步到点或手动刷新后生成。" />
              ) : (
                <ResponsiveContainer width="100%" height={220}>
                  <LineChart data={trend} margin={{ top: 5, right: 10, bottom: 0, left: 0 }}>
                    <CartesianGrid strokeDasharray="3 3" stroke="currentColor" className="text-slate-200 dark:text-zinc-800" />
                    <XAxis dataKey="date" tick={{ fontSize: 11 }} />
                    <YAxis tick={{ fontSize: 11 }} width={54} />
                    <Tooltip />
                    <Line type="monotone" dataKey="balance" name="总余额" stroke="#10b981" dot={false} strokeWidth={2} />
                  </LineChart>
                </ResponsiveContainer>
              )}
            </div>
            <div className="rounded-lg border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
              <h2 className="mb-3 text-sm font-medium">每日签到获得（近 30 天）</h2>
              {checkinRewards.length === 0 ? (
                <EmptyState title="暂无签到获得" hint="签到成功后按日聚合展示。" />
              ) : (
                <ResponsiveContainer width="100%" height={220}>
                  <BarChart data={trend} margin={{ top: 5, right: 10, bottom: 0, left: 0 }}>
                    <CartesianGrid strokeDasharray="3 3" stroke="currentColor" className="text-slate-200 dark:text-zinc-800" />
                    <XAxis dataKey="date" tick={{ fontSize: 11 }} />
                    <YAxis tick={{ fontSize: 11 }} width={54} allowDecimals={false} />
                    <Tooltip />
                    <Bar dataKey="earned" name="签到获得" fill="#3b82f6" radius={[3, 3, 0, 0]} />
                  </BarChart>
                </ResponsiveContainer>
              )}
            </div>
          </div>
        </>
      )}
    </div>
  );
}
