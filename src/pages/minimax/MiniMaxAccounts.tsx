import { useCallback, useEffect, useRef, useState } from 'react';
import { ExternalLink, RefreshCw, Trash2, Upload } from 'lucide-react';
import PageHeader from '../../components/PageHeader';
import { Badge, EmptyState, Modal, Spinner } from '../../components/ui';
import { api } from '../../lib/tauri';
import { useAppStore } from '../../store';
import { withMinDelay } from '../../lib/delay';
import type {
  MinimaxAccountView,
  MinimaxAuthjsonRecord,
  MinimaxDeviceStart,
  MinimaxRegion,
} from '../../types';

/** MiniMax · 账号管理页（VENU-36 重建）。
 *  凭证双通道：① OAuth 设备码授权（RFC 8628 + S256 PKCE，独立刷新链，推荐）；
 *  ② 本机 MiniMax Code auth.json 只读导入（不写回、不主动刷新）。
 *  无 PAT 通道（MiniMax 无 PAT，VENU-5 定稿口径）。 */

const REGION_LABEL: Record<MinimaxRegion, string> = {
  minimax: '国内区',
  'minimax-global': '国际区',
};

const SOURCE_LABEL: Record<string, string> = {
  oauth_device: '设备码',
  authjson: '本机导入',
};

function fmtExpiry(ms: number | null | undefined): string {
  if (!ms || ms <= 0) return '未知';
  const d = new Date(ms);
  const expired = ms <= Date.now();
  const text = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
  return expired ? `${text}（已过期）` : text;
}

export default function MiniMaxAccounts() {
  const pushToast = useAppStore((s) => s.pushToast);
  const [accounts, setAccounts] = useState<MinimaxAccountView[]>([]);
  const [loading, setLoading] = useState(true);
  const [authRegion, setAuthRegion] = useState<MinimaxRegion>('minimax');
  const [device, setDevice] = useState<MinimaxDeviceStart | null>(null);
  const [deviceStatus, setDeviceStatus] = useState<'idle' | 'waiting' | 'success' | 'failed'>('idle');
  const [deviceMessage, setDeviceMessage] = useState('');
  const [importing, setImporting] = useState(false);
  const [scan, setScan] = useState<{ records: MinimaxAuthjsonRecord[]; count: number } | null>(null);
  const [editing, setEditing] = useState<MinimaxAccountView | null>(null);
  const [editName, setEditName] = useState('');
  const [editNote, setEditNote] = useState('');
  const pollTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const deviceRef = useRef<MinimaxDeviceStart | null>(null);
  const cancelledRef = useRef(false);

  const refresh = useCallback(async () => {
    try {
      setAccounts(await api.minimax.accountsList());
    } catch (e) {
      pushToast('error', String(e));
    } finally {
      setLoading(false);
    }
  }, [pushToast]);

  useEffect(() => {
    void refresh();
    void api.minimax.authjsonScan().then(setScan).catch(() => setScan(null));
    return () => {
      cancelledRef.current = true;
      if (pollTimer.current) clearTimeout(pollTimer.current);
    };
  }, [refresh]);

  const stopPolling = () => {
    if (pollTimer.current) {
      clearTimeout(pollTimer.current);
      pollTimer.current = null;
    }
  };

  const poll = useCallback(
    (intervalSecs: number) => {
      pollTimer.current = setTimeout(async () => {
        if (cancelledRef.current) return;
        try {
          const r = await api.minimax.devicePoll();
          if (cancelledRef.current) return;
          if (r.status === 'success') {
            setDeviceStatus('success');
            setDeviceMessage(`授权成功：${r.account?.name || r.account?.email || ''}（${REGION_LABEL[r.account?.region as MinimaxRegion] || ''}）`);
            deviceRef.current = null;
            await refresh();
          } else if (r.status === 'failed') {
            setDeviceStatus('failed');
            setDeviceMessage(r.message || '授权失败');
            deviceRef.current = null;
          } else {
            // pending / slow_down：继续按建议间隔轮询
            poll(intervalSecs);
          }
        } catch (e) {
          setDeviceStatus('failed');
          setDeviceMessage(String(e));
          deviceRef.current = null;
        }
      }, intervalSecs * 1000);
    },
    [refresh],
  );

  const startDevice = async () => {
    stopPolling();
    cancelledRef.current = false;
    setDeviceStatus('waiting');
    setDeviceMessage('正在申请设备码…');
    try {
      const s = await api.minimax.deviceStart(authRegion);
      setDevice(s);
      deviceRef.current = s;
      setDeviceMessage('请在浏览器完成登录授权（页面停留此处即可，授权成功自动入池）');
      poll(s.interval || 3);
    } catch (e) {
      setDeviceStatus('failed');
      setDeviceMessage(String(e));
    }
  };

  const cancelDevice = () => {
    cancelledRef.current = true;
    stopPolling();
    void api.minimax.deviceCancel();
    setDevice(null);
    deviceRef.current = null;
    setDeviceStatus('idle');
    setDeviceMessage('');
  };

  const doImport = async () => {
    setImporting(true);
    try {
      const r = await withMinDelay(api.minimax.authjsonImport(undefined), 400);
      const parts: string[] = [`新增 ${r.imported}`, `更新 ${r.updated}`, `跳过 ${r.skipped}`];
      pushToast(r.imported + r.updated > 0 ? 'success' : 'error', `导入完成：${parts.join(' · ')}${r.errors.length ? `；${r.errors.join('；')}` : ''}`);
      await refresh();
    } catch (e) {
      pushToast('error', String(e));
    } finally {
      setImporting(false);
    }
  };

  const removeAccount = async (a: MinimaxAccountView) => {
    if (!window.confirm(`确定移除账号「${a.name || a.email || a.user_id}」（${REGION_LABEL[a.region]}）？将同时清除本地凭证，不影响 MiniMax 客户端登录。`)) return;
    try {
      await api.minimax.accountRemove(a.id);
      pushToast('success', '账号已移除');
      await refresh();
    } catch (e) {
      pushToast('error', String(e));
    }
  };

  const saveEdit = async () => {
    if (!editing) return;
    try {
      await api.minimax.accountSave(editing.id, editName || undefined, editNote);
      pushToast('success', '已保存');
      setEditing(null);
      await refresh();
    } catch (e) {
      pushToast('error', String(e));
    }
  };

  const expiredCount = accounts.filter((a) => a.needs_relogin).length;

  return (
    <div>
      <PageHeader
        title="MiniMax 账号管理"
        desc="双区（minimax.cn / minimax.io）账号入池；凭证仅存本应用数据目录，绝不写入 MiniMax 客户端文件"
        actions={
          <>
            <select
              value={authRegion}
              onChange={(e) => setAuthRegion(e.target.value as MinimaxRegion)}
              className="h-9 rounded-lg border border-slate-200 bg-white px-2 text-sm dark:border-zinc-700 dark:bg-zinc-900"
            >
              <option value="minimax">国内区（minimax.cn）</option>
              <option value="minimax-global">国际区（minimax.io）</option>
            </select>
            <button
              onClick={() => void startDevice()}
              disabled={deviceStatus === 'waiting'}
              className="flex h-9 items-center gap-1.5 rounded-lg bg-zinc-900 px-4 text-sm font-medium text-white transition hover:bg-zinc-800 disabled:opacity-40 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-zinc-200"
            >
              设备码授权
            </button>
            <button
              onClick={() => void doImport()}
              disabled={importing}
              className="flex h-9 items-center gap-1.5 rounded-lg border border-slate-200 bg-white px-4 text-sm font-medium transition hover:bg-slate-50 disabled:opacity-40 dark:border-zinc-700 dark:bg-zinc-900 dark:hover:bg-zinc-800"
            >
              {importing ? <Spinner className="h-4 w-4" /> : <Upload size={15} />}
              导入本机登录态
            </button>
          </>
        }
      />

      {/* 本机登录态扫描提示 */}
      {scan && scan.count > 0 && (
        <div className="mb-4 rounded-lg border border-emerald-200 bg-emerald-50 px-4 py-2.5 text-sm text-emerald-700 dark:border-emerald-900 dark:bg-emerald-950 dark:text-emerald-300">
          检测到本机 MiniMax Code 登录态 {scan.count} 条（{scan.records.map((r) => `${REGION_LABEL[r.region]}${r.expired ? '·已过期' : ''}`).join('、')}），可点「导入本机登录态」一键入池。
        </div>
      )}

      {expiredCount > 0 && (
        <div className="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-2.5 text-sm text-amber-700 dark:border-amber-900 dark:bg-amber-950 dark:text-amber-300">
          {expiredCount} 个账号凭证已失效（重新授权后恢复）；签到失败原因见「每日签到」页记录。
        </div>
      )}

      {loading ? (
        <div className="flex justify-center py-12"><Spinner className="h-5 w-5" /></div>
      ) : accounts.length === 0 ? (
        <EmptyState
          title="暂无 MiniMax 账号"
          hint="推荐「设备码授权」（独立刷新链，不影响本机 MiniMax Code 客户端）；已在本机登录过 MiniMax Code 也可直接「导入本机登录态」。"
        />
      ) : (
        <div className="overflow-hidden rounded-lg border border-slate-200 bg-white dark:border-zinc-800 dark:bg-zinc-900">
          <div className="flex items-center justify-between border-b border-slate-100 px-4 py-3 dark:border-zinc-800">
            <h2 className="text-sm font-medium">账号池（{accounts.length}）</h2>
            <button onClick={() => void refresh()} className="flex items-center gap-1 text-xs text-zinc-500 hover:text-zinc-700 dark:text-zinc-400 dark:hover:text-zinc-200">
              <RefreshCw size={12} /> 刷新
            </button>
          </div>
          <div className="divide-y divide-slate-100 dark:divide-zinc-800">
            {accounts.map((a) => (
              <div key={a.id} className="flex flex-wrap items-center gap-2 px-4 py-3 text-sm">
                <Badge tone={a.region === 'minimax' ? 'blue' : 'violet'}>{REGION_LABEL[a.region] || a.region}</Badge>
                <div className="min-w-0">
                  <div className="flex items-center gap-1.5">
                    <span className="truncate font-medium text-slate-800 dark:text-zinc-100">{a.name || a.email || a.user_id}</span>
                    {a.needs_relogin && <Badge tone="amber" title={a.relogin_reason}>需重新授权</Badge>}
                    <Badge tone="slate" title="凭证来源">{SOURCE_LABEL[a.credential_source] || a.credential_source}</Badge>
                  </div>
                  <div className="mt-0.5 truncate text-xs text-slate-500 dark:text-zinc-400">
                    {a.email || a.user_id}
                    {a.note ? ` · ${a.note}` : ''}
                    {` · Token 至 ${fmtExpiry(a.token_expires_at_ms)}`}
                    {a.credits_total != null ? ` · 余额 ${a.credits_total}` : ''}
                    {a.last_checkin_status ? ` · 最近签到 ${a.last_checkin_status === 'success' ? '成功' : a.last_checkin_status === 'already' ? '已领取' : a.last_checkin_status === 'fail' ? '失败' : a.last_checkin_status}（${a.last_checkin_date || '—'}）` : ''}
                  </div>
                </div>
                <div className="ml-auto flex items-center gap-1">
                  <button
                    onClick={() => {
                      setEditing(a);
                      setEditName(a.name || '');
                      setEditNote(a.note || '');
                    }}
                    className="rounded px-2 py-1 text-xs text-zinc-600 hover:bg-slate-100 dark:text-zinc-300 dark:hover:bg-zinc-800"
                  >
                    编辑
                  </button>
                  <button
                    onClick={() => void removeAccount(a)}
                    className="flex items-center gap-1 rounded px-2 py-1 text-xs text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-950"
                  >
                    <Trash2 size={13} /> 移除
                  </button>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* 设备码授权弹窗 */}
      <Modal open={deviceStatus !== 'idle'} onClose={() => cancelDevice()} title="MiniMax 设备码授权" widthClass="max-w-lg">
        <div className="space-y-4 text-sm">
          {deviceStatus === 'waiting' && device && (
            <>
              <p className="text-slate-600 dark:text-zinc-300">
                1. 复制用户码：<code className="rounded bg-slate-100 px-1.5 py-0.5 font-mono text-base font-semibold dark:bg-zinc-800">{device.user_code}</code>
              </p>
              <p className="text-slate-600 dark:text-zinc-300">
                2. 打开授权链接完成登录（{REGION_LABEL[device.region]}）：
              </p>
              <a
                href={device.verification_uri_complete}
                target="_blank"
                rel="noreferrer"
                className="flex items-center gap-1.5 text-blue-600 underline-offset-2 hover:underline dark:text-blue-400"
              >
                <ExternalLink size={14} /> {device.verification_uri_complete}
              </a>
              <div className="flex items-center gap-2 text-slate-500 dark:text-zinc-400">
                <Spinner className="h-4 w-4" /> {deviceMessage || '等待授权中…'}
              </div>
              <p className="text-xs text-slate-400">
                设备码 {device.expires_in} 秒内有效；授权成功后账号自动入池。此通道拿到的是独立刷新链，不影响本机 MiniMax Code 客户端登录。
              </p>
              <div className="flex justify-end">
                <button onClick={() => cancelDevice()} className="rounded-lg border border-slate-200 px-3 py-1.5 text-sm dark:border-zinc-700">
                  取消
                </button>
              </div>
            </>
          )}
          {deviceStatus === 'success' && (
            <>
              <div className="flex items-center gap-2 text-emerald-600 dark:text-emerald-400">
                <RefreshCw size={16} /> {deviceMessage}
              </div>
              <div className="flex justify-end">
                <button onClick={() => cancelDevice()} className="rounded-lg bg-zinc-900 px-3 py-1.5 text-sm text-white dark:bg-zinc-100 dark:text-zinc-900">
                  完成
                </button>
              </div>
            </>
          )}
          {deviceStatus === 'failed' && (
            <>
              <div className="text-rose-600 dark:text-rose-400">{deviceMessage}</div>
              <div className="flex justify-end gap-2">
                <button onClick={() => cancelDevice()} className="rounded-lg border border-slate-200 px-3 py-1.5 text-sm dark:border-zinc-700">
                  关闭
                </button>
                <button onClick={() => void startDevice()} className="rounded-lg bg-zinc-900 px-3 py-1.5 text-sm text-white dark:bg-zinc-100 dark:text-zinc-900">
                  重试
                </button>
              </div>
            </>
          )}
        </div>
      </Modal>

      {/* 编辑弹窗 */}
      <Modal open={editing != null} onClose={() => setEditing(null)} title="编辑账号" widthClass="max-w-md">
        <div className="space-y-3 text-sm">
          <div>
            <label className="mb-1 block text-slate-500 dark:text-zinc-400">名称</label>
            <input
              value={editName}
              onChange={(e) => setEditName(e.target.value)}
              className="w-full rounded-lg border border-slate-200 px-3 py-2 dark:border-zinc-700 dark:bg-zinc-900"
              placeholder="账号显示名"
            />
          </div>
          <div>
            <label className="mb-1 block text-slate-500 dark:text-zinc-400">备注</label>
            <input
              value={editNote}
              onChange={(e) => setEditNote(e.target.value)}
              className="w-full rounded-lg border border-slate-200 px-3 py-2 dark:border-zinc-700 dark:bg-zinc-900"
              placeholder="备注（可选）"
            />
          </div>
          <div className="flex justify-end gap-2">
            <button onClick={() => setEditing(null)} className="rounded-lg border border-slate-200 px-3 py-1.5 dark:border-zinc-700">
              取消
            </button>
            <button onClick={() => void saveEdit()} className="rounded-lg bg-zinc-900 px-3 py-1.5 text-white dark:bg-zinc-100 dark:text-zinc-900">
              保存
            </button>
          </div>
        </div>
      </Modal>
    </div>
  );
}
