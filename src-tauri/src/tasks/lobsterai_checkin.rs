//! LobsterAI（网易有道龙虾）每日签到。
//!
//! 协议依据（社区逆向实现 LobsterAI-AutoCheckin，2026-11 实测）：
//! 1. `GET /api/client-activities/slot?placement=desktop_sidebar&clientVersion=<ver>
//!    &containerApiVersion=2&platform=win32` → 活动槽（activityCode/configRevision）。
//! 2. `GET /api/client-activities/<code>/context?configRevision=<rev>` →
//!    state.claimedToday / claimedDays / streakDays 与 actions（含 `check_in`）。
//! 3. `POST /api/client-activities/<code>/actions/check_in`
//!    body `{configRevision, idempotencyKey, payload:{}}` → 发放积分。
//! 4. 再查一次 context 复核 `claimedToday` / `claimedDays` 确实变化，才计成功。
//!
//! 鉴权：`Authorization: Bearer <accessToken>` + `User-Agent: LobsterAI/<客户端版本>`；
//! 客户端版本从官方更新接口取（失败用兜底常量）。
//!
//! 登录态来源：本机桌面端 `%APPDATA%\LobsterAI\lobsterai.sqlite` 的 `kv` 表
//! `auth_tokens`（`{accessToken, refreshToken}`）。只读，不写回、不续期
//! （refresh token 一次性轮换，后台消费会破坏桌面端登录态）。

use serde::Serialize;
use serde_json::{json, Value};

use crate::state::AppState;

const DEFAULT_BASE: &str = "https://lobsterai-server.youdao.com";
const UPDATE_API_URL: &str =
    "https://api-overmind.youdao.com/openapi/get/luna/hardware/lobsterai/prod/update";
const DEFAULT_CLIENT_VERSION: &str = "2026.9.4";
const PLACEMENT_SLOT: &str = "desktop_sidebar";

/// 本机登录态（脱敏视图）。
#[derive(Serialize, Clone, Default)]
pub struct LobsterAiStatus {
    pub installed: bool,
    pub logged_in: bool,
    pub user_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_expires_at: Option<i64>,
    pub token_expired: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Default)]
struct LocalAccount {
    user_id: String,
    name: String,
    access_token: String,
    token_expires_at: Option<i64>,
}

// ── 本机登录态 ──────────────────────────────────────────────────────────────

fn sqlite_path() -> Option<std::path::PathBuf> {
    for key in ["APPDATA", "LOCALAPPDATA"] {
        if let Ok(root) = std::env::var(key) {
            let path = std::path::PathBuf::from(root)
                .join("LobsterAI")
                .join("lobsterai.sqlite");
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn read_tokens() -> Result<Option<(String, String)>, String> {
    let Some(path) = sqlite_path() else {
        return Ok(None);
    };
    use rusqlite::{Connection, OpenFlags};
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("打开 LobsterAI 本地数据库失败：{e}"))?;
    let raw: Option<String> = conn
        .query_row("select value from kv where key = ?1", ["auth_tokens"], |row| {
            row.get::<_, String>(0)
        })
        .ok();
    let Some(raw) = raw else {
        return Ok(None);
    };
    let value: Value =
        serde_json::from_str(&raw).map_err(|e| format!("LobsterAI auth_tokens 解析失败：{e}"))?;
    let access = text_of(&value, &["accessToken", "access_token", "token"]);
    let refresh = text_of(&value, &["refreshToken", "refresh_token"]);
    if access.is_empty() {
        return Ok(None);
    }
    let _ = refresh;
    Ok(Some((access, String::new())))
}

fn text_of(v: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = v.get(*key).and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

fn jwt_exp(token: &str) -> Option<i64> {
    crate::jwt::parse(token).exp_timestamp
}

fn jwt_uid(token: &str) -> String {
    crate::jwt::parse(token).user_id.unwrap_or_default()
}

fn local_account() -> Result<Option<LocalAccount>, String> {
    let Some((access_token, _)) = read_tokens()? else {
        return Ok(None);
    };
    let user_id = jwt_uid(&access_token);
    let token_expires_at = jwt_exp(&access_token);
    Ok(Some(LocalAccount {
        user_id: if user_id.is_empty() {
            "lobsterai-local".to_string()
        } else {
            user_id
        },
        name: "LobsterAI 账号".to_string(),
        access_token,
        token_expires_at,
    }))
}

pub fn status() -> LobsterAiStatus {
    let installed = sqlite_path().is_some();
    if !installed {
        return LobsterAiStatus {
            installed: false,
            error: Some("未检测到 LobsterAI 桌面端（%APPDATA%\\LobsterAI）".into()),
            ..Default::default()
        };
    }
    match local_account() {
        Ok(Some(account)) => {
            let expired = account
                .token_expires_at
                .map(|exp| exp <= chrono::Utc::now().timestamp())
                .unwrap_or(false);
            LobsterAiStatus {
                installed: true,
                logged_in: !expired,
                user_id: account.user_id,
                name: account.name,
                token_expires_at: account.token_expires_at,
                token_expired: expired,
                error: if expired {
                    Some("登录态已过期：请打开 LobsterAI 桌面端（会自动续期）后重试".into())
                } else {
                    None
                },
            }
        }
        Ok(None) => LobsterAiStatus {
            installed: true,
            error: Some("LobsterAI 桌面端未登录：请打开客户端完成登录".into()),
            ..Default::default()
        },
        Err(e) => LobsterAiStatus {
            installed: true,
            error: Some(e),
            ..Default::default()
        },
    }
}

// ── 客户端版本 ──────────────────────────────────────────────────────────────

fn fetch_latest_version() -> Option<String> {
    let agent = crate::tasks::http_agent(8);
    let (status, parsed, _) = crate::tasks::wb_common::get_json(
        &agent,
        UPDATE_API_URL,
        &[("Accept".into(), "application/json".into())],
    );
    if status != 200 {
        return None;
    }
    let version = parsed
        .as_ref()
        .and_then(|v| v.get("data"))
        .and_then(|v| v.get("value"))
        .and_then(|v| v.get("version"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let trimmed = version.trim();
    let valid = !trimmed.is_empty()
        && trimmed.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    if valid {
        Some(trimmed.to_string())
    } else {
        None
    }
}

fn client_version() -> String {
    fetch_latest_version().unwrap_or_else(|| DEFAULT_CLIENT_VERSION.to_string())
}

// ── 签到协议 ────────────────────────────────────────────────────────────────

fn unwrap_envelope(status: u16, parsed: Option<Value>, raw: &str) -> Result<Value, String> {
    let Some(value) = parsed else {
        return Err(format!("HTTP {status} 响应不是 JSON：{}", excerpt(raw)));
    };
    if status != 200 {
        let message = text_of(&value, &["msg", "message", "error_msg"]);
        return Err(format!(
            "HTTP {status}{}",
            if message.is_empty() {
                String::new()
            } else {
                format!("：{message}")
            }
        ));
    }
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code != 0 {
        let message = text_of(&value, &["msg", "message"]);
        return Err(format!("上游返回 code={code}：{message}"));
    }
    Ok(value.get("data").cloned().unwrap_or(Value::Null))
}

fn excerpt(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect()
}

fn api_get(account: &LocalAccount, version: &str, path: &str) -> Result<Value, String> {
    let url = format!("{DEFAULT_BASE}{path}");
    let agent = crate::tasks::http_agent(30);
    let headers = [
        ("Authorization".to_string(), format!("Bearer {}", account.access_token)),
        ("Accept".to_string(), "application/json".to_string()),
        ("User-Agent".to_string(), format!("LobsterAI/{version}")),
    ];
    let (status, parsed, raw) = crate::tasks::wb_common::get_json(&agent, &url, &headers);
    unwrap_envelope(status, parsed, &raw)
}

fn api_post(account: &LocalAccount, version: &str, path: &str, body: &Value) -> Result<Value, String> {
    let url = format!("{DEFAULT_BASE}{path}");
    let agent = crate::tasks::http_agent(30);
    let headers = [
        ("Authorization".to_string(), format!("Bearer {}", account.access_token)),
        ("Accept".to_string(), "application/json".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("User-Agent".to_string(), format!("LobsterAI/{version}")),
    ];
    let (status, parsed, raw) = crate::tasks::wb_common::post_json_raw(&agent, &url, &headers, body);
    unwrap_envelope(status, parsed, &raw)
}

fn extract_credits(data: &Value) -> f64 {
    let keys = [
        "creditsGranted",
        "rewardCredits",
        "credits",
        "grantedCredits",
        "creditAmount",
    ];
    for key in keys {
        if let Some(value) = data.get(key) {
            let parsed = match value {
                Value::Number(n) => n.as_f64(),
                Value::String(s) => s.trim().parse::<f64>().ok(),
                _ => None,
            };
            if let Some(points) = parsed.filter(|v| *v > 0.0) {
                return points;
            }
        }
    }
    data.get("result")
        .map(extract_credits)
        .unwrap_or(0.0)
}

fn str_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim().to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default()
}

fn int_of(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

/// 一次签到：返回 (status, message, reward)。
fn do_checkin(account: &LocalAccount) -> (String, String, Option<f64>) {
    let version = client_version();
    let slot_path = format!(
        "/api/client-activities/slot?placement={PLACEMENT_SLOT}&clientVersion={version}&containerApiVersion=2&platform=win32"
    );
    let slot = match api_get(account, &version, &slot_path) {
        Ok(value) => value,
        Err(e) => return ("fail".to_string(), e, None),
    };
    let activity = slot.get("activity").cloned().unwrap_or(Value::Null);
    let slot_state = text_of(&slot, &["slotState"]);
    if activity.is_null() || (!slot_state.is_empty() && slot_state != "available") {
        return (
            "already".to_string(),
            format!("上游未投放签到活动（slotState={slot_state}）"),
            None,
        );
    }
    let code = text_of(&activity, &["activityCode"]);
    if code.is_empty() {
        return ("fail".to_string(), "签到活动缺少 activityCode".to_string(), None);
    }
    let mut revision = activity
        .get("configRevision")
        .and_then(Value::as_i64)
        .unwrap_or(0);

    let context_path = |rev: i64| {
        format!("/api/client-activities/{code}/context?configRevision={rev}")
    };
    let ctx = match api_get(account, &version, &context_path(revision)) {
        Ok(value) => value,
        Err(e) => return ("fail".to_string(), e, None),
    };
    if let Some(rev) = ctx.get("configRevision").and_then(Value::as_i64) {
        revision = rev;
    }
    let state = ctx.get("state").cloned().unwrap_or(Value::Null);
    let actions = str_list(ctx.get("actions"));
    let claimed_today = state
        .get("claimedToday")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let claimed_days = int_of(state.get("claimedDays"));
    let mut streak = int_of(state.get("streakDays"));
    if streak == 0 {
        streak = int_of(state.get("streak"));
    }
    let total_days = int_of(state.get("totalDays"));

    if claimed_today || !actions.is_empty() && !actions.contains(&"check_in".to_string()) {
        let message = if claimed_today && streak > 0 {
            format!("今天已签到（连登 {streak} 天）")
        } else {
            "今天已签到".to_string()
        };
        return ("already".to_string(), message, None);
    }

    let body = json!({
        "configRevision": revision,
        "idempotencyKey": uuid::Uuid::new_v4().to_string(),
        "payload": {},
    });
    let action_path = format!("/api/client-activities/{code}/actions/check_in");
    let response = match api_post(account, &version, &action_path, &body) {
        Ok(value) => value,
        Err(e) => return ("fail".to_string(), e, None),
    };
    let granted = extract_credits(&response);

    // 复核：只有 context 里 claimedToday / claimedDays 确实变化才计成功
    let mut claimed = granted > 0.0;
    let mut new_claimed_days = claimed_days;
    if let Ok(after) = api_get(account, &version, &context_path(revision)) {
        let after_state = after.get("state").cloned().unwrap_or(Value::Null);
        let after_claimed_today = after_state
            .get("claimedToday")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let after_days = int_of(after_state.get("claimedDays"));
        if after_claimed_today || after_days > claimed_days {
            claimed = true;
        }
        if after_days > 0 {
            new_claimed_days = after_days;
        }
        let after_streak = {
            let primary = int_of(after_state.get("streakDays"));
            if primary > 0 {
                primary
            } else {
                int_of(after_state.get("streak"))
            }
        };
        if after_streak > 0 {
            streak = after_streak;
        }
    }
    if !claimed {
        return ("already".to_string(), "本次未发放（今日已领过）".to_string(), None);
    }
    if new_claimed_days == 0 {
        new_claimed_days = claimed_days + 1;
    }
    let remaining = if total_days > 0 {
        total_days - new_claimed_days
    } else {
        0
    };

    let mut message = if granted > 0.0 {
        format!("签到成功 +{} 分", granted)
    } else {
        "签到成功".to_string()
    };
    if streak > 0 {
        message.push_str(&format!("（连登 {streak} 天）"));
    }
    if remaining > 0 {
        message.push_str(&format!("，剩余 {remaining} 天"));
    }
    ("success".to_string(), message, Some(granted).filter(|v| *v > 0.0))
}

// ── 积分 / 连签数据（只读，签到同一份本机登录态） ────────────────────────────

/// 积分 / 连签查询结果（client-activities context；脱敏：不含令牌）。
#[derive(Serialize, Clone)]
pub struct LobsterAiQuota {
    pub user_id: String,
    pub name: String,
    /// 签到活动 code（上游未投放活动时为空串）
    pub activity_code: String,
    pub claimed_today: bool,
    pub claimed_days: i64,
    pub streak_days: i64,
    /// 活动总天数（上游未下发为 0）
    pub total_days: i64,
    /// context 下发的可用动作（含 check_in = 今日可签）
    pub actions: Vec<String>,
    /// 当前积分余额（上游未下发余额字段时 None，前端显示「未知」而非 0）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub balance: Option<f64>,
    /// 本地查询时刻（YYYY-MM-DD HH:MM:SS）
    pub fetched_at: String,
}

/// 余额字段宽容提取：只认明确表示余额的键（credits / balance 系），
/// 认不出返回 None——不猜、不冒充 0。
fn balance_of(state: &Value) -> Option<f64> {
    const KEYS: [&str; 5] = ["credits", "creditsBalance", "creditBalance", "balance", "remainCredits"];
    for key in KEYS {
        let Some(value) = state.get(key) else { continue };
        let parsed = match value {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        };
        if let Some(v) = parsed.filter(|v| *v >= 0.0 && v.is_finite()) {
            return Some(v);
        }
    }
    None
}

/// 读取积分 / 连签数据。上游不可达 / 登录态缺失时返回 Err——调用方必须展示
/// 错误而不是 0；上游未下发余额字段时 balance=None（如实「未知」）。
pub fn quota() -> Result<LobsterAiQuota, String> {
    let Some(account) = local_account()? else {
        return Err("未检测到 LobsterAI 本机登录态：请先安装并登录 LobsterAI 桌面端".into());
    };
    if let Some(exp) = account.token_expires_at {
        if exp <= chrono::Utc::now().timestamp() {
            return Err("登录态已过期：请打开 LobsterAI 桌面端（会自动续期）后重试".into());
        }
    }
    quota_from_context(&account)
}

fn quota_from_context(account: &LocalAccount) -> Result<LobsterAiQuota, String> {
    let version = client_version();
    let slot_path = format!(
        "/api/client-activities/slot?placement={PLACEMENT_SLOT}&clientVersion={version}&containerApiVersion=2&platform=win32"
    );
    let slot = api_get(account, &version, &slot_path)?;
    let activity = slot.get("activity").cloned().unwrap_or(Value::Null);
    let slot_state = text_of(&slot, &["slotState"]);
    if activity.is_null() || (!slot_state.is_empty() && slot_state != "available") {
        return Err(format!("上游未投放签到活动（slotState={slot_state}），无法读取积分数据"));
    }
    let code = text_of(&activity, &["activityCode"]);
    if code.is_empty() {
        return Err("签到活动缺少 activityCode，无法读取积分数据".into());
    }
    let revision = activity.get("configRevision").and_then(Value::as_i64).unwrap_or(0);
    let ctx = api_get(
        account,
        &version,
        &format!("/api/client-activities/{code}/context?configRevision={revision}"),
    )?;
    let state = ctx.get("state").cloned().unwrap_or(Value::Null);
    let mut streak = int_of(state.get("streakDays"));
    if streak == 0 {
        streak = int_of(state.get("streak"));
    }
    Ok(LobsterAiQuota {
        user_id: account.user_id.clone(),
        name: account.name.clone(),
        activity_code: code,
        claimed_today: state
            .get("claimedToday")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        claimed_days: int_of(state.get("claimedDays")),
        streak_days: streak,
        total_days: int_of(state.get("totalDays")),
        actions: str_list(ctx.get("actions"))
            .into_iter()
            .map(|action| action.to_string())
            .collect(),
        balance: balance_of(&state),
        fetched_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    })
}

// ── 签到轮次 ────────────────────────────────────────────────────────────────

#[derive(Serialize, Clone)]
pub struct LobsterAiCheckinRecord {
    pub date: String,
    pub time: String,
    pub time_ms: i64,
    pub user_id: String,
    pub name: String,
    pub status: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reward: Option<f64>,
}

/// 签到轮次全局锁（应用内调度器与 UI 手动触发互斥；CLI 另用跨进程互斥体）。
static LOBSTERAI_ROUND_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) fn try_acquire_round() -> Result<tokio::sync::MutexGuard<'static, ()>, String> {
    LOBSTERAI_ROUND_LOCK
        .try_lock()
        .map_err(|_| "已有 LobsterAI 签到任务在执行中，请等待当前轮次完成".to_string())
}

pub fn run_checkin_round(state: &AppState, emit: &mut dyn FnMut(&Value)) -> Value {
    emit(&json!({ "type": "start", "total": 1 }));
    let mut ok = 0i64;
    let mut already = 0i64;
    let mut failed = 0i64;
    let (status, message, reward, uid, name) = match local_account() {
        Ok(Some(account)) => {
            if let Some(exp) = account.token_expires_at {
                if exp <= chrono::Utc::now().timestamp() {
                    failed = 1;
                    (
                        "fail".to_string(),
                        "登录态已过期：请打开 LobsterAI 桌面端（会自动续期）后重试".to_string(),
                        None,
                        account.user_id.clone(),
                        account.name.clone(),
                    )
                } else {
                    let (status, message, reward) = do_checkin(&account);
                    match status.as_str() {
                        "success" => ok = 1,
                        "already" => already = 1,
                        _ => failed = 1,
                    }
                    (status, message, reward, account.user_id.clone(), account.name.clone())
                }
            } else {
                let (status, message, reward) = do_checkin(&account);
                match status.as_str() {
                    "success" => ok = 1,
                    "already" => already = 1,
                    _ => failed = 1,
                }
                (status, message, reward, account.user_id.clone(), account.name.clone())
            }
        }
        Ok(None) => {
            failed = 1;
            (
                "fail".to_string(),
                "未检测到 LobsterAI 本机登录态：请先安装并登录 LobsterAI 桌面端".to_string(),
                None,
                String::new(),
                String::new(),
            )
        }
        Err(e) => {
            failed = 1;
            ("fail".to_string(), e, None, String::new(), String::new())
        }
    };

    emit(&json!({
        "type": "account",
        "index": 1,
        "user_id": uid.clone(),
        "name": name.clone(),
        "status": status.clone(),
        "message": message.clone(),
        "reward": reward,
    }));

    let now = chrono::Local::now();
    let record = LobsterAiCheckinRecord {
        date: now.format("%Y-%m-%d").to_string(),
        time: now.format("%H:%M:%S").to_string(),
        time_ms: now.timestamp_millis(),
        user_id: if uid.is_empty() { "lobsterai-local".to_string() } else { uid },
        name,
        status,
        message,
        reward,
    };
    if let Ok(value) = serde_json::to_value(&record) {
        if let Err(e) =
            crate::store::docs::lobsterai_checkin_results_upsert(&crate::store::db(&state.data_dir), &value)
        {
            crate::fs_utils::app_log(&state.data_dir, &format!("LobsterAI 签到结果落库失败：{e}"));
        }
    }
    if failed > 0 {
        crate::fs_utils::app_log(
            &state.data_dir,
            &format!("LobsterAI 签到失败：{}", record.message),
        );
    }

    let done = json!({
        "type": "done",
        "ok": ok,
        "already": already,
        "failed": failed,
    });
    emit(&done);
    done
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn extract_credits_reads_multiple_field_names() {
        assert_eq!(extract_credits(&json!({ "creditsGranted": 100 })), 100.0);
        assert_eq!(extract_credits(&json!({ "rewardCredits": "50" })), 50.0);
        assert_eq!(extract_credits(&json!({ "result": { "credits": 8 } })), 8.0);
        assert_eq!(extract_credits(&json!({ "nothing": 1 })), 0.0);
    }

    #[test]
    fn unwrap_envelope_rejects_business_code() {
        let err = unwrap_envelope(200, Some(json!({ "code": 401, "msg": "token expired" })), "")
            .expect_err("code 非 0 应失败");
        assert!(err.contains("401"));
        assert!(err.contains("token expired"));
        let data = unwrap_envelope(200, Some(json!({ "code": 0, "data": { "ok": true } })), "")
            .expect("code 0 应成功");
        assert_eq!(data["ok"], json!(true));
    }

    #[test]
    fn str_list_lowercases_action_names() {
        let actions = str_list(Some(&json!(["CHECK_IN", " share ", 1])));
        assert_eq!(actions, vec!["check_in".to_string(), "share".to_string()]);
        assert!(actions.contains(&"check_in".to_string()));
    }

    #[test]
    fn jwt_exp_and_uid_parse_from_payload() {
        // 构造无签名意义的 JWT（仅 payload，供本地解析）
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"alg\":\"HS512\"}");
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"sub":"170764","exp":1893456000}"#);
        let token = format!("{header}.{payload}.sig");
        assert_eq!(jwt_uid(&token), "170764");
        assert_eq!(jwt_exp(&token), Some(1893456000));
    }

    #[test]
    fn balance_of_reads_known_keys_only_and_never_invents_zero() {
        assert_eq!(balance_of(&json!({ "credits": 120 })), Some(120.0));
        assert_eq!(balance_of(&json!({ "creditsBalance": "88.5" })), Some(88.5));
        assert_eq!(balance_of(&json!({ "balance": 0 })), Some(0.0));
        // 未知字段名 / 负值 / 非数字：认不出就 None（如实「未知」，不冒充 0）
        assert_eq!(balance_of(&json!({ "claimedDays": 3 })), None);
        assert_eq!(balance_of(&json!({ "credits": -5 })), None);
        assert_eq!(balance_of(&json!({ "credits": "abc" })), None);
        assert_eq!(balance_of(&json!({})), None);
    }
}