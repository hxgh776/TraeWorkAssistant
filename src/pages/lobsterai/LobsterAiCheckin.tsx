import LocalCheckinPage from '../../components/LocalCheckinPage';

/** LobsterAI（网易有道龙虾）每日签到：复用本机桌面端登录态，走活动 slot/check_in 协议。 */
export default function LobsterAiCheckin() {
  return <LocalCheckinPage platform="lobsterai" />;
}