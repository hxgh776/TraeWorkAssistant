import { useState } from 'react';
import PageHeader from '../../components/PageHeader';
import { Badge } from '../../components/ui';
import { useAppStore } from '../../store';
import type { Settings } from '../../types';

/** CodeArts · 环境配置页（VENU-37 重建）：调度（签到 00:20 / 积分同步 23:58，旧 3.9.3
 *  行为基准）与多账号间隔、凭证通道说明。设置经 settings_set patch 落库（同 MiniMax 机制）。 */

function HhmmInput({ value, onSave }: { value: string; onSave: (v: string) => void }) {
  const [draft, setDraft] = useState(value);
  const invalid = !/^\d{2}:\d{2}$/.test(draft.trim());
  return (
    <span className="inline-flex items-center gap-1.5">
      <input
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={() => {
          if (!invalid && draft.trim() !== value) onSave(draft.trim());
        }}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && !invalid && draft.trim() !== value) onSave(draft.trim());
        }}
        className={`h-8 w-16 rounded-lg border px-2 text-sm dark:bg-zinc-900 ${invalid ? 'border-rose-400' : 'border-slate-200 dark:border-zinc-700'}`}
        placeholder="HH:MM"
      />
      {invalid && <span className="text-xs text-rose-500">HH:MM</span>}
    </span>
  );
}

export default function CodeArtsSettings() {
  const pushToast = useAppStore((s) => s.pushToast);
  const settings = useAppStore((s) => s.settings);
  const saveSettings = useAppStore((s) => s.saveSettings);

  const patchSettings = async (patch: Partial<Settings>) => {
    try {
      await saveSettings(patch);
      pushToast('success', 'CodeArts 设置已保存');
    } catch {
      /* saveSettings 内部已 toast 并回滚 */
    }
  };

  if (!settings) {
    return (
      <div>
        <PageHeader title="CodeArts 环境配置" desc="设置加载中…" />
      </div>
    );
  }

  const syncMode = settings.codearts_credits_sync_mode || 'daily';

  return (
    <div>
      <PageHeader
        title="CodeArts 环境配置"
        desc="调度时刻 / 多账号间隔；凭证只读本机登录态，绝不写回 state.vscdb"
      />

      {/* 每日签到 */}
      <div className="mb-4 rounded-lg border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
        <div className="mb-3 flex items-center gap-2">
          <h2 className="text-sm font-medium">每日签到</h2>
          <Badge tone={settings.codearts_checkin_enabled ? 'green' : 'slate'}>
            {settings.codearts_checkin_enabled ? '已开启' : '已关闭'}
          </Badge>
        </div>
        <div className="space-y-3 text-sm">
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={settings.codearts_checkin_enabled}
              onChange={(e) => void patchSettings({ codearts_checkin_enabled: e.target.checked })}
              className="h-4 w-4 rounded"
            />
            <span>启用自动签到（应用内调度器到点执行 + 启动补签；关闭后仍可手动签到）</span>
          </label>
          <div className="flex items-center gap-2">
            <span className="text-slate-500 dark:text-zinc-400">触发时刻</span>
            <HhmmInput
              value={settings.codearts_checkin_hhmm || '00:20'}
              onSave={(v) => void patchSettings({ codearts_checkin_hhmm: v })}
            />
            <span className="text-xs text-slate-400">默认 00:20（旧 3.9.3 行为基准；与 MiniMax 00:05 错峰）</span>
          </div>
          <div className="flex items-center gap-2">
            <span className="text-slate-500 dark:text-zinc-400">多账号间隔</span>
            <input
              type="number"
              min={0}
              max={600}
              value={settings.codearts_checkin_gap_secs ?? 3}
              onChange={(e) => {
                const v = Math.max(0, Math.min(600, Number(e.target.value) || 0));
                void patchSettings({ codearts_checkin_gap_secs: v });
              }}
              className="h-8 w-20 rounded-lg border border-slate-200 px-2 text-sm dark:border-zinc-700 dark:bg-zinc-900"
            />
            <span className="text-xs text-slate-400">秒（0 = 不间隔；防上游频控）</span>
          </div>
        </div>
      </div>

      {/* 积分数据同步 */}
      <div className="mb-4 rounded-lg border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
        <div className="mb-3 flex items-center gap-2">
          <h2 className="text-sm font-medium">积分数据同步</h2>
          <Badge tone="slate">
            {syncMode === 'off' ? '已关闭' : syncMode === 'hourly' ? '每小时' : syncMode === 'daily' ? '每日定时' : `每 ${settings.codearts_credits_sync_hours || '2'} 小时`}
          </Badge>
        </div>
        <div className="space-y-3 text-sm">
          <div className="flex items-center gap-2">
            <span className="text-slate-500 dark:text-zinc-400">同步模式</span>
            <select
              value={syncMode}
              onChange={(e) => void patchSettings({ codearts_credits_sync_mode: e.target.value })}
              className="h-8 rounded-lg border border-slate-200 bg-white px-2 text-sm dark:border-zinc-700 dark:bg-zinc-900"
            >
              <option value="daily">每日定时</option>
              <option value="every_hours">每 N 小时</option>
              <option value="hourly">每小时</option>
              <option value="off">关闭</option>
            </select>
          </div>
          {syncMode === 'daily' && (
            <div className="flex items-center gap-2">
              <span className="text-slate-500 dark:text-zinc-400">触发时刻</span>
              <HhmmInput
                value={settings.codearts_credits_sync_hhmm || '23:58'}
                onSave={(v) => void patchSettings({ codearts_credits_sync_hhmm: v })}
              />
              <span className="text-xs text-slate-400">默认 23:58（旧 3.9.3 基准：积分当日领取当日有效，23:58 捕捉当日末态）</span>
            </div>
          )}
          {syncMode === 'every_hours' && (
            <div className="flex items-center gap-2">
              <span className="text-slate-500 dark:text-zinc-400">间隔小时</span>
              <input
                type="number"
                min={1}
                max={24}
                value={settings.codearts_credits_sync_hours || '2'}
                onChange={(e) => {
                  const v = Math.max(1, Math.min(24, Number(e.target.value) || 2));
                  void patchSettings({ codearts_credits_sync_hours: String(v) });
                }}
                className="h-8 w-20 rounded-lg border border-slate-200 px-2 text-sm dark:border-zinc-700 dark:bg-zinc-900"
              />
              <span className="text-xs text-slate-400">1–24 小时（默认 2）</span>
            </div>
          )}
        </div>
      </div>

      {/* 凭证说明 */}
      <div className="rounded-lg border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
        <h2 className="mb-3 text-sm font-medium">凭证与安全说明（三通道，只读优先）</h2>
        <ul className="list-disc space-y-1.5 pl-5 text-sm text-slate-600 dark:text-zinc-300">
          <li>
            <b>桌面端登录态导入（推荐）</b>：只读 <code>%APPDATA%\codearts-agent</code>（DPAPI 主密钥 +
            <code> state.vscdb</code> 华为云会话）。<b>绝不写回</b>、<b>不消费 refresh_token</b>
            （单次轮转会作废桌面端登录链）；过期时在码道客户端重新登录后重扫即可对齐。
          </li>
          <li>
            <b>浏览器 OAuth</b>：授权码 + PKCE + DPoP，本机回环回调。建立<b>独立授权链</b>（自有 DPoP 密钥），
            凭证可自动刷新，不影响桌面端登录；回调未自动返回时可粘贴回调地址兜底。
          </li>
          <li>
            <b>手工 IAM AK/SK</b>：不过期的华为云访问密钥（高级用户兜底）；如配 SecurityToken 一并填入。
          </li>
          <li>凭证仅存本应用数据目录（%APPDATA%\AIWorkAssistant），日志与错误信息不含 AK/SK/SecurityToken。</li>
          <li>
            业务请求带华为云 SDK-HMAC-SHA256 签名（含码道客户端两处非标准实现：CanonicalURI 强制尾斜杠、
            SignedHeaders 前空行）；<code>APIG.0301/0303/0602</code> 归「凭证失效/需重新登录」，不会提示「请稍后重试」。
          </li>
          <li>签到活动官方截止 2026-12-31（UTC+8）；活动结束后签到页转「无可领取」常态，不报错刷屏。</li>
        </ul>
      </div>
    </div>
  );
}
