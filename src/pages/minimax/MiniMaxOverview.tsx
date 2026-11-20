import { useCallback, useEffect, useState } from 'react';
import { CalendarCheck } from 'lucide-react';
import PageHeader from '../../components/PageHeader';
import { Badge, EmptyState, Spinner, StatCard } from '../../components/ui';
import { api } from '../../lib/tauri';
import { useAppStore } from '../../store';
import type { MinimaxAccountView, MinimaxCheckinRecord, MinimaxRegion } from '../../types';

/** MiniMax · 概述页（VENU-36 重建）：账号分布 / 今日签到 / 调度与同步状态总览。 */

const REGION_LABEL: Record<MinimaxRegion, string> = {
  minimax: '国内区',
  'minimax-global': '国际区',
};

export default function MiniMaxOverview() {
  const settings = useAppStore((s) => s.settings);
  const [accounts, setAccounts] = useState<MinimaxAccountView[]>([]);
  const [results, setResults] = useState<MinimaxCheckinRecord[]>([]);
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    try {
      const [list, rec] = await Promise.all([api.minimax.accountsList(), api.minimax.checkinResults(7)]);
      setAccounts(list);
      setResults(rec);
    } catch {
      /* 概述页容错：读失败展示空态，详细错误在对应页呈现 */
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const today = new Date().toISOString().slice(0, 10);
  const cnCount = accounts.filter((a) => a.region === 'minimax').length;
  const globalCount = accounts.filter((a) => a.region === 'minimax-global').length;
  const needsRelogin = accounts.filter((a) => a.needs_relogin).length;
  const todayOk = results.filter((r) => r.date === today && r.status === 'success').length;
  const todayAlready = results.filter((r) => r.date === today && r.status === 'already').length;
  const todayTotal = accounts.length;
  const scheduled = settings?.minimax_checkin_enabled !== false;
  const hhmm = settings?.minimax_checkin_hhmm || '00:05';
  const syncMode = settings?.minimax_credits_sync_mode || 'every_hours';
  const syncHours = settings?.minimax_credits_sync_hours || '2';
  const creditsKnown = accounts.filter((a) => a.credits_total != null);
  const creditsTotal = creditsKnown.reduce((s, a) => s + (a.credits_total ?? 0), 0);

  return (
    <div>
      <PageHeader
        title="MiniMax 概述"
        desc="MiniMax Code 双区（国内 minimax.cn / 国际 minimax.io）账号池、每日签到与积分同步总览"
        actions={
          <button onClick={() => void load()} className="flex h-9 items-center gap-1.5 rounded-lg border border-slate-200 bg-white px-3 text-sm transition hover:bg-slate-50 dark:border-zinc-700 dark:bg-zinc-900 dark:hover:bg-zinc-800">
            <CalendarCheck size={15} /> 刷新
          </button>
        }
      />

      {loading ? (
        <div className="flex justify-center py-12"><Spinner className="h-5 w-5" /></div>
      ) : (
        <>
          <div className="mb-4 grid grid-cols-2 gap-3 lg:grid-cols-4">
            <StatCard label="账号总数" value={String(accounts.length)} />
            <StatCard label="今日签到" value={todayTotal > 0 ? `${todayOk + todayAlready}/${todayTotal}` : '—'} tone="brand" />
            <StatCard label="积分余额" value={creditsKnown.length > 0 ? creditsTotal.toLocaleString() : '—'} tone="green" />
            <StatCard label="需重新授权" value={String(needsRelogin)} tone={needsRelogin > 0 ? 'amber' : 'slate'} />
          </div>

          <div className="grid gap-4 lg:grid-cols-2">
            {/* 区分布与调度 */}
            <div className="rounded-lg border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
              <h2 className="mb-3 text-sm font-medium">区分布与调度</h2>
              <div className="space-y-2 text-sm">
                <div className="flex items-center gap-2">
                  <Badge tone="blue">国内区</Badge>
                  <span className="text-slate-600 dark:text-zinc-300">{cnCount} 个账号</span>
                </div>
                <div className="flex items-center gap-2">
                  <Badge tone="violet">国际区</Badge>
                  <span className="text-slate-600 dark:text-zinc-300">{globalCount} 个账号</span>
                  <span className="text-xs text-slate-400">（大陆网络直连可能被拦截，需代理兜底）</span>
                </div>
                <div className="flex items-center gap-2 pt-1">
                  <Badge tone={scheduled ? 'green' : 'slate'}>{scheduled ? '自动签到开' : '自动签到关'}</Badge>
                  <span className="text-slate-600 dark:text-zinc-300">每日 {hhmm} 首跑</span>
                </div>
                <div className="flex items-center gap-2">
                  <Badge tone="slate">积分同步</Badge>
                  <span className="text-slate-600 dark:text-zinc-300">
                    {syncMode === 'off' ? '已关闭' : syncMode === 'hourly' ? '每小时' : syncMode === 'daily' ? `每日 ${settings?.minimax_credits_sync_hhmm || '23:35'}` : `每 ${syncHours} 小时`}
                  </span>
                </div>
                <div className="flex items-center gap-2">
                  <Badge tone="slate">双区开关</Badge>
                  <span className="text-slate-600 dark:text-zinc-300">
                    国内 {settings?.minimax_cn_enabled !== false ? '参与' : '停用'} · 国际 {settings?.minimax_global_enabled !== false ? '参与' : '停用'}
                  </span>
                </div>
              </div>
            </div>

            {/* 最近签到 */}
            <div className="rounded-lg border border-slate-200 bg-white dark:border-zinc-800 dark:bg-zinc-900">
              <div className="border-b border-slate-100 px-4 py-3 text-sm font-medium dark:border-zinc-800">最近签到（近 7 天）</div>
              {results.length === 0 ? (
                <EmptyState title="暂无记录" hint="手动签到或调度到点后展示。" />
              ) : (
                <div className="divide-y divide-slate-100 text-sm dark:divide-zinc-800">
                  {results
                    .slice()
                    .reverse()
                    .slice(0, 8)
                    .map((r, i) => (
                      <div key={`${r.date}-${r.user_id}-${r.time_ms}-${i}`} className="flex items-center gap-2 px-4 py-2">
                        <Badge tone={r.status === 'success' ? 'green' : r.status === 'already' ? 'amber' : 'red'}>
                          {r.status === 'success' ? '成功' : r.status === 'already' ? '已领取' : '失败'}
                        </Badge>
                        <span className="text-slate-500 dark:text-zinc-400">{r.date} {r.time}</span>
                        <span className="text-slate-700 dark:text-zinc-200">{r.name || r.user_id}</span>
                        <Badge tone="slate">{REGION_LABEL[r.region] || r.region}</Badge>
                        {typeof r.reward === 'number' && r.reward > 0 && <Badge tone="green">+{r.reward}</Badge>}
                      </div>
                    ))}
                </div>
              )}
            </div>
          </div>
        </>
      )}
    </div>
  );
}
