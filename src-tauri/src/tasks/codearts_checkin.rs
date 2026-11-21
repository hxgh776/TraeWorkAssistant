//! CodeArts 每日签到（运营活动 ops 接口）。
//!
//! 协议（VENU-5 §2.5 定稿，CodeArts2API 实测交叉验证；旧 3.9.3 行为基准对齐）：
//! 1. `GET  /v1/ops/delivery?channel=IDE` → 信封 `{code:0, data:{items:[...]}}`；
//!    只领 `claimable==true && extra.triggerMode=="MANUAL_CLAIM"` 的活动
//!    （**绝不伪造触发事件**——事件类活动服务端拒绝直接领取）。
//! 2. `POST /v1/ops/claim` body `{campaignId, idempotentKey:<uuid>, channel:"IDE"}`
//!    （身份头额外 `Agent-Type: PromptCenter`；idempotentKey 每次新 UUID，重复调用不重复发放）。
//! 3. 业务码语义：`0` 成功；`40001` 今日已领（幂等中性）；`4006` 名额用完（中性）；
//!    `4004` 活动过期（中性跳过）；`4003` 账号禁用（熔断 needs_relogin）。
//!    HTTP 层 `401`/`APIG.0602`/`APIG.0301`/`APIG.0303` → 凭证失效/需重新登录
//!    （旧 3.9.3 基准：APIG.0301/0303 归 relogin，不落「请稍后重试」）。
//! 4. 无活动/无可领活动 → 中性 noop（`暂无可领取活动（活动可能未开放或已结束）`），
//!    不报错刷屏（活动 2026-12-31 截止，之后转 noop 常态）。
//!
//! 领取金额一律读 delivery 的 `benefitAmount`（官方 100–200 与社区实测 1000 矛盾，
//! 禁止硬编码）。`/v1/ops/confirm` 与已死旧 benefit 通道明确不做（claim 即到账）。

use serde::Serialize;
use serde_json::{json, Value};

use super::codearts_common::{
    classify_needs_relogin, ensure_fresh_credential, excerpt, snap_request,
};
use crate::state::AppState;

/// 单账号签到记录（落 codearts_checkin_results，90 天滚动）。
#[derive(Serialize, Clone)]
pub struct CodeArtsCheckinRecord {
    pub date: String,
    pub time: String,
    pub time_ms: i64,
    pub user_id: String,
    pub name: String,
    /// success | already | noop | fail
    pub status: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reward: Option<f64>,
    /// 领取的活动明细（campaignId / 名称 / 金额）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub campaigns: Option<Value>,
    /// 失败归因：needs_relogin = 凭证失效（重试注定失败，调度器据此剔除重试判定）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fail_kind: Option<String>,
}

/// 账号池条目的只读视图（签到轮内使用）
struct PoolEntry {
    id: String,
    user_id: String,
    name: String,
}

/// 读取待签到账号清单（可选 user_ids 指定）。
/// 池读取失败走 Err（审查 M-4 同款：不静默吞成空池）。
fn pool_entries(st: &AppState, user_ids: &[String]) -> Result<Vec<PoolEntry>, String> {
    let pool = crate::store::docs::codearts_pool_load_checked(&crate::store::db(&st.data_dir))?;
    let empty = Vec::new();
    let accounts = pool.get("accounts").and_then(Value::as_array).unwrap_or(&empty);
    let mut out = Vec::new();
    for acc in accounts {
        let uid = acc.get("user_id").and_then(Value::as_str).unwrap_or("").to_string();
        if !user_ids.is_empty() && !user_ids.iter().any(|u| *u == uid) {
            continue;
        }
        out.push(PoolEntry {
            id: acc.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
            user_id: uid,
            name: acc.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
        });
    }
    Ok(out)
}

/// delivery 单项是否可自动领取：`claimable && extra.triggerMode=="MANUAL_CLAIM"`
fn claimable_manual(item: &Value) -> bool {
    item.get("claimable").and_then(Value::as_bool).unwrap_or(false)
        && item
            .pointer("/extra/triggerMode")
            .and_then(Value::as_str)
            .map(|m| m.eq_ignore_ascii_case("MANUAL_CLAIM"))
            .unwrap_or(false)
}

/// claim 业务码 → (归类, 展示) 归类：success / already / noop / fail / disabled
fn classify_claim_code(code: i64, message: &str) -> (&'static str, &'static str) {
    match code {
        0 => ("success", "领取成功"),
        // 今日已领（幂等中性）；4006 今日名额已用完（中性）
        40001 | 4006 => ("already", "今日已领取"),
        // 活动过期 / 权益不存在：中性跳过
        4004 => ("noop", "活动已过期，已跳过"),
        // 账号禁用：熔断（needs_relogin）
        4003 => ("disabled", "账号被禁用"),
        // 事件类活动需真实触发（过滤器漏网兜底）
        _ if message.contains("触发接口激活") || message.contains("需通过触发") => {
            ("noop", "需真实事件触发，已跳过")
        }
        _ => ("fail", "领取失败"),
    }
}

/// 单账号签到：delivery → 过滤可领活动 → 逐个 claim → 聚合。
/// 返回 (status, message, reward, campaigns 明细, fail_kind)。
fn checkin_one(st: &AppState, account: &Value) -> (String, String, Option<f64>, Option<Value>, Option<String>) {
    let cred = match ensure_fresh_credential(st, account) {
        Ok(c) => c,
        Err(e) => {
            let kind = if e.contains("重新导入") || e.contains("重新授权") || e.contains("已过期") {
                Some("needs_relogin")
            } else {
                None
            };
            return ("fail".into(), e, None, None, kind.map(str::to_string));
        }
    };
    // ① 列活动（只读）
    let (status, parsed, raw) =
        snap_request("GET", "/v1/ops/delivery", &[("channel", "IDE")], None, &cred, true);
    if status != 200 || classify_needs_relogin(status, &raw) {
        let kind = if classify_needs_relogin(status, &raw) { Some("needs_relogin") } else { None };
        return (
            "fail".into(),
            format!("查询活动失败（HTTP {status}）：{}", excerpt(&raw)),
            None,
            None,
            kind.map(str::to_string),
        );
    }
    let Some(value) = parsed else {
        return (
            "fail".into(),
            format!("活动响应不是 JSON：{}", excerpt(&raw)),
            None,
            None,
            None,
        );
    };
    // 信封 code != 0 → 业务失败（如实落库；401/APIG 类已在上面归类）
    let envelope_code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
    if envelope_code != 0 {
        let msg = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        return (
            "fail".into(),
            format!("查询活动业务失败（code={envelope_code}）：{msg}"),
            None,
            None,
            None,
        );
    }
    let items = value
        .pointer("/data/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if items.is_empty() {
        return (
            "noop".into(),
            "暂无可领取活动（活动可能未开放或已结束）".into(),
            None,
            None,
            None,
        );
    }
    let claimable_items: Vec<&Value> =
        items.iter().filter(|item| claimable_manual(item)).collect();
    if claimable_items.is_empty() {
        return (
            "noop".into(),
            "无可自动领取的活动（事件类活动需真实触发，已跳过）".into(),
            None,
            None,
            None,
        );
    }
    // ② 逐个领取（幂等键每次新 UUID；金额读 benefitAmount，禁止硬编码）
    let mut ok = 0usize;
    let mut already = 0usize;
    let mut failed = 0usize;
    let mut reward = 0f64;
    let mut parts: Vec<String> = Vec::new();
    let mut disabled_reason: Option<String> = None;
    let mut campaigns: Vec<Value> = Vec::new();
    for item in &claimable_items {
        let campaign_id = item.get("campaignId").and_then(Value::as_i64).unwrap_or(0);
        let title = item.get("title").and_then(Value::as_str).unwrap_or("活动");
        let amount = item.get("benefitAmount").and_then(Value::as_f64).unwrap_or(0.0);
        let idempotent_key = uuid::Uuid::new_v4().to_string();
        let body = json!({ "campaignId": campaign_id, "idempotentKey": idempotent_key, "channel": "IDE" });
        let (c_status, c_parsed, c_raw) =
            snap_request("POST", "/v1/ops/claim", &[], Some(&body), &cred, true);
        if c_status != 200 || classify_needs_relogin(c_status, &c_raw) {
            if classify_needs_relogin(c_status, &c_raw) {
                return (
                    "fail".into(),
                    format!(
                        "凭证失效（HTTP {c_status}，APIG.0301/0303/0602 归凭证失效），请重新导入本机登录态或更新 AK/SK"
                    ),
                    None,
                    None,
                    Some("needs_relogin".into()),
                );
            }
            failed += 1;
            parts.push(format!("{title}：领取失败（HTTP {c_status}）{}", excerpt(&c_raw)));
            continue;
        }
        let Some(c_value) = c_parsed else {
            failed += 1;
            parts.push(format!("{title}：领取响应不是 JSON {}", excerpt(&c_raw)));
            continue;
        };
        let code = c_value.get("code").and_then(Value::as_i64).unwrap_or(-1);
        let message = c_value.get("message").and_then(Value::as_str).unwrap_or("");
        let (kind, _) = classify_claim_code(code, message);
        match kind {
            "success" => {
                ok += 1;
                reward += amount;
                campaigns.push(json!({ "id": campaign_id, "name": title, "reward": amount }));
                parts.push(format!("{title}：+{} 积分", fmt_amount(amount)));
            }
            "already" => {
                already += 1;
                parts.push(format!("{title}：今日已领取"));
            }
            "noop" => {
                parts.push(format!("{title}：需真实事件触发，已跳过"));
            }
            "disabled" => {
                disabled_reason = Some(format!("账号被禁用（{title}）"));
                parts.push(format!("{title}：账号被禁用"));
            }
            _ => {
                failed += 1;
                parts.push(format!("{title}：领取失败（code={code} {message}）"));
            }
        }
    }
    // ③ 聚合（禁用熔断优先；部分成功按 success 记账 + 明细呈现）
    if let Some(reason) = disabled_reason {
        return ("fail".into(), reason, None, None, Some("needs_relogin".into()));
    }
    let status = if ok > 0 {
        "success"
    } else if failed > 0 {
        "fail"
    } else if already > 0 {
        "already"
    } else {
        "noop"
    };
    let message = if status == "success" {
        format!("领取成功 {} 项：{}", ok, parts.join("；"))
    } else {
        parts.join("；")
    };
    let campaigns_json = if campaigns.is_empty() {
        None
    } else {
        Some(Value::Array(campaigns))
    };
    (
        status.into(),
        message,
        if reward > 0.0 { Some(reward) } else { None },
        campaigns_json,
        None,
    )
}

fn fmt_amount(amount: f64) -> String {
    if amount.fract() == 0.0 {
        format!("{}", amount as i64)
    } else {
        format!("{amount}")
    }
}

/// 执行一轮签到（多账号间隔防频控；进度经 emit 回调推送 NDJSON 行；
/// 结果按日落库——同 uid 同日重试轮覆盖）。
pub fn run_checkin_round(st: &AppState, user_ids: &[String], emit: &mut dyn FnMut(&Value)) -> Value {
    let entries = match pool_entries(st, user_ids) {
        Ok(v) => v,
        Err(e) => {
            let done = json!({ "type": "done", "ok": 0, "already": 0, "noop": 0, "failed": 1, "error": e });
            emit(&done);
            return done;
        }
    };
    emit(&json!({ "type": "start", "total": entries.len() }));
    let mut ok = 0i64;
    let mut already = 0i64;
    let mut noop = 0i64;
    let mut failed = 0i64;
    let mut failed_permanent = 0i64;
    let gap = crate::models::effective_checkin_gap(Some(st.settings().codearts_checkin_gap_secs));
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 && gap > 0 {
            std::thread::sleep(std::time::Duration::from_secs(gap));
        }
        let db = crate::store::db(&st.data_dir);
        let account = crate::store::docs::codearts_pool_load(&db)
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
                "name": entry.name,
                "status": "fail", "message": "账号不存在（池已变更）", "fail_kind": "gone",
            }));
            continue;
        };
        let (status, message, reward, campaigns, fail_kind) = checkin_one(st, &account);
        match status.as_str() {
            "success" => ok += 1,
            "already" => already += 1,
            "noop" => noop += 1,
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
            "status": status,
            "message": message,
            "reward": reward,
        }));
        let now = chrono::Local::now();
        let record = CodeArtsCheckinRecord {
            date: now.format("%Y-%m-%d").to_string(),
            time: now.format("%H:%M:%S").to_string(),
            time_ms: now.timestamp_millis(),
            user_id: entry.user_id.clone(),
            name: entry.name.clone(),
            status: status.clone(),
            message: message.clone(),
            reward,
            campaigns,
            fail_kind: fail_kind.clone(),
        };
        if let Ok(value) = serde_json::to_value(&record) {
            if let Err(e) =
                crate::store::docs::codearts_checkin_results_upsert(&crate::store::db(&st.data_dir), &value)
            {
                crate::fs_utils::app_log(&st.data_dir, &format!("CodeArts 签到结果落库失败：{e}"));
            }
        }
        if status == "fail" {
            crate::fs_utils::app_log(&st.data_dir, &format!("CodeArts 签到失败（{}）：{message}", entry.name));
        }
        write_back_checkin_state(st, &entry.id, &status, &message);
        // needs_relogin 熔断标记（与 minimax 同款：失效账号不再自动重试）
        if fail_kind.as_deref() == Some("needs_relogin") {
            let db = crate::store::db(&st.data_dir);
            if let Err(e) =
                super::codearts_common::mark_needs_relogin(&db, &entry.id, &message)
            {
                crate::fs_utils::app_log(&st.data_dir, &format!("[codearts] 熔断标记落库失败：{e}"));
            }
        }
    }
    let done = json!({
        "type": "done",
        "ok": ok,
        "already": already,
        "noop": noop,
        "failed": failed,
        // 调度器重试口径：凭证失效类失败重试注定失败，剔除（对齐 minimax failed_permanent）
        "failed_permanent": failed_permanent,
    });
    emit(&done);
    done
}

/// 账号级最近签到状态回写（last_checkin_status / last_checkin_date / last_checkin_message）。
/// 落库失败静默：仅影响账号列表徽标展示，不影响签到结果表与调度链。
fn write_back_checkin_state(st: &AppState, account_id: &str, status: &str, message: &str) {
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let db = crate::store::db(&st.data_dir);
    let mut pool = crate::store::docs::codearts_pool_load(&db);
    let Some(accounts) = pool.get_mut("accounts").and_then(Value::as_array_mut) else {
        return;
    };
    for acc in accounts.iter_mut() {
        if acc.get("id").and_then(Value::as_str) == Some(account_id) {
            acc["last_checkin_status"] = json!(status);
            acc["last_checkin_date"] = json!(date);
            acc["last_checkin_message"] = json!(message);
            if let Err(e) = crate::store::docs::codearts_pool_save(&db, &pool) {
                crate::fs_utils::app_log(&st.data_dir, &format!("CodeArts 签到状态回写失败：{e}"));
            }
            return;
        }
    }
}

/// 签到轮次全局锁（应用内调度器与 UI 手动触发互斥；CLI 另用跨进程互斥体）。
static CODEARTS_ROUND_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) fn try_acquire_round() -> Result<tokio::sync::MutexGuard<'static, ()>, String> {
    CODEARTS_ROUND_LOCK
        .try_lock()
        .map_err(|_| "已有 CodeArts 签到任务在执行中，请等待当前轮次完成".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claimable_manual_filters_trigger_mode() {
        let item = json!({
            "campaignId": 1, "title": "每日签到", "type": "USER_LOGIN",
            "benefitAmount": 1000, "status": "ELIGIBLE", "claimable": true,
            "extra": { "triggerMode": "MANUAL_CLAIM", "triggerEvent": "user.login" }
        });
        assert!(claimable_manual(&item));
        // 事件类（非 MANUAL_CLAIM）不可自动领取
        let event_item = json!({
            "campaignId": 2, "claimable": true,
            "extra": { "triggerMode": "TRIGGER_EVENT" }
        });
        assert!(!claimable_manual(&event_item));
        // 不可领状态
        let not_claimable = json!({
            "campaignId": 3, "claimable": false,
            "extra": { "triggerMode": "MANUAL_CLAIM" }
        });
        assert!(!claimable_manual(&not_claimable));
        // 字段缺失不 panic
        assert!(!claimable_manual(&json!({})));
    }

    #[test]
    fn classify_claim_code_semantics() {
        assert_eq!(classify_claim_code(0, "").0, "success");
        assert_eq!(classify_claim_code(40001, "等待下次权益刷新").0, "already");
        assert_eq!(classify_claim_code(4006, "名额已用完").0, "already");
        assert_eq!(classify_claim_code(4004, "权益不存在").0, "noop");
        assert_eq!(classify_claim_code(4003, "禁用").0, "disabled");
        assert_eq!(classify_claim_code(-1, "该活动类型需要先通过触发接口激活，无法直接领取").0, "noop");
        assert_eq!(classify_claim_code(500, "boom").0, "fail");
    }

    #[test]
    fn pool_entries_filters_users() {
        let dir = std::env::temp_dir().join(format!("aiwork_codearts_pool_test_{}", std::process::id()));
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
        let db = crate::store::db(&st.data_dir);
        let pool = json!({ "accounts": [
            { "id": "ca-1", "user_id": "u1", "name": "甲" },
            { "id": "ca-2", "user_id": "u2", "name": "乙" }
        ]});
        crate::store::docs::codearts_pool_save(&db, &pool).unwrap();
        let all = pool_entries(&st, &[]).unwrap();
        assert_eq!(all.len(), 2);
        let picked = pool_entries(&st, &["u2".to_string()]).unwrap();
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].name, "乙");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn probe_state(tag: &str) -> (std::path::PathBuf, AppState) {
        let dir = std::env::temp_dir().join(format!(
            "aiwork_codearts_probe_{tag}_{}",
            std::process::id()
        ));
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
        (dir, st)
    }

    /// 真机签到探针（#[ignore]，`cargo test codearts_live_checkin_probe -- --ignored --nocapture`）：
    /// 只读本机登录态 → 建临时池/凭证 → run_checkin_round 完整轮（delivery → claim）→
    /// 积分同步（statistics/plugin 快照）。等价应用每日签到真实行为——领取的是
    /// 本机账号当日积分，claim 幂等（idempotentKey 重复不重复发放）。
    /// 输出脱敏：进度行只含状态与 message，不打印凭证。
    #[test]
    #[ignore]
    fn codearts_live_checkin_probe() {
        let Some(session) = crate::tasks::codearts_common::read_local_session() else {
            println!("[checkin-probe] 无本机登录态，跳过");
            return;
        };
        let (_dir, st) = probe_state("checkin");
        let cred = session.credential("local_import".into());
        let id = crate::tasks::codearts_common::account_id(&cred.access_key, &cred.secret_key);
        let display = if session.identity_name.is_empty() {
            format!("华为云 {}", &cred.access_key[..cred.access_key.len().min(6)])
        } else {
            session.identity_name.clone()
        };
        let db = crate::store::db(&st.data_dir);
        crate::store::docs::codearts_pool_save(
            &db,
            &json!({ "accounts": [{ "id": id, "user_id": format!("probe-{}", &id[3..]), "name": display }] }),
        )
        .expect("落临时池");
        crate::store::docs::codearts_token_put(&db, &id, &cred.to_json()).expect("落临时凭证");
        let done = run_checkin_round(&st, &[], &mut |ev| {
            println!("[checkin-probe] {}", serde_json::to_string(ev).unwrap_or_default());
        });
        println!("[checkin-probe] done={done}");
        // 积分同步（只读统计 + 快照）
        match crate::tasks::codearts_credits::run_sync_task(&st) {
            Ok(v) => println!("[checkin-probe] credits={v}"),
            Err(e) => println!("[checkin-probe] credits 失败：{e}"),
        }
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }
}
