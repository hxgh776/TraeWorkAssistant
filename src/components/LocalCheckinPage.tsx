import { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { CheckCircle2, Clock, PlayCircle, RefreshCw, XCircle } from 'lucide-react';
import PageHeader from './PageHeader';
import { Badge } from './ui';
import { api } from '../lib/tauri';
import { useAppStore } from '../store';
import type {
  LocalCheckinDone,
  LocalCheckinLine,
  LocalCheckinRecord,
  LobsterAiStatus,
  OfficeAceStatus,
  Settings,
} from '../types';

type Platform = 'officeace' | 'lobsterai';
type LocalStatus = OfficeAceStatus | LobsterAiStatus;

const META: Record<
  Platform,
  {
    title: string;
    desc: string;
    event: string;
    hhmmKey: 'officeace_checkin_hhmm' | 'lobsterai_checkin_hhmm';
    enabledKey: 'officeace_checkin_enabled' | 'lobsterai_checkin_enabled';
    defaultHhmm: string;
    hint: string;
    status: () => Promise<LocalStatus>;
    start: () => Promise<void>;
    results: (days: number) => Promise<LocalCheckinRecord[]>;
  }
> = {
  officeace: {
    title: 'OfficeAce（华为果办）每日签到',
    desc: '复用本机 OfficeAce 桌面端登录态领取每日奖励积分（只读登录态，不保存凭据）',
    event: 'officeace-checkin-progress',
    hhmmKey: 'officeace_checkin_hhmm',
    enabledKey: 'officeace_checkin_enabled',
    defaultHhmm: '09:40',
    hint: '签到走控制面 /v1/subscription/bonus/claim（V11 签名）。桌面端临时凭证约 2 小时有效：过期时请打开并登录 OfficeAce 客户端后重试。',
    status: () => api.officeace.status(),
    start: () => api.officeace.checkinStart(),
    results: (days) => api.officeace.checkinResults(days),
  },
  lobsterai: {
    title: 'LobsterAI（有道龙虾）每日签到',
    desc: '复用本机 LobsterAI 桌面端登录态完成每日签到领龙虾积分（只读登录态，不保存凭据）',
    event: 'lobsterai-checkin-progress',
    hhmmKey: 'lobsterai_checkin_hhmm',
    enabledKey: 'lobsterai_checkin_enabled',
    defaultHhmm: '09:50',
    hint: '签到流程：查活动槽 → 查活动状态 → 提交 check_in → 复核到账。登录态过期时请打开 LobsterAI 桌面端（会自动续期）后重试。',
    status: () => api.lobsterai.status(),
    start: () => api.lobsterai.checkinStart(),
    results: (days) => api.lobsterai.checkinResults(days),
  },
};

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

function todayOf(): string {
  const now = new Date();
  return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`;
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

export default function LocalCheckinPage({ platform }: { platform: Platform }) {
  const meta = META[platform];
  const pushToast = useAppStore((s) => s.pushToast);
  const settings = useAppStore((s) => s.settings);
  const saveSettings = useAppStore((s) => s.saveSettings);
  const [status, setStatus] = useState<LocalStatus | null>(null);
  const [records, setRecords] = useState<LocalCheckinRecord[]>([]);
  const [running, setRunning] = useState(false);
  const [lines, setLines] = useState<LocalCheckinLine[]>([]);
  const [done, setDone] = useState<LocalCheckinDone | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const aliveRef = useRef(true);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    try {
      const [nextStatus, nextRecords] = await Promise.all([
        meta.status(),
        meta.results(30),
      ]);
      if (!aliveRef.current) return;
      setStatus(nextStatus);
      setRecords(nextRecords);
    } catch (err) {
      pushToast('error', `读取${platform === 'officeace' ? 'OfficeAce' : 'LobsterAI'}签到数据失败：${String(err)}`);
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

  useEffect(() => {
    let disposed = false;
    const unlisten = listen<string>(meta.event, (ev) => {
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
  }, [meta.event]);

  useEffect(() => {
    if (done) void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [done]);

  const start = async () => {
    if (running) return;
    setRunning(true);
    setLines([]);
    setDone(null);
    try {
      await meta.start();
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

  const hhmm = (settings?.[meta.hhmmKey] as string | undefined) ?? meta.defaultHhmm;
  const enabled = Boolean(settings?.[meta.enabledKey]);
  const today = todayOf();
  const todayRecords = records.filter((r) => r.date === today);

  const credentialLine = (() => {
    if (!status) return '读取中…';
    if (platform === 'officeace') {
      const s = status as OfficeAceStatus;
      if (!s.installed) return s.error ?? '未检测到桌面端';
      if (!s.logged_in) return s.error ?? '未登录';
      return `临时凭证：${s.credential_fresh ? '有效' : '已过期'}，${formatExpiry(s.credential_expires_at)}`;
    }
    const s = status as LobsterAiStatus;
    if (!s.installed) return s.error ?? '未检测到桌面端';
    if (!s.logged_in) return s.error ?? '未登录';
    return `登录态：${s.token_expired ? '已过期' : '有效'}，${formatExpiry(s.token_expires_at ? s.token_expires_at * 1000 : null)}`;
  })();

  return (
    <div className="space-y-5">
      <PageHeader
        title={meta.title}
        desc={meta.desc}
        actions={
          <button
            className="inline-flex items-center gap-1.5 rounded-lg border border-slate-200 px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-50 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-800"
            onClick={() => void refresh()}
            disabled={refreshing}
          >
            <RefreshCw size={14} className={refreshing ? 'animate-spin' : ''} /> 刷新
          </button>
        }
      />

      <div className="rounded-xl border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="space-y-1">
            <div className="flex items-center gap-2 text-sm text-slate-700 dark:text-zinc-200">
              <span className="font-medium">本机账号</span>
              {status?.logged_in ? (
                <Badge tone="green">已登录</Badge>
              ) : (
                <Badge tone="red">未就绪</Badge>
              )}
              {status && 'credential_fresh' in status && !status.credential_fresh && (
                <Badge tone="amber">凭证临期</Badge>
              )}
            </div>
            <div className="text-sm text-slate-600 dark:text-zinc-400">
              {status?.logged_in ? `${status.name || '账号'}（${status.user_id || '未知 ID'}）` : '—'}
            </div>
            <div className="flex items-center gap-1.5 text-xs text-slate-500 dark:text-zinc-500">
              <Clock size={12} /> {credentialLine}
            </div>
          </div>
          <button
            className="inline-flex items-center gap-1.5 rounded-lg bg-emerald-600 px-4 py-2 text-sm font-medium text-white hover:bg-emerald-500 disabled:cursor-not-allowed disabled:opacity-50"
            onClick={() => void start()}
            disabled={running}
          >
            <PlayCircle size={16} /> {running ? '签到中…' : '立即签到'}
          </button>
        </div>
        <p className="mt-3 text-xs leading-relaxed text-slate-500 dark:text-zinc-500">{meta.hint}</p>
      </div>

      <div className="grid gap-4 md:grid-cols-2">
        <div className="rounded-xl border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
          <div className="mb-3 text-sm font-medium text-slate-700 dark:text-zinc-200">签到进度</div>
          {lines.length === 0 && !done && (
            <p className="text-sm text-slate-500 dark:text-zinc-500">尚未执行签到</p>
          )}
          <div className="space-y-2">
            {lines.map((line) => (
              <div
                key={line.index}
                className="flex items-start gap-2 rounded-lg bg-slate-50 px-3 py-2 text-sm dark:bg-zinc-800/60"
              >
                {line.status === 'fail' ? (
                  <XCircle size={15} className="mt-0.5 shrink-0 text-rose-500" />
                ) : (
                  <CheckCircle2 size={15} className="mt-0.5 shrink-0 text-emerald-500" />
                )}
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-1.5">
                    <span className="font-medium text-slate-700 dark:text-zinc-200">{line.name || line.user_id}</span>
                    <Badge tone={statusTone(line.status)}>{statusText(line.status)}</Badge>
                    {line.reward != null && line.reward > 0 && (
                      <span className="text-xs font-medium text-emerald-600 dark:text-emerald-400">+{line.reward} 积分</span>
                    )}
                  </div>
                  {line.message && (
                    <div className="mt-0.5 break-words text-xs text-slate-500 dark:text-zinc-400">{line.message}</div>
                  )}
                </div>
              </div>
            ))}
          </div>
          {done && (
            <div className="mt-3 text-xs text-slate-500 dark:text-zinc-400">
              本轮完成：成功 {done.ok} / 已签 {done.already} / 失败 {done.failed}
            </div>
          )}
        </div>

        <div className="rounded-xl border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
          <div className="mb-3 flex items-center justify-between">
            <div className="text-sm font-medium text-slate-700 dark:text-zinc-200">签到记录（近 30 天）</div>
            <span className="text-xs text-slate-500 dark:text-zinc-500">今日 {todayRecords.length} 条</span>
          </div>
          {records.length === 0 ? (
            <p className="text-sm text-slate-500 dark:text-zinc-500">暂无记录</p>
          ) : (
            <div className="max-h-72 space-y-2 overflow-auto pr-1">
              {records.slice(0, 30).map((record) => (
                <div
                  key={`${record.date}|${record.user_id}|${record.time_ms}`}
                  className="flex items-start gap-2 rounded-lg border border-slate-100 px-3 py-2 text-sm dark:border-zinc-800"
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-1.5">
                      <span className="text-xs text-slate-500 dark:text-zinc-500">{record.date} {record.time}</span>
                      <Badge tone={statusTone(record.status)}>{statusText(record.status)}</Badge>
                      {record.reward != null && record.reward > 0 && (
                        <span className="text-xs font-medium text-emerald-600 dark:text-emerald-400">+{record.reward}</span>
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

      <div className="rounded-xl border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
        <div className="mb-3 text-sm font-medium text-slate-700 dark:text-zinc-200">自动签到</div>
        <div className="flex flex-wrap items-center gap-4 text-sm text-slate-600 dark:text-zinc-300">
          <label className="inline-flex items-center gap-2">
            <input
              type="checkbox"
              checked={enabled}
              onChange={(e) => void patchSettings({ [meta.enabledKey]: e.target.checked } as Partial<Settings>)}
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
                  void patchSettings({ [meta.hhmmKey]: value } as Partial<Settings>);
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