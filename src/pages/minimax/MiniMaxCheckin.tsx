import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { CheckCircle2, Clock, Gift, PlayCircle, RefreshCw, XCircle } from 'lucide-react';
import PageHeader from '../../components/PageHeader';
import { Badge, EmptyState } from '../../components/ui';
import CheckinCalendarCard, { type CheckinCalendarDay } from '../../components/CheckinCalendar';
import { daysFromWbRecords } from '../../components/checkinCalendarModel';
import { api } from '../../lib/tauri';
import { useAppStore } from '../../store';
import type {
  MinimaxAccountView,
  MinimaxCheckinLine,
  MinimaxCheckinRecord,
  MinimaxRegion,
  Settings,
} from '../../types';

/**
 * minimax-checkin 每日签到（VENU-41 风格对齐，对照 BuddyCheckin/QoderCheckin 同构）：
 * 双区说明卡 + 一键签到卡（账号池表格 + 区域筛选）+ 实时进度（NDJSON 逐行）
 * + 活动档期日历（checkinCalendarModel 聚合，含单测口径）+ 签到记录表 + 自动签到开关。
 * 能力不回退：区域筛选签到、claim_result=2 幂等已领取、失败原因如实落库、
 * 自动签到开关与时刻全部保留（事件：minimax-checkin-progress NDJSON 行
 * type=start/account/done/exit，进度行为追加语义，exit 行收尾并刷新）。
 */

const REGION_LABEL: Record<MinimaxRegion, string> = {
  minimax: '国内区',
  'minimax-global': '国际区',
};

function statusTone(status: string): 'green' | 'amber' | 'red' | 'slate' {
  if (status === 'success') return 'green';
  if (status === 'already') return 'amber';
  if (status === 'fail') return 'red';
  return 'slate';
}

function statusText(status: string): string {
  if (status === 'success') return '成功';
  if (status === 'already') return '已领取';
  if (status === 'fail') return '失败';
  return status || '—';
}

/** 登录态徽标（对齐 Buddy MiniTokenBadge 形态：needs_relogin 优先，其次按 token 剩余时长分色） */
function MiniTokenBadge({ a }: { a: MinimaxAccountView }) {
  if (a.needs_relogin)
    return <span className="text-xs text-rose-500"><XCircle size={11} className="inline" /> 需重新登录</span>;
  const exp = a.token_expires_at_ms;
  if (!exp || exp <= 0) return <span className="text-xs text-slate-400">未知</span>;
  const hours = (exp - Date.now()) / 3600_000;
  if (hours <= 0) return <span className="text-xs text-rose-500"><XCircle size={11} className="inline" /> 已过期</span>;
  if (hours <= 24) return <span className="text-xs text-amber-500"><Clock size={11} className="inline" /> {hours.toFixed(1)}h</span>;
  return <span className="text-xs text-emerald-500">{hours.toFixed(0)}h</span>;
}

export default function MiniMaxCheckin() {
  const pushToast = useAppStore((s) => s.pushToast);
  const settings = useAppStore((s) => s.settings);
  const saveSettings = useAppStore((s) => s.saveSettings);
  const [accounts, setAccounts] = useState<MinimaxAccountView[]>([]);
  const [regionFilter, setRegionFilter] = useState<'' | MinimaxRegion>('');
  const [running, setRunning] = useState(false);
  const [lines, setLines] = useState<MinimaxCheckinLine[]>([]);
  const [done, setDone] = useState<{ ok: number; already: number; failed: number } | null>(null);
  const [records, setRecords] = useState<MinimaxCheckinRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const runningRef = useRef(false);
  // 活动档期日历（近 90 天逐账号记录按日聚合，聚合在 checkinCalendarModel.daysFromWbRecords）
  const [calDays, setCalDays] = useState<CheckinCalendarDay[]>([]);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    try {
      const [list, rec] = await Promise.all([api.minimax.accountsList(), api.minimax.checkinResults(90)]);
      setAccounts(list);
      setRecords(rec);
      // daysFromWbRecords 形参 reward 不含 null：null 归一为 undefined 再聚合
      setCalDays(daysFromWbRecords(rec.map((r) => ({ ...r, reward: r.reward ?? undefined }))));
    } catch (e) {
      pushToast('error', String(e));
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
  }, [pushToast]);

  useEffect(() => {
    void refresh();
    // NDJSON 事件流归约（页面卸载不丢终态：running 由 exit 行收尾）
    let unlisten: (() => void) | undefined;
    void listen<string>('minimax-checkin-progress', (ev) => {
      let line: MinimaxCheckinLine;
      try {
        const payload = ev.payload;
        line = (typeof payload === 'string' ? JSON.parse(payload) : payload) as MinimaxCheckinLine;
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
        setDone({ ok: line.ok ?? 0, already: line.already ?? 0, failed: line.failed ?? 0 });
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refresh]);

  const start = async () => {
    if (runningRef.current) return;
    try {
      setLines([]);
      setDone(null);
      runningRef.current = true;
      setRunning(true);
      await api.minimax.checkinStart(regionFilter === '' ? undefined : regionFilter);
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
  const todayResults = useMemo(() => records.filter((r) => r.date === today), [records, today]);
  // 调度开关沿用原口径：settings 缺省视为开启（!== false）
  const scheduled = settings?.minimax_checkin_enabled !== false;
  const hhmm = settings?.minimax_checkin_hhmm || '00:05';
  const emptyPool = accounts.length === 0;

  // 实时结果按 账号(user_id+region) 索引（运行中覆盖列表列展示）
  const liveByAcc = useMemo(() => {
    const m = new Map<string, MinimaxCheckinLine>();
    for (const l of lines) m.set(`${l.user_id}|${l.region ?? ''}`, l);
    return m;
  }, [lines]);
  // 今日签到状态：实时行优先，其次今日记录（任一成功/已领取 → 已领；否则最近一条失败）
  const checkinStatusOf = (a: MinimaxAccountView): 'success' | 'already' | 'fail' | null => {
    const live = liveByAcc.get(`${a.user_id}|${a.region}`);
    if (live?.status) return live.status as 'success' | 'already' | 'fail';
    const recs = todayResults.filter((r) => r.user_id === a.user_id && r.region === a.region);
    if (recs.some((r) => r.status === 'success' || r.status === 'already'))
      return recs.some((r) => r.status === 'success') ? 'success' : 'already';
    return recs.find((r) => r.status === 'fail') ? 'fail' : null;
  };
  // 今日累计获得：成功记录求和；无成功记录取最近一条含奖励记录（recs 新→旧序，find 即最新）
  const todayEarnedOf = (a: MinimaxAccountView): number | null => {
    const recs = todayResults.filter((r) => r.user_id === a.user_id && r.region === a.region);
    const sum = recs.reduce((s, r) => s + (r.status === 'success' && r.reward != null ? r.reward : 0), 0);
    if (sum > 0) return sum;
    return recs.find((r) => r.reward != null)?.reward ?? null;
  };
  // 签到记录表（近 30 天）：90 天记录按时间倒序过滤
  const tableRecords = useMemo(() => {
    const cutoff = new Date();
    cutoff.setFullYear(cutoff.getFullYear(), cutoff.getMonth(), cutoff.getDate() - 30);
    const cutoffStr = `${cutoff.getFullYear()}-${String(cutoff.getMonth() + 1).padStart(2, '0')}-${String(cutoff.getDate()).padStart(2, '0')}`;
    return [...records]
      .sort((a, b) => (b.time_ms || 0) - (a.time_ms || 0))
      .filter((r) => r.date >= cutoffStr);
  }, [records]);
  const earned = Math.round(lines.reduce((s, l) => s + (l.reward ?? 0), 0) * 100) / 100;

  return (
    <div className="animate-fade-in">
      <PageHeader
        title="MiniMax · 每日签到"
        desc="双区账号领取每日积分：claim_result=2 幂等视为已领取，失败原因如实落库"
      />

      {/* 双区说明卡（对齐 Qoder 双活动说明卡形态；MiniMax 为国内/国际双区） */}
      <div className="card p-4">
        <div className="mb-3 flex items-center gap-2">
          <Gift size={16} className="text-violet-500" />
          <span className="text-sm font-medium">签到说明（双区一次执行，按筛选收窄）</span>
        </div>
        <div className="grid gap-3 lg:grid-cols-2">
          <div className="rounded-lg border border-slate-100 p-3 text-sm dark:border-zinc-800">
            <div className="font-medium">国内区（agent.minimax.cn）</div>
            <div className="mt-1 text-xs text-slate-400">
              每日 00:05 首跑（时刻见环境配置）；claim_result=2 幂等视为已领取，失败日内退避重试
            </div>
          </div>
          <div className="rounded-lg border border-slate-100 p-3 text-sm dark:border-zinc-800">
            <div className="font-medium">国际区（agent.minimax.io）</div>
            <div className="mt-1 text-xs text-slate-400">
              大陆网络直连可能被拦截，需代理兜底；oauth_device 凭证链可自动刷新，authjson 为只读导入
            </div>
          </div>
        </div>
        <p className="mt-3 text-xs text-slate-400">
          可用「区域筛选」只跑指定区；已领取账号自动跳过，重复执行不会重复领取。
        </p>
      </div>

      {/* 一键签到卡（对齐 Buddy/Qoder：账号列表表格 + 按钮右下角） */}
      <div className="mt-4 card p-4">
        <div className="mb-3 flex items-center justify-between">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium">一键签到</span>
            {running && <Badge tone="blue">执行中</Badge>}
          </div>
          {accounts.length > 0 && !running && (
            <span className="text-xs text-slate-400">已领取账号自动跳过（幂等）</span>
          )}
        </div>

        {/* 参与签到的账号池列表（账号 / 区域 / 登录态 / 签到状态 / 今日获得 / 可用总积分） */}
        <div className="rounded-lg border border-slate-200 dark:border-zinc-700">
          <table className="w-full text-sm">
            <thead className="bg-slate-50 text-xs text-slate-500 dark:bg-zinc-900">
              <tr>
                <th className="px-3 py-1.5 text-left">账号</th>
                <th className="px-3 py-1.5 text-left">区域</th>
                <th className="px-3 py-1.5 text-left">登录态</th>
                <th className="px-3 py-1.5 text-left">签到状态</th>
                <th className="px-3 py-1.5 text-right">今日获得</th>
                <th className="px-3 py-1.5 text-right">可用总积分</th>
              </tr>
            </thead>
            <tbody>
              {accounts.length === 0 ? (
                <tr>
                  <td colSpan={6} className="px-3 py-4 text-center text-xs text-slate-400">
                    {loading
                      ? '读取中…'
                      : '暂无账号：先到「账号管理」完成设备码授权或导入本机 MiniMax Code 登录态'}
                  </td>
                </tr>
              ) : (
                accounts.map((a) => {
                  const st = checkinStatusOf(a);
                  const earn = lines.find(
                    (l) => l.user_id === a.user_id && l.region === a.region,
                  )?.reward ?? todayEarnedOf(a);
                  return (
                    <tr
                      key={a.id}
                      className={
                        regionFilter && a.region !== regionFilter
                          ? 'border-t border-slate-100 opacity-40 dark:border-zinc-800'
                          : 'border-t border-slate-100 dark:border-zinc-800'
                      }
                    >
                      <td className="px-3 py-1.5">
                        <div className="font-medium">{a.name || a.user_id}</div>
                        <div className="text-xs text-slate-400">{a.email || a.user_id}</div>
                      </td>
                      <td className="px-3 py-1.5">
                        <Badge tone={a.region === 'minimax' ? 'blue' : 'violet'}>
                          {REGION_LABEL[a.region] || a.region}
                        </Badge>
                      </td>
                      <td className="px-3 py-1.5"><MiniTokenBadge a={a} /></td>
                      <td className="px-3 py-1.5">
                        {st == null ? (
                          <Badge tone="slate">未签</Badge>
                        ) : st === 'success' ? (
                          <Badge tone="green">已领</Badge>
                        ) : st === 'already' ? (
                          <Badge tone="blue">已领取（此前已领）</Badge>
                        ) : (
                          <Badge tone="red">失败</Badge>
                        )}
                      </td>
                      <td className="px-3 py-1.5 text-right tabular-nums text-xs">
                        {earn != null && earn > 0 ? (
                          <span className="text-emerald-600 dark:text-emerald-400">+{earn}</span>
                        ) : (
                          <span className="text-slate-300 dark:text-zinc-600">—</span>
                        )}
                      </td>
                      <td className="px-3 py-1.5 text-right tabular-nums text-xs">
                        {a.credits_total != null ? a.credits_total.toLocaleString() : '—'}
                      </td>
                    </tr>
                  );
                })
              )}
            </tbody>
          </table>
        </div>

        {/* 操作区：区域筛选 + 刷新/开始按钮右下角（对齐 Buddy） */}
        <div className="mt-3 flex items-center justify-between">
          <span className="text-xs text-slate-400">
            {running
              ? '签到进行中，逐账号结果见下方实时进度…'
              : '签到结果将展示在下方实时进度卡'}
          </span>
          <div className="flex items-center gap-2">
            <select
              value={regionFilter}
              onChange={(e) => setRegionFilter(e.target.value as '' | MinimaxRegion)}
              disabled={running}
              className="h-8 rounded-lg border border-slate-200 bg-white px-2 text-sm dark:border-zinc-700 dark:bg-zinc-900"
            >
              <option value="">全部区域</option>
              <option value="minimax">国内区</option>
              <option value="minimax-global">国际区</option>
            </select>
            <button className="btn-outline" onClick={() => void refresh()} disabled={refreshing}>
              <RefreshCw size={15} className={refreshing ? 'animate-spin' : ''} /> 刷新
            </button>
            <button className="btn-outline" onClick={() => void start()} disabled={running || emptyPool}>
              <PlayCircle size={15} /> {running ? '签到中…' : '开始签到'}
            </button>
          </div>
        </div>
        {emptyPool && !loading && (
          <div className="mt-3">
            <EmptyState
              title="暂无 MiniMax 账号"
              hint="先到「账号管理」完成设备码授权或导入本机 MiniMax Code 登录态，再回到本页签到。"
            />
          </div>
        )}
      </div>

      {/* 实时进度卡（对齐 Buddy：标题 + 汇总徽标 + 逐账号 NDJSON 结果行；结束后保留避免提示一闪而过） */}
      {(running || lines.length > 0) && (
        <div className="mt-4 card p-4">
          <div className="mb-3 flex items-center justify-between">
            <h3 className="font-medium">实时进度</h3>
            {running ? (
              <Badge tone="blue">运行中</Badge>
            ) : done ? (
              <Badge tone={done.failed > 0 ? 'amber' : 'green'}>
                完成：成功 {done.ok} · 已领取 {done.already} · 失败 {done.failed}
                {earned > 0 && ` · 获得 ${earned} 积分`}
              </Badge>
            ) : null}
          </div>
          <div className="space-y-1">
            {lines.map((l, i) => {
              const tone =
                l.status === 'success'
                  ? 'text-emerald-600 dark:text-emerald-300'
                  : l.status === 'already'
                    ? 'text-sky-600 dark:text-sky-300'
                    : 'text-rose-600 dark:text-rose-300';
              const Icon = l.status === 'fail' ? XCircle : CheckCircle2;
              return (
                <div key={i} className="flex items-center gap-2 rounded border border-slate-200 px-3 py-2 text-sm dark:border-zinc-700">
                  <Icon size={14} className={tone} />
                  <span className="flex-1 truncate">{l.name || l.user_id}</span>
                  {l.region && <Badge tone="slate">{REGION_LABEL[l.region] || l.region}</Badge>}
                  <span className={`max-w-[45%] truncate text-xs ${tone}`} title={l.message}>
                    {l.message || l.status}
                  </span>
                  {typeof l.reward === 'number' && l.reward > 0 && (
                    <span className="shrink-0 rounded bg-emerald-50 px-1.5 py-0.5 text-xs font-medium text-emerald-600 dark:bg-emerald-500/10 dark:text-emerald-300">
                      获取积分 +{l.reward}
                    </span>
                  )}
                </div>
              );
            })}
            {lines.length === 0 && running && (
              <div className="text-sm text-slate-400">正在排队账号…</div>
            )}
          </div>
        </div>
      )}

      {/* 活动档期日历（对齐 Buddy/Qoder「每日签到」）：近 90 天签到结果按日可视化 */}
      <CheckinCalendarCard
        title="活动档期日历"
        days={calDays}
        footnote="档期：每日 00:05 首跑（时刻见环境配置） · 失败日内退避重试 · 国内区/国际区日格状态点独立"
      />

      {/* 签到记录表（近 30 天，保留原自研表格的全部口径：时间/账号/区域/状态/奖励/说明） */}
      <div className="mt-4 card p-4">
        <div className="mb-3 flex items-center justify-between">
          <h3 className="font-medium">签到记录（近 30 天）</h3>
          <span className="text-xs text-slate-400">
            共 {tableRecords.length} 条 · 今日 {todayResults.length} 条 · 按时间倒序
          </span>
        </div>
        <div className="rounded-lg border border-slate-200 dark:border-zinc-700">
          <table className="w-full text-sm">
            <thead className="bg-slate-50 text-xs text-slate-500 dark:bg-zinc-900">
              <tr>
                <th className="px-3 py-1.5 text-left">时间</th>
                <th className="px-3 py-1.5 text-left">账号</th>
                <th className="px-3 py-1.5 text-left">区域</th>
                <th className="px-3 py-1.5 text-left">状态</th>
                <th className="px-3 py-1.5 text-right">奖励</th>
                <th className="px-3 py-1.5 text-left">说明</th>
              </tr>
            </thead>
            <tbody>
              {tableRecords.length === 0 ? (
                <tr>
                  <td colSpan={6} className="px-3 py-4 text-center text-xs text-slate-400">
                    {loading ? '读取中…' : '暂无记录：手动签到或自动调度到点后，结果会按日落库展示在这里。'}
                  </td>
                </tr>
              ) : (
                tableRecords.map((r, i) => (
                  <tr key={`${r.date}-${r.user_id}-${r.time_ms}-${i}`} className="border-t border-slate-100 dark:border-zinc-800">
                    <td className="whitespace-nowrap px-3 py-1.5 text-xs text-slate-500 dark:text-zinc-400">
                      {r.date} {r.time}
                    </td>
                    <td className="px-3 py-1.5 text-xs">{r.name || r.user_id || '—'}</td>
                    <td className="px-3 py-1.5">
                      <Badge tone="slate">{REGION_LABEL[r.region] || r.region}</Badge>
                    </td>
                    <td className="px-3 py-1.5">
                      <Badge tone={statusTone(r.status)}>{statusText(r.status)}</Badge>
                    </td>
                    <td className="px-3 py-1.5 text-right tabular-nums text-xs">
                      {typeof r.reward === 'number' && r.reward > 0 ? (
                        <span className="text-emerald-600 dark:text-emerald-400">+{r.reward}</span>
                      ) : (
                        <span className="text-slate-300 dark:text-zinc-600">—</span>
                      )}
                    </td>
                    <td className="max-w-[320px] px-3 py-1.5">
                      <span className="block truncate text-xs text-slate-500 dark:text-zinc-400" title={r.message}>
                        {r.message || '—'}
                      </span>
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </div>

      {/* 自动签到开关（能力保留：原调度状态条同款开关，时刻在环境配置可改） */}
      <div className="mt-4 rounded-xl border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
        <div className="mb-3 text-sm font-medium text-slate-700 dark:text-zinc-200">自动签到</div>
        <div className="flex flex-wrap items-center gap-4 text-sm text-slate-600 dark:text-zinc-300">
          <label className="inline-flex items-center gap-2">
            <input
              type="checkbox"
              checked={scheduled}
              onChange={(e) => void patchSettings({ minimax_checkin_enabled: e.target.checked })}
            />
            启用每日自动签到（每日首跑 + 失败日内退避重试）
          </label>
          <label className="inline-flex items-center gap-2">
            触发时刻
            <input
              className="w-20 rounded-md border border-slate-200 px-2 py-1 text-sm dark:border-zinc-700 dark:bg-zinc-800"
              defaultValue={hhmm}
              onBlur={(e) => {
                const value = e.target.value.trim();
                if (value && value !== hhmm) {
                  void patchSettings({ minimax_checkin_hhmm: value });
                }
              }}
              placeholder="HH:MM"
            />
          </label>
        </div>
      </div>
    </div>
  );
}
