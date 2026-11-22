import { useEffect, useMemo, useState } from 'react';
import { CheckCircle2, Circle, ChevronRight, RefreshCw } from 'lucide-react';
import {
  BarChart,
  Bar,
  XAxis,
  YAxis,
  ResponsiveContainer,
  Tooltip,
  CartesianGrid,
  Legend,
} from 'recharts';
import PageHeader from '../../components/PageHeader';
import { StatCard, Badge } from '../../components/ui';
import { aggregateCheckinTrends } from '../../components/overviewTrendModel';
import { api } from '../../lib/tauri';
import { useAppStore } from '../../store';
import { useIsDark } from '../../lib/useIsDark';
import { LOCAL_APP_META } from '../../lib/localApps';
import type { ViewKey, LocalCheckinRecord, OfficeAceQuota, OfficeAceStatus } from '../../types';

/**
 * officeace-overview 概述（VENU-41 风格对齐，对照 QoderOverview 结构逐块对齐）：
 * 顶部统计卡行（账号 / 今日签到 / 积分余额 / 调度状态 / 需重新授权）+ 近 30 天签到结果
 * （堆叠柱状）+ 配置导航。
 * 差异：本机单账号（只读桌面端登录态），无账号池 / 积分榜；积分余额读只读 quota 命令，
 * 上游不可达时显「—」并如实提示（不用 0 冒充）。
 */

/** 配置导航步骤（对齐 Qoder/Buddy 概述 SetupGuide 形态；optional 步骤不计入完成度） */
interface Step {
  key: string;
  title: string;
  desc: string;
  done: boolean;
  actionLabel: string;
  view: ViewKey;
  optional?: boolean;
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

export default function OfficeAceOverview() {
  const pushToast = useAppStore((s) => s.pushToast);
  const setView = useAppStore((s) => s.setView);
  const settings = useAppStore((s) => s.settings);
  const isDark = useIsDark();
  const [status, setStatus] = useState<OfficeAceStatus | null>(null);
  const [records, setRecords] = useState<LocalCheckinRecord[]>([]);
  const [quota, setQuota] = useState<OfficeAceQuota | null>(null);
  const [quotaError, setQuotaError] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);

  // quota 失败不拖垮整页（对齐 QoderOverview creditsFetch 包裹口径）：失败显式提示并保留「—」，
  // 登录态/记录读取失败单独落横幅如实报错（不用「暂无记录」掩盖读取异常）
  const refresh = async () => {
    setRefreshing(true);
    try {
      const [stSettled, recSettled, quotaSettled] = await Promise.all([
        api.officeace.status().then(
          (s) => ({ ok: true as const, s }),
          (err: unknown) => ({ ok: false as const, err }),
        ),
        api.officeace.checkinResults(30).then(
          (r) => ({ ok: true as const, r }),
          (err: unknown) => ({ ok: false as const, err }),
        ),
        api.officeace.quota().then(
          (q) => ({ ok: true as const, q }),
          (err: unknown) => ({ ok: false as const, err }),
        ),
      ]);
      setStatus(stSettled.ok ? stSettled.s : null);
      setRecords(recSettled.ok ? recSettled.r : []);
      const errs: string[] = [];
      if (!stSettled.ok) errs.push(`登录态读取失败：${String(stSettled.err)}`);
      if (!recSettled.ok) errs.push(`签到记录读取失败：${String(recSettled.err)}`);
      setLoadError(errs.length > 0 ? errs.join('；') : null);
      if (quotaSettled.ok) {
        setQuota(quotaSettled.q);
        setQuotaError(null);
      } else {
        setQuota(null);
        setQuotaError(String(quotaSettled.err));
      }
    } catch (err) {
      pushToast('error', `OfficeAce 概述刷新失败：${String(err)}`);
    } finally {
      setRefreshing(false);
    }
  };

  useEffect(() => {
    void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const meta = LOCAL_APP_META.officeace;
  const installed = Boolean(status?.installed);
  const loggedIn = Boolean(status?.logged_in);
  const today = localDateStr(new Date());
  // 今日签到状态（本机单账号：今日任一非失败记录即视为已签）
  const todayRecords = useMemo(() => records.filter((r) => r.date === today), [records, today]);
  const checkedToday = todayRecords.some((r) => r.status !== 'fail');
  const todayEarned = useMemo(
    () =>
      todayRecords.reduce((s, r) => s + (r.status !== 'fail' && r.reward ? r.reward : 0), 0),
    [todayRecords],
  );
  const latest = useMemo(
    () => [...records].sort((a, b) => (b.time_ms || 0) - (a.time_ms || 0))[0],
    [records],
  );

  // 近 30 天趋势聚合（共享口径：任一成功即当日已领，全失败才计失败——overviewTrendModel 含单测）
  const trends = useMemo(() => aggregateCheckinTrends(records), [records]);

  // 告警提醒（本机单账号）：凭证过期（logged_in 但 credential_fresh=false）+ 登录态失效
  //（已安装但未登录）；未安装属环境问题不计入（无凭据可重新授权）
  const credExpiredCount = installed && loggedIn && status && !status.credential_fresh ? 1 : 0;
  const notLoggedInCount = installed && !loggedIn ? 1 : 0;
  const alertCount = credExpiredCount + notLoggedInCount;

  // 调度状态（settings 即全局快照；缺省回退默认时刻）
  const enabled = Boolean(settings?.officeace_checkin_enabled);
  const hhmm = settings?.officeace_checkin_hhmm || meta.defaultHhmm;

  // 配置导航（步骤完成态实时判定）
  const steps: Step[] = [
    {
      key: 'client',
      title: '安装 OfficeAce 桌面端',
      desc: `本机检测登录态数据源：${meta.dataPath}。未自动识别时请确认已安装并登录过一次。`,
      done: installed,
      actionLabel: '安装后自动识别',
      view: 'officeace-settings',
    },
    {
      key: 'account',
      title: '登录 OfficeAce 账号',
      desc: '在 OfficeAce 桌面端登录后回到「账号管理」重新检测，本应用只读登录态、不保存凭据。',
      done: loggedIn,
      actionLabel: '去检测',
      view: 'officeace-accounts',
    },
    {
      key: 'checkin',
      title: '完成首次签到',
      desc: '验证签到链路是否跑通（上游幂等：已领过的当天重试不会重复领）。',
      done: checkedToday,
      actionLabel: '去签到',
      view: 'officeace-checkin',
    },
    {
      key: 'credits',
      title: '查询积分余额',
      desc: '只读查询订阅奖励余量（/v1/subscription）；上游不可达时如实报错，不用 0 冒充。',
      done: quota != null,
      actionLabel: '查看积分',
      view: 'officeace-credits',
    },
    {
      key: 'schedule',
      title: '开启每日自动签到',
      desc: '应用启动补签 + 定时调度（错过时刻下次启动 75 秒后自动补签一次）。',
      done: enabled,
      actionLabel: '去环境配置',
      view: 'officeace-settings',
    },
  ];
  const required = steps.filter((s) => !s.optional);
  const completed = required.filter((s) => s.done).length;
  const allDone = completed === required.length;

  return (
    <div className="animate-fade-in">
      <PageHeader
        title="OfficeAce · 概述"
        desc="OfficeAce（华为果办）本机登录态运行总览 · 账号 / 今日签到 / 积分余额 / 调度状态 / 告警提醒"
        actions={
          <button onClick={() => void refresh()} className="btn-outline" disabled={refreshing}>
            <RefreshCw size={15} className={refreshing ? 'animate-spin' : ''} /> 刷新
          </button>
        }
      />

      {/* 顶部统计卡（对齐 Qoder 概述：数值一眼掌握） */}
      <div className="grid grid-cols-2 gap-3 md:grid-cols-5">
        <StatCard
          label="本机账号"
          value={status ? (loggedIn ? status.name || '—' : installed ? '未登录' : '未安装') : '读取中…'}
          hint={
            status
              ? loggedIn
                ? `ID：${status.user_id || '未知'}`
                : status.error ?? '打开桌面端登录后自动识别'
              : '读取中…'
          }
          tone={loggedIn ? 'brand' : 'slate'}
        />
        <StatCard
          label="今日签到"
          value={checkedToday ? '已签' : '未签'}
          hint={
            todayEarned > 0
              ? `今日获得 ${fmtNum(todayEarned)} 积分`
              : latest
                ? `最近一次：${latest.date} ${statusText(latest.status)}`
                : '到「每日签到」执行首轮签到'
          }
          tone={checkedToday ? 'green' : 'slate'}
        />
        <StatCard
          label="积分余额"
          value={quota ? fmtNum(quota.remaining) : '—'}
          hint={
            quota
              ? `总额度 ${fmtNum(quota.total)} · 已用 ${fmtNum(quota.used)}`
              : quotaError
                ? '余额查询失败（见下方横幅）'
                : '打开桌面端登录后自动查询'
          }
          tone={quota ? 'amber' : 'slate'}
        />
        <StatCard
          label="调度状态"
          value={enabled ? `每日 ${hhmm}` : '已关闭'}
          hint="应用启动补签 + 定时调度（环境配置可改）"
          tone={enabled ? 'violet' : 'slate'}
        />
        <StatCard
          label="需重新授权"
          value={alertCount}
          hint={`凭证过期 ${credExpiredCount} · 未登录 ${notLoggedInCount}（打开桌面端重新登录后恢复）`}
          tone={alertCount > 0 ? 'red' : 'slate'}
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

      {quotaError && (
        <div className="mt-4 rounded-xl border border-amber-200 bg-amber-50 p-3 text-sm text-amber-700 dark:border-amber-500/30 dark:bg-amber-500/10 dark:text-amber-300">
          积分余额查询失败（上游不可达或凭证过期）：{quotaError}
        </div>
      )}

      {/* 近 30 天签到结果（对齐 Qoder 概述：堆叠柱状按日汇总） */}
      <div className="mt-5 card p-5">
        <div className="mb-4 flex items-center justify-between">
          <h3 className="font-medium">近 30 天签到结果</h3>
          <span className="text-xs text-slate-400">按日汇总 · 成功 / 已签 / 失败</span>
        </div>
        {trends.length === 0 ? (
          <div className="flex h-40 items-center justify-center text-sm text-slate-400">
            暂无签到记录，完成一次签到后这里会显示趋势。
          </div>
        ) : (
          <div className="h-64">
            <ResponsiveContainer>
              <BarChart data={trends} margin={{ top: 8, right: 16, left: 0, bottom: 4 }} barCategoryGap="24%">
                <CartesianGrid strokeDasharray="3 3" stroke={isDark ? '#3f3f46' : '#e2e8f0'} opacity={0.25} vertical={false} />
                <XAxis
                  dataKey="date"
                  tickFormatter={(v: string) => v.slice(5)}
                  tick={{ fontSize: 11, fill: isDark ? '#a1a1aa' : '#94a3b8' }}
                  axisLine={{ stroke: isDark ? '#3f3f46' : '#e2e8f0' }}
                  tickLine={false}
                />
                <YAxis allowDecimals={false} tick={{ fontSize: 11, fill: isDark ? '#a1a1aa' : '#94a3b8' }} axisLine={false} tickLine={false} width={36} />
                <Tooltip
                  cursor={{ fill: isDark ? 'rgba(255,255,255,0.05)' : 'rgba(0,0,0,0.03)' }}
                  contentStyle={{
                    fontSize: 12,
                    borderRadius: 10,
                    border: `1px solid ${isDark ? '#3f3f46' : '#e2e8f0'}`,
                    background: isDark ? '#18181b' : '#fff',
                    color: isDark ? '#e4e4e7' : '#1e293b',
                    boxShadow: '0 6px 16px rgba(0,0,0,0.1)',
                    padding: '8px 12px',
                  }}
                />
                <Legend wrapperStyle={{ fontSize: 12 }} />
                <Bar dataKey="ok" name="成功" stackId="trend" fill="#10b981" maxBarSize={28} />
                <Bar dataKey="already" name="已签" stackId="trend" fill="#0ea5e9" maxBarSize={28} />
                <Bar dataKey="failed" name="失败" stackId="trend" fill="#f43f5e" maxBarSize={28} radius={[4, 4, 0, 0]} />
              </BarChart>
            </ResponsiveContainer>
          </div>
        )}
      </div>

      {/* 配置导航（对齐 Qoder 概述 SetupGuide 形态） */}
      <div className="mt-5 card overflow-hidden">
        <div className="flex items-center justify-between border-b border-slate-100 px-4 py-3 dark:border-zinc-800">
          <div>
            <h3 className="font-medium">配置导航</h3>
            <p className="text-xs text-slate-500">按步骤完成初始化，已完成的步骤无需重复处理。</p>
          </div>
          <Badge tone={allDone ? 'green' : 'amber'}>
            {completed}/{required.length} 已完成
          </Badge>
        </div>
        <ol className="divide-y divide-slate-100 dark:divide-slate-800">
          {steps.map((step, i) => (
            <li key={step.key} className="flex items-center gap-3 px-4 py-3">
              <div className={step.done ? 'text-emerald-500' : 'text-slate-300 dark:text-zinc-600'}>
                {step.done ? <CheckCircle2 size={20} /> : <Circle size={20} />}
              </div>
              <div className="min-w-0 flex-1">
                <div className="text-sm font-medium text-slate-800 dark:text-zinc-100">
                  {i + 1}. {step.title}
                </div>
                <div className="text-xs text-slate-500">{step.desc}</div>
              </div>
              {step.done ? (
                <span className="shrink-0 rounded-full bg-emerald-50 px-2.5 py-1 text-xs font-medium text-emerald-600 dark:bg-emerald-500/15 dark:text-emerald-400">
                  已完成
                </span>
              ) : (
                <button onClick={() => setView(step.view)} className="btn-outline shrink-0">
                  {step.actionLabel}
                  <ChevronRight size={14} />
                </button>
              )}
            </li>
          ))}
        </ol>
        {/* OfficeAce 说明（沿用原「运行详情」口径：会话 / 网关 / 数据源不丢） */}
        <div className="mt-3 space-y-1 border-t border-slate-100 px-4 pt-3 text-xs leading-relaxed text-slate-400 dark:border-zinc-800">
          <p>
            <b className="font-medium text-slate-500 dark:text-zinc-400">OfficeAce 说明：</b>
            签到走控制面 /v1/subscription/bonus/claim（V11 签名），复用桌面端登录态；桌面端临时凭证约
            2 小时有效，过期时请打开并登录 OfficeAce 客户端后重试。
          </p>
          <p>
            会话有效期：{status?.session_expires_at ?? '未知'} · 模型网关：
            <span className="break-all">{status?.gateway_host || '—'}</span> · 数据源：
            <span className="break-all">{meta.dataPath}</span>
          </p>
        </div>
        {allDone && (
          <div className="border-t border-slate-100 bg-emerald-50/60 px-4 py-3 text-sm text-emerald-700 dark:border-zinc-800 dark:bg-emerald-500/10 dark:text-emerald-300">
            全部配置已完成，每日签到交给自动化即可！
          </div>
        )}
      </div>
    </div>
  );
}
