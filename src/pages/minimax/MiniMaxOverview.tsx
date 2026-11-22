import { useCallback, useEffect, useMemo, useState } from 'react';
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
import { StatCard, Badge, Spinner } from '../../components/ui';
import { aggregateCheckinTrends } from '../../components/overviewTrendModel';
import { api } from '../../lib/tauri';
import { useAppStore } from '../../store';
import { useIsDark } from '../../lib/useIsDark';
import type { MinimaxAccountView, MinimaxCheckinRecord, MinimaxRegion, ViewKey } from '../../types';

/**
 * minimax-overview 概述（VENU-41 风格对齐，对照 QoderOverview/OfficeAceOverview 同构）：
 * 顶部统计卡行（账号总数 / 今日签到 / 积分余额 / 需重新授权）+ 近 30 天签到结果（堆叠柱状）
 * + 配置导航。
 * 差异：双区多账号池（设备码授权 / auth.json 导入）；积分余额为账号池已缓存余额合计
 * （未知账号不计入，全部未知显「—」不用 0 冒充）；原「区分布与调度」口径并入配置导航脚注。
 */

const REGION_LABEL: Record<MinimaxRegion, string> = {
  minimax: '国内区',
  'minimax-global': '国际区',
};

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

export default function MiniMaxOverview() {
  const setView = useAppStore((s) => s.setView);
  const settings = useAppStore((s) => s.settings);
  const isDark = useIsDark();
  const [accounts, setAccounts] = useState<MinimaxAccountView[]>([]);
  const [results, setResults] = useState<MinimaxCheckinRecord[]>([]);
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    try {
      const [list, rec] = await Promise.all([api.minimax.accountsList(), api.minimax.checkinResults(30)]);
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
  const needsReloginNames = accounts.filter((a) => a.needs_relogin).map((a) => a.name || a.user_id);
  const todayOk = results.filter((r) => r.date === today && r.status === 'success').length;
  const todayAlready = results.filter((r) => r.date === today && r.status === 'already').length;
  const todayTotal = accounts.length;
  // 调度开关沿用原口径：settings 缺省视为开启（!== false）
  const scheduled = settings?.minimax_checkin_enabled !== false;
  const hhmm = settings?.minimax_checkin_hhmm || '00:05';
  const syncMode = settings?.minimax_credits_sync_mode || 'every_hours';
  const syncHours = settings?.minimax_credits_sync_hours || '2';
  const creditsKnown = accounts.filter((a) => a.credits_total != null);
  const creditsTotal = creditsKnown.reduce((s, a) => s + (a.credits_total ?? 0), 0);

  const trends = useMemo(() => aggregateCheckinTrends(results), [results]);

  // 配置导航（步骤完成态实时判定）
  const steps: Step[] = [
    {
      key: 'account',
      title: '接入 MiniMax 账号',
      desc: '设备码授权（独立刷新链）或导入本机 MiniMax Code 登录态（auth.json 只读），到「账号管理」完成。',
      done: accounts.length > 0,
      actionLabel: '去账号管理',
      view: 'minimax-accounts',
    },
    {
      key: 'checkin',
      title: '完成首次签到',
      desc: '验证双区签到链路是否跑通（claim_result=2 幂等视为已领取，重复执行不重复领）。',
      done: todayTotal > 0 && todayOk + todayAlready > 0,
      actionLabel: '去签到',
      view: 'minimax-checkin',
    },
    {
      key: 'credits',
      title: '查询积分余额',
      desc: '全账号积分实时查询并回写池缓存；单账号失败行内带 error，不掩盖。',
      done: creditsKnown.length > 0,
      actionLabel: '查看积分',
      view: 'minimax-credits',
    },
    {
      key: 'schedule',
      title: '确认每日自动签到',
      desc: `每日 ${hhmm} 首跑，失败日内退避重试；积分同步${syncMode === 'off' ? '已关闭' : syncMode === 'hourly' ? '每小时' : syncMode === 'daily' ? `每日 ${settings?.minimax_credits_sync_hhmm || '23:35'}` : `每 ${syncHours} 小时`}。`,
      done: scheduled,
      actionLabel: '去环境配置',
      view: 'minimax-settings',
    },
  ];
  const required = steps.filter((s) => !s.optional);
  const completed = required.filter((s) => s.done).length;
  const allDone = completed === required.length;

  return (
    <div className="animate-fade-in">
      <PageHeader
        title="MiniMax · 概述"
        desc="MiniMax Code 双区（国内 minimax.cn / 国际 minimax.io）账号池、每日签到与积分同步总览"
        actions={
          <button onClick={() => void load()} className="btn-outline" disabled={loading}>
            <RefreshCw size={15} className={loading ? 'animate-spin' : ''} /> 刷新
          </button>
        }
      />

      {/* 顶部统计卡（对齐 Qoder 概述：数值一眼掌握） */}
      <div className="grid grid-cols-2 gap-3 md:grid-cols-5">
        <StatCard
          label="账号总数"
          value={loading ? '—' : String(accounts.length)}
          hint={`国内区 ${cnCount} · 国际区 ${globalCount}`}
          tone="brand"
        />
        <StatCard
          label="今日签到"
          value={todayTotal > 0 ? `${todayOk + todayAlready}/${todayTotal}` : '—'}
          hint={todayTotal > 0 ? '成功 + 已领取（去重账号数）' : '先到「账号管理」接入账号'}
          tone="green"
        />
        <StatCard
          label="积分余额"
          value={creditsKnown.length > 0 ? creditsTotal.toLocaleString() : '—'}
          hint={
            accounts.length === 0
              ? '接入账号后自动同步'
              : creditsKnown.length === accounts.length
                ? `${creditsKnown.length} 个账号已缓存`
                : `${creditsKnown.length}/${accounts.length} 个账号已缓存（其余未知）`
          }
          tone="amber"
        />
        <StatCard
          label="调度状态"
          value={scheduled ? `每日 ${hhmm}` : '已关闭'}
          hint="失败日内退避重试（环境配置可改）"
          tone={scheduled ? 'violet' : 'slate'}
        />
        <StatCard
          label="需重新授权"
          value={loading ? '—' : needsRelogin}
          hint={
            needsRelogin > 0
              ? `${needsReloginNames.slice(0, 3).join('、')} 等凭证失效，请到「账号管理」重新授权`
              : '全部账号凭证链正常'
          }
          tone={needsRelogin > 0 ? 'red' : 'slate'}
        />
      </div>

      {/* 近 30 天签到结果（对齐 Qoder 概述：堆叠柱状按日汇总） */}
      <div className="mt-5 card p-5">
        <div className="mb-4 flex items-center justify-between">
          <h3 className="font-medium">近 30 天签到结果</h3>
          <span className="text-xs text-slate-400">按日汇总 · 成功 / 已领取 / 失败</span>
        </div>
        {loading ? (
          <div className="flex h-40 items-center justify-center"><Spinner className="h-5 w-5" /></div>
        ) : trends.length === 0 ? (
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
                <Bar dataKey="already" name="已领取" stackId="trend" fill="#0ea5e9" maxBarSize={28} />
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
        {/* MiniMax 说明（沿用原「区分布与调度」口径：区分布 / 双区开关 / 同步节奏不丢） */}
        <div className="mt-3 space-y-1 border-t border-slate-100 px-4 pt-3 text-xs leading-relaxed text-slate-400 dark:border-zinc-800">
          <p>
            <b className="font-medium text-slate-500 dark:text-zinc-400">MiniMax 说明：</b>
            双区开关——国内 {settings?.minimax_cn_enabled !== false ? '参与' : '停用'} · 国际{' '}
            {settings?.minimax_global_enabled !== false ? '参与' : '停用'}；国际区（agent.minimax.io）
            大陆网络直连可能被拦截，需代理兜底。
          </p>
          <p>
            区分布：国内区 {cnCount} 个账号 · 国际区 {globalCount} 个账号 · 凭证链：oauth_device
            可自动刷新，authjson 为本机登录态只读导入。
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
