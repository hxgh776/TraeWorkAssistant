//! MiniMax Code 每日签到（双区：minimax CN / minimax-global Global）。
//!
//! 协议（VENU-5 §1.3 定稿，不重新逆向）：
//! 1. `GET  /minimax-cloud/api/v1/signin/status?timezone_offset=28800&is_desktop=1&client=desktop`
//!    `data.days[]` 找 `is_today==true`：`status==1` 今日未领 / `status==3` 今日已领。
//! 2. `POST /minimax-cloud/api/v1/signin/claim?...`（body `{}`）：
//!    `claim_result==1` 新签成功（points = 本次积分）；`claim_result==2` 重复签到幂等返回，
//!    **不算错误**（调度重试不会把已签当失败）。
//! 3. 业务错误 `base_resp.status_code != 0`（HTTP 层可能 200），如实落库不静默。
//!
//! 双通道凭证：设备码流（独立链，可自动刷新）/ 本机 auth.json 只读导入（不主动刷新）。
//! 401 → 互斥刷新 → 重试一次 → 仍失败落 needs_relogin 熔断（minimax_common::ensure_fresh_token）。

use serde::Serialize;
use serde_json::{json, Value};

use super::minimax_common::{
    agent_request, ensure_fresh_token, excerpt, reject_business_error, Region, SIGNIN_QUERY,
};
use crate::state::AppState;

/// 单账号签到记录（落 minimax_checkin_results，90 天滚动）。
#[derive(Serialize, Clone)]
pub struct MinimaxCheckinRecord {
    pub date: String,
    pub time: String,
    pub time_ms: i64,
    pub user_id: String,
    pub name: String,
    /// minimax | minimax-global
    pub region: String,
    /// success | already | fail
    pub status: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reward: Option<f64>,
    /// 失败归因：needs_relogin = 凭证失效（重试注定失败，调度器据此剔除重试判定）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fail_kind: Option<String>,
}

/// 账号池条目的只读视图（签到轮内使用）
struct PoolEntry {
    id: String,
    user_id: String,
    name: String,
    region: Region,
}

/// 读取待签到账号清单：双区（按 settings 区开关过滤）+ 可选 user_ids 指定。
/// 池读取失败走 Err（审查 M-4 同款：不静默吞成空池）。
fn pool_entries(st: &AppState, region_filter: Option<&str>, user_ids: &[String]) -> Result<Vec<PoolEntry>, String> {
    let settings = st.settings();
    let pool = crate::store::docs::minimax_pool_load_checked(&crate::store::db(&st.data_dir))?;
    let empty = Vec::new();
    let accounts = pool.get("accounts").and_then(Value::as_array).unwrap_or(&empty);
    let mut out = Vec::new();
    for acc in accounts {
        let region = match acc
            .get("region")
            .and_then(Value::as_str)
            .and_then(Region::parse)
        {
            Some(r) => r,
            None => continue,
        };
        // 区开关：关闭的区不参与签到
        let enabled = match region {
            Region::Cn => settings.minimax_cn_enabled,
            Region::Global => settings.minimax_global_enabled,
        };
        if !enabled {
            continue;
        }
        if let Some(f) = region_filter {
            if Region::parse(f) != Some(region) {
                continue;
            }
        }
        let uid = acc.get("user_id").and_then(Value::as_str).unwrap_or("").to_string();
        if !user_ids.is_empty() && !user_ids.iter().any(|u| u == &uid) {
            continue;
        }
        out.push(PoolEntry {
            id: acc.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
            user_id: uid,
            name: acc.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            region,
        });
    }
    Ok(out)
}

/// signin/status 响应 → 今日状态（Some(3)=已领，Some(1)=未领，None=取不到）。
fn today_signin_status(value: &Value) -> Option<i64> {
    let days = value.get("data")?.get("days")?.as_array()?;
    for day in days {
        if day.get("is_today").and_then(Value::as_bool).unwrap_or(false) {
            return day.get("status").and_then(Value::as_i64);
        }
    }
    None
}

/// claim 响应 → (claim_result, points)。
fn parse_claim(value: &Value) -> (i64, f64) {
    let data = value.get("data").unwrap_or(&Value::Null);
    let claim_result = data.get("claim_result").and_then(Value::as_i64).unwrap_or(0);
    let points = data
        .get("points")
        .and_then(Value::as_f64)
        .or_else(|| {
            data.get("points")
                .and_then(Value::as_str)
                .and_then(|s| s.trim().parse::<f64>().ok())
        })
        .unwrap_or(0.0);
    (claim_result, points)
}

/// 执行一轮签到（双区账号按 settings 区开关与可选过滤；多账号间隔防频控）。
/// 进度经 emit 回调推送 NDJSON 行；结果按日落库（同 uid 同日重试轮覆盖）。
pub fn run_checkin_round(
    st: &AppState,
    region_filter: Option<&str>,
    user_ids: &[String],
    emit: &mut dyn FnMut(&Value),
) -> Value {
    let entries = match pool_entries(st, region_filter, user_ids) {
        Ok(v) => v,
        Err(e) => {
            let done = json!({ "type": "done", "ok": 0, "already": 0, "failed": 1, "error": e });
            emit(&done);
            return done;
        }
    };
    emit(&json!({ "type": "start", "total": entries.len() }));
    let mut ok = 0i64;
    let mut already = 0i64;
    let mut failed = 0i64;
    let mut failed_permanent = 0i64;
    let gap = crate::models::effective_checkin_gap(Some(st.settings().minimax_checkin_gap_secs));
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 && gap > 0 {
            std::thread::sleep(std::time::Duration::from_secs(gap));
        }
        let db = crate::store::db(&st.data_dir);
        let account = crate::store::docs::minimax_pool_load(&db)
            .get("accounts")
            .and_then(Value::as_array)
            .and_then(|arr| {
                arr.iter()
                    .find(|a| a.get("id").and_then(Value::as_str) == Some(entry.id.as_str()))
                    .cloned()
            });
        let Some(account) = account else {
            failed += 1;
            emit(&json!({
                "type": "account", "index": index + 1, "user_id": entry.user_id,
                "name": entry.name, "region": entry.region.key(),
                "status": "fail", "message": "账号不存在（池已变更）", "fail_kind": "gone",
            }));
            continue;
        };
        let (status, message, reward, fail_kind) = checkin_one(st, &account, entry.region);
        match status.as_str() {
            "success" => ok += 1,
            "already" => already += 1,
            _ => {
                failed += 1;
                // 凭证失效类（needs_relogin / 账号被移除）：重试注定失败，
                // 计入 failed_permanent 供调度器/CLI 从重试判定剔除
                if fail_kind
                    .as_deref()
                    .map(|k| k == "needs_relogin" || k == "gone")
                    .unwrap_or(false)
                {
                    failed_permanent += 1;
                }
            }
        }
        emit(&json!({
            "type": "account",
            "index": index + 1,
            "user_id": entry.user_id,
            "name": entry.name,
            "region": entry.region.key(),
            "status": status,
            "message": message,
            "reward": reward,
        }));
        let now = chrono::Local::now();
        let record = MinimaxCheckinRecord {
            date: now.format("%Y-%m-%d").to_string(),
            time: now.format("%H:%M:%S").to_string(),
            time_ms: now.timestamp_millis(),
            user_id: entry.user_id.clone(),
            name: entry.name.clone(),
            region: entry.region.key().to_string(),
            status: status.clone(),
            message: message.clone(),
            reward,
            fail_kind: fail_kind.clone(),
        };
        if let Ok(value) = serde_json::to_value(&record) {
            if let Err(e) =
                crate::store::docs::minimax_checkin_results_upsert(&crate::store::db(&st.data_dir), &value)
            {
                crate::fs_utils::app_log(&st.data_dir, &format!("MiniMax 签到结果落库失败：{e}"));
            }
        }
        if status == "fail" {
            crate::fs_utils::app_log(
                &st.data_dir,
                &format!(
                    "MiniMax 签到失败（{} / {}）：{message}",
                    entry.name,
                    entry.region.key()
                ),
            );
        }
        write_back_checkin_state(st, &entry.id, &status, &message);
    }
    let done = json!({
        "type": "done",
        "ok": ok,
        "already": already,
        "failed": failed,
        // 调度器重试口径：凭证失效类失败重试注定失败，剔除（对齐 qoder failed_permanent）
        "failed_permanent": failed_permanent,
    });
    emit(&done);
    done
}

/// 单账号签到：status → claim → 结果分类。
fn checkin_one(st: &AppState, account: &Value, region: Region) -> (String, String, Option<f64>, Option<String>) {
    let uid = account.get("user_id").and_then(Value::as_str).unwrap_or("0").to_string();
    let token = match ensure_fresh_token(st, account) {
        Ok(t) => t,
        Err(e) => {
            let kind = if e.contains("重新授权") || e.contains("已过期") {
                Some("needs_relogin")
            } else {
                None
            };
            return ("fail".into(), e, None, kind.map(str::to_string));
        }
    };
    let path = "/minimax-cloud/api/v1/signin/status";
    let (status, parsed, raw) = agent_request(
        region,
        "GET",
        path,
        SIGNIN_QUERY,
        None,
        &token.access_token,
        &uid,
        region.lang(),
    );
    if status == 401 {
        return (
            "fail".into(),
            "凭证被拒绝（HTTP 401）：请在账号管理重新授权".into(),
            None,
            Some("needs_relogin".into()),
        );
    }
    if status != 200 {
        return (
            "fail".into(),
            format!("签到状态查询失败（HTTP {status}）：{}", excerpt(&raw)),
            None,
            None,
        );
    }
    let value = match parsed {
        Some(v) => v,
        None => {
            return (
                "fail".into(),
                format!("签到状态响应不是 JSON：{}", excerpt(&raw)),
                None,
                None,
            )
        }
    };
    if let Err(e) = reject_business_error(&value) {
        return ("fail".into(), e, None, None);
    }
    match today_signin_status(&value) {
        // status==3：今日已领（幂等，不算失败）
        Some(3) => (
            "already".into(),
            "今日已领取".into(),
            None,
            None,
        ),
        // status==1 或取不到（宽容：部分账号 days 缺失时仍尝试 claim，由 claim_result 兜底）
        _ => {
            let path = "/minimax-cloud/api/v1/signin/claim";
            let (status, parsed, raw) = agent_request(
                region,
                "POST",
                path,
                SIGNIN_QUERY,
                Some(&json!({})),
                &token.access_token,
                &uid,
                region.lang(),
            );
            if status == 401 {
                return (
                    "fail".into(),
                    "凭证被拒绝（HTTP 401）：请在账号管理重新授权".into(),
                    None,
                    Some("needs_relogin".into()),
                );
            }
            if status != 200 {
                return (
                    "fail".into(),
                    format!("签到请求失败（HTTP {status}）：{}", excerpt(&raw)),
                    None,
                    None,
                );
            }
            let value = match parsed {
                Some(v) => v,
                None => {
                    return (
                        "fail".into(),
                        format!("签到响应不是 JSON：{}", excerpt(&raw)),
                        None,
                        None,
                    )
                }
            };
            if let Err(e) = reject_business_error(&value) {
                return ("fail".into(), e, None, None);
            }
            let (claim_result, points) = parse_claim(&value);
            match claim_result {
                // 1 = 新签成功
                1 => (
                    "success".into(),
                    format!("签到成功 +{} 积分", format_points(points)),
                    Some(points),
                    None,
                ),
                // 2 = 重复签到幂等返回，不算错误（规格 §1.3）
                2 => ("already".into(), "今日已领取（重复签到幂等返回）".into(), None, None),
                other => (
                    "fail".into(),
                    format!("签到响应 claim_result 异常（{other}）：{}", excerpt(
                        &serde_json::to_string(&value).unwrap_or_default()
                    )),
                    None,
                    None,
                ),
            }
        }
    }
}

fn format_points(points: f64) -> String {
    if points.fract() == 0.0 {
        format!("{}", points as i64)
    } else {
        format!("{points}")
    }
}

/// 账号级最近签到状态回写（last_checkin_status / last_checkin_date / last_checkin_message）。
/// 落库失败静默：仅影响账号列表徽标展示，不影响签到结果表与调度链。
fn write_back_checkin_state(st: &AppState, account_id: &str, status: &str, message: &str) {
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let db = crate::store::db(&st.data_dir);
    let mut pool = crate::store::docs::minimax_pool_load(&db);
    let Some(accounts) = pool.get_mut("accounts").and_then(Value::as_array_mut) else {
        return;
    };
    for acc in accounts.iter_mut() {
        if acc.get("id").and_then(Value::as_str) == Some(account_id) {
            acc["last_checkin_status"] = json!(status);
            acc["last_checkin_date"] = json!(date);
            acc["last_checkin_message"] = json!(message);
            if let Err(e) = crate::store::docs::minimax_pool_save(&db, &pool) {
                crate::fs_utils::app_log(&st.data_dir, &format!("MiniMax 签到状态回写失败：{e}"));
            }
            return;
        }
    }
}

/// 签到轮次全局锁（应用内调度器与 UI 手动触发互斥；CLI 另用跨进程互斥体）。
static MINIMAX_ROUND_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) fn try_acquire_round() -> Result<tokio::sync::MutexGuard<'static, ()>, String> {
    MINIMAX_ROUND_LOCK
        .try_lock()
        .map_err(|_| "已有 MiniMax 签到任务在执行中，请等待当前轮次完成".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn today_signin_status_finds_is_today() {
        let v = json!({ "data": { "days": [
            { "is_today": false, "status": 3, "day_no": 4 },
            { "is_today": true, "status": 1, "day_no": 5, "points": 400, "bonus_points": 0 }
        ]}, "base_resp": { "status_code": 0 }});
        assert_eq!(today_signin_status(&v), Some(1));
        let claimed = json!({ "data": { "days": [
            { "is_today": true, "status": 3, "day_no": 5 }
        ]}});
        assert_eq!(today_signin_status(&claimed), Some(3));
        // days 缺失 = 取不到
        assert_eq!(today_signin_status(&json!({ "data": {} })), None);
    }

    #[test]
    fn parse_claim_reads_result_and_points() {
        assert_eq!(parse_claim(&json!({ "data": { "claim_result": 1, "day_no": 5, "points": 400 }})), (1, 400.0));
        assert_eq!(parse_claim(&json!({ "data": { "claim_result": 2, "points": "0" }})), (2, 0.0));
        // 缺字段不 panic
        assert_eq!(parse_claim(&json!({})), (0, 0.0));
    }

    #[test]
    fn pool_entries_filters_region_and_users() {
        let dir = std::env::temp_dir().join(format!("aiwork_minimax_pool_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let st = AppState {
            data_dir: dir.clone(),
            jwt_refresh_lock: std::sync::Arc::new(std::sync::Mutex::new(())),
            qoder_pool_lock: std::sync::Arc::new(std::sync::Mutex::new(())),
            wb_pool_lock: std::sync::Arc::new(std::sync::Mutex::new(())),
            buddy_ai_pool_lock: std::sync::Arc::new(std::sync::Mutex::new(())),
            doubao_pool_lock: std::sync::Arc::new(std::sync::Mutex::new(())),
            group_defs_lock: std::sync::Arc::new(std::sync::Mutex::new(())),
        };
        // 预置设置（双区开）：直建 AppState 不经 state 归一，需显式落默认设置
        {
            let db = crate::store::db(&st.data_dir);
            let mut sv =
                serde_json::to_value(crate::models::Settings::default()).expect("序列化默认设置");
            sv["minimax_cn_enabled"] = json!(true);
            sv["minimax_global_enabled"] = json!(true);
            sv["minimax_checkin_gap_secs"] = json!(3);
            db.kv_set("app_settings", &sv).expect("落设置");
        }
        let db = crate::store::db(&st.data_dir);
        let pool = json!({ "accounts": [
            { "id": "mm-1", "user_id": "u1", "name": "甲", "region": "minimax" },
            { "id": "mm-2", "user_id": "u2", "name": "乙", "region": "minimax-global" },
            { "id": "mm-3", "user_id": "u3", "name": "丙", "region": "bogus" }
        ]});
        crate::store::docs::minimax_pool_save(&db, &pool).unwrap();
        let all = pool_entries(&st, None, &[]).unwrap();
        assert_eq!(all.len(), 2); // bogus 区被丢弃
        let cn = pool_entries(&st, Some("minimax"), &[]).unwrap();
        assert_eq!(cn.len(), 1);
        assert_eq!(cn[0].user_id, "u1");
        let picked = pool_entries(&st, None, &["u2".to_string()]).unwrap();
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].region, Region::Global);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
