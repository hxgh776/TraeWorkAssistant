//! MiniMax Code 积分/额度（只读拉取 + 快照落库）。
//!
//! 协议（VENU-5 §1.4 定稿）：`POST /matrix/api/v1/commerce/get_membership_info`（签名请求）
//! 快照取 `op_credit_summary.free_remaining_amount`（签到所得）/ `purchased_remaining_amount`（购买），
//! **禁止硬编码签到金额**——数额一律以接口返回为准。
//!
//! 快照链路：全账号取数 → 账号池三键回写（credits_free / credits_purchased / credits_total +
//! credits_fetched_at）→ `minimax_credits_history` 当日覆盖快照（看板余额趋势数据源）。
//! 「近 30 天获得」来自签到结果 reward 合计（签到轮写入），本模块只供余额。

use serde::Serialize;
use serde_json::{json, Value};

use super::minimax_common::{
    agent_request, ensure_fresh_token, excerpt, reject_business_error, Region,
};
use crate::state::AppState;

const MEMBERSHIP_PATH: &str = "/matrix/api/v1/commerce/get_membership_info";

/// 单账号积分信息。
#[derive(Serialize, Clone)]
pub struct MinimaxCreditsInfo {
    pub account_id: String,
    pub user_id: String,
    pub name: String,
    /// minimax | minimax-global
    pub region: String,
    /// 套餐名（免费版可能为空串）
    pub plan_name: String,
    /// 免费/活动积分余额（签到所得在此）
    pub free: f64,
    /// 购买积分余额
    pub purchased: f64,
    /// 合计余额
    pub total: f64,
    /// 套餐到期（毫秒；0 = 无到期/免费版）
    pub expires_at_ms: i64,
    pub fetched_at: String,
    /// 单账号失败信息（ok=false 时非空；其余账号不受影响）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// get_membership_info 响应 → 积分归一（free/purchased 拆分，缺失按 0；
/// 「完全取不到」由调用方以信封校验与 op_credit_summary 缺失判定）。
fn credits_from_response(value: &Value) -> (f64, f64, f64, String, i64) {
    let summary = value
        .get("op_credit_summary")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let num = |v: &Value, key: &str| -> f64 {
        v.get(key)
            .and_then(Value::as_f64)
            .or_else(|| {
                v.get(key)
                    .and_then(Value::as_str)
                    .and_then(|s| s.trim().parse::<f64>().ok())
            })
            .unwrap_or(0.0)
    };
    let free = num(&summary, "free_remaining_amount");
    let purchased = num(&summary, "purchased_remaining_amount");
    let total = {
        let declared = num(&summary, "total_remaining_amount");
        // total 缺失时按 free + purchased 兜底；声明值优先
        if declared > 0.0 || (summary.get("total_remaining_amount").is_some()) {
            declared
        } else {
            free + purchased
        }
    };
    let plan_name = value
        .get("plan_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let expires_at_ms = value
        .get("expires_at")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    (free, purchased, total, plan_name, expires_at_ms)
}

/// 单账号实时积分查询（凭证就绪 → 签名请求 → 归一）。
pub fn fetch_credits(st: &AppState, account: &Value) -> Result<MinimaxCreditsInfo, String> {
    let id = account.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let region = account
        .get("region")
        .and_then(Value::as_str)
        .and_then(Region::parse)
        .ok_or_else(|| "账号缺少有效 region".to_string())?;
    let uid = account.get("user_id").and_then(Value::as_str).unwrap_or("0").to_string();
    let name = account.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let token = ensure_fresh_token(st, account)?;
    let (status, parsed, raw) = agent_request(
        region,
        "POST",
        MEMBERSHIP_PATH,
        "",
        Some(&json!({})),
        &token.access_token,
        &uid,
        region.lang(),
    );
    if status == 401 {
        return Err("凭证被拒绝（HTTP 401）：请在账号管理重新授权".into());
    }
    if status != 200 {
        return Err(format!("积分查询失败（HTTP {status}）：{}", excerpt(&raw)));
    }
    let value = parsed.ok_or_else(|| format!("积分响应不是 JSON：{}", excerpt(&raw)))?;
    reject_business_error(&value)?;
    // op_credit_summary 完全缺失 = 取不到，报错而非 0 冒充（对齐 officeace quota 口径）
    if value.get("op_credit_summary").is_none() {
        return Err("积分响应缺少 op_credit_summary，无法读取余额".into());
    }
    let (free, purchased, total, plan_name, expires_at_ms) = credits_from_response(&value);
    Ok(MinimaxCreditsInfo {
        account_id: id,
        user_id: uid,
        name,
        region: region.key().to_string(),
        plan_name,
        free,
        purchased,
        total,
        expires_at_ms,
        fetched_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        error: None,
    })
}

/// 全账号积分同步（调度任务与 UI「刷新」共用）：
/// 逐账号取数 → 池三键回写 → 当日快照覆盖落库。返回汇总 JSON。
/// 空池返回 skipped（普通 skipped 语义：无账号空转，mark_run 关闭当日重复触发）。
pub fn run_sync_task(st: &AppState) -> Result<Value, String> {
    let pool = crate::store::docs::minimax_pool_load_checked(&crate::store::db(&st.data_dir))?;
    let empty = Vec::new();
    let accounts = pool.get("accounts").and_then(Value::as_array).unwrap_or(&empty);
    if accounts.is_empty() {
        return Ok(json!({ "ok": true, "skipped": "无 MiniMax 账号" }));
    }
    let settings = st.settings();
    let mut ok = 0i64;
    let mut failed = 0i64;
    let mut snaps: Vec<Value> = Vec::new();
    for account in accounts {
        let region = account
            .get("region")
            .and_then(Value::as_str)
            .and_then(Region::parse);
        // 区开关过滤（关闭的区不拉取）
        let region_enabled = match region {
            Some(Region::Cn) => settings.minimax_cn_enabled,
            Some(Region::Global) => settings.minimax_global_enabled,
            None => false,
        };
        if !region_enabled {
            continue;
        }
        match fetch_credits(st, account) {
            Ok(info) => {
                ok += 1;
                snaps.push(json!({
                    "id": info.account_id,
                    "user_id": info.user_id,
                    "name": info.name,
                    "region": info.region,
                    "free": info.free,
                    "purchased": info.purchased,
                    "total": info.total,
                    "plan_name": info.plan_name,
                }));
                write_back_pool(st, &info);
            }
            Err(e) => {
                failed += 1;
                crate::fs_utils::app_log(
                    &st.data_dir,
                    &format!(
                        "MiniMax 积分同步失败（{} / {}）：{e}",
                        account.get("name").and_then(Value::as_str).unwrap_or("?"),
                        account.get("region").and_then(Value::as_str).unwrap_or("?")
                    ),
                );
            }
        }
    }
    // 当日快照（pk=date 同日覆盖；含失败账号时仍落成功部分——余额趋势以成功快照为准）
    if !snaps.is_empty() {
        let now = chrono::Local::now();
        let snap = json!({
            "date": now.format("%Y-%m-%d").to_string(),
            "ts": now.timestamp_millis(),
            "accounts": snaps,
            "failed": failed,
        });
        if let Err(e) = crate::store::docs::minimax_credits_history_upsert(&crate::store::db(&st.data_dir), &snap)
        {
            crate::fs_utils::app_log(&st.data_dir, &format!("MiniMax 积分快照落库失败：{e}"));
        }
    }
    if ok == 0 && failed > 0 {
        return Err(format!("MiniMax 积分同步：{failed} 个账号全部失败（稍后自动重试）"));
    }
    Ok(json!({ "ok": true, "ok_count": ok, "failed": failed }))
}

/// 同步结果回写账号池（credits_free / credits_purchased / credits_total / plan_name /
/// credits_fetched_at 三键拨新；失败不动池内旧值）。调度任务与 UI 实时刷新共用。
pub fn write_back_pool(st: &AppState, info: &MinimaxCreditsInfo) {
    let db = crate::store::db(&st.data_dir);
    let mut pool = crate::store::docs::minimax_pool_load(&db);
    let Some(accounts) = pool.get_mut("accounts").and_then(Value::as_array_mut) else {
        return;
    };
    let mut changed = false;
    for acc in accounts.iter_mut() {
        if acc.get("id").and_then(Value::as_str) == Some(info.account_id.as_str()) {
            acc["credits_free"] = json!(info.free);
            acc["credits_purchased"] = json!(info.purchased);
            acc["credits_total"] = json!(info.total);
            if !info.plan_name.is_empty() {
                acc["plan_name"] = json!(info.plan_name);
            }
            acc["credits_fetched_at"] = json!(info.fetched_at);
            changed = true;
            break;
        }
    }
    if changed {
        if let Err(e) = crate::store::docs::minimax_pool_save(&db, &pool) {
            crate::fs_utils::app_log(&st.data_dir, &format!("MiniMax 积分回写账号池失败：{e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credits_from_response_splits_free_and_purchased() {
        let v = json!({
            "op_credit_summary": {
                "total_remaining_amount": 1234.5,
                "free_remaining_amount": 1234.5,
                "purchased_remaining_amount": 0
            },
            "total_remains_credit": 1234.5,
            "plan_name": "M Plan",
            "expires_at": 0,
            "base_resp": { "status_code": 0 }
        });
        let (free, purchased, total, plan, exp) = credits_from_response(&v);
        assert_eq!(free, 1234.5);
        assert_eq!(purchased, 0.0);
        assert_eq!(total, 1234.5);
        assert_eq!(plan, "M Plan");
        assert_eq!(exp, 0); // 免费版 0 = 无到期
    }

    #[test]
    fn credits_from_response_falls_back_total() {
        // total 缺失 → free + purchased 兜底；字符串数值宽容解析
        let v = json!({
            "op_credit_summary": {
                "free_remaining_amount": "400",
                "purchased_remaining_amount": "600"
            }
        });
        let (free, purchased, total, _, _) = credits_from_response(&v);
        assert_eq!(free, 400.0);
        assert_eq!(purchased, 600.0);
        assert_eq!(total, 1000.0);
    }

    #[test]
    fn credits_missing_summary_is_zero_safe() {
        // 全缺失归一为 0（调用方以「op_credit_summary 缺失报错」挡住 0 冒充）
        let (free, purchased, total, _, _) = credits_from_response(&json!({}));
        assert_eq!((free, purchased, total), (0.0, 0.0, 0.0));
    }
}
