//! CodeArts 每日签到（运营活动 ops 接口）。
//!
//! 协议（VENU-5 §2.5 定稿，CodeArts2API 实测交叉验证；旧 3.9.3 行为基准对齐）：
//! 1. `GET  /v1/ops/delivery?channel=IDE` → 信封 `{code:0, data:{items:[...]}}`；
//!    只领 `claimable==true` 且 **type=USER_LOGIN** 且触发模式为 MANUAL_CLAIM（或
//!    未声明）的活动（**绝不伪造触发事件**——TRIGGER_EVENT 事件类活动不自动领取，
//!    服务端拒绝直接领取；未声明 triggerMode 的 USER_LOGIN 可领项交服务器仲裁，
//!    拒绝则如实落库为 noop）。非 USER_LOGIN 类型显式排除代领（VENU-42 复核口径）：
//!    STUDENT_CERTIFIED（学生认证）/ NEW_USER_REGISTER 只展示不领取——认证状态由
//!    官方端核验，issue 原口径「仅当旧版已代领时应用侧才代领」，旧落库行无记录。
//!    VENU-42：旧 3.9.3 的解析面与本构建一致（exe 串 `claimable/extra/triggerMode`、
//!    单 MANUAL_CLAIM 字面量、channel=IDE），且旧版日实领 Skill 大赛 1000 CREDIT
//!    （11-17/11-18 落库行）——「重建后只领 100」主因是凭证失效而非过滤器；
//!    放宽未声明 triggerMode 是对活动面结构漂移的防御，金测锁定语义。
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

/// delivery 单项是否可自动领取（VENU-42 复核修复版）：`claimable==true` 且
/// **type=USER_LOGIN** 且触发模式为 MANUAL_CLAIM 或未声明（extra/triggerMode 缺失、
/// 空串——活动面结构漂移防御）。
/// 非 USER_LOGIN 类型显式排除代领（复核口径，不靠 claimable 单一条件）：
/// `STUDENT_CERTIFIED`（学生认证——一次性认证奖励，认证状态由官方端核验，本端
/// 代领等于替用户确认未核验状态；issue 原口径「仅当旧版已代领时应用侧才代领」，
/// 旧落库行无 STUDENT_CERTIFIED 记录）与 `NEW_USER_REGISTER`（AUTO_GRANT 一次性）
/// 只展示不领取；`TRIGGER_EVENT` 明确排除：事件类活动需真实触发，本端绝不伪造事件；
/// 未声明 triggerMode 的 USER_LOGIN 可领项交服务器仲裁（拒绝返回归 noop）。
/// 类型缺失/未知同样不领取（宁漏不越权，标题经如实枚举可见）。
fn claimable_manual(item: &Value) -> bool {
    match item.get("type").and_then(Value::as_str) {
        Some(t) if t.eq_ignore_ascii_case("USER_LOGIN") => {}
        _ => return false,
    }
    if !item.get("claimable").and_then(Value::as_bool).unwrap_or(false) {
        return false;
    }
    match item.pointer("/extra/triggerMode").and_then(Value::as_str) {
        None => true,
        Some(m) => m.is_empty() || m.eq_ignore_ascii_case("MANUAL_CLAIM"),
    }
}

/// delivery 中「事件触发类、本端不自动领取」的活动标题（claimable 且
/// TRIGGER_EVENT）。VENU-42 复验后仅金测使用——生产路径由 delivery_enumeration
/// 的事件触发类桶承担（R1：领取轮不再单独追加事件类段落，避免重复）。
#[cfg(test)]
fn event_gated_titles(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter(|item| {
            item.get("claimable").and_then(Value::as_bool).unwrap_or(false)
                && item
                    .pointer("/extra/triggerMode")
                    .and_then(Value::as_str)
                    .map(|m| m.eq_ignore_ascii_case("TRIGGER_EVENT"))
                    .unwrap_or(false)
        })
        .map(|item| {
            item.get("title")
                .and_then(Value::as_str)
                .unwrap_or("活动")
                .to_string()
        })
        .collect()
}

/// delivery 全量活动的如实枚举（VENU-42：每个渠道的领取结果对用户可见）。
/// 只归类不可自动领取项（可自动领取项走 claim 路径，逐项如实落库）：
/// `status==CONFIRMED/CLAIMED/CONSUMED` → 已领取/已发放（CONSUMED = 已领且积分已
/// 消耗，2026-11-22 装机版真机 dump 实测的新状态值，同属已领）；`triggerMode==
/// TRIGGER_EVENT` → 事件触发类（列标题，官方端完成）；其余 → 暂不可领（需官方端
/// 完成对应条件，列标题——学生认证 STUDENT_CERTIFIED 渠道无论 claimable 与否都落
/// 此桶（类型被排除代领），标题必须可见，复核口径「展示 + 引导」）。
fn delivery_enumeration(items: &[Value]) -> String {
    let mut confirmed = 0usize;
    let mut gated: Vec<String> = Vec::new();
    let mut other: Vec<String> = Vec::new();
    for item in items {
        if claimable_manual(item) {
            continue;
        }
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("活动")
            .to_string();
        let confirmed_item = item
            .get("status")
            .and_then(Value::as_str)
            .map(|s| {
                s.eq_ignore_ascii_case("CONFIRMED")
                    || s.eq_ignore_ascii_case("CLAIMED")
                    || s.eq_ignore_ascii_case("CONSUMED")
            })
            .unwrap_or(false);
        let gated_item = item
            .pointer("/extra/triggerMode")
            .and_then(Value::as_str)
            .map(|m| m.eq_ignore_ascii_case("TRIGGER_EVENT"))
            .unwrap_or(false);
        if confirmed_item {
            confirmed += 1;
        } else if gated_item {
            gated.push(title);
        } else {
            other.push(title);
        }
    }
    let mut parts = vec![format!("delivery 共 {} 个活动", items.len())];
    if confirmed > 0 {
        parts.push(format!("已领取/已发放 {confirmed}"));
    }
    if !gated.is_empty() {
        parts.push(format!(
            "事件触发类 {} 个需在官方端完成（{}）",
            gated.len(),
            gated.join("、")
        ));
    }
    if !other.is_empty() {
        parts.push(format!(
            "暂不可领（需官方端完成对应条件）{}（{}）",
            other.len(),
            other.join("、")
        ));
    }
    parts.join("；")
}

/// 「无可自动领取」的如实说明（VENU-42）：不再沿用旧文案的事件类误归因
/// （stage-3 实测：filter 空转时 3 个活动实为客户端已领 CONFIRMED，非事件类），
/// 改为基线短语 + delivery 全量枚举，被跳过的渠道标题可见。
fn noop_delivery_message(items: &[Value]) -> String {
    format!("无可自动领取的活动；{}", delivery_enumeration(items))
}

/// 签到轮消息统一组装（VENU-42 F1 复验修复）：success / already / fail 聚合、
/// 禁用熔断、claim 期凭证熔断共用——非 noop 分支同样追加 delivery 全量枚举，
/// 保证「真正赚积分的路径」上学生认证/Skill 大赛等被跳过渠道的标题依旧可见。
/// 事件触发类桶已并入枚举（不再单独追加段落，避免重复）；items 为空（凭证/
/// 网络在 delivery 之前就失败）时只拼 parts，不追加枚举。
fn compose_round_message(status: &str, ok: usize, parts: &[String], items: &[Value]) -> String {
    let mut message = if status == "success" {
        format!("领取成功 {ok} 项：{}", parts.join("；"))
    } else {
        parts.join("；")
    };
    if !items.is_empty() {
        message = format!("{message}；{}", delivery_enumeration(items));
    }
    message
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
            noop_delivery_message(&items),
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
                // 凭证失效熔断（claim 期）：消息走统一组装——已领明细 + 全量枚举不丢
                parts.push(format!(
                    "凭证失效（HTTP {c_status}，APIG.0301/0303/0602 归凭证失效），请重新导入本机登录态或更新 AK/SK"
                ));
                return (
                    "fail".into(),
                    compose_round_message("fail", ok, &parts, &items),
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
    if disabled_reason.is_some() {
        // 禁用熔断：消息走统一组装——parts 已含逐项明细（含「账号被禁用」行），
        // 追加全量枚举不丢被跳过渠道
        return (
            "fail".into(),
            compose_round_message("fail", ok, &parts, &items),
            None,
            None,
            Some("needs_relogin".into()),
        );
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
    // VENU-42 F1 复验修复：非 noop 分支同样追加全量枚举——学生认证/Skill 大赛等
    // 被跳过渠道的标题在「真正赚积分的路径」上也可见（F1：之前只在 noop 早退有）
    let message = compose_round_message(&status, ok, &parts, &items);
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

/// 只读 delivery 探针（VENU-42 步骤 1）：全量活动 dump（campaignId / title / type /
/// claimable / status / benefitAmount / triggerMode / triggerEvent），
/// **不 claim、不改池、不改签到状态**——凭证恢复后跑「活动清单对照报告」的入口
/// （CLI `--task-run codearts-delivery-dump`；与旧版实领清单对照定稿过滤器语义）。
/// 输出脱敏：仅活动字段，无凭证内容；凭证失败如实记 error 不熔断。
pub fn dump_delivery(st: &AppState) -> Result<Value, String> {
    let pool = crate::store::docs::codearts_pool_load_checked(&crate::store::db(&st.data_dir))?;
    let empty = Vec::new();
    let accounts = pool.get("accounts").and_then(Value::as_array).unwrap_or(&empty);
    if accounts.is_empty() {
        return Ok(json!({ "ok": true, "skipped": "CodeArts 账号池为空" }));
    }
    let mut rows = Vec::new();
    for acc in accounts {
        let user_id = acc.get("user_id").and_then(Value::as_str).unwrap_or("").to_string();
        let name = acc.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let cred = match ensure_fresh_credential(st, acc) {
            Ok(c) => c,
            Err(e) => {
                rows.push(json!({ "user_id": user_id, "name": name, "error": e }));
                continue;
            }
        };
        let (status, parsed, raw) =
            snap_request("GET", "/v1/ops/delivery", &[("channel", "IDE")], None, &cred, true);
        if status != 200 || classify_needs_relogin(status, &raw) {
            rows.push(json!({
                "user_id": user_id, "name": name, "http": status,
                "error": excerpt(&raw),
            }));
            continue;
        }
        let Some(value) = parsed else {
            rows.push(json!({
                "user_id": user_id, "name": name, "http": status,
                "error": format!("响应不是 JSON：{}", excerpt(&raw)),
            }));
            continue;
        };
        let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
        let empty_arr = Vec::new();
        let items = value
            .pointer("/data/items")
            .and_then(Value::as_array)
            .unwrap_or(&empty_arr);
        // 只保留活动字段（脱敏白名单：extra 仅取 triggerMode / triggerEvent）
        let slim: Vec<Value> = items
            .iter()
            .map(|item| {
                json!({
                    "campaignId": item.get("campaignId"),
                    "title": item.get("title"),
                    "type": item.get("type"),
                    "claimable": item.get("claimable"),
                    "status": item.get("status"),
                    "benefitAmount": item.get("benefitAmount"),
                    "triggerMode": item.pointer("/extra/triggerMode"),
                    "triggerEvent": item.pointer("/extra/triggerEvent"),
                })
            })
            .collect();
        rows.push(json!({
            "user_id": user_id, "name": name, "http": status, "code": code,
            "items": slim,
        }));
    }
    Ok(json!({ "ok": true, "accounts": rows }))
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
        // 事件类（TRIGGER_EVENT）不可自动领取——绝不伪造触发事件（VENU-42 金测）
        let event_item = json!({
            "campaignId": 2, "type": "USER_LOGIN", "claimable": true,
            "extra": { "triggerMode": "TRIGGER_EVENT" }
        });
        assert!(!claimable_manual(&event_item));
        // 不可领状态
        let not_claimable = json!({
            "campaignId": 3, "type": "USER_LOGIN", "claimable": false,
            "extra": { "triggerMode": "MANUAL_CLAIM" }
        });
        assert!(!claimable_manual(&not_claimable));
        // 字段缺失不 panic（claimable 缺失 → 不可领）
        assert!(!claimable_manual(&json!({})));
        // VENU-42 放宽：triggerMode 未声明（extra 整体缺失 / triggerMode 缺失 / 空串）
        // 的 USER_LOGIN 可领项交服务器仲裁，不再因结构漂移被静默过滤
        assert!(claimable_manual(&json!({ "type": "USER_LOGIN", "claimable": true })));
        assert!(claimable_manual(&json!({
            "type": "USER_LOGIN", "claimable": true, "extra": {}
        })));
        assert!(claimable_manual(&json!({
            "type": "USER_LOGIN", "claimable": true, "extra": { "triggerMode": "" }
        })));
        // 小写形态同旧基线：大小写不敏感（type 与 triggerMode 双向）
        assert!(claimable_manual(&json!({
            "type": "user_login", "claimable": true,
            "extra": { "triggerMode": "manual_claim" }
        })));
        // 复核修复项 1 金测（VENU-42）：非 USER_LOGIN 类型显式排除代领，
        // 不靠 claimable 单一条件——学生认证（STUDENT_CERTIFIED）即便转
        // claimable=true + MANUAL_CLAIM 也绝不自动领取（认证状态由官方端核验，
        // 旧落库行无代领记录；issue 原口径维持）
        let student_claimable = json!({
            "campaignId": 39, "title": "Student Certification: Claim 5000 Credits in 2026",
            "type": "STUDENT_CERTIFIED", "claimable": true, "status": "ELIGIBLE",
            "benefitAmount": 5000, "extra": { "triggerMode": "MANUAL_CLAIM" }
        });
        assert!(!claimable_manual(&student_claimable));
        let student_lowercase = json!({
            "campaignId": 2, "type": "student_certified", "claimable": true,
            "extra": { "triggerMode": "MANUAL_CLAIM" }
        });
        assert!(!claimable_manual(&student_lowercase));
        // NEW_USER_REGISTER（AUTO_GRANT 一次性）同理只展示
        let new_user = json!({
            "campaignId": 4, "type": "NEW_USER_REGISTER", "claimable": true,
            "extra": { "triggerMode": "MANUAL_CLAIM" }
        });
        assert!(!claimable_manual(&new_user));
        // 类型缺失/未知 → 不领取（宁漏不越权，标题经如实枚举可见）
        assert!(!claimable_manual(&json!({ "claimable": true })));
        assert!(!claimable_manual(&json!({
            "type": "SOMETHING_ELSE", "claimable": true,
            "extra": { "triggerMode": "MANUAL_CLAIM" }
        })));
    }

    /// VENU-42 金测：事件触发类标题提取 + delivery 如实枚举 + noop 消息。
    /// 场景取自 stage-3 真机探针观测（6 活动中 3 个 MANUAL_CLAIM 均已 CONFIRMED）
    /// 与「Skill 大赛可能为 TRIGGER_EVENT」假设的合成 fixture。
    #[test]
    fn delivery_enumeration_reports_skipped_channels() {
        let items = vec![
            // 可自动领取（USER_LOGIN，走 claim 路径，不在枚举内）
            json!({
                "campaignId": 1, "title": "Daily Check-in: Claim 100 Credits",
                "type": "USER_LOGIN", "claimable": true, "status": "ELIGIBLE",
                "benefitAmount": 100,
                "extra": { "triggerMode": "MANUAL_CLAIM" }
            }),
            // 已领取（客户端/历史轮 CONFIRMED）
            json!({
                "campaignId": 2, "title": "Daily Check-in Reward for Skill Innovation Challenge",
                "type": "USER_LOGIN", "claimable": false, "status": "CONFIRMED",
                "benefitAmount": 1000,
                "extra": { "triggerMode": "MANUAL_CLAIM" }
            }),
            // 已领取且积分已消耗（CONSUMED——2026-11-22 装机版真机 dump 实测状态值，
            // 同属已领，不得误落「暂不可领」）
            json!({
                "campaignId": 34, "title": "Daily Check-in: Claim 100 Credits",
                "type": "USER_LOGIN", "claimable": false, "status": "CONSUMED",
                "benefitAmount": 100,
                "extra": { "triggerMode": "MANUAL_CLAIM" }
            }),
            // 学生认证（STUDENT_CERTIFIED）**已认证转可领**（claimable=true）仍不代领
            // （复核修复项 1）——归「暂不可领（需官方端完成对应条件）」桶，标题可见
            json!({
                "campaignId": 39, "title": "Student Certification: Claim 5000 Credits in 2026",
                "type": "STUDENT_CERTIFIED", "claimable": true, "status": "ELIGIBLE",
                "benefitAmount": 5000,
                "extra": { "triggerMode": "MANUAL_CLAIM" }
            }),
            // 事件触发类（本端不自动领取，标题如实上报）
            json!({
                "campaignId": 3, "title": "学生认证奖励",
                "type": "USER_LOGIN", "claimable": true, "status": "ELIGIBLE",
                "benefitAmount": 200,
                "extra": { "triggerMode": "TRIGGER_EVENT" }
            }),
            // 事件触发类但不可领（未完成事件）
            json!({
                "campaignId": 4, "title": "Skill 大赛事件奖励",
                "type": "USER_LOGIN", "claimable": false, "status": "IN_PROGRESS",
                "extra": { "triggerMode": "TRIGGER_EVENT" }
            }),
            // 其余未开放（类型缺失同样不领取）
            json!({
                "campaignId": 5, "title": "其他活动", "claimable": false, "status": "ELIGIBLE",
            }),
        ];
        let gated = event_gated_titles(&items);
        // 可领取的 USER_LOGIN 事件类才计入「领取路径跳过」清单（post-loop note 用）
        assert_eq!(gated, vec!["学生认证奖励".to_string()]);
        let enumeration = delivery_enumeration(&items);
        // 枚举更宽：所有 TRIGGER_EVENT 渠道（含不可领）都列标题，渠道可见性优先
        assert!(enumeration.contains("delivery 共 7 个活动"), "{enumeration}");
        assert!(enumeration.contains("已领取/已发放 2"), "{enumeration}");
        assert!(
            enumeration.contains(
                "事件触发类 2 个需在官方端完成（学生认证奖励、Skill 大赛事件奖励）"
            ),
            "{enumeration}"
        );
        assert!(
            enumeration.contains(
                "暂不可领（需官方端完成对应条件）2（Student Certification: Claim 5000 Credits in 2026、其他活动）"
            ),
            "{enumeration}"
        );
        // 复核修复项 1 断言：学生认证渠道即便 claimable=true 也绝不进入领取集合，
        // 只在枚举中如实展示（标题 + 需官方端完成条件）
        assert!(!claimable_manual(&items[2]));
        let noop = noop_delivery_message(&items);
        assert!(noop.starts_with("无可自动领取的活动；delivery 共 7 个活动"), "{noop}");
        // 全部可领时枚举只剩总数（USER_LOGIN 可领项不进枚举）
        let all_claimable = vec![json!({
            "campaignId": 9, "type": "USER_LOGIN", "claimable": true
        })];
        assert_eq!(delivery_enumeration(&all_claimable), "delivery 共 1 个活动");
    }

    /// VENU-42 F1/R1 金测：存在可领活动（真正赚积分的路径）时，success 消息仍保留
    /// 被跳过渠道的全量枚举——学生认证标题可见、事件触发类整条恰好出现一次
    /// （compose_round_message 统一组装后不再单独追加事件类段落，杜绝重复）。
    #[test]
    fn round_message_keeps_skipped_channels_when_claimable_exists() {
        let items = vec![
            // ① USER_LOGIN 可领（走 claim，success）
            json!({
                "campaignId": 34, "title": "Daily Check-in: Claim 100 Credits",
                "type": "USER_LOGIN", "claimable": true, "status": "ELIGIBLE",
                "benefitAmount": 100,
                "extra": { "triggerMode": "MANUAL_CLAIM" }
            }),
            // ② 学生认证（STUDENT_CERTIFIED，claimable=false，条件未满足）——枚举可见
            json!({
                "campaignId": 39, "title": "Student Certification: Claim 5000 Credits in 2026",
                "type": "STUDENT_CERTIFIED", "claimable": false, "status": "ELIGIBLE",
                "benefitAmount": 5000,
                "extra": { "triggerMode": "MANUAL_CLAIM" }
            }),
            // ③ 事件触发类（不自动领取，枚举事件触发类桶列标题）
            json!({
                "campaignId": 3, "title": "邀请好友事件奖励",
                "type": "USER_LOGIN", "claimable": true, "status": "ELIGIBLE",
                "benefitAmount": 200,
                "extra": { "triggerMode": "TRIGGER_EVENT" }
            }),
        ];
        let parts = vec!["Daily Check-in: Claim 100 Credits：+100 积分".to_string()];
        let message = compose_round_message("success", 1, &parts, &items);
        assert!(message.starts_with("领取成功 1 项："), "{message}");
        assert!(message.contains("Daily Check-in: Claim 100 Credits：+100 积分"), "{message}");
        assert!(message.contains("暂不可领（需官方端完成对应条件）"), "{message}");
        assert!(message.contains("Student Certification"), "{message}");
        // 事件触发类整条恰好出现一次（删除单独追加段落后由枚举桶唯一承担）
        assert_eq!(message.matches("事件触发类").count(), 1, "{message}");
        assert!(message.contains("事件触发类 1 个需在官方端完成（邀请好友事件奖励）"), "{message}");
        // 对照：already / fail 聚合同样带全量枚举（F1 的两条常规路径）
        let already_parts = vec!["Daily Check-in: Claim 100 Credits：今日已领取".to_string()];
        let already_message = compose_round_message("already", 0, &already_parts, &items);
        assert!(already_message.contains("今日已领取"), "{already_message}");
        assert!(already_message.contains("Student Certification"), "{already_message}");
        assert_eq!(already_message.matches("事件触发类").count(), 1, "{already_message}");
        // 对照：items 为空（凭证/网络早于 delivery 失败）只拼 parts，不追加枚举
        assert_eq!(
            compose_round_message("fail", 0, &["凭证已失效".to_string()], &[]),
            "凭证已失效"
        );
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

    /// VENU-42 金测：只读探针 dump_delivery 的离线路径——空池跳过；
    /// 有账号无凭证时如实记 error（不发网络、不熔断、不改池）。
    #[test]
    fn dump_delivery_offline_paths() {
        let dir = std::env::temp_dir().join(format!("aiwork_codearts_dump_{}", std::process::id()));
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
        // 空池 → skipped
        let out = dump_delivery(&st).unwrap();
        assert_eq!(
            out.get("skipped").and_then(Value::as_str),
            Some("CodeArts 账号池为空")
        );
        // 有账号无凭证 → 每账号如实 error，不 panic
        let db = crate::store::db(&st.data_dir);
        crate::store::docs::codearts_pool_save(
            &db,
            &json!({ "accounts": [{ "id": "ca-d1", "user_id": "u1", "name": "甲" }] }),
        )
        .unwrap();
        let out = dump_delivery(&st).unwrap();
        let rows = out.get("accounts").and_then(Value::as_array).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].get("error").and_then(Value::as_str).unwrap_or("").contains("无凭证"));
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
