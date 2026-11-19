// OfficeAce（华为果办）/ LobsterAI（网易有道龙虾）两应用共用元数据：
// 五页子导航（概述 / 账号管理 / 每日签到 / 积分看板 / 环境配置）同构渲染，
// 页面组件在 components/LocalAppPages.tsx，薄壳页在 pages/{officeace,lobsterai}/。
// 注意：签到页（LocalCheckinPage）自带一套 META，本文件不与其合并，避免牵动已上线签到页。
import { api } from './tauri';
import type {
  LocalCheckinRecord,
  LobsterAiQuota,
  LobsterAiStatus,
  OfficeAceQuota,
  OfficeAceStatus,
  Settings,
} from '../types';

export type LocalPlatform = 'officeace' | 'lobsterai';

interface LocalAppMetaBase {
  /** 应用 Tab 名（英文） */
  name: string;
  /** 全称（中文说明） */
  fullName: string;
  /** 本机登录态数据源路径（环境配置页展示） */
  dataPath: string;
  /** 数据源补充说明 */
  dataPathNote: string;
  /** 凭据只读说明（环境配置页展示） */
  credentialNote: string;
  enabledKey: 'officeace_checkin_enabled' | 'lobsterai_checkin_enabled';
  hhmmKey: 'officeace_checkin_hhmm' | 'lobsterai_checkin_hhmm';
  defaultHhmm: string;
  status: () => Promise<OfficeAceStatus | LobsterAiStatus>;
  results: (days: number) => Promise<LocalCheckinRecord[]>;
  quota: () => Promise<OfficeAceQuota | LobsterAiQuota>;
}

const OFFICEACE_META: LocalAppMetaBase = {
  name: 'OfficeAce',
  fullName: 'OfficeAce（华为果办）',
  dataPath: '%LOCALAPPDATA%\\Programs\\OfficeAce\\data\\storage.sqlite',
  dataPathNote: '登录会话存于 oc_kv_string 表（office-claw:oc:session:user:*），模型网关与临时凭证经 Windows 凭据管理器 provider-state 解出。',
  credentialNote:
    '签到与积分查询复用桌面端登录态：只读 Windows 凭据管理器与本地状态库，不写回、不落盘凭据、不消费一次性 refresh token。控制面临时凭证（AK/SK/STS）约 2 小时有效，过期请打开并登录 OfficeAce 桌面端后重试。',
  enabledKey: 'officeace_checkin_enabled',
  hhmmKey: 'officeace_checkin_hhmm',
  defaultHhmm: '09:40',
  status: () => api.officeace.status(),
  results: (days) => api.officeace.checkinResults(days),
  quota: () => api.officeace.quota(),
};

const LOBSTERAI_META: LocalAppMetaBase = {
  name: 'LobsterAI',
  fullName: 'LobsterAI（网易有道龙虾）',
  dataPath: '%APPDATA%\\LobsterAI\\lobsterai.sqlite',
  dataPathNote: '登录令牌存于 kv 表 auth_tokens（accessToken）；令牌由桌面端自动续期，本应用只读不消费。',
  credentialNote:
    '签到与积分查询复用桌面端登录态：只读本地数据库中的访问令牌，不写回、不落盘凭据（refresh token 一次性轮换，消费会破坏桌面端登录态）。令牌过期请打开 LobsterAI 桌面端（会自动续期）后重试。',
  enabledKey: 'lobsterai_checkin_enabled',
  hhmmKey: 'lobsterai_checkin_hhmm',
  defaultHhmm: '09:50',
  status: () => api.lobsterai.status(),
  results: (days) => api.lobsterai.checkinResults(days),
  quota: () => api.lobsterai.quota(),
};

export const LOCAL_APP_META: Record<LocalPlatform, LocalAppMetaBase> = {
  officeace: OFFICEACE_META,
  lobsterai: LOBSTERAI_META,
};

/** 设置补丁键（自动签到开关 / 时刻），供环境配置页 patchSettings 用 */
export type LocalCheckinSettingsPatch = Partial<Pick<Settings, 'officeace_checkin_enabled' | 'officeace_checkin_hhmm' | 'lobsterai_checkin_enabled' | 'lobsterai_checkin_hhmm'>>;
