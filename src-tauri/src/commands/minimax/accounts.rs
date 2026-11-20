//! MiniMax 账号命令：列表（脱敏）/ 改名备注 / 移除（联动清凭证）。

use serde_json::{json, Value};

use super::common::{all_account_views, with_pool};
use crate::state::AppState;

/// 账号列表（脱敏视图；token_expires_at_ms 为凭证表合并值）。
#[tauri::command]
pub fn minimax_accounts_list(state: tauri::State<'_, AppState>) -> Vec<Value> {
    all_account_views(&state)
}

/// 改名 / 备注（脏检查：均未变直接跳过，不触发落库）。
#[tauri::command]
pub fn minimax_account_save(
    state: tauri::State<'_, AppState>,
    id: String,
    name: Option<String>,
    note: Option<String>,
) -> Result<(), String> {
    if id.is_empty() {
        return Err("账号 id 不能为空".into());
    }
    with_pool(&state, |pool| {
        let accounts = pool
            .get_mut("accounts")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| "账号池结构异常".to_string())?;
        let acc = accounts
            .iter_mut()
            .find(|a| a.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .ok_or_else(|| format!("账号不存在：{id}"))?;
        let mut changed = false;
        if let Some(new_name) = name {
            let trimmed = new_name.trim();
            if !trimmed.is_empty() && acc.get("name").and_then(Value::as_str) != Some(trimmed) {
                acc["name"] = json!(trimmed);
                changed = true;
            }
        }
        if let Some(new_note) = note {
            if acc.get("note").and_then(Value::as_str) != Some(new_note.as_str()) {
                acc["note"] = json!(new_note);
                changed = true;
            }
        }
        if !changed {
            return Ok(());
        }
        Ok(())
    })
}

/// 移除账号（联动清凭证表；池外孤儿凭证仍可清理——指定 id 不存在时同样清 token）。
#[tauri::command]
pub fn minimax_account_remove(state: tauri::State<'_, AppState>, id: String) -> Result<(), String> {
    if id.is_empty() {
        return Err("账号 id 不能为空".into());
    }
    with_pool(&state, |pool| {
        let accounts = pool
            .get_mut("accounts")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| "账号池结构异常".to_string())?;
        let before = accounts.len();
        accounts.retain(|a| a.get("id").and_then(Value::as_str) != Some(id.as_str()));
        if accounts.len() == before {
            return Err(format!("账号不存在：{id}"));
        }
        Ok(())
    })?;
    // 凭证清理（移除账号主体后联动；清理失败不影响移除结果，孤儿由再次移除兜底）
    if let Err(e) = crate::store::docs::minimax_token_del(&crate::store::db(&state.data_dir), &id) {
        crate::fs_utils::app_log(&state.data_dir, &format!("MiniMax 凭证清理失败（{id}）：{e}"));
    }
    Ok(())
}
