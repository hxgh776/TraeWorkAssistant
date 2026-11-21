import { useCallback, useEffect, useState } from 'react';
import { KeyRound, Link2, MonitorCheck, RefreshCw, Trash2 } from 'lucide-react';
import PageHeader from '../../components/PageHeader';
import { Badge, EmptyState, Spinner } from '../../components/ui';
import { api } from '../../lib/tauri';
import { useAppStore } from '../../store';
import type { CodeArtsAccountView, CodeArtsLocalScan } from '../../types';

/** CodeArts · 账号管理页（VENU-37 重建）：
 *  三通道凭证导入（桌面端登录态扫描 / 浏览器 OAuth / 手工 AK/SK）+ 账号卡片（脱敏）。
 *  凭证本体永不下发；本机导入通道不消费 refresh_token（不破坏桌面端登录）。 */

const SOURCE_LABEL: Record<string, string> = {
  local_import: '登录态导入',
  oauth: '浏览器 OAuth',
  aksk: '手工 AK/SK',
};

export default function CodeArtsAccounts() {
  const pushToast = useAppStore((s) => s.pushToast);
  const [accounts, setAccounts] = useState<CodeArtsAccountView[]>([]);
  const [scan, setScan] = useState<CodeArtsLocalScan | null>(null);
  const [loading, setLoading] = useState(true);
  const [importing, setImporting] = useState(false);
  // OAuth 状态机：idle → authorizing（轮询中）→ done
  const [oauthBusy, setOauthBusy] = useState(false);
  const [oauthHint, setOauthHint] = useState<string | null>(null);
  // 手工 AK/SK 表单
  const [ak, setAk] = useState('');
  const [sk, setSk] = useState('');
  const [st, setSt] = useState('');
  const [manualBusy, setManualBusy] = useState(false);
  const [pasteUrl, setPasteUrl] = useState('');
  const [showManual, setShowManual] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [list, local] = await Promise.all([
        api.codearts.accountsList(),
        api.codearts.localScan().catch(() => null),
      ]);
      setAccounts(list);
      setScan(local);
    } catch (e) {
      pushToast('error', String(e));
    } finally {
      setLoading(false);
    }
  }, [pushToast]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const importLocal = async () => {
    setImporting(true);
    try {
      const view = await api.codearts.localImport();
      pushToast('success', `${(view as { _merged?: boolean })._merged ? '已更新' : '已导入'}本机登录态账号：${view.name}`);
      await refresh();
    } catch (e) {
      pushToast('error', String(e));
    } finally {
      setImporting(false);
    }
  };

  const startOauth = async () => {
    try {
      const start = await api.codearts.oauthStart();
      window.open(start.authorize_url, '_blank');
      setOauthBusy(true);
      setOauthHint('已打开华为云授权页，请登录并授权……完成后本页自动导入。');
      // 2s 轮询授权结果
      const timer = window.setInterval(async () => {
        try {
          const poll = await api.codearts.oauthPoll();
          if (poll.status === 'success') {
            window.clearInterval(timer);
            setOauthBusy(false);
            setOauthHint(null);
            pushToast('success', `OAuth 授权完成，已导入：${poll.account?.name ?? ''}`);
            await refresh();
          } else if (poll.status === 'failed') {
            window.clearInterval(timer);
            setOauthBusy(false);
            setOauthHint(poll.message ?? '授权失败，请重试');
          }
        } catch {
          window.clearInterval(timer);
          setOauthBusy(false);
          setOauthHint('授权轮询中断，请重试');
        }
      }, 2000);
    } catch (e) {
      setOauthBusy(false);
      pushToast('error', String(e));
    }
  };

  const cancelOauth = async () => {
    await api.codearts.oauthCancel().catch(() => {});
    setOauthBusy(false);
    setOauthHint(null);
  };

  const completePaste = async () => {
    if (!pasteUrl.trim()) return;
    try {
      const poll = await api.codearts.oauthCompleteUrl(pasteUrl.trim());
      if (poll.status === 'success') {
        pushToast('success', `OAuth 授权完成，已导入：${poll.account?.name ?? ''}`);
        setPasteUrl('');
        setOauthHint(null);
        await refresh();
      } else {
        pushToast('error', poll.message ?? '授权失败');
      }
    } catch (e) {
      pushToast('error', String(e));
    }
  };

  const importManual = async () => {
    if (!ak.trim() || !sk.trim()) {
      pushToast('error', 'AccessKey 与 SecretKey 均不能为空');
      return;
    }
    setManualBusy(true);
    try {
      const view = await api.codearts.importManual(ak.trim(), sk.trim(), st.trim() || undefined);
      pushToast('success', `${(view as { _merged?: boolean })._merged ? '已更新' : '已导入'} AK/SK 账号：${view.name}`);
      setAk('');
      setSk('');
      setSt('');
      await refresh();
    } catch (e) {
      pushToast('error', String(e));
    } finally {
      setManualBusy(false);
    }
  };

  const remove = async (id: string, name: string) => {
    if (!window.confirm(`移除账号「${name}」？凭证将一并清理。`)) return;
    try {
      await api.codearts.accountRemove(id);
      pushToast('success', `已移除 ${name}`);
      await refresh();
    } catch (e) {
      pushToast('error', String(e));
    }
  };

  const checkCredential = async (id: string, name: string) => {
    try {
      const s = await api.codearts.credentialStatus(id);
      if (!s.found) {
        pushToast('error', `${name}：无凭证记录`);
      } else if (s.usable) {
        const until = s.expires_at_ms ? new Date(s.expires_at_ms).toLocaleString() : '不过期（AK/SK）';
        pushToast('success', `${name}：凭证可用${s.expires_at_ms ? `，至 ${until}` : `（${until}）`}`);
      } else {
        pushToast('error', `${name}：凭证已失效${s.reason ? `（${s.reason}）` : ''}，请重新导入`);
      }
    } catch (e) {
      pushToast('error', String(e));
    }
  };

  const scanExpired = scan?.found && scan.expired;

  return (
    <div>
      <PageHeader
        title="CodeArts 账号管理"
        desc="三通道凭证导入：桌面端登录态（推荐）/ 浏览器 OAuth / 手工 AK/SK；凭证脱敏展示"
      />

      {/* 通道 A：桌面端登录态 */}
      <div className="mb-4 rounded-lg border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
        <div className="mb-3 flex items-center gap-2">
          <MonitorCheck size={16} className="text-slate-500" />
          <h2 className="text-sm font-medium">通道一：本机桌面端登录态（推荐）</h2>
        </div>
        <div className="space-y-2 text-sm">
          <div className="text-slate-500 dark:text-zinc-400">
            只读扫描 <code>%APPDATA%\codearts-agent</code> 的华为云登录态（state.vscdb + DPAPI 解密），
            <b>不写回</b>、不消费 refresh_token——桌面端登录不受影响。STS 有效期与到期状态在账号卡片与下方如实展示。
          </div>
          {scan?.found ? (
            <div className="flex flex-wrap items-center gap-2">
              <Badge tone={scanExpired ? 'amber' : 'green'}>
                {scanExpired ? '本机登录态已过期' : '本机登录态可用'}
              </Badge>
              {scan.identity_name && <span className="text-slate-600 dark:text-zinc-300">{scan.identity_name}</span>}
              {typeof scan.expires_at_ms === 'number' && scan.expires_at_ms > 0 && (
                <span className="text-xs text-slate-400">至 {new Date(scan.expires_at_ms).toLocaleString()}</span>
              )}
              <button
                onClick={() => void importLocal()}
                disabled={importing || scanExpired}
                className="ml-auto flex h-8 items-center gap-1.5 rounded-lg bg-zinc-900 px-3 text-xs font-medium text-white transition hover:bg-zinc-800 disabled:opacity-40 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-zinc-200"
              >
                {importing ? <Spinner className="h-3.5 w-3.5" /> : <RefreshCw size={13} />}
                {scanExpired ? '请在码道客户端重新登录后重扫' : '扫描并导入'}
              </button>
            </div>
          ) : (
            <div className="flex flex-wrap items-center gap-2 text-slate-500 dark:text-zinc-400">
              {scan?.reason || '未找到本机登录态'}
              <button
                onClick={() => void importLocal()}
                disabled={importing}
                className="ml-auto flex h-8 items-center gap-1.5 rounded-lg border border-slate-200 px-3 text-xs transition hover:bg-slate-50 disabled:opacity-40 dark:border-zinc-700 dark:hover:bg-zinc-800"
              >
                {importing ? <Spinner className="h-3.5 w-3.5" /> : <RefreshCw size={13} />}
                重新扫描
              </button>
            </div>
          )}
        </div>
      </div>

      {/* 通道 B：浏览器 OAuth */}
      <div className="mb-4 rounded-lg border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
        <div className="mb-3 flex items-center gap-2">
          <Link2 size={16} className="text-slate-500" />
          <h2 className="text-sm font-medium">通道二：浏览器 OAuth 授权</h2>
        </div>
        <div className="space-y-2 text-sm">
          <div className="text-slate-500 dark:text-zinc-400">
            打开华为云授权页完成登录授权（本机回环回调，端口 ≥10000）。此通道建立<b>独立授权链</b>（自有 DPoP 密钥），
            凭证可自动刷新，不影响桌面端登录。
          </div>
          {oauthBusy ? (
            <div className="flex flex-wrap items-center gap-2">
              <Spinner className="h-4 w-4" />
              <span className="text-slate-600 dark:text-zinc-300">{oauthHint ?? '等待授权完成……'}</span>
              <button onClick={() => void cancelOauth()} className="ml-auto text-xs text-rose-500 hover:underline">
                取消授权
              </button>
            </div>
          ) : (
            <div className="flex flex-wrap items-center gap-2">
              <button
                onClick={() => void startOauth()}
                className="flex h-8 items-center gap-1.5 rounded-lg bg-zinc-900 px-3 text-xs font-medium text-white transition hover:bg-zinc-800 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-zinc-200"
              >
                <Link2 size={13} />
                开始授权
              </button>
              <span className="text-xs text-slate-400">回调未自动返回时，把浏览器最终地址粘贴到下方：</span>
            </div>
          )}
          <div className="flex gap-2">
            <input
              value={pasteUrl}
              onChange={(e) => setPasteUrl(e.target.value)}
              placeholder="粘贴回调地址（含 code= 参数的完整 URL）"
              className="h-8 min-w-0 flex-1 rounded-lg border border-slate-200 px-2 text-sm dark:border-zinc-700 dark:bg-zinc-900"
            />
            <button
              onClick={() => void completePaste()}
              disabled={!pasteUrl.trim()}
              className="h-8 shrink-0 rounded-lg border border-slate-200 px-3 text-xs transition hover:bg-slate-50 disabled:opacity-40 dark:border-zinc-700 dark:hover:bg-zinc-800"
            >
              导入
            </button>
          </div>
          {oauthHint && !oauthBusy && <div className="text-xs text-amber-500">{oauthHint}</div>}
        </div>
      </div>

      {/* 通道 C：手工 AK/SK */}
      <div className="mb-4 rounded-lg border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
        <div className="mb-3 flex items-center gap-2">
          <KeyRound size={16} className="text-slate-500" />
          <h2 className="text-sm font-medium">通道三：手工 IAM AK/SK（高级）</h2>
          <button onClick={() => setShowManual(!showManual)} className="ml-auto text-xs text-zinc-500 hover:underline dark:text-zinc-400">
            {showManual ? '收起' : '展开'}
          </button>
        </div>
        {showManual && (
          <div className="space-y-2 text-sm">
            <div className="text-slate-500 dark:text-zinc-400">
              华为云 IAM 永久 AccessKey/SecretKey（不过期；如配 SecurityToken 一并填入）。仅存本应用数据目录，不上传。
            </div>
            <div className="grid gap-2 sm:grid-cols-3">
              <input
                value={ak}
                onChange={(e) => setAk(e.target.value)}
                placeholder="AccessKey"
                autoComplete="off"
                className="h-8 rounded-lg border border-slate-200 px-2 dark:border-zinc-700 dark:bg-zinc-900"
              />
              <input
                value={sk}
                onChange={(e) => setSk(e.target.value)}
                placeholder="SecretKey"
                type="password"
                autoComplete="off"
                className="h-8 rounded-lg border border-slate-200 px-2 dark:border-zinc-700 dark:bg-zinc-900"
              />
              <input
                value={st}
                onChange={(e) => setSt(e.target.value)}
                placeholder="SecurityToken（可选）"
                autoComplete="off"
                className="h-8 rounded-lg border border-slate-200 px-2 dark:border-zinc-700 dark:bg-zinc-900"
              />
            </div>
            <button
              onClick={() => void importManual()}
              disabled={manualBusy}
              className="flex h-8 items-center gap-1.5 rounded-lg bg-zinc-900 px-3 text-xs font-medium text-white transition hover:bg-zinc-800 disabled:opacity-40 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-zinc-200"
            >
              {manualBusy ? <Spinner className="h-3.5 w-3.5" /> : <KeyRound size={13} />}
              导入 AK/SK
            </button>
          </div>
        )}
      </div>

      {/* 账号列表 */}
      <div className="rounded-lg border border-slate-200 bg-white dark:border-zinc-800 dark:bg-zinc-900">
        <div className="flex items-center justify-between border-b border-slate-100 px-4 py-3 dark:border-zinc-800">
          <h2 className="text-sm font-medium">账号列表（{accounts.length}）</h2>
          <button onClick={() => void refresh()} className="text-xs text-zinc-500 hover:text-zinc-700 dark:text-zinc-400 dark:hover:text-zinc-200">
            刷新
          </button>
        </div>
        {loading ? (
          <div className="flex justify-center py-8"><Spinner className="h-5 w-5" /></div>
        ) : accounts.length === 0 ? (
          <EmptyState title="暂无账号" hint="先通过上方任一通道导入凭证，再到「每日签到」领取积分。" />
        ) : (
          <div className="divide-y divide-slate-100 dark:divide-zinc-800">
            {accounts.map((a) => (
              <div key={a.id} className="flex flex-wrap items-center gap-2 px-4 py-3 text-sm">
                <span className="font-medium text-slate-800 dark:text-zinc-100">{a.name}</span>
                <Badge tone="slate">{SOURCE_LABEL[a.credential_source] || a.credential_source || '—'}</Badge>
                {a.needs_relogin && <Badge tone="amber">需重新导入</Badge>}
                <span className="text-xs text-slate-400">{a.id}</span>
                {typeof a.token_expires_at_ms === 'number' && a.token_expires_at_ms > 0 && (
                  <span className="text-xs text-slate-400">凭证至 {new Date(a.token_expires_at_ms).toLocaleString()}</span>
                )}
                {a.last_checkin_status && (
                  <span className="text-xs text-slate-400">
                    最近签到：{a.last_checkin_date ?? '—'}（{a.last_checkin_status}）
                  </span>
                )}
                <div className="ml-auto flex items-center gap-2">
                  <button
                    onClick={() => void checkCredential(a.id, a.name)}
                    className="rounded-lg border border-slate-200 px-2.5 py-1 text-xs transition hover:bg-slate-50 dark:border-zinc-700 dark:hover:bg-zinc-800"
                  >
                    重新检测
                  </button>
                  <button
                    onClick={() => void remove(a.id, a.name)}
                    className="flex items-center gap-1 rounded-lg border border-rose-200 px-2.5 py-1 text-xs text-rose-500 transition hover:bg-rose-50 dark:border-rose-900 dark:hover:bg-rose-950"
                  >
                    <Trash2 size={12} /> 移除
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
