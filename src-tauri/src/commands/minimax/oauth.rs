//! MiniMax 凭证通道命令：OAuth 设备码流（start / poll / cancel）与本机 auth.json 导入。
//!
//! 设备码流（RFC 8628 + S256 PKCE）：start 起会话（内存态），前端展示 user_code 与
//! verification_uri_complete 引导浏览器授权，按 interval 调 poll；poll 单步轮询，
//! 成功即取 profile 建档入池 + 凭证落库。cancel 丢弃会话。
//!
//! auth.json 导入：只读扫描本机 MiniMax Code 登录态（`~/.minimax/auth/prod/{cn|en}/
//! mcode-public/auth.json`），profile 校验归属后入池。**绝不写回该文件**（通道 B
//! 不主动刷新，避免一次性轮换作废客户端链——见 tasks/minimax_common::ensure_fresh_token）。

use serde_json::{json, Value};
use tauri::State;

use super::common::with_pool;
use crate::state::AppState;
use crate::tasks::minimax_common::{
    account_id, device_code_poll, device_code_start, fetch_profile, scan_local_authjson,
    DevicePollOutcome, Region, TokenRec,
};

/// 内存中的设备码会话（单会话；重复 start 覆盖旧的）
struct DeviceSession {
    region: Region,
    device_code: String,
    verifier: String,
    expires_at_ms: i64,
}

static DEVICE_SESSION: std::sync::Mutex<Option<DeviceSession>> = std::sync::Mutex::new(None);

fn session_slot() -> std::sync::MutexGuard<'static, Option<DeviceSession>> {
    DEVICE_SESSION.lock().unwrap_or_else(|p| p.into_inner())
}

/// 设备码授权起点：申请 device_code，返回 user_code 与授权链接（前端弹窗展示）。
#[tauri::command(async)]
pub fn minimax_device_start(region: String) -> Result<Value, String> {
    let region = Region::parse(&region).ok_or_else(|| format!("无效 region：{region}"))?;
    let (device_code, user_code, verify_uri, expires_in, interval) = device_code_start(region)?;
    let mut slot = session_slot();
    *slot = Some(DeviceSession {
        region,
        device_code,
        verifier: crate::tasks::minimax_common::generate_verifier(),
        expires_at_ms: chrono::Utc::now().timestamp_millis() + expires_in * 1000,
    });
    Ok(json!({
        "region": region.key(),
        "user_code": user_code,
        "verification_uri_complete": verify_uri,
        "expires_in": expires_in,
        "interval": interval.max(2),
    }))
}

/// 设备码轮询单步（前端按 interval 周期调用）。
/// 返回 { status: pending | slow_down | success | failed, message? }；
/// success 时已完成建档入池 + 凭证落库（附账号视图）。
#[tauri::command(async)]
pub fn minimax_device_poll(state: State<'_, AppState>) -> Result<Value, String> {
    let (region, device_code, verifier, expired) = {
        let slot = session_slot();
        let Some(s) = slot.as_ref() else {
            return Err("无进行中的设备码授权，请先点「开始授权」".into());
        };
        (
            s.region,
            s.device_code.clone(),
            s.verifier.clone(),
            chrono::Utc::now().timestamp_millis() > s.expires_at_ms,
        )
    };
    if expired {
        *session_slot() = None;
        return Ok(json!({ "status": "failed", "message": "授权已超时（设备码 5 分钟有效），请重新开始" }));
    }
    match device_code_poll(region, &device_code, &verifier) {
        DevicePollOutcome::Pending => Ok(json!({ "status": "pending" })),
        DevicePollOutcome::SlowDown => Ok(json!({ "status": "slow_down" })),
        DevicePollOutcome::Failed(e) => {
            // access_denied / expired_token 等终态：清会话
            *session_slot() = None;
            Ok(json!({ "status": "failed", "message": e }))
        }
        DevicePollOutcome::Success {
            access_token,
            refresh_token,
            expires_in_secs,
        } => {
            *session_slot() = None;
            let account = import_credential(
                &state,
                region,
                &access_token,
                &refresh_token,
                chrono::Utc::now().timestamp_millis() + expires_in_secs * 1000,
                "oauth_device",
            )?;
            Ok(json!({ "status": "success", "account": account }))
        }
    }
}

/// 取消进行中的设备码授权（无会话时静默成功）。
#[tauri::command]
pub fn minimax_device_cancel() {
    *session_slot() = None;
}

/// 扫描本机 MiniMax Code 登录态（只读；返回可导入记录的脱敏摘要）。
#[tauri::command]
pub fn minimax_authjson_scan() -> Value {
    let records: Vec<Value> = scan_local_authjson()
        .iter()
        .map(|r| {
            json!({
                "region": r.region.key(),
                "subject": r.subject,
                "account_id": r.account_id,
                // 毫秒；0 = 未知
                "expires_at_ms": r.expires_at_ms,
                "path": r.path,
                "expired": r.expires_at_ms > 0
                    && r.expires_at_ms <= chrono::Utc::now().timestamp_millis(),
            })
        })
        .collect();
    json!({ "records": records, "count": records.len() })
}

/// 导入本机 auth.json（region 为空 = 双区都尝试）。返回导入结果摘要。
#[tauri::command(async)]
pub fn minimax_authjson_import(state: State<'_, AppState>, region: Option<String>) -> Result<Value, String> {
    let wanted: Vec<Region> = match region.as_deref().and_then(Region::parse) {
        Some(r) => vec![r],
        None => vec![Region::Cn, Region::Global],
    };
    let mut imported = 0usize;
    let mut updated = 0usize;
    let mut skipped = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for r in wanted {
        let Some(record) = crate::tasks::minimax_common::read_local_authjson(r) else {
            continue;
        };
        if record.access_token.trim().is_empty() {
            continue;
        }
        let expired = record.expires_at_ms > 0
            && record.expires_at_ms <= chrono::Utc::now().timestamp_millis();
        if expired {
            skipped += 1;
            continue; // 过期凭证不入池（导入即 needs_relogin 没有意义）
        }
        match import_credential(&state, r, &record.access_token, &record.refresh_token, record.expires_at_ms, "authjson") {
            Ok(view) => {
                // 已存在 = 更新凭证
                if view.get("_merged").and_then(Value::as_bool).unwrap_or(false) {
                    updated += 1;
                } else {
                    imported += 1;
                }
            }
            Err(e) => errors.push(format!("{}：{e}", r.key())),
        }
    }
    Ok(json!({
        "imported": imported,
        "updated": updated,
        "skipped": skipped,
        "errors": errors,
    }))
}

/// 凭证 → 建档入池（profile 校验归属；同账号合并更新）。
/// 返回账号视图 + `_merged` 标记（导入摘要用，不进前端契约）。
fn import_credential(
    state: &AppState,
    region: Region,
    access_token: &str,
    refresh_token: &str,
    expires_at_ms: i64,
    source: &str,
) -> Result<Value, String> {
    // profile 校验凭证归属并取 user_id / email / 名称（纯 Bearer，不签名）
    let profile = fetch_profile(region, access_token)?;
    let user_id = profile
        .get("user_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if user_id.is_empty() {
        return Err("profile 缺少 user_id，无法建档".into());
    }
    let email = profile.get("email").and_then(Value::as_str).unwrap_or("").to_string();
    let name = {
        let from_profile = profile
            .get("user_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if !from_profile.is_empty() {
            from_profile
        } else if !email.is_empty() {
            email.clone()
        } else {
            format!("MiniMax 账号 {}", &user_id[..user_id.len().min(8)])
        }
    };
    let id = account_id(region, &user_id);
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let merged = with_pool(state, |pool| {
        // 池结构兜底：无 accounts 数组时重建
        if !pool.get("accounts").map(Value::is_array).unwrap_or(false) {
            pool["accounts"] = json!([]);
        }
        let accounts = pool
            .get_mut("accounts")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| "账号池结构异常".to_string())?;
        let mut was_merged = false;
        if let Some(acc) = accounts
            .iter_mut()
            .find(|a| a.get("id").and_then(Value::as_str) == Some(id.as_str()))
        {
            // 同账号合并：更新身份字段与凭证来源，保留改名 / 备注 / 积分缓存
            if acc.get("name").and_then(Value::as_str).map(|n| n.is_empty()).unwrap_or(true) {
                acc["name"] = json!(name);
            }
            acc["email"] = json!(email);
            acc["credential_source"] = json!(source);
            acc["needs_relogin"] = json!(false);
            acc["relogin_reason"] = json!("");
            acc["auth_saved_at"] = json!(now);
            was_merged = true;
        } else {
            accounts.push(json!({
                "id": id,
                "region": region.key(),
                "user_id": user_id,
                "name": name,
                "email": email,
                "note": "",
                "credential_source": source,
                "added_at": now,
                "auth_saved_at": now,
                "needs_relogin": false,
                "relogin_reason": "",
            }));
        }
        Ok(was_merged)
    })?;
    // 凭证落库（设备码新链 / auth.json 副本；绝不出现在日志）
    let token = TokenRec {
        access_token: access_token.to_string(),
        refresh_token: refresh_token.to_string(),
        expires_at_ms,
        credential_source: source.to_string(),
    };
    crate::store::docs::minimax_token_put(&crate::store::db(&state.data_dir), &id, &token.to_json())?;
    crate::fs_utils::app_log(
        &state.data_dir,
        &format!(
            "[minimax] 账号{}（{}）凭证已保存（来源 {source}）",
            if merged { "更新" } else { "导入" },
            region.key()
        ),
    );
    // 返回视图（脱敏）
    let db = crate::store::db(&state.data_dir);
    let acc = crate::store::docs::minimax_pool_load(&db)
        .get("accounts")
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter()
                .find(|a| a.get("id").and_then(Value::as_str) == Some(id.as_str()))
                .cloned()
        })
        .ok_or_else(|| "建档后读取账号失败".to_string())?;
    let mut view = super::common::account_view(&acc, Some(&token.to_json()));
    view["_merged"] = json!(merged);
    Ok(view)
}
