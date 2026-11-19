import { LocalAccountsPage } from '../../components/LocalAppPages';

/** LobsterAI（网易有道龙虾）账号管理：本机单账号只读卡片（账号名 / ID / 登录态 / 重新检测）。 */
export default function LobsterAiAccounts() {
  return <LocalAccountsPage platform="lobsterai" />;
}
