//! MiniMax 积分命令：全账号实时查询（回写池 + 快照）/ 历史快照。

use serde_json::Value;
use tauri::State;

use crate::state::AppState;
use crate::tasks::minimax_credits;

/// 全账号积分实时查询（签到同款凭证链）：逐账号取数 → 池三键回写 → 当日快照落库。
/// 失败账号在行内带 error 字段（前端如实展示），全部失败时 Err。
#[tauri::command(async)]
pub fn minimax_credits_fetch(state: State<'_, AppState>) -> Result<Vec<Value>, String> {
    let st = state.inner().clone();
    let pool = crate::store::docs::minimax_pool_load_checked(&crate::store::db(&st.data_dir))?;
    let empty = Vec::new();
    let accounts = pool.get("accounts").and_then(Value::as_array).unwrap_or(&empty);
    let settings = st.settings();
    let mut rows: Vec<Value> = Vec::new();
    let mut ok_count = 0i64;
    let mut total = 0i64;
    for account in accounts {
        let region = account
            .get("region")
            .and_then(Value::as_str)
            .and_then(crate::tasks::minimax_common::Region::parse);
        let region_enabled = match region {
            Some(crate::tasks::minimax_common::Region::Cn) => settings.minimax_cn_enabled,
            Some(crate::tasks::minimax_common::Region::Global) => settings.minimax_global_enabled,
            None => false,
        };
        if !region_enabled {
            continue;
        }
        total += 1;
        match minimax_credits::fetch_credits(&st, account) {
            Ok(info) => {
                ok_count += 1;
                minimax_credits::write_back_pool(&st, &info);
                rows.push(serde_json::to_value(&info).unwrap_or(Value::Null));
            }
            Err(e) => {
                rows.push(serde_json::json!({
                    "account_id": account.get("id"),
                    "user_id": account.get("user_id"),
                    "name": account.get("name"),
                    "region": account.get("region"),
                    "error": e,
                }));
            }
        }
    }
    // 成功部分落当日快照（覆盖式，与调度任务同款）
    if ok_count > 0 {
        let snaps: Vec<Value> = rows
            .iter()
            .filter(|r| r.get("error").is_none())
            .map(|r| {
                serde_json::json!({
                    "id": r.get("account_id"),
                    "user_id": r.get("user_id"),
                    "name": r.get("name"),
                    "region": r.get("region"),
                    "free": r.get("free"),
                    "purchased": r.get("purchased"),
                    "total": r.get("total"),
                    "plan_name": r.get("plan_name"),
                })
            })
            .collect();
        let now = chrono::Local::now();
        let snap = serde_json::json!({
            "date": now.format("%Y-%m-%d").to_string(),
            "ts": now.timestamp_millis(),
            "accounts": snaps,
            "failed": total - ok_count,
        });
        if let Err(e) =
            crate::store::docs::minimax_credits_history_upsert(&crate::store::db(&st.data_dir), &snap)
        {
            crate::fs_utils::app_log(&st.data_dir, &format!("MiniMax 积分快照落库失败：{e}"));
        }
    }
    if ok_count == 0 && total > 0 {
        return Err("MiniMax 积分查询全部失败（详见各行错误信息）".into());
    }
    Ok(rows)
}

/// 积分快照历史（近 days 天，默认 30，看板余额趋势数据源）。
#[tauri::command]
pub fn minimax_credits_history(state: State<'_, AppState>, days: Option<i64>) -> Vec<Value> {
    let days = days.unwrap_or(30).clamp(1, 365);
    let cutoff = (chrono::Local::now().date_naive() - chrono::Duration::days(days))
        .format("%Y-%m-%d")
        .to_string();
    crate::store::docs::minimax_credits_history_load(&crate::store::db(&state.data_dir))
        .into_iter()
        .filter(|snap| {
            snap.get("date")
                .and_then(Value::as_str)
                .map(|d| d >= cutoff.as_str())
                .unwrap_or(false)
        })
        .collect()
}
