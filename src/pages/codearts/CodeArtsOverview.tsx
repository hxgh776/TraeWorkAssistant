import { useCallback, useEffect, useState } from 'react';
import { CalendarCheck } from 'lucide-react';
import PageHeader from '../../components/PageHeader';
import { Badge, EmptyState, Spinner, StatCard } from '../../components/ui';
import { api } from '../../lib/tauri';
import { useAppStore } from '../../store';
import type { CodeArtsAccountView, CodeArtsCheckinRecord, CodeArtsLocalScan } from '../../types';

/** CodeArts · 概述页（VENU-37 重建）：账号与凭证状态 / 今日签到 / 调度与同步总览。 */

function statusBadge(status: string): { tone: 'green' | 'amber' | 'slate' | 'red'; text: string } {
  if (status === 'success') return { tone: 'green', text: '成功' };
  if (status === 'already') return { tone: 'amber', text: '已领取' };
  if (status === 'noop') return { tone: 'slate', text: '无可领取' };
  if (status === 'fail') return { tone: 'red', text: '失败' };
  return { tone: 'slate', text: status || '—' };
}

const SOURCE_LABEL: Record<string, string> = {
  local_import: '登录态导入',
  oauth: '浏览器 OAuth',
  aksk: '手工 AK/SK',
};

export default function CodeArtsOverview() {
  const settings = useAppStore((s) => s.settings);
  const [accounts, setAccounts] = useState<CodeArtsAccountView[]>([]);
  const [results, setResults] = useState<CodeArtsCheckinRecord[]>([]);
  const [scan, setScan] = useState<CodeArtsLocalScan | null>(null);
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    try {
      const [list, rec, local] = await Promise.all([
        api.codearts.accountsList(),
        api.codearts.checkinResults(7),
        api.codearts.localScan().catch(() => null),
      ]);
      setAccounts(list);
      setResults(rec);
      setScan(local);
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
  const needsRelogin = accounts.filter((a) => a.needs_relogin);
  const todayOk = results.filter((r) => r.date === today && r.status === 'success').length;
  const todayAlready = results.filter((r) => r.date === today && r.status === 'already').length;
  const todayTotal = accounts.length;
  const scheduled = settings?.codearts_checkin_enabled !== false;
  const hhmm = settings?.codearts_checkin_hhmm || '00:20';
  const syncMode = settings?.codearts_credits_sync_mode || 'daily';
  const creditsKnown = accounts.filter((a) => a.credits_total != null);
  const creditsTotal = creditsKnown.reduce((s, a) => s + (a.credits_total ?? 0), 0);

  return (
    <div>
      <PageHeader
        title="CodeArts 概述"
        desc="华为云 CodeArts（码道）账号、每日签到与积分同步总览"
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
            <StatCard label="需重新导入" value={String(needsRelogin.length)} tone={needsRelogin.length > 0 ? 'amber' : 'slate'} />
          </div>

          <div className="grid gap-4 lg:grid-cols-2">
            {/* 凭证与调度 */}
            <div className="rounded-lg border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
              <h2 className="mb-3 text-sm font-medium">凭证与调度</h2>
              <div className="space-y-2 text-sm">
                <div className="flex items-center gap-2">
                  <Badge tone="blue">本机登录态</Badge>
                  {scan?.found ? (
                    <span className="text-slate-600 dark:text-zinc-300">
                      已找到{scan.expired ? '（已过期）' : ''}{scan.identity_name ? `：${scan.identity_name}` : ''}
                    </span>
                  ) : (
                    <span className="text-slate-500 dark:text-zinc-400">未找到（账号管理页可扫描导入 / OAuth / AK/SK）</span>
                  )}
                </div>
                <div className="flex items-center gap-2">
                  <Badge tone={scheduled ? 'green' : 'slate'}>{scheduled ? '自动签到开' : '自动签到关'}</Badge>
                  <span className="text-slate-600 dark:text-zinc-300">每日 {hhmm} 首跑</span>
                </div>
                <div className="flex items-center gap-2">
                  <Badge tone="slate">积分同步</Badge>
                  <span className="text-slate-600 dark:text-zinc-300">
                    {syncMode === 'off' ? '已关闭' : syncMode === 'hourly' ? '每小时' : syncMode === 'daily' ? `每日 ${settings?.codearts_credits_sync_hhmm || '23:58'}` : `每 ${settings?.codearts_credits_sync_hours || '2'} 小时`}
                  </span>
                </div>
                {needsRelogin.length > 0 && (
                  <div className="flex items-center gap-2">
                    <Badge tone="amber">需重新导入</Badge>
                    <span className="min-w-0 flex-1 text-slate-600 dark:text-zinc-300">
                      {needsRelogin.map((a) => a.name).join('、')}——请在码道客户端/插件重新登录后扫描导入
                    </span>
                  </div>
                )}
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
                    .map((r, i) => {
                      const badge = statusBadge(r.status);
                      return (
                        <div key={`${r.date}-${r.user_id}-${r.time_ms}-${i}`} className="flex items-center gap-2 px-4 py-2">
                          <Badge tone={badge.tone}>{badge.text}</Badge>
                          <span className="text-slate-500 dark:text-zinc-400">{r.date} {r.time}</span>
                          <span className="text-slate-700 dark:text-zinc-200">{r.name || r.user_id}</span>
                          <span className="min-w-0 flex-1 truncate text-slate-500 dark:text-zinc-400">{r.message}</span>
                          {typeof r.reward === 'number' && r.reward > 0 && <Badge tone="green">+{r.reward}</Badge>}
                        </div>
                      );
                    })}
                </div>
              )}
            </div>
          </div>

          {/* 账号速览 */}
          {accounts.length > 0 && (
            <div className="mt-4 overflow-hidden rounded-lg border border-slate-200 bg-white dark:border-zinc-800 dark:bg-zinc-900">
              <div className="border-b border-slate-100 px-4 py-3 text-sm font-medium dark:border-zinc-800">账号速览</div>
              <div className="divide-y divide-slate-100 text-sm dark:divide-zinc-800">
                {accounts.map((a) => (
                  <div key={a.id} className="flex flex-wrap items-center gap-2 px-4 py-2.5">
                    <span className="font-medium text-slate-800 dark:text-zinc-100">{a.name}</span>
                    <Badge tone="slate">{SOURCE_LABEL[a.credential_source] || a.credential_source || '—'}</Badge>
                    {a.needs_relogin && <Badge tone="amber">需重新导入</Badge>}
                    {typeof a.token_expires_at_ms === 'number' && a.token_expires_at_ms > 0 && (
                      <span className="text-xs text-slate-400">
                        凭证至 {new Date(a.token_expires_at_ms).toLocaleString()}
                      </span>
                    )}
                    <span className="ml-auto text-slate-600 dark:text-zinc-300">
                      {a.credits_total != null ? <>余额 <b>{a.credits_total.toLocaleString()}</b></> : '余额 —'}
                    </span>
                  </div>
                ))}
              </div>
            </div>
          )}
        </>
      )}
    </div>
  );
}
