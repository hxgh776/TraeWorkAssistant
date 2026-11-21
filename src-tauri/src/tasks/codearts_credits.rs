//! CodeArts 积分/额度（只读统计 + 快照落库）。
//!
//! 协议（VENU-5 §2.6 定稿）：`GET {snap}/snap-manager/v1/statistics/plugin`
//! （SDK-HMAC 签名，普通身份头，**无 Agent-Type**；只读统计查询，不消耗积分）。
//!
//! - `usageTotalPackageCredit` 缺失 = 账号没有积分套餐（token 套餐/白名单/未开通），
//!   **报错而非 0 冒充**（展示原因）。
//! - 奖励金额一律以接口返回为准；赠送/签到积分单独读 `usageBonusPackageCredit`。
//! - 旧 benefit 通道（`/api/v1/user/tokens/balance` 等）2026-10 起已死，不实现。
//!
//! 快照链路：全账号取数 → 账号池回写（credits_* 缓存键）→ `codearts_credits_history`
//! 当日覆盖快照（pk=date，365 天裁剪；看板余额趋势数据源）。

use serde::Serialize;
use serde_json::{json, Value};

use super::codearts_common::{ensure_fresh_credential, excerpt, snap_request};
use crate::state::AppState;

const STATISTICS_PATH: &str = "/snap-manager/v1/statistics/plugin";

/// 单账号积分信息。
#[derive(Serialize, Clone)]
pub struct CodeArtsCreditsInfo {
    pub account_id: String,
    pub user_id: String,
    pub name: String,
    /// 套餐名（package_name_cn）
    pub package_name: String,
    /// 是否积分套餐（is_credit_package；token 套餐用户如实展示）
    pub is_credit_package: bool,
    /// 套餐总积分（usageTotalPackageCredit.package_credit_amount）
    pub total: f64,
    /// 已用（package_credit_used）
    pub used: f64,
    /// 余额（package_credit_remain）
    pub remain: f64,
    /// 即将到期（package_credit_expiring_amount；积分当日领取当日有效口径参考）
    pub expiring: f64,
    /// 赠送/签到积分余额（usageBonusPackageCredit.package_credit_remain；缺失 = 无数据）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bonus_remain: Option<f64>,
    /// 套餐/积分到期或重置日（end_date，YYYY-MM-DD）
    pub end_date: String,
    pub fetched_at: String,
    /// 单账号失败信息（ok=false 时非空；其余账号不受影响）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// metrics[] 里按 name 找指标（数值字段字符串宽容解析）
fn metric_of<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value
        .get("metrics")
        .and_then(Value::as_array)?
        .iter()
        .find(|m| m.get("name").and_then(Value::as_str) == Some(name))
}

fn num_of(metric: &Value, key: &str) -> f64 {
    metric
        .get(key)
        .and_then(Value::as_f64)
        .or_else(|| {
            metric
                .get(key)
                .and_then(Value::as_str)
                .and_then(|s| s.trim().parse::<f64>().ok())
        })
        .unwrap_or(0.0)
}

/// statistics/plugin 响应 → 积分归一。`Err` = usageTotalPackageCredit 缺失
/// （无积分套餐，报错不冒充 0）。
fn credits_from_response(value: &Value) -> Result<(String, bool, f64, f64, f64, f64, Option<f64>, String), String> {
    let total_metric = metric_of(value, "usageTotalPackageCredit")
        .ok_or_else(|| "无积分套餐（usageTotalPackageCredit 缺失——账号未开通积分计费或为 token 套餐）".to_string())?;
    let package = value.get("package").cloned().unwrap_or_else(|| json!({}));
    let package_name = package
        .get("package_name_cn")
        .and_then(Value::as_str)
        .or_else(|| package.get("package_name_en").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    let is_credit_package = package
        .get("is_credit_package")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let bonus_remain = metric_of(value, "usageBonusPackageCredit")
        .map(|m| num_of(m, "package_credit_remain"));
    let end_date = value
        .get("end_date")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok((
        package_name,
        is_credit_package,
        num_of(total_metric, "package_credit_amount"),
        num_of(total_metric, "package_credit_used"),
        num_of(total_metric, "package_credit_remain"),
        num_of(total_metric, "package_credit_expiring_amount"),
        bonus_remain,
        end_date,
    ))
}

/// 单账号实时积分查询（凭证就绪 → 签名请求 → 归一）。
pub fn fetch_credits(st: &AppState, account: &Value) -> Result<CodeArtsCreditsInfo, String> {
    let id = account.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let uid = account.get("user_id").and_then(Value::as_str).unwrap_or("").to_string();
    let name = account.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let cred = ensure_fresh_credential(st, account)?;
    let (status, parsed, raw) = snap_request("GET", STATISTICS_PATH, &[], None, &cred, false);
    if status != 200 {
        if super::codearts_common::classify_needs_relogin(status, &raw) {
            return Err("凭证失效（APIG.0301/0303/0602），请重新导入本机登录态或更新 AK/SK".into());
        }
        return Err(format!("积分查询失败（HTTP {status}）：{}", excerpt(&raw)));
    }
    let value = parsed.ok_or_else(|| format!("积分响应不是 JSON：{}", excerpt(&raw)))?;
    let (package_name, is_credit, total, used, remain, expiring, bonus_remain, end_date) =
        credits_from_response(&value)?;
    Ok(CodeArtsCreditsInfo {
        account_id: id,
        user_id: uid,
        name,
        package_name,
        is_credit_package: is_credit,
        total,
        used,
        remain,
        expiring,
        bonus_remain,
        end_date,
        fetched_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        error: None,
    })
}

/// 同步结果回写账号池（credits_* 缓存键拨新；失败不动池内旧值）。
/// 调度任务与 UI 实时刷新共用。
pub fn write_back_pool(st: &AppState, info: &CodeArtsCreditsInfo) {
    let db = crate::store::db(&st.data_dir);
    let mut pool = crate::store::docs::codearts_pool_load(&db);
    let Some(accounts) = pool.get_mut("accounts").and_then(Value::as_array_mut) else {
        return;
    };
    let mut changed = false;
    for acc in accounts.iter_mut() {
        if acc.get("id").and_then(Value::as_str) == Some(info.account_id.as_str()) {
            acc["credits_total"] = json!(info.remain);
            acc["credits_amount"] = json!(info.total);
            acc["credits_used"] = json!(info.used);
            acc["credits_expiring"] = json!(info.expiring);
            if let Some(bonus) = info.bonus_remain {
                acc["credits_bonus_remain"] = json!(bonus);
            }
            if !info.package_name.is_empty() {
                acc["package_name"] = json!(info.package_name);
            }
            acc["end_date"] = json!(info.end_date);
            acc["credits_fetched_at"] = json!(info.fetched_at);
            changed = true;
            break;
        }
    }
    if changed {
        if let Err(e) = crate::store::docs::codearts_pool_save(&db, &pool) {
            crate::fs_utils::app_log(&st.data_dir, &format!("CodeArts 积分回写账号池失败：{e}"));
        }
    }
}

/// 全账号积分同步（调度任务与 UI「刷新」共用）：
/// 逐账号取数 → 池回写 → 当日快照覆盖落库。返回汇总 JSON。
/// 空池返回 skipped（普通 skipped 语义：mark_run 关闭当日重复触发）。
pub fn run_sync_task(st: &AppState) -> Result<Value, String> {
    let pool = crate::store::docs::codearts_pool_load_checked(&crate::store::db(&st.data_dir))?;
    let empty = Vec::new();
    let accounts = pool.get("accounts").and_then(Value::as_array).unwrap_or(&empty);
    if accounts.is_empty() {
        return Ok(json!({ "ok": true, "skipped": "无 CodeArts 账号" }));
    }
    let mut ok = 0i64;
    let mut failed = 0i64;
    let mut snaps: Vec<Value> = Vec::new();
    for account in accounts {
        match fetch_credits(st, account) {
            Ok(info) => {
                ok += 1;
                snaps.push(json!({
                    "id": info.account_id,
                    "user_id": info.user_id,
                    "name": info.name,
                    "package_name": info.package_name,
                    "total": info.total,
                    "used": info.used,
                    "remain": info.remain,
                    "expiring": info.expiring,
                    "bonus_remain": info.bonus_remain,
                    "end_date": info.end_date,
                }));
                write_back_pool(st, &info);
            }
            Err(e) => {
                failed += 1;
                crate::fs_utils::app_log(
                    &st.data_dir,
                    &format!(
                        "CodeArts 积分同步失败（{}）：{e}",
                        account.get("name").and_then(Value::as_str).unwrap_or("?")
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
        if let Err(e) =
            crate::store::docs::codearts_credits_history_upsert(&crate::store::db(&st.data_dir), &snap)
        {
            crate::fs_utils::app_log(&st.data_dir, &format!("CodeArts 积分快照落库失败：{e}"));
        }
    }
    if ok == 0 && failed > 0 {
        return Err(format!("CodeArts 积分同步：{failed} 个账号全部失败（稍后自动重试）"));
    }
    Ok(json!({ "ok": true, "ok_count": ok, "failed": failed }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credits_from_response_normal_metrics() {
        let v = json!({
            "package": { "status": "normal", "package_name_cn": "个人体验版",
                         "is_credit_package": true, "is_token_package": false },
            "metrics": [
                { "name": "usageTotalPackageCredit", "package_credit_amount": 4000,
                  "package_credit_used": 123, "package_credit_remain": 3877,
                  "package_credit_expiring_amount": 4000, "value": 3.1 },
                { "name": "usageBonusPackageCredit", "package_credit_remain": 1000 }
            ],
            "end_date": "2026-12-31"
        });
        let (name, is_credit, total, used, remain, expiring, bonus, end_date) =
            credits_from_response(&v).unwrap();
        assert_eq!(name, "个人体验版");
        assert!(is_credit);
        assert_eq!((total, used, remain, expiring), (4000.0, 123.0, 3877.0, 4000.0));
        assert_eq!(bonus, Some(1000.0));
        assert_eq!(end_date, "2026-12-31");
    }

    #[test]
    fn credits_missing_total_metric_is_error_not_zero() {
        // usageTotalPackageCredit 缺失 = 无积分套餐 → Err（不用 0 冒充）
        let v = json!({
            "package": { "package_name_cn": "token 套餐", "is_credit_package": false },
            "metrics": [ { "name": "usageTokenChatMessages", "usage_token_num": 0 } ]
        });
        let e = credits_from_response(&v).unwrap_err();
        assert!(e.contains("usageTotalPackageCredit 缺失"), "{e}");
    }

    #[test]
    fn credits_string_numbers_tolerant() {
        let v = json!({
            "metrics": [
                { "name": "usageTotalPackageCredit", "package_credit_amount": "4000",
                  "package_credit_used": "123", "package_credit_remain": "3877" }
            ]
        });
        let (_, _, total, used, remain, _, _, _) = credits_from_response(&v).unwrap();
        assert_eq!((total, used, remain), (4000.0, 123.0, 3877.0));
    }
}
