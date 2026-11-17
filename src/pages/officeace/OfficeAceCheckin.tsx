import LocalCheckinPage from '../../components/LocalCheckinPage';

/** OfficeAce（华为果办）每日签到：复用本机桌面端登录态，控制面 V11 签名领取奖励。 */
export default function OfficeAceCheckin() {
  return <LocalCheckinPage platform="officeace" />;
}