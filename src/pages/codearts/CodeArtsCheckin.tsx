import { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { CheckCircle2, Clock, MinusCircle, PlayCircle, XCircle } from 'lucide-react';
import PageHeader from '../../components/PageHeader';
import { Badge, EmptyState, Spinner } from '../../components/ui';
import { api } from '../../lib/tauri';
import { useAppStore } from '../../store';
import type { CodeArtsCheckinLine, CodeArtsCheckinRecord, Settings } from '../../types';

/** CodeArts · 每日签到页（VENU-37 重建）。
 *  手动签到 + 进度事件流 + 近 30 天结果表。
 *  事件：codearts-checkin-progress NDJSON 行（type=start/account/done/exit）。
 *  领取语义：只领 claimable && MANUAL_CLAIM 活动（绝不伪造触发事件）；
 *  40001/4006 幂等中性「已领取」；4004/无活动转 noop 不报错刷屏。 */

function statusTone(status: string): 'green' | 'amber' | 'slate' | 'red' {
  if (status === 'success') return 'green';
  if (status === 'already') return 'amber';
  if (status === 'noop') return 'slate';
  if (status === 'fail') return 'red';
  return 'slate';
}

function statusText(status: string): string {
  if (status === 'success') return '成功';
  if (status === 'already') return '已领取';
  if (status === 'noop') return '无可领取';
  if (status === 'fail') return '失败';
  return status || '—';
}

export default function CodeArtsCheckin() {
  const pushToast = useAppStore((s) => s.pushToast);
  const settings = useAppStore((s) => s.settings);
  const saveSettings = useAppStore((s) => s.saveSettings);
  const [accounts, setAccounts] = useState<{ id: string; name: string; needs_relogin: boolean }[]>([]);
  const [running, setRunning] = useState(false);
  const [lines, setLines] = useState<CodeArtsCheckinLine[]>([]);
  const [done, setDone] = useState<{ ok: number; already: number; noop: number; failed: number } | null>(null);
  const [results, setResults] = useState<CodeArtsCheckinRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const runningRef = useRef(false);

  const refresh = useCallback(async () => {
    try {
      const [list, rec] = await Promise.all([api.codearts.accountsList(), api.codearts.checkinResults(30)]);
      setAccounts(list.map((a) => ({ id: a.id, name: a.name, needs_relogin: a.needs_relogin })));
      setResults(rec);
    } catch (e) {
      pushToast('error', String(e));
    } finally {
      setLoading(false);
    }
  }, [pushToast]);

  useEffect(() => {
    void refresh();
    // NDJSON 事件流归约（页面卸载不丢终态：running 由 exit 行收尾）
    let unlisten: (() => void) | undefined;
    void listen<string>('codearts-checkin-progress', (ev) => {
      let line: CodeArtsCheckinLine;
      try {
        const payload = ev.payload;
        line = (typeof payload === 'string' ? JSON.parse(payload) : payload) as CodeArtsCheckinLine;
      } catch {
        return;
      }
      if (line.type === 'start') {
        runningRef.current = true;
        setRunning(true);
        setLines([]);
        setDone(null);
        return;
      }
      if (line.type === 'account') {
        setLines((prev) => [...prev, line]);
        return;
      }
      if (line.type === 'done') {
        setDone({ ok: line.ok ?? 0, already: line.already ?? 0, noop: line.noop ?? 0, failed: line.failed ?? 0 });
        return;
      }
      if (line.type === 'exit') {
        runningRef.current = false;
        setRunning(false);
        void refresh();
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, [refresh]);

  const start = async () => {
    if (runningRef.current) return;
    try {
      setLines([]);
      setDone(null);
      runningRef.current = true;
      setRunning(true);
      await api.codearts.checkinStart();
    } catch (e) {
      runningRef.current = false;
      setRunning(false);
      pushToast('error', String(e));
    }
  };

  const patchSettings = async (patch: Partial<Settings>) => {
    try {
      await saveSettings(patch);
      pushToast('success', '调度设置已保存');
    } catch {
      /* saveSettings 内部已 toast 并回滚 */
    }
  };

  const today = new Date().toISOString().slice(0, 10);
  const todayResults = results.filter((r) => r.date === today);
  const scheduled = settings?.codearts_checkin_enabled !== false;
  const hhmm = settings?.codearts_checkin_hhmm || '00:20';
  const emptyPool = accounts.length === 0;

  return (
    <div>
      <PageHeader
        title="CodeArts 每日签到"
        desc="领取华为云码道每日活动积分：只领可自动领取的活动，失败原因如实落库"
        actions={
          <button
            onClick={() => void start()}
            disabled={running || emptyPool}
            className="flex h-9 items-center gap-1.5 rounded-lg bg-zinc-900 px-4 text-sm font-medium text-white transition hover:bg-zinc-800 disabled:cursor-not-allowed disabled:opacity-40 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-zinc-200"
          >
            {running ? <Spinner className="h-4 w-4" /> : <PlayCircle size={16} />}
            {running ? '签到中…' : '开始签到'}
          </button>
        }
      />

      {/* 调度状态条 */}
      <div className="mb-4 flex flex-wrap items-center gap-2 rounded-lg border border-slate-200 bg-white px-4 py-2.5 text-sm dark:border-zinc-800 dark:bg-zinc-900">
        <Badge tone={scheduled ? 'green' : 'slate'}>{scheduled ? '自动签到已开启' : '自动签到已关闭'}</Badge>
        <span className="text-slate-500 dark:text-zinc-400">每日 {hhmm} 首跑，失败日内退避重试</span>
        <button
          onClick={() => void patchSettings({ codearts_checkin_enabled: !scheduled })}
          className="ml-auto text-xs text-zinc-600 underline-offset-2 hover:underline dark:text-zinc-400"
        >
          {scheduled ? '关闭自动签到' : '开启自动签到'}
        </button>
      </div>

      {emptyPool && !loading && (
        <div className="mb-4">
          <EmptyState
            title="暂无 CodeArts 账号"
            hint="先到「账号管理」导入本机登录态（或 OAuth / AK/SK），再回到本页签到。"
          />
        </div>
      )}

      {/* 本轮进度 */}
      {(running || lines.length > 0 || done) && (
        <div className="mb-4 rounded-lg border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
          <div className="mb-2 flex items-center gap-2 text-sm font-medium">
            {running && <Spinner className="h-4 w-4" />}
            本轮签到进度
            {done && (
              <span className="ml-auto text-xs text-slate-500 dark:text-zinc-400">
                成功 {done.ok} · 已领取 {done.already} · 无可领取 {done.noop} · 失败 {done.failed}
              </span>
            )}
          </div>
          <div className="space-y-1.5">
            {lines.map((l, i) => (
              <div key={i} className="flex items-start gap-2 text-sm">
                {l.status === 'success' ? (
                  <CheckCircle2 size={15} className="mt-0.5 shrink-0 text-emerald-500" />
                ) : l.status === 'already' ? (
                  <Clock size={15} className="mt-0.5 shrink-0 text-amber-500" />
                ) : l.status === 'noop' ? (
                  <MinusCircle size={15} className="mt-0.5 shrink-0 text-slate-400" />
                ) : (
                  <XCircle size={15} className="mt-0.5 shrink-0 text-rose-500" />
                )}
                <span className="text-slate-700 dark:text-zinc-200">{l.name || l.user_id}</span>
                <span className="min-w-0 flex-1 break-all text-slate-500 dark:text-zinc-400">{l.message}</span>
                {typeof l.reward === 'number' && l.reward > 0 && (
                  <Badge tone="green">+{l.reward}</Badge>
                )}
              </div>
            ))}
            {lines.length === 0 && running && (
              <div className="text-sm text-slate-400">正在排队账号…</div>
            )}
          </div>
        </div>
      )}

      {/* 近 30 天结果 */}
      <div className="rounded-lg border border-slate-200 bg-white dark:border-zinc-800 dark:bg-zinc-900">
        <div className="flex items-center justify-between border-b border-slate-100 px-4 py-3 dark:border-zinc-800">
          <h2 className="text-sm font-medium">近 30 天签到记录（今日 {todayResults.length} 条）</h2>
          <button onClick={() => void refresh()} className="text-xs text-zinc-500 hover:text-zinc-700 dark:text-zinc-400 dark:hover:text-zinc-200">
            刷新
          </button>
        </div>
        {loading ? (
          <div className="flex justify-center py-8"><Spinner className="h-5 w-5" /></div>
        ) : results.length === 0 ? (
          <EmptyState title="暂无签到记录" hint="手动签到或自动调度到点后，结果会按日落库展示在这里。" />
        ) : (
          <div className="divide-y divide-slate-100 dark:divide-zinc-800">
            {results
              .slice()
              .reverse()
              .map((r, i) => (
                <div key={`${r.date}-${r.user_id}-${r.time_ms}-${i}`} className="flex items-start gap-2 px-4 py-2.5 text-sm">
                  <Badge tone={statusTone(r.status)}>{statusText(r.status)}</Badge>
                  <span className="w-36 shrink-0 text-slate-500 dark:text-zinc-400">{r.date} {r.time}</span>
                  <span className="text-slate-700 dark:text-zinc-200">{r.name || r.user_id}</span>
                  <span className="min-w-0 flex-1 break-all text-slate-500 dark:text-zinc-400">{r.message}</span>
                  {typeof r.reward === 'number' && r.reward > 0 && <Badge tone="green">+{r.reward}</Badge>}
                </div>
              ))}
          </div>
        )}
      </div>
    </div>
  );
}
