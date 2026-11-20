import { useState } from 'react';
import PageHeader from '../../components/PageHeader';
import { Badge } from '../../components/ui';
import { useAppStore } from '../../store';
import type { Settings } from '../../types';

/** MiniMax · 环境配置页（VENU-36 重建）：调度（签到/积分同步）与双区开关、
 *  凭证通道说明。设置经 settings_set patch 落库（与 OfficeAce/LobsterAI 同机制）。 */

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

export default function MiniMaxSettings() {
  const pushToast = useAppStore((s) => s.pushToast);
  const settings = useAppStore((s) => s.settings);
  const saveSettings = useAppStore((s) => s.saveSettings);

  const patchSettings = async (patch: Partial<Settings>) => {
    try {
      await saveSettings(patch);
      pushToast('success', 'MiniMax 设置已保存');
    } catch {
      /* saveSettings 内部已 toast 并回滚 */
    }
  };

  if (!settings) {
    return (
      <div>
        <PageHeader title="MiniMax 环境配置" desc="设置加载中…" />
      </div>
    );
  }

  const syncMode = settings.minimax_credits_sync_mode || 'every_hours';

  return (
    <div>
      <PageHeader
        title="MiniMax 环境配置"
        desc="调度时刻 / 双区开关 / 积分同步；凭证只读本机登录态，绝不写回 MiniMax 客户端"
      />

      {/* 每日签到 */}
      <div className="mb-4 rounded-lg border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
        <div className="mb-3 flex items-center gap-2">
          <h2 className="text-sm font-medium">每日签到</h2>
          <Badge tone={settings.minimax_checkin_enabled ? 'green' : 'slate'}>
            {settings.minimax_checkin_enabled ? '已开启' : '已关闭'}
          </Badge>
        </div>
        <div className="space-y-3 text-sm">
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={settings.minimax_checkin_enabled}
              onChange={(e) => void patchSettings({ minimax_checkin_enabled: e.target.checked })}
              className="h-4 w-4 rounded"
            />
            <span>启用自动签到（应用内调度器到点执行 + 启动补签；关闭后仍可手动签到）</span>
          </label>
          <div className="flex items-center gap-2">
            <span className="text-slate-500 dark:text-zinc-400">触发时刻</span>
            <HhmmInput
              value={settings.minimax_checkin_hhmm || '00:05'}
              onSave={(v) => void patchSettings({ minimax_checkin_hhmm: v })}
            />
            <span className="text-xs text-slate-400">默认 00:05（跨零点即可领当日积分，旧 3.9.3 行为基准）</span>
          </div>
          <div className="flex items-center gap-2">
            <span className="text-slate-500 dark:text-zinc-400">多账号间隔</span>
            <input
              type="number"
              min={0}
              max={600}
              value={settings.minimax_checkin_gap_secs ?? 3}
              onChange={(e) => {
                const v = Math.max(0, Math.min(600, Number(e.target.value) || 0));
                void patchSettings({ minimax_checkin_gap_secs: v });
              }}
              className="h-8 w-20 rounded-lg border border-slate-200 px-2 text-sm dark:border-zinc-700 dark:bg-zinc-900"
            />
            <span className="text-xs text-slate-400">秒（0 = 不间隔；防上游频控）</span>
          </div>
        </div>
      </div>

      {/* 双区开关 */}
      <div className="mb-4 rounded-lg border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
        <h2 className="mb-3 text-sm font-medium">区域开关</h2>
        <div className="space-y-3 text-sm">
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={settings.minimax_cn_enabled !== false}
              onChange={(e) => void patchSettings({ minimax_cn_enabled: e.target.checked })}
              className="h-4 w-4 rounded"
            />
            <Badge tone="blue">国内区</Badge>
            <span className="text-slate-500 dark:text-zinc-400">agent.minimax.cn 账号参与签到与积分同步</span>
          </label>
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={settings.minimax_global_enabled !== false}
              onChange={(e) => void patchSettings({ minimax_global_enabled: e.target.checked })}
              className="h-4 w-4 rounded"
            />
            <Badge tone="violet">国际区</Badge>
            <span className="text-slate-500 dark:text-zinc-400">agent.minimax.io 账号参与签到与积分同步（大陆网络直连可能 307，需代理兜底）</span>
          </label>
        </div>
      </div>

      {/* 积分数据同步 */}
      <div className="mb-4 rounded-lg border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
        <div className="mb-3 flex items-center gap-2">
          <h2 className="text-sm font-medium">积分数据同步</h2>
          <Badge tone="slate">
            {syncMode === 'off' ? '已关闭' : syncMode === 'hourly' ? '每小时' : syncMode === 'daily' ? '每日' : `每 ${settings.minimax_credits_sync_hours || '2'} 小时`}
          </Badge>
        </div>
        <div className="space-y-3 text-sm">
          <div className="flex items-center gap-2">
            <span className="text-slate-500 dark:text-zinc-400">同步模式</span>
            <select
              value={syncMode}
              onChange={(e) => void patchSettings({ minimax_credits_sync_mode: e.target.value })}
              className="h-8 rounded-lg border border-slate-200 bg-white px-2 text-sm dark:border-zinc-700 dark:bg-zinc-900"
            >
              <option value="every_hours">每 N 小时</option>
              <option value="hourly">每小时</option>
              <option value="daily">每日定时</option>
              <option value="off">关闭</option>
            </select>
          </div>
          {syncMode === 'every_hours' && (
            <div className="flex items-center gap-2">
              <span className="text-slate-500 dark:text-zinc-400">间隔小时</span>
              <input
                type="number"
                min={1}
                max={24}
                value={settings.minimax_credits_sync_hours || '2'}
                onChange={(e) => {
                  const v = Math.max(1, Math.min(24, Number(e.target.value) || 2));
                  void patchSettings({ minimax_credits_sync_hours: String(v) });
                }}
                className="h-8 w-20 rounded-lg border border-slate-200 px-2 text-sm dark:border-zinc-700 dark:bg-zinc-900"
              />
              <span className="text-xs text-slate-400">1–24 小时（默认 2）</span>
            </div>
          )}
          {syncMode === 'daily' && (
            <div className="flex items-center gap-2">
              <span className="text-slate-500 dark:text-zinc-400">触发时刻</span>
              <HhmmInput
                value={settings.minimax_credits_sync_hhmm || '23:35'}
                onSave={(v) => void patchSettings({ minimax_credits_sync_hhmm: v })}
              />
              <span className="text-xs text-slate-400">默认 23:35（错峰其他平台看板同步）</span>
            </div>
          )}
        </div>
      </div>

      {/* 凭证说明 */}
      <div className="rounded-lg border border-slate-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
        <h2 className="mb-3 text-sm font-medium">凭证与安全说明</h2>
        <ul className="list-disc space-y-1.5 pl-5 text-sm text-slate-600 dark:text-zinc-300">
          <li>
            <b>设备码授权（推荐）</b>：OAuth 设备码流（RFC 8628 + S256 PKCE，client <code>mcode-public</code>），
            拿到的是独立刷新链，与本机 MiniMax Code 客户端互不影响，可自动刷新。
          </li>
          <li>
            <b>本机登录态导入</b>：只读 <code>~/.minimax/auth/prod/&#123;cn|en&#125;/mcode-public/auth.json</code>。
            该通道<b>不主动刷新</b>（refresh token 一次性轮换，主动刷新会作废客户端登录链）；
            过期时请打开 MiniMax Code 客户端（自动续期）后重试，或改用设备码授权。
          </li>
          <li>凭证仅存本应用数据目录（%APPDATA%\AIWorkAssistant），不写回 MiniMax 客户端文件，日志与错误信息不含 token。</li>
          <li>业务请求带客户端签名（yy / x-signature，常量来自社区逆向，客户端升级更换时集中热更）；MiniMax 无 PAT 通道。</li>
        </ul>
      </div>
    </div>
  );
}
