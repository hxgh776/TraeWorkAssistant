import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { CheckCircle2, Clock, Gift, PlayCircle, RefreshCw, XCircle } from 'lucide-react';
import PageHeader from '../../components/PageHeader';
import { Badge } from '../../components/ui';
import CheckinCalendarCard, { type CheckinCalendarDay } from '../../components/CheckinCalendar';
import { daysFromWbRecords } from '../../components/checkinCalendarModel';
import { api } from '../../lib/tauri';
import { useAppStore } from '../../store';
import { LOCAL_APP_META } from '../../lib/localApps';
import type {
  LocalCheckinDone,
  LocalCheckinLine,
  LocalCheckinRecord,
  LobsterAiQuota,
  LobsterAiStatus,
  Settings,
} from '../../types';

/**
 * lobsterai-checkin 每日签到（VENU-41 风格对齐，对照 OfficeAceCheckin/BuddyCheckin 同构）：
 * 活动说明卡 + 一键签到卡（本机账号行）+ 实时进度（NDJSON 逐行）+ 活动档期日历
 * （checkinCalendarModel 聚合，含单测口径）+ 签到记录表 + 自动签到开关。
 * 能力不回退：本机登录态检测 / 手动签到 / 自动签到开关 / 结果如实展示全部保留
 * （原 LocalCheckinPage 薄壳的本机账号卡与两栏记录由本页同构替换）。
 */

const META = LOCAL_APP_META.lobsterai;

/** 令牌徽标（对齐 Buddy MiniTokenBadge 形态：按剩余时长分色；token_expires_at 为 Unix 秒） */
function TokenBadge({ status }: { status: LobsterAiStatus | null }) {
  if (!status) return <span className="text-xs text-slate-400">读取中…</span>;
  if (!status.installed) return <span className="text-xs text-slate-400"><XCircle size={11} className="inline" /> 未安装</span>;
  if (!status.logged_in) return <span className="text-xs text-rose-500"><XCircle size={11} className="inline" /> 未登录</span>;
  const expMs = status.token_expires_at ? status.token_expires_at * 1000 : null;
  if (status.token_expired)
    return <span className="text-xs text-rose-500"><XCircle size={11} className="inline" /> 已过期</span>;
  if (!expMs) return <span className="text-xs text-slate-400">未知</span>;
  const hours = (expMs - Date.now()) / 3600_000;
  if (hours <= 0) return <span className="text-xs text-rose-500"><XCircle size={11} className="inline" /> 已过期</span>;
  if (hours <= 24) return <span className="text-xs text-amber-500"><Clock size={11} className="inline" /> {hours.toFixed(1)}h</span>;
  return <span className="text-xs text-emerald-500">{hours.toFixed(0)}h</span>;
}

function statusText(status: string): string {
  if (status === 'success') return '成功';
  if (status === 'already') return '已签到';
  if (status === 'fail') return '失败';
  return status || '—';
}

function fmtNum(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return '—';
  return Number.isInteger(value) ? String(value) : value.toFixed(2);
}

function localDateStr(dt: Date): string {
  return `${dt.getFullYear()}-${String(dt.getMonth() + 1).padStart(2, '0')}-${String(dt.getDate()).padStart(2, '0')}`;
}

export default function LobsterAiCheckin() {
  const pushToast = useAppStore((s) => s.pushToast);
  const settings = useAppStore((s) => s.settings);
  const saveSettings = useAppStore((s) => s.saveSettings);
  const [status, setStatus] = useState<LobsterAiStatus | null>(null);
  const [quota, setQuota] = useState<LobsterAiQuota | null>(null);
  const [quotaError, setQuotaError] = useState<string | null>(null);
  const [records, setRecords] = useState<LocalCheckinRecord[]>([]);
  const [running, setRunning] = useState(false);
  const [lines, setLines] = useState<LocalCheckinLine[]>([]);
  const [done, setDone] = useState<LocalCheckinDone | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const aliveRef = useRef(true);
  // 活动档期日历（近 90 天逐账号记录按日聚合，聚合在 checkinCalendarModel.daysFromWbRecords）
  const [calDays, setCalDays] = useState<CheckinCalendarDay[]>([]);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    try {
      // status / records 任一路失败不静默空转：toast 如实报错
      const [stSettled, recSettled, quotaSettled] = await Promise.all([
        api.lobsterai.status().then(
          (s) => ({ ok: true as const, s }),
          (err: unknown) => ({ ok: false as const, err }),
        ),
        api.lobsterai.checkinResults(90).then(
          (r) => ({ ok: true as const, r }),
          (err: unknown) => ({ ok: false as const, err }),
        ),
        // 余额列只读查询：失败不拖垮签到主流程，列显「—」并在按钮左侧如实提示
        api.lobsterai.quota().then(
          (q) => ({ ok: true as const, q }),
          (err: unknown) => ({ ok: false as const, err }),
        ),
      ]);
      if (!aliveRef.current) return;
      if (stSettled.ok) setStatus(stSettled.s);
      else pushToast('error', `登录态读取失败：${String(stSettled.err)}`);
      if (recSettled.ok) {
        setRecords(recSettled.r);
        // daysFromWbRecords 形参 reward 不含 null（WbCheckinRecord 口径）：LocalCheckinRecord
        // 的 null 归一为 undefined 再聚合（聚合内核对两者同按「无奖励」处理）
        setCalDays(daysFromWbRecords(recSettled.r.map((r) => ({ ...r, reward: r.reward ?? undefined }))));
      } else {
        pushToast('error', `签到记录读取失败：${String(recSettled.err)}`);
      }
      if (quotaSettled.ok) {
        setQuota(quotaSettled.q);
        setQuotaError(null);
      } else {
        setQuota(null);
        setQuotaError(String(quotaSettled.err));
      }
    } finally {
      if (aliveRef.current) setRefreshing(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    aliveRef.current = true;
    void refresh();
    return () => {
      aliveRef.current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 签到进度事件（NDJSON 行）：本地状态持有（与原 LocalCheckinPage 同语义，页面切走即停）
  useEffect(() => {
    let disposed = false;
    const unlisten = listen<string>('lobsterai-checkin-progress', (ev) => {
      let parsed: unknown;
      try {
        parsed = JSON.parse(ev.payload);
      } catch {
        return;
      }
      if (!parsed || typeof parsed !== 'object') return;
      const event = parsed as Record<string, unknown>;
      if (event.type === 'start') {
        setRunning(true);
        setLines([]);
        setDone(null);
        return;
      }
      if (event.type === 'done') {
        const num = (v: unknown) => (typeof v === 'number' && Number.isFinite(v) ? v : 0);
        setDone({ ok: num(event.ok), already: num(event.already), failed: num(event.failed) });
        setRunning(false);
        void refresh();
        return;
      }
      if (event.type === 'exit') {
        setRunning(false);
        return;
      }
      if (typeof event.index === 'number' && event.index > 0) {
        const index = event.index;
        const rewardRaw = event.reward;
        const reward =
          typeof rewardRaw === 'number' && Number.isFinite(rewardRaw) ? rewardRaw : undefined;
        setLines((prev) => {
          const next = prev.slice();
          next[index - 1] = {
            index,
            user_id: String(event.user_id ?? ''),
            name: String(event.name ?? ''),
            status: String(event.status ?? 'fail'),
            message: event.message != null ? String(event.message) : undefined,
            reward,
          };
          return next;
        });
      }
    });
    return () => {
      disposed = true;
      void unlisten.then((fn) => {
        if (disposed) fn();
      });
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const start = async () => {
    if (running) return;
    if (status && !status.logged_in) {
      pushToast('warn', '本机账号未登录：请打开 LobsterAI 桌面端登录后再签到');
      return;
    }
    setRunning(true);
    setLines([]);
    setDone(null);
    try {
      await api.lobsterai.checkinStart();
    } catch (err) {
      setRunning(false);
      pushToast('error', `发起签到失败：${String(err)}`);
    }
  };

  const patchSettings = async (patch: Partial<Settings>) => {
    try {
      await saveSettings(patch);
      pushToast('success', '签到设置已保存');
    } catch {
      // saveSettings 内部已 toast 并回滚
    }
  };

  const today = localDateStr(new Date());
  const todayRecords = useMemo(() => records.filter((r) => r.date === today), [records, today]);
  // 今日获得（本机单账号）：非失败记录奖励合计
  const todayEarned = useMemo(
    () => todayRecords.reduce((s, r) => s + (r.status !== 'fail' && r.reward ? r.reward : 0), 0),
    [todayRecords],
  );
  // 签到状态：实时行优先，其次今日记录（任一成功/已签 → 已签；否则最近一条失败）
  const checkinStatusOf = (): 'success' | 'already' | 'fail' | null => {
    const live = lines[0];
    if (live) return live.status as 'success' | 'already' | 'fail';
    if (todayRecords.some((r) => r.status === 'success' || r.status === 'already'))
      return todayRecords.some((r) => r.status === 'success') ? 'success' : 'already';
    return todayRecords.find((r) => r.status === 'fail') ? 'fail' : null;
  };
  const st = checkinStatusOf();
  const enabled = Boolean(settings?.lobsterai_checkin_enabled);
  const hhmm = settings?.lobsterai_checkin_hhmm || META.defaultHhmm;
  // 签到记录表（近 30 天）：90 天记录按时间倒序过滤
  const tableRecords = useMemo(() => {
    const cutoffStr = localDateStr(new Date(Date.now() - 30 * 86400000));
    return [...records]
      .sort((a, b) => (b.time_ms || 0) - (a.time_ms || 0))
      .filter((r) => r.date >= cutoffStr);
  }, [records]);
  const earned = Math.round(lines.reduce((s, l) => s + (l.reward ?? 0), 0) * 100) / 100;

  return (
    <div className="animate-fade-in">
      <PageHeader
        title="LobsterAI · 每日签到"
        desc="复用本机 LobsterAI 桌面端登录态完成每日签到领龙虾积分（只读登录态，不保存凭据）"
      />

      {/* 活动说明卡（对齐 Qoder 双活动说明卡形态；LobsterAI 为签到活动单活动） */}
      <div className="card p-4">
        <div className="mb-3 flex items-center gap-2">
          <Gift size={16} className="text-violet-500" />
          <span className="text-sm font-medium">签到说明（龙虾积分领取）</span>
        </div>
        <div className="grid gap-3 lg:grid-cols-2">
          <div className="rounded-lg border border-slate-100 p-3 text-sm dark:border-zinc-800">
            <div className="font-medium">每日签到奖励</div>
            <div className="mt-1 text-xs text-slate-400">
              签到流程：查活动槽 → 查活动状态 → 提交 check_in → 复核到账；上游幂等：当天已领取重试不会重复领
            </div>
          </div>
          <div className="rounded-lg border border-slate-100 p-3 text-sm dark:border-zinc-800">
            <div className="font-medium">凭证要求</div>
            <div className="mt-1 text-xs text-slate-400">
              令牌由桌面端自动续期：登录态过期时请打开 LobsterAI 客户端后重试（过期会如实提示，不静默重试）
            </div>
          </div>
        </div>
        <p className="mt-3 text-xs text-slate-400">
          调度默认每天 {META.defaultHhmm} 自动执行一次（时刻可在环境配置修改）；错过时刻会在下次启动 75
          秒后自动补签。
        </p>
      </div>

      {/* 一键签到卡（对齐 Buddy/Qoder：账号列表表格 + 按钮右下角） */}
      <div className="mt-4 card p-4">
        <div className="mb-3 flex items-center justify-between">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium">一键签到</span>
            {running && <Badge tone="blue">执行中</Badge>}
          </div>
          {!running && <span className="text-xs text-slate-400">本机单账号 · 上游幂等（已领不重复领）</span>}
        </div>
        <div className="rounded-lg border border-slate-200 dark:border-zinc-700">
          <table className="w-full text-sm">
            <thead className="bg-slate-50 text-xs text-slate-500 dark:bg-zinc-900">
              <tr>
                <th className="px-3 py-1.5 text-left">账号</th>
                <th className="px-3 py-1.5 text-left">登录态</th>
                <th className="px-3 py-1.5 text-left">签到状态</th>
                <th className="px-3 py-1.5 text-right">今日获得</th>
                <th className="px-3 py-1.5 text-right">可用总积分</th>
              </tr>
            </thead>
            <tbody>
              {!status || !status.installed || !status.logged_in ? (
                <tr>
                  <td colSpan={5} className="px-3 py-4 text-center text-xs text-slate-400">
                    {status
                      ? status.installed
                        ? status.error ?? '本机账号未登录：请打开 LobsterAI 桌面端登录后「刷新」'
                        : '未检测到 LobsterAI 桌面端：请先安装并登录'
                      : '读取中…'}
                  </td>
                </tr>
              ) : (
                <tr className="border-t border-slate-100 dark:border-zinc-800">
                  <td className="px-3 py-1.5">
                    <div className="font-medium">{status.name || '本机账号'}</div>
                    <div className="text-xs text-slate-400">{status.user_id || '未知 ID'}</div>
                  </td>
                  <td className="px-3 py-1.5"><TokenBadge status={status} /></td>
                  <td className="px-3 py-1.5">
                    {st == null ? (
                      <Badge tone="slate">未签</Badge>
                    ) : st === 'success' ? (
                      <Badge tone="green">已领</Badge>
                    ) : st === 'already' ? (
                      <Badge tone="blue">已领（此前已领）</Badge>
                    ) : (
                      <Badge tone="red">失败</Badge>
                    )}
                  </td>
                  <td className="px-3 py-1.5 text-right tabular-nums text-xs">
                    {todayEarned > 0 ? (
                      <span className="text-emerald-600 dark:text-emerald-400">+{fmtNum(todayEarned)}</span>
                    ) : (
                      <span className="text-slate-300 dark:text-zinc-600">—</span>
                    )}
                  </td>
                  <td
                    className="px-3 py-1.5 text-right tabular-nums text-xs"
                    title={quotaError ? `余额查询失败：${quotaError}` : quota && quota.balance == null ? '上游未下发余额字段' : undefined}
                  >
                    {quota ? (
                      quota.balance != null ? (
                        fmtNum(quota.balance)
                      ) : (
                        <span className="text-slate-400">未知</span>
                      )
                    ) : (
                      <span className="text-slate-300 dark:text-zinc-600" title={quotaError ?? undefined}>—</span>
                    )}
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
        <div className="mt-3 flex items-center justify-between">
          <span className="text-xs text-slate-400">
            {running
              ? '签到进行中，结果见下方实时进度…'
              : quotaError
                ? `积分余额查询失败：${quotaError}`
                : '签到结果将展示在下方实时进度卡'}
          </span>
          <div className="flex items-center gap-2">
            <button className="btn-outline" onClick={() => void refresh()} disabled={refreshing}>
              <RefreshCw size={15} className={refreshing ? 'animate-spin' : ''} /> 刷新
            </button>
            <button className="btn-outline" onClick={() => void start()} disabled={running}>
              <PlayCircle size={15} /> {running ? '签到中…' : '开始签到'}
            </button>
          </div>
        </div>
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
                完成：成功 {done.ok} · 已签 {done.already} · 失败 {done.failed}
                {earned > 0 && ` · 获得 ${fmtNum(earned)} 积分`}
              </Badge>
            ) : null}
          </div>
          <div className="space-y-1">
            {/* 过滤稀疏数组空洞：乱序事件按 index 跳写产生 hole，直接 map 会在 hole 上取 status 崩溃 */}
            {lines
              .filter((l) => l && l.user_id)
              .map((l) => {
                const tone =
                  l.status === 'success'
                    ? 'text-emerald-600 dark:text-emerald-300'
                    : l.status === 'already'
                      ? 'text-sky-600 dark:text-sky-300'
                      : 'text-rose-600 dark:text-rose-300';
                const Icon = l.status === 'fail' ? XCircle : CheckCircle2;
                return (
                  <div key={l.index} className="flex items-center gap-2 rounded border border-slate-200 px-3 py-2 text-sm dark:border-zinc-700">
                    <Icon size={14} className={tone} />
                    <span className="w-8 text-right text-xs text-slate-400">{l.index}</span>
                    <span className="flex-1 truncate">{l.name || l.user_id}</span>
                    <span className={`max-w-[50%] truncate text-xs ${tone}`} title={l.message}>
                      {l.message || l.status}
                    </span>
                    {l.reward != null && (
                      <span className="shrink-0 rounded bg-emerald-50 px-1.5 py-0.5 text-xs font-medium text-emerald-600 dark:bg-emerald-500/10 dark:text-emerald-300">
                        获取积分 +{l.reward}
                      </span>
                    )}
                  </div>
                );
              })}
          </div>
        </div>
      )}

      {/* 活动档期日历（对齐 Buddy/Qoder「每日签到」）：近 90 天签到结果按日可视化 */}
      <CheckinCalendarCard
        title="活动档期日历"
        days={calDays}
        footnote="档期：每日定时任务到点自动签到（时刻见环境配置） · 启动补签当日已过时刻 · 令牌由桌面端自动续期"
      />

      {/* 签到记录表（对齐 MiniMax/CodeArts 记录表形态：表格列 + 状态徽标 + 奖励） */}
      <div className="mt-4 card p-4">
        <div className="mb-3 flex items-center justify-between">
          <h3 className="font-medium">签到记录（近 30 天）</h3>
          <span className="text-xs text-slate-400">共 {tableRecords.length} 条 · 按时间倒序</span>
        </div>
        <div className="rounded-lg border border-slate-200 dark:border-zinc-700">
          <table className="w-full text-sm">
            <thead className="bg-slate-50 text-xs text-slate-500 dark:bg-zinc-900">
              <tr>
                <th className="px-3 py-1.5 text-left">时间</th>
                <th className="px-3 py-1.5 text-left">账号</th>
                <th className="px-3 py-1.5 text-left">状态</th>
                <th className="px-3 py-1.5 text-right">奖励</th>
                <th className="px-3 py-1.5 text-left">说明</th>
              </tr>
            </thead>
            <tbody>
              {tableRecords.length === 0 ? (
                <tr>
                  <td colSpan={5} className="px-3 py-4 text-center text-xs text-slate-400">
                    暂无记录
                  </td>
                </tr>
              ) : (
                tableRecords.map((record) => (
                  <tr
                    key={`${record.date}|${record.user_id}|${record.time_ms}`}
                    className="border-t border-slate-100 dark:border-zinc-800"
                  >
                    <td className="whitespace-nowrap px-3 py-1.5 text-xs text-slate-500 dark:text-zinc-400">
                      {record.date} {record.time}
                    </td>
                    <td className="px-3 py-1.5 text-xs">{record.name || record.user_id || '—'}</td>
                    <td className="px-3 py-1.5">
                      {record.status === 'success' ? (
                        <Badge tone="green">成功</Badge>
                      ) : record.status === 'already' ? (
                        <Badge tone="blue">已签到</Badge>
                      ) : record.status === 'fail' ? (
                        <Badge tone="red">失败</Badge>
                      ) : (
                        <Badge tone="slate">{statusText(record.status)}</Badge>
                      )}
                    </td>
                    <td className="px-3 py-1.5 text-right tabular-nums text-xs">
                      {record.reward != null && record.reward > 0 && record.status !== 'fail' ? (
                        <span className="text-emerald-600 dark:text-emerald-400">+{fmtNum(record.reward)}</span>
                      ) : (
                        <span className="text-slate-300 dark:text-zinc-600">—</span>
                      )}
                    </td>
                    <td className="max-w-[320px] px-3 py-1.5">
                      <span className="block truncate text-xs text-slate-500 dark:text-zinc-400" title={record.message}>
                        {record.message || '—'}
                      </span>
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </div>

      {/* 自动签到开关（能力保留：原 LocalCheckinPage 同款，时刻在环境配置可改） */}
      <div className="mt-4 rounded-xl border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
        <div className="mb-3 text-sm font-medium text-slate-700 dark:text-zinc-200">自动签到</div>
        <div className="flex flex-wrap items-center gap-4 text-sm text-slate-600 dark:text-zinc-300">
          <label className="inline-flex items-center gap-2">
            <input
              type="checkbox"
              checked={enabled}
              onChange={(e) => void patchSettings({ lobsterai_checkin_enabled: e.target.checked })}
            />
            启用每日自动签到（应用启动补签 + 定时调度）
          </label>
          <label className="inline-flex items-center gap-2">
            触发时刻
            <input
              className="w-20 rounded-md border border-slate-200 px-2 py-1 text-sm dark:border-zinc-700 dark:bg-zinc-800"
              defaultValue={hhmm}
              onBlur={(e) => {
                const value = e.target.value.trim();
                if (value && value !== hhmm) {
                  void patchSettings({ lobsterai_checkin_hhmm: value });
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
