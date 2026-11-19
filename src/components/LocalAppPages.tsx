import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Bar, BarChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts';
import { CheckCircle2, Clock, ExternalLink, KeyRound, RefreshCw, XCircle } from 'lucide-react';
import { Badge, StatCard } from './ui';
import PageHeader from './PageHeader';
import { useAppStore } from '../store';
import {
  LOCAL_APP_META,
  type LocalCheckinSettingsPatch,
  type LocalPlatform,
} from '../lib/localApps';
import type {
  LocalCheckinRecord,
  LobsterAiQuota,
  LobsterAiStatus,
  OfficeAceQuota,
  OfficeAceStatus,
} from '../types';

// OfficeAce / LobsterAI 五页子导航的同构渲染体（概述 / 账号管理 / 积分看板 / 环境配置；
// 每日签到在 LocalCheckinPage）。页面只做「本机登录态只读」展示：账号、凭证、积分、
// 签到开关全部来自 *_status / *_quota / *_checkin_results 与 settings，不新增写通道。

type LocalStatus = OfficeAceStatus | LobsterAiStatus;
type LocalQuota = OfficeAceQuota | LobsterAiQuota;

function isOfficeAceStatus(status: LocalStatus): status is OfficeAceStatus {
  return 'credential_fresh' in status;
}

function isOfficeAceQuota(quota: LocalQuota): quota is OfficeAceQuota {
  return 'items' in quota;
}

function statusTone(status: string): 'green' | 'amber' | 'red' | 'slate' {
  if (status === 'success') return 'green';
  if (status === 'already') return 'amber';
  if (status === 'fail') return 'red';
  return 'slate';
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

function formatExpiry(ms: number | null | undefined): string {
  if (!ms || ms <= 0) return '未知';
  const date = new Date(ms);
  const diff = ms - Date.now();
  if (diff <= 0) return `已过期（${date.toLocaleString()}）`;
  const hours = Math.floor(diff / 3_600_000);
  const minutes = Math.floor((diff % 3_600_000) / 60_000);
  return `${hours}h${minutes}m 后到期（${date.toLocaleString()}）`;
}

function localDateStr(dt: Date): string {
  return `${dt.getFullYear()}-${String(dt.getMonth() + 1).padStart(2, '0')}-${String(dt.getDate()).padStart(2, '0')}`;
}

/** 状态行的统一展示口径：未安装/未登录时如实回落，不用占位假数据 */
function statusBadge(status: LocalStatus | null) {
  if (!status) return <Badge tone="slate">读取中…</Badge>;
  if (!status.installed) return <Badge tone="slate">未安装</Badge>;
  if (!status.logged_in) return <Badge tone="red">未登录</Badge>;
  return <Badge tone="green">已登录</Badge>;
}

// ── 概述 ────────────────────────────────────────────────────────────────────

export function LocalOverviewPage({ platform }: { platform: LocalPlatform }) {
  const meta = LOCAL_APP_META[platform];
  const setView = useAppStore((s) => s.setView);
  const settings = useAppStore((s) => s.settings);
  const [status, setStatus] = useState<LocalStatus | null>(null);
  const [records, setRecords] = useState<LocalCheckinRecord[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const aliveRef = useRef(true);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    try {
      const [nextStatus, nextRecords] = await Promise.all([meta.status(), meta.results(30)]);
      if (!aliveRef.current) return;
      setStatus(nextStatus);
      setRecords(nextRecords);
      setLoadError(null);
    } catch (err) {
      // 概述页容错：单条命令失败不整页白屏，横幅如实报错、卡片显示「—」
      if (aliveRef.current) setLoadError(String(err));
    } finally {
      if (aliveRef.current) setRefreshing(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [platform]);

  useEffect(() => {
    aliveRef.current = true;
    void refresh();
    return () => {
      aliveRef.current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [platform]);

  const latest = useMemo(
    () => [...records].sort((a, b) => (b.time_ms || 0) - (a.time_ms || 0))[0],
    [records],
  );
  const enabled = Boolean(settings?.[meta.enabledKey]);
  const hhmm = (settings?.[meta.hhmmKey] as string | undefined) ?? meta.defaultHhmm;

  const credentialLine = (() => {
    if (!status) return '读取中…';
    if (!status.installed || !status.logged_in) return status.error ?? '—';
    if (isOfficeAceStatus(status)) {
      return `临时凭证${status.credential_fresh ? '有效' : '已过期'}，${formatExpiry(status.credential_expires_at)}`;
    }
    return `登录态${status.token_expired ? '已过期' : '有效'}，${formatExpiry(status.token_expires_at ? status.token_expires_at * 1000 : null)}`;
  })();

  return (
    <div className="animate-fade-in">
      <PageHeader
        title={`${meta.name} · 概述`}
        desc={`${meta.fullName}本机登录态运行总览：安装 / 登录 / 凭证 / 最近签到 / 自动签到`}
        actions={
          <button onClick={() => void refresh()} className="btn-outline" disabled={refreshing}>
            <RefreshCw size={15} className={refreshing ? 'animate-spin' : ''} /> 刷新
          </button>
        }
      />

      <div className="grid grid-cols-2 gap-3 md:grid-cols-3">
        <StatCard
          label="本机安装"
          value={status ? (status.installed ? '已安装' : '未安装') : '—'}
          hint={meta.dataPath}
          tone={status?.installed ? 'green' : 'slate'}
        />
        <StatCard
          label="登录状态"
          value={status ? (status.logged_in ? '已登录' : '未登录') : '—'}
          hint={status && !status.installed ? status.error ?? '未检测到桌面端' : '复用桌面端登录态（只读）'}
          tone={status?.logged_in ? 'green' : status ? 'red' : 'slate'}
        />
        <StatCard
          label="本机账号"
          value={status?.logged_in ? status.name || '—' : '—'}
          hint={status?.logged_in ? `ID：${status.user_id || '未知'}` : status?.error ?? '未登录'}
          tone={status?.logged_in ? 'brand' : 'slate'}
        />
        <StatCard
          label="凭证有效期"
          value={status?.logged_in ? (isOfficeAceStatus(status) ? (status.credential_fresh ? '有效' : '已过期') : status.token_expired ? '已过期' : '有效') : '—'}
          hint={credentialLine}
          tone={status?.logged_in ? 'amber' : 'slate'}
        />
        <StatCard
          label="最近一次签到"
          value={latest ? statusText(latest.status) : '暂无'}
          hint={latest ? `${latest.date} ${latest.time}${latest.reward ? ` · +${fmtNum(latest.reward)} 积分` : ''}` : '到「每日签到」执行首轮签到'}
          tone={latest ? (latest.status === 'fail' ? 'red' : 'green') : 'slate'}
        />
        <StatCard
          label="自动签到"
          value={enabled ? `每日 ${hhmm}` : '已关闭'}
          hint="应用启动补签 + 定时调度（环境配置可改）"
          tone={enabled ? 'violet' : 'slate'}
        />
      </div>

      {loadError && (
        <div className="mt-4 rounded-xl border border-rose-200 bg-rose-50 p-3 text-sm text-rose-700 dark:border-rose-500/30 dark:bg-rose-500/10 dark:text-rose-300">
          概述数据读取失败：{loadError}
        </div>
      )}

      {status?.error && (
        <div className="mt-4 rounded-xl border border-amber-200 bg-amber-50 p-3 text-sm text-amber-700 dark:border-amber-500/30 dark:bg-amber-500/10 dark:text-amber-300">
          {status.error}
        </div>
      )}

      <div className="card mt-5 p-4">
        <div className="mb-3 text-sm font-medium">运行详情</div>
        <div className="space-y-2 text-sm">
          {status && isOfficeAceStatus(status) ? (
            <>
              <div className="flex justify-between gap-4">
                <span className="text-slate-500 dark:text-zinc-400">会话有效期</span>
                <span>{status.session_expires_at ?? '未知'}</span>
              </div>
              <div className="flex justify-between gap-4">
                <span className="text-slate-500 dark:text-zinc-400">模型网关</span>
                <span className="break-all">{status.gateway_host || '—'}</span>
              </div>
            </>
          ) : null}
          {status && !isOfficeAceStatus(status) ? (
            <div className="flex justify-between gap-4">
              <span className="text-slate-500 dark:text-zinc-400">令牌有效期</span>
              <span>{formatExpiry(status.token_expires_at ? status.token_expires_at * 1000 : null)}</span>
            </div>
          ) : null}
          <div className="flex justify-between gap-4">
            <span className="text-slate-500 dark:text-zinc-400">数据源</span>
            <span className="break-all">{meta.dataPath}</span>
          </div>
          <div className="flex justify-between gap-4">
            <span className="text-slate-500 dark:text-zinc-400">近 30 天签到</span>
            <span>
              成功 {records.filter((r) => r.status === 'success').length} · 已签 {records.filter((r) => r.status === 'already').length} · 失败 {records.filter((r) => r.status === 'fail').length}
            </span>
          </div>
        </div>
        <button className="btn-outline mt-4" onClick={() => setView(`${platform}-checkin`)}>
          <ExternalLink size={14} /> 去每日签到
        </button>
      </div>
    </div>
  );
}

// ── 账号管理 ────────────────────────────────────────────────────────────────

export function LocalAccountsPage({ platform }: { platform: LocalPlatform }) {
  const meta = LOCAL_APP_META[platform];
  const [status, setStatus] = useState<LocalStatus | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const aliveRef = useRef(true);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    try {
      const next = await meta.status();
      if (aliveRef.current) setStatus(next);
    } catch {
      // 重新检测失败保留上次结果，徽标不改口
    } finally {
      if (aliveRef.current) setRefreshing(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [platform]);

  useEffect(() => {
    aliveRef.current = true;
    void refresh();
    return () => {
      aliveRef.current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [platform]);

  const loggedIn = Boolean(status?.logged_in);

  return (
    <div className="animate-fade-in">
      <PageHeader
        title={`${meta.name} · 账号管理`}
        desc={`本机单账号（只读桌面端登录态）：${meta.name} 无账号池，不支持多账号导入`}
        actions={
          <button onClick={() => void refresh()} className="btn-outline" disabled={refreshing}>
            <RefreshCw size={15} className={refreshing ? 'animate-spin' : ''} /> 重新检测
          </button>
        }
      />

      <div className="card p-4">
        <div className="mb-3 flex items-center justify-between">
          <div className="text-sm font-medium">本机账号</div>
          {statusBadge(status)}
        </div>
        {status?.logged_in ? (
          <div className="space-y-2 text-sm">
            <div className="flex justify-between gap-4">
              <span className="text-slate-500 dark:text-zinc-400">账号名</span>
              <span className="font-medium">{status.name || '—'}</span>
            </div>
            <div className="flex justify-between gap-4">
              <span className="text-slate-500 dark:text-zinc-400">账号 ID</span>
              <span className="break-all">{status.user_id || '未知'}</span>
            </div>
            <div className="flex items-center justify-between gap-4">
              <span className="text-slate-500 dark:text-zinc-400">凭证有效期</span>
              <span className="inline-flex items-center gap-1">
                <Clock size={13} />
                {isOfficeAceStatus(status)
                  ? formatExpiry(status.credential_expires_at)
                  : formatExpiry(status.token_expires_at ? status.token_expires_at * 1000 : null)}
              </span>
            </div>
          </div>
        ) : (
          <div className="rounded-lg bg-slate-50 px-3 py-2 text-sm text-slate-600 dark:bg-zinc-800/60 dark:text-zinc-300">
            {status
              ? status.installed
                ? status.error ?? '未登录：请打开桌面端完成登录后「重新检测」'
                : '未检测到桌面端：请先安装并登录后再检测'
              : '读取中…'}
          </div>
        )}
      </div>

      <div className="card mt-5 p-4">
        <div className="mb-2 text-sm font-medium">数据源说明</div>
        <div className="space-y-1.5 text-xs text-slate-500 dark:text-zinc-400">
          <div className="break-all">登录态存储：{meta.dataPath}</div>
          <div className="leading-relaxed">{meta.dataPathNote}</div>
          <div className="leading-relaxed">
            本应用只读以上数据，不修改、不删除任何桌面端账号信息；账号的新增 / 退出请直接在
            {' '}{meta.name} 桌面端操作。
          </div>
        </div>
      </div>
    </div>
  );
}

// ── 积分看板 ────────────────────────────────────────────────────────────────

export function LocalCreditsPage({ platform }: { platform: LocalPlatform }) {
  const meta = LOCAL_APP_META[platform];
  const [status, setStatus] = useState<LocalStatus | null>(null);
  const [records, setRecords] = useState<LocalCheckinRecord[]>([]);
  const [quota, setQuota] = useState<LocalQuota | null>(null);
  const [quotaError, setQuotaError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const aliveRef = useRef(true);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    try {
      // quota 失败单独落 quotaError（上游不可达必须报错，不能用 0 / 「暂无数据」掩盖）
      const [nextStatus, nextRecords, quotaSettled] = await Promise.all([
        meta.status(),
        meta.results(30),
        meta.quota().then(
          (q) => ({ ok: true as const, q }),
          (e) => ({ ok: false as const, e }),
        ),
      ]);
      if (!aliveRef.current) return;
      setStatus(nextStatus);
      setRecords(nextRecords);
      if (quotaSettled.ok) {
        setQuota(quotaSettled.q);
        setQuotaError(null);
      } else {
        setQuota(null);
        setQuotaError(String(quotaSettled.e));
      }
    } finally {
      if (aliveRef.current) {
        setLoading(false);
        setRefreshing(false);
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [platform]);

  useEffect(() => {
    aliveRef.current = true;
    void refresh();
    return () => {
      aliveRef.current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [platform]);

  const earned30 = useMemo(
    () =>
      records.reduce((sum, r) => sum + (r.status !== 'fail' && r.reward ? r.reward : 0), 0),
    [records],
  );
  const okCount = records.filter((r) => r.status === 'success').length;
  const oaQuota = quota && isOfficeAceQuota(quota) ? quota : null;
  const lbQuota = quota && !isOfficeAceQuota(quota) ? quota : null;
  const quotaHint = (label: string) =>
    quotaError ? `${label}（见上方错误）` : '读取中…';

  // 近 30 天每日签到获得（零填充，镜像 BuddyAI 概述趋势口径）
  const trend = useMemo(() => {
    const days: string[] = [];
    for (let i = 29; i >= 0; i--) days.push(localDateStr(new Date(Date.now() - i * 86400000)));
    const byDate = new Map<string, number>();
    for (const r of records) {
      if (r.status === 'fail' || !r.reward) continue;
      byDate.set(r.date, (byDate.get(r.date) ?? 0) + r.reward);
    }
    return days.map((d) => ({ date: d.slice(5), 获得: Number((byDate.get(d) ?? 0).toFixed(2)) }));
  }, [records]);

  const quotaCards = (() => {
    if (platform === 'officeace') {
      return (
        <>
          <StatCard
            label="当前可用积分"
            value={oaQuota ? fmtNum(oaQuota.remaining) : '—'}
            hint={oaQuota ? `总额度 ${fmtNum(oaQuota.total)} · 已用 ${fmtNum(oaQuota.used)}` : quotaHint('余额查询失败')}
            tone="amber"
          />
          <StatCard label="订阅奖励条目" value={oaQuota ? oaQuota.items.length : '—'} hint={oaQuota ? 'bonus_skus 活动（明细见下方）' : quotaHint('条目数未知')} tone="brand" />
        </>
      );
    }
    return (
      <>
        <StatCard
          label="当前积分余额"
          value={lbQuota ? (lbQuota.balance != null ? fmtNum(lbQuota.balance) : '未知') : '—'}
          hint={lbQuota ? (lbQuota.balance != null ? '上游实时余额' : '上游未下发余额字段') : quotaHint('余额查询失败')}
          tone="amber"
        />
        <StatCard
          label="连续签到"
          value={lbQuota ? `${lbQuota.streak_days} 天` : '—'}
          hint={lbQuota ? `累计签到 ${lbQuota.claimed_days} 天${lbQuota.total_days > 0 ? ` / 活动共 ${lbQuota.total_days} 天` : ''}` : quotaHint('连签数据读取失败')}
          tone="violet"
        />
      </>
    );
  })();

  return (
    <div className="animate-fade-in">
      <PageHeader
        title={`${meta.name} · 积分看板`}
        desc={`当前余额（实时只读查询）+ 近 30 天签到获得（读本机落库记录）`}
        actions={
          <button onClick={() => void refresh()} className="btn-outline" disabled={refreshing}>
            <RefreshCw size={15} className={refreshing ? 'animate-spin' : ''} /> 刷新
          </button>
        }
      />

      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        {quotaCards}
        <StatCard label="近 30 天签到获得" value={fmtNum(earned30)} hint={`成功 ${okCount} 次（本机记录）`} tone="green" />
        <StatCard
          label="今日签到"
          value={records.some((r) => r.date === localDateStr(new Date()) && r.status !== 'fail') ? '已签' : '未签'}
          hint="以本机签到记录为准"
          tone="slate"
        />
      </div>

      {quotaError && (
        <div className="mt-4 flex items-start gap-2 rounded-xl border border-rose-200 bg-rose-50 p-3 text-sm text-rose-700 dark:border-rose-500/30 dark:bg-rose-500/10 dark:text-rose-300">
          <XCircle size={16} className="mt-0.5 shrink-0" />
          <div className="min-w-0">
            <div className="font-medium">积分余额查询失败</div>
            <div className="mt-0.5 break-words text-xs">{quotaError}</div>
          </div>
        </div>
      )}

      {/* 奖励明细（OfficeAce：bonus_skus 逐活动；LobsterAI：签到活动状态） */}
      <div className="card mt-5 p-4">
        <div className="mb-3 text-sm font-medium">奖励明细</div>
        {!quota && !quotaError ? (
          <p className="text-sm text-slate-500 dark:text-zinc-500">{loading ? '读取中…' : '暂无上游数据'}</p>
        ) : platform === 'officeace' ? (
          oaQuota ? (
            oaQuota.items.length === 0 ? (
              <p className="text-sm text-slate-500 dark:text-zinc-500">
                上游订阅未返回奖励条目（bonus_skus 为空）——非错误，可能当前无进行中的奖励活动。
              </p>
            ) : (
              <div className="space-y-2">
                {oaQuota.items.map((item) => (
                  <div
                    key={item.activity}
                    className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-slate-100 px-3 py-2 text-sm dark:border-zinc-800"
                  >
                    <span className="min-w-0 break-all font-medium">{item.activity}</span>
                    <span className="text-xs text-slate-500 dark:text-zinc-400">
                      总 {fmtNum(item.total)} · 已用 {fmtNum(item.used)} · 剩余{' '}
                      <span className="font-medium text-emerald-600 dark:text-emerald-400">{fmtNum(item.remaining)}</span>
                    </span>
                  </div>
                ))}
              </div>
            )
          ) : (
            <p className="text-sm text-slate-500 dark:text-zinc-500">余额查询失败，明细不可用（见上方错误）。</p>
          )
        ) : lbQuota ? (
          <div className="space-y-2 text-sm">
            <div className="flex justify-between gap-4">
              <span className="text-slate-500 dark:text-zinc-400">今日已签</span>
              <span>{lbQuota.claimed_today ? '是' : '否'}</span>
            </div>
            <div className="flex justify-between gap-4">
              <span className="text-slate-500 dark:text-zinc-400">累计 / 连续签到</span>
              <span>
                {lbQuota.claimed_days} 天 / {lbQuota.streak_days} 天
              </span>
            </div>
            {lbQuota.activity_code && (
              <div className="flex justify-between gap-4">
                <span className="text-slate-500 dark:text-zinc-400">签到活动</span>
                <span className="break-all">{lbQuota.activity_code}</span>
              </div>
            )}
            {lbQuota.actions.length > 0 && (
              <div className="flex justify-between gap-4">
                <span className="text-slate-500 dark:text-zinc-400">可用动作</span>
                <span className="break-all">{lbQuota.actions.join(' / ')}</span>
              </div>
            )}
          </div>
        ) : (
          <p className="text-sm text-slate-500 dark:text-zinc-500">余额查询失败，明细不可用（见上方错误）。</p>
        )}
      </div>

      {/* 近 30 天签到获得趋势（读 *_checkin_results 落库记录） */}
      <div className="card mt-5 p-4">
        <div className="mb-3 text-sm font-medium">近 30 天签到获得积分</div>
        <div className="h-44">
          <ResponsiveContainer width="100%" height="100%">
            <BarChart data={trend} margin={{ top: 4, right: 8, bottom: 0, left: -20 }}>
              <CartesianGrid strokeDasharray="3 3" stroke="#8884" vertical={false} />
              <XAxis dataKey="date" tick={{ fontSize: 10 }} interval={4} tickLine={false} axisLine={false} />
              <YAxis allowDecimals={false} tick={{ fontSize: 10 }} tickLine={false} axisLine={false} />
              <Tooltip cursor={{ fill: '#8881' }} contentStyle={{ fontSize: 12, borderRadius: 8 }} />
              <Bar dataKey="获得" fill="#10b981" radius={[3, 3, 0, 0]} maxBarSize={18} />
            </BarChart>
          </ResponsiveContainer>
        </div>
      </div>

      <div className="card mt-5 p-4">
        <div className="mb-3 flex items-center justify-between">
          <div className="text-sm font-medium">签到记录（近 30 天）</div>
          <span className="text-xs text-slate-500 dark:text-zinc-500">共 {records.length} 条</span>
        </div>
        {records.length === 0 ? (
          <p className="text-sm text-slate-500 dark:text-zinc-500">暂无记录</p>
        ) : (
          <div className="max-h-80 space-y-2 overflow-auto pr-1">
            {[...records]
              .sort((a, b) => (b.time_ms || 0) - (a.time_ms || 0))
              .map((record) => (
                <div
                  key={`${record.date}|${record.user_id}|${record.time_ms}`}
                  className="flex items-start gap-2 rounded-lg border border-slate-100 px-3 py-2 text-sm dark:border-zinc-800"
                >
                  {record.status === 'fail' ? (
                    <XCircle size={15} className="mt-0.5 shrink-0 text-rose-500" />
                  ) : (
                    <CheckCircle2 size={15} className="mt-0.5 shrink-0 text-emerald-500" />
                  )}
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-1.5">
                      <span className="text-xs text-slate-500 dark:text-zinc-500">{record.date} {record.time}</span>
                      <Badge tone={statusTone(record.status)}>{statusText(record.status)}</Badge>
                      {record.reward != null && record.reward > 0 && record.status !== 'fail' && (
                        <span className="text-xs font-medium text-emerald-600 dark:text-emerald-400">+{fmtNum(record.reward)}</span>
                      )}
                    </div>
                    <div className="mt-0.5 break-words text-xs text-slate-500 dark:text-zinc-400">{record.message}</div>
                  </div>
                </div>
              ))}
          </div>
        )}
      </div>
    </div>
  );
}

// ── 环境配置 ────────────────────────────────────────────────────────────────

export function LocalSettingsPage({ platform }: { platform: LocalPlatform }) {
  const meta = LOCAL_APP_META[platform];
  const pushToast = useAppStore((s) => s.pushToast);
  const settings = useAppStore((s) => s.settings);
  const saveSettings = useAppStore((s) => s.saveSettings);
  const [status, setStatus] = useState<LocalStatus | null>(null);
  const aliveRef = useRef(true);

  useEffect(() => {
    aliveRef.current = true;
    meta
      .status()
      .then((s) => {
        if (aliveRef.current) setStatus(s);
      })
      .catch(() => {});
    return () => {
      aliveRef.current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [platform]);

  const patchSettings = async (patch: LocalCheckinSettingsPatch) => {
    try {
      await saveSettings(patch);
      pushToast('success', '签到设置已保存');
    } catch {
      // saveSettings 内部已 toast 并回滚
    }
  };

  const enabledKey = meta.enabledKey;
  const hhmmKey = meta.hhmmKey;
  const enabled = Boolean(settings?.[enabledKey]);
  const hhmm = (settings?.[hhmmKey] as string | undefined) ?? meta.defaultHhmm;

  return (
    <div className="animate-fade-in">
      <PageHeader
        title={`${meta.name} · 环境配置`}
        desc={`${meta.fullName}自动签到调度与本机数据源说明`}
      />

      <div className="card p-4">
        <div className="mb-3 text-sm font-medium">自动签到</div>
        <div className="flex flex-wrap items-center gap-4 text-sm text-slate-600 dark:text-zinc-300">
          <label className="inline-flex items-center gap-2">
            <input
              type="checkbox"
              checked={enabled}
              onChange={(e) => void patchSettings({ [enabledKey]: e.target.checked } as LocalCheckinSettingsPatch)}
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
                  void patchSettings({ [hhmmKey]: value } as LocalCheckinSettingsPatch);
                }
              }}
              placeholder="HH:MM"
            />
          </label>
        </div>
        <p className="mt-3 text-xs leading-relaxed text-slate-500 dark:text-zinc-500">
          调度仅在应用运行期间生效；错过时刻（关机 / 未打开应用）会在下次启动 75 秒后自动补签一次。
        </p>
      </div>

      <div className="card mt-5 p-4">
        <div className="mb-3 text-sm font-medium">本机数据源</div>
        <div className="space-y-2 text-sm">
          <div className="flex flex-wrap justify-between gap-2">
            <span className="text-slate-500 dark:text-zinc-400">登录态存储</span>
            <span className="break-all">{meta.dataPath}</span>
          </div>
          <div className="flex flex-wrap justify-between gap-2">
            <span className="text-slate-500 dark:text-zinc-400">检测状态</span>
            <span className="inline-flex items-center gap-2">
              {statusBadge(status)}
              {status?.installed ? (
                <span className="text-xs text-slate-400">已安装</span>
              ) : null}
            </span>
          </div>
        </div>
        <p className="mt-3 text-xs leading-relaxed text-slate-500 dark:text-zinc-500">{meta.dataPathNote}</p>
      </div>

      <div className="card mt-5 p-4">
        <div className="mb-3 flex items-center gap-2 text-sm font-medium">
          <KeyRound size={15} /> 凭据只读说明
        </div>
        <p className="text-xs leading-relaxed text-slate-500 dark:text-zinc-500">{meta.credentialNote}</p>
        <ul className="mt-2 space-y-1 text-xs text-slate-500 dark:text-zinc-400">
          <li>· 只读登录态，不写回桌面端存储，不落盘任何凭据；</li>
          <li>· 凭据仅在本进程内存中使用，日志与事件不含 AK/SK/STS 等敏感字段；</li>
          <li>· 登录态过期时如实报错并引导打开桌面端，不会静默使用失效凭据。</li>
        </ul>
      </div>
    </div>
  );
}
