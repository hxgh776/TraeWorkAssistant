//! CodeArts 命令域共享：池读写互斥与账号视图组装。

use serde_json::{json, Value};

use crate::state::AppState;

/// CodeArts 账号池读-改-写互斥（命令线程 / 签到轮 / 积分同步三方并发防丢更新）。
pub(crate) static CODEARTS_POOL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 在池互斥内执行读-改-写（与 minimax with_pool 同语义）。
pub(crate) fn with_pool<T>(
    st: &AppState,
    f: impl FnOnce(&mut Value) -> Result<T, String>,
) -> Result<T, String> {
    let _guard = CODEARTS_POOL_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let db = crate::store::db(&st.data_dir);
    let mut pool = crate::store::docs::codearts_pool_load(&db);
    let result = f(&mut pool)?;
    crate::store::docs::codearts_pool_save(&db, &pool)?;
    Ok(result)
}

/// 账号池条目 → 前端视图（脱敏：凭证只在 codearts_tokens，永不进本视图）。
pub(crate) fn account_view(acc: &Value, token: Option<&Value>) -> Value {
    let mut view = json!({
        "id": acc.get("id").cloned().unwrap_or(Value::Null),
        "user_id": acc.get("user_id").cloned().unwrap_or(Value::Null),
        "name": acc.get("name").cloned().unwrap_or(Value::Null),
        "note": acc.get("note").cloned().unwrap_or(json!("")),
        // local_import | oauth | aksk
        "credential_source": acc.get("credential_source").cloned().unwrap_or(Value::Null),
        "added_at": acc.get("added_at").cloned().unwrap_or(Value::Null),
        "needs_relogin": acc.get("needs_relogin").cloned().unwrap_or(json!(false)),
        "relogin_reason": acc.get("relogin_reason").cloned().unwrap_or(json!("")),
        // 积分缓存（积分同步/实时刷新回写；语义对齐 statistics/plugin）
        "credits_total": acc.get("credits_total").cloned().unwrap_or(Value::Null),
        "credits_amount": acc.get("credits_amount").cloned().unwrap_or(Value::Null),
        "credits_used": acc.get("credits_used").cloned().unwrap_or(Value::Null),
        "credits_expiring": acc.get("credits_expiring").cloned().unwrap_or(Value::Null),
        "credits_bonus_remain": acc.get("credits_bonus_remain").cloned().unwrap_or(Value::Null),
        "package_name": acc.get("package_name").cloned().unwrap_or(json!("")),
        "end_date": acc.get("end_date").cloned().unwrap_or(json!("")),
        "credits_fetched_at": acc.get("credits_fetched_at").cloned().unwrap_or(Value::Null),
        "last_checkin_status": acc.get("last_checkin_status").cloned().unwrap_or(Value::Null),
        "last_checkin_date": acc.get("last_checkin_date").cloned().unwrap_or(Value::Null),
    });
    if let Some(token) = token {
        if let Some(exp) = token.get("expires_at_ms").and_then(Value::as_i64) {
            view["token_expires_at_ms"] = json!(exp);
        }
    }
    view
}

/// 全部账号视图（含 STS 过期时间合并）。
pub(crate) fn all_account_views(st: &AppState) -> Vec<Value> {
    let db = crate::store::db(&st.data_dir);
    let pool = crate::store::docs::codearts_pool_load(&db);
    let empty = Vec::new();
    let accounts = pool.get("accounts").and_then(Value::as_array).unwrap_or(&empty);
    accounts
        .iter()
        .map(|acc| {
            let id = acc.get("id").and_then(Value::as_str).unwrap_or("");
            let token = if id.is_empty() {
                None
            } else {
                crate::store::docs::codearts_token_get(&db, id)
            };
            account_view(acc, token.as_ref())
        })
        .collect()
}
