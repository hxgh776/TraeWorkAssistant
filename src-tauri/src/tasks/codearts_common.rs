//! CodeArts（华为云码道 CodeArts Agent）共享层：SDK-HMAC-SHA256 签名器、
//! STS 凭证存储与刷新、本机登录态（state.vscdb）导入、身份查询。
//!
//! 协议依据：VENU-5《MiniMax与CodeArts签到积分接入规格.md》§2（Stage 1 定稿，
//! CodeArts2API 社区实测交叉验证）+ 旧 3.9.3 行为基准（VENU-37 重建，旧 exe 检索）。
//!
//! - 业务 API 主域 `https://snap-access.cn-north-4.myhuaweicloud.com`，签名
//!   SDK-HMAC-SHA256。真机探针 + CodeArts2API signer.go 逐字对齐，三处要点
//!   （金测锁定，缺一必 APIG.0301）：
//!   ① CanonicalURI 路径按段 urlEncode 后强制补尾斜杠；
//!   ② CanonicalHeaders 每行自带尾 `\n`，拼 CanonicalRequest 时再补一个 `\n`
//!   ——SignedHeaders 前多一个空行；
//!   ③ Authorization 用华为云 JS SDK 真实格式 `SDK-HMAC-SHA256 Access=<AK>,
//!   SignedHeaders=…, Signature=…`（**非 AWS 风格 Credential=AK/date/region/…**，
//!   本机真机探针实测 Credential 形态恒 0301、Access= 形态过签）。全部请求头
//!   （+ host）参与签名。
//! - 凭证 = 华为云 STS 临时密钥三元组 {AccessKey, SecretKey, SecurityToken}
//!   （+ refresh_token，无 PAT）。三通道：桌面端登录态导入（主）/ 浏览器 OAuth
//!   （授权码 + PKCE + DPoP）/ 手工 IAM AK/SK。
//! - 刷新红线（issue 硬约束）：**绝不消费本机导入凭证的 refresh_token**——
//!   单次轮转会作废桌面端登录链。local_import / aksk 通道不主动刷新：过期时
//!   重读 state.vscdb 对齐客户端续期结果；仅 oauth 通道（本应用自建的独立授权链）
//!   允许互斥刷新 + 成功立即落库 + 失效熔断。
//! - 只读约束：不写回 state.vscdb；凭证只落本应用 codearts_tokens 表；
//!   日志/事件/错误文案不含 AK/SK/SecurityToken。

use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::state::AppState;

// ── 域名与常量（VENU-5 §2 定稿；客户端升级更换时集中热更此处并补金测）──────

/// 业务 API 主域（备用 `…myhuaweicloud.cn` 等价，暂不启用）
pub const SNAP_BASE: &str = "https://snap-access.cn-north-4.myhuaweicloud.com";
/// IAM / STS 令牌域（OAuth 换票 + caller-identity 查身份）
pub const STS_BASE: &str = "https://sts.cn-north-4.myhuaweicloud.com";
/// 业务身份头（对齐 CodeArts Agent 26.9.902，VENU-5 §2.4）
const APP_ID: &str = "CodeAgent3.0";
const PLUGIN_NAME: &str = "codemate_vscode";
pub const PLUGIN_VERSION: &str = "26.9.902";
const IDE_NAME: &str = "VSCode";
/// 运营活动端点（ops/*）额外身份头
pub const AGENT_TYPE: &str = "PromptCenter";
/// state.vscdb 主键（桌面端登录态；HuaweiCloudSession）
pub const SESSION_VSCDB_KEY: &str =
    r#"secret://{"extensionId":"huaweicloud.authentication","key":"HuaweiCloudSession"}"#;
/// STS 凭证有效期余量（提前 5 分钟视为过期，留刷新/重读窗口）
pub const EXPIRY_MARGIN_MS: i64 = 5 * 60 * 1000;

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn md5_hex(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// RFC3986 percent-encode（华为云签名口径：非 [A-Za-z0-9-_.~] 全转义，空格 → %20）
fn rfc3986_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(fixed_hex_hi(byte));
            out.push(fixed_hex_lo(byte));
        }
    }
    out
}

fn fixed_hex_hi(byte: u8) -> char {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    HEX[(byte >> 4) as usize] as char
}

fn fixed_hex_lo(byte: u8) -> char {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    HEX[(byte & 0x0f) as usize] as char
}

// ── SDK-HMAC-SHA256 签名器（两个坑：尾斜杠 + 空行，金测锁定）──────────────

/// CanonicalURI：路径按段 urlEncode 后**强制补尾斜杠**（坑①）。
/// `/v1/ops/claim` → `/v1/ops/claim/`；已带尾斜杠或根路径 `/` 原样。
pub fn canonical_uri(path: &str) -> String {
    let trimmed = path.trim();
    let trimmed = trimmed.strip_prefix(SNAP_BASE).unwrap_or(trimmed);
    let encoded = trimmed
        .split('/')
        .map(rfc3986_encode)
        .collect::<Vec<_>>()
        .join("/");
    if encoded.ends_with('/') {
        encoded
    } else {
        format!("{encoded}/")
    }
}

/// CanonicalQueryString：参数按 key 排序（key 相同按 value），key/value 各自
/// rfc3986 编码后 `key=value` 用 `&` 连接；空 query 返回空串。
pub fn canonical_query(pairs: &[(&str, &str)]) -> String {
    let mut encoded: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| (rfc3986_encode(k), rfc3986_encode(v)))
        .collect();
    encoded.sort();
    encoded
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// 参与签名的头集合：**全部**请求头 + host（CodeArts2API signer.go 同款——
/// 头映射里有什么签什么，host 由签名器补入），小写后按字典序排列。
fn signed_header_names(headers: &[(String, String)]) -> Vec<String> {
    let mut names: Vec<String> = headers
        .iter()
        .map(|(k, _)| k.trim().to_ascii_lowercase())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// CanonicalHeaders：按 SignedHeaders 顺序 `name:value.trim()` 每行尾 `\n`
/// （含最后一行——坑②的前半段）。
fn canonical_headers(headers: &[(String, String)], names: &[String]) -> String {
    let lookup = |name: &str| -> String {
        headers
            .iter()
            .find(|(k, _)| k.trim().to_ascii_lowercase() == name)
            .map(|(_, v)| v.trim().to_string())
            .unwrap_or_default()
    };
    names
        .iter()
        .map(|n| format!("{n}:{}\n", lookup(n)))
        .collect()
}

/// CanonicalRequest（坑②在此成形：CanonicalHeaders 自带尾 `\n` 后再补一个 `\n`
/// ——SignedHeaders 前多一个空行）+ StringToSign + HMAC-SHA256(SK) 签名。
/// Authorization 用华为云 JS SDK 真实格式 `SDK-HMAC-SHA256 Access=<AK>, …`
/// （**非 AWS 风格 Credential=AK/date/region/service/sdk_request**——本机真机探针
/// 证明 Credential 形态恒 `APIG.0301`，Access= 形态过签）。
/// 返回 (authorization 头, x-sdk-date 头值, 参与签名的头名列表)。
pub fn sign_request(
    method: &str,
    path: &str,
    query_pairs: &[(&str, &str)],
    headers: &[(String, String)],
    body: &str,
    ak: &str,
    sk: &str,
) -> (String, String, Vec<String>) {
    // host 参与签名（官方要求 CanonicalHeaders 必含 host；HTTP 客户端按 URL 自带，不注入）
    let host = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| {
            let base = if path.starts_with("http") { path } else { SNAP_BASE };
            url::Url::parse(base)
                .ok()
                .and_then(|u| u.host_str().map(|h| match u.port() {
                    Some(p) => format!("{h}:{p}"),
                    None => h.to_string(),
                }))
                .unwrap_or_else(|| "snap-access.cn-north-4.myhuaweicloud.com".to_string())
        });
    let mut signing_map: Vec<(String, String)> = headers.to_vec();
    if !signing_map.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        signing_map.push(("host".into(), host));
    }
    // x-sdk-date：头集合里已有则沿用（调用方统一注入），否则取当前 UTC
    let sdk_date = signing_map
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("x-sdk-date"))
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string());
    if !signing_map.iter().any(|(k, _)| k.eq_ignore_ascii_case("x-sdk-date")) {
        signing_map.push(("x-sdk-date".into(), sdk_date.clone()));
    }
    let names = signed_header_names(&signing_map);
    let canonical = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method.to_ascii_uppercase(),
        canonical_uri(path),
        canonical_query(query_pairs),
        canonical_headers(&signing_map, &names),
        names.join(";"),
        sha256_hex(body.as_bytes()),
    );
    let string_to_sign = format!(
        "SDK-HMAC-SHA256\n{}\n{}",
        sdk_date,
        sha256_hex(canonical.as_bytes()),
    );
    let mut mac = Hmac::<Sha256>::new_from_slice(sk.as_bytes()).expect("HMAC accepts any key len");
    mac.update(string_to_sign.as_bytes());
    let signature: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let authorization = format!(
        "SDK-HMAC-SHA256 Access={ak}, SignedHeaders={}, Signature={signature}",
        names.join(";"),
    );
    (authorization, sdk_date, names)
}

// ── 业务身份头 ───────────────────────────────────────────────────────────

/// 随机 hex（安全场景；oauth.rs 的 random_hex_result 为命令域私有，本地实现同款 CSPRNG+兜底）
pub fn random_hex(len: usize) -> String {
    use rand::RngCore;
    let mut bytes = vec![0u8; len.div_ceil(2)];
    rand::thread_rng().fill_bytes(&mut bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    hex[..len].to_string()
}

/// 组装一次 snap 业务请求的身份头矩阵（CodeArts2API client.go baseHeaders 同款；
/// 不含 Authorization——由 [snap_request] 按实际方法签名后注入；host 由
/// [sign_request] 补入签名映射，HTTP 客户端按 URL 自带）。
fn business_headers(cred: &StsCredential, ops_endpoint: bool) -> Vec<(String, String)> {
    let trace_id = random_hex(16);
    let session_id = format!("ses_{}", &md5_hex(cred.session_seed.as_bytes())[..12]);
    let mut headers: Vec<(String, String)> = vec![
        ("content-type".into(), "application/json".into()),
        ("x-sdk-date".into(), chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()),
        ("app-id".into(), APP_ID.into()),
        ("x-ot-function".into(), "agent-general".into()),
        ("x-ot-trace-id".into(), trace_id.clone()),
        ("x-ot-span-id".into(), random_hex(8)),
        ("x-snap-traceid".into(), format!("{trace_id}_{}", random_hex(8))),
        ("x-ot-client-type".into(), "IDE".into()),
        ("x-ot-client-version".into(), PLUGIN_VERSION.into()),
        ("plugin-name".into(), PLUGIN_NAME.into()),
        ("plugin-version".into(), PLUGIN_VERSION.into()),
        ("ide-name".into(), IDE_NAME.into()),
        ("ide-version".into(), PLUGIN_VERSION.into()),
        ("created-time".into(), chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()),
        ("user-msg-id".into(), format!("msg_{}", random_hex(12))),
        ("x-ot-session-id".into(), session_id.clone()),
        // 空值头也参与签名（CodeArts2API 同款恒携带）
        ("x-ot-parent-session-id".into(), String::new()),
        ("user-session-id".into(), session_id),
        ("is_confidential".into(), "false".into()),
    ];
    if !cred.security_token.is_empty() {
        headers.push(("x-security-token".into(), cred.security_token.clone()));
    }
    if ops_endpoint {
        headers.push(("Agent-Type".into(), AGENT_TYPE.into()));
    }
    headers
}

/// 发一次 snap 业务请求（签名 + 身份头）。返回 (http_status, 解析 JSON, 原文)。
/// `ops_endpoint=true` 时额外携带 `Agent-Type: PromptCenter`（ops/* 专用）。
pub fn snap_request(
    method: &str,
    path: &str,
    query_pairs: &[(&str, &str)],
    body: Option<&Value>,
    cred: &StsCredential,
    ops_endpoint: bool,
) -> (u16, Option<Value>, String) {
    let body_str = body.map(|v| v.to_string()).unwrap_or_default();
    let mut headers = business_headers(cred, ops_endpoint);
    let (authorization, _, _) = sign_request(
        method,
        path,
        query_pairs,
        &headers,
        &body_str,
        &cred.access_key,
        &cred.secret_key,
    );
    headers.push(("Authorization".into(), authorization));
    let query = canonical_query(query_pairs);
    let url = if query.is_empty() {
        format!("{SNAP_BASE}{path}")
    } else {
        format!("{SNAP_BASE}{path}?{query}")
    };
    let agent = crate::tasks::http_agent(20);
    let mut request = if method == "POST" {
        agent.post(&url)
    } else {
        agent.get(&url)
    };
    for (name, value) in &headers {
        if name == "content-type" {
            request = request.set("Content-Type", value);
        } else {
            request = request.set(name, value);
        }
    }
    let response = if method == "POST" {
        request.send_string(&body_str)
    } else {
        request.call()
    };
    match response {
        Ok(resp) => {
            let raw = resp.into_string().unwrap_or_default();
            let parsed = serde_json::from_str(&raw).ok();
            (200, parsed, raw)
        }
        Err(ureq::Error::Status(code, resp)) => {
            let raw = resp.into_string().unwrap_or_default();
            let parsed = serde_json::from_str(&raw).ok();
            (code, parsed, raw)
        }
        Err(e) => (0, None, e.to_string()),
    }
}

/// 响应原文截断（错误文案用；响应体不含凭证，截前 240 字符防刷屏）
pub fn excerpt(text: &str) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    cleaned.chars().take(240).collect()
}

/// 上游错误归类（旧 3.9.3 基准：APIG.0301/0303 与 401/APIG.0602 均归
/// 「凭证失效/需重新登录」，不得落「请稍后重试」——照做永远不可能成功）。
/// 返回 Some(fail_kind) = needs_relogin；None = 其他错误。
pub fn classify_needs_relogin(http_status: u16, raw: &str) -> bool {
    if http_status == 401 {
        return true;
    }
    let upper = raw.to_ascii_uppercase();
    // APIG.0602 = security token expired；0301/0303 = 签名/凭证校验失败（旧基准
    // 归 relogin：STS 三元组过期后签名必然失败，重签无法自愈，需重导登录态）
    if upper.contains("APIG.0602")
        || upper.contains("APIG.0301")
        || upper.contains("APIG.0303")
        || raw.to_ascii_lowercase().contains("security token has expired")
    {
        return true;
    }
    false
}

// ── STS 凭证记录 ─────────────────────────────────────────────────────────

/// STS 临时密钥三元组 + refresh 材料（落 codearts_tokens 表；绝不进日志/事件）。
#[derive(Clone, Debug, Default)]
pub struct StsCredential {
    pub access_key: String,
    pub secret_key: String,
    pub security_token: String,
    /// OAuth 通道 refresh_token（local_import / aksk 通道恒空——红线：不消费）
    pub refresh_token: String,
    /// OAuth 通道原始 code_verifier（刷新请求按规格随 DPoP 头附带）
    pub code_verifier: String,
    /// OAuth 通道 DPoP 私钥（JWK 序列化；刷新 proof 用）
    pub dpop_private_jwk: String,
    /// OAuth 通道 redirect_uri（刷新请求 Material 一致性）
    pub redirect_uri: String,
    /// 毫秒；0 = 未知（AK/SK 永久凭证）
    pub expires_at_ms: i64,
    /// local_import | oauth | aksk
    pub credential_source: String,
    /// 会话 ID 种子（同一凭证复用同一 ses_ 会话 ID）
    pub session_seed: String,
}

impl StsCredential {
    pub fn to_json(&self) -> Value {
        json!({
            "access_key": self.access_key,
            "secret_key": self.secret_key,
            "security_token": self.security_token,
            "refresh_token": self.refresh_token,
            "code_verifier": self.code_verifier,
            "dpop_private_jwk": self.dpop_private_jwk,
            "redirect_uri": self.redirect_uri,
            "expires_at_ms": self.expires_at_ms,
            "credential_source": self.credential_source,
            "session_seed": self.session_seed,
        })
    }

    pub fn from_json(v: &Value) -> StsCredential {
        StsCredential {
            access_key: v.get("access_key").and_then(Value::as_str).unwrap_or("").to_string(),
            secret_key: v.get("secret_key").and_then(Value::as_str).unwrap_or("").to_string(),
            security_token: v.get("security_token").and_then(Value::as_str).unwrap_or("").to_string(),
            refresh_token: v.get("refresh_token").and_then(Value::as_str).unwrap_or("").to_string(),
            code_verifier: v.get("code_verifier").and_then(Value::as_str).unwrap_or("").to_string(),
            dpop_private_jwk: v.get("dpop_private_jwk").and_then(Value::as_str).unwrap_or("").to_string(),
            redirect_uri: v.get("redirect_uri").and_then(Value::as_str).unwrap_or("").to_string(),
            expires_at_ms: v.get("expires_at_ms").and_then(Value::as_i64).unwrap_or(0),
            credential_source: v.get("credential_source").and_then(Value::as_str).unwrap_or("").to_string(),
            session_seed: v.get("session_seed").and_then(Value::as_str).unwrap_or("").to_string(),
        }
    }

    /// 凭证就绪判定：AK/SK 齐备且（无 STS token 或未临期）
    pub fn usable(&self) -> bool {
        if self.access_key.is_empty() || self.secret_key.is_empty() {
            return false;
        }
        if self.security_token.is_empty() {
            return true; // 手工 AK/SK 无 SecurityToken，不过期
        }
        self.expires_at_ms == 0
            || self.expires_at_ms - EXPIRY_MARGIN_MS > chrono::Utc::now().timestamp_millis()
    }
}

/// 账号池 id：`ca-` + ak 前 8 位 + md5(ak|sk) 前 8 位（同凭证稳定，跨通道合流）
pub fn account_id(ak: &str, sk: &str) -> String {
    format!("ca-{}{}", &ak[..ak.len().min(8)].to_ascii_lowercase(), &md5_hex(format!("{ak}|{sk}").as_bytes())[..8])
}

// ── 凭证就绪（只读优先；仅 oauth 通道允许互斥刷新）────────────────────────

/// 签到 / 积分共用的凭证就绪：返回可用 StsCredential。
/// 路径：有效直用 → local_import 重读 state.vscdb 对齐客户端续期结果 /
/// oauth 互斥刷新（成功立即落库）→ 失败标记 needs_relogin 熔断。
pub fn ensure_fresh_credential(st: &AppState, account: &Value) -> Result<StsCredential, String> {
    let id = account.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    if id.is_empty() {
        return Err("账号缺少 id".into());
    }
    let db = crate::store::db(&st.data_dir);
    let rec = crate::store::docs::codearts_token_get(&db, &id)
        .map(|v| StsCredential::from_json(&v))
        .ok_or_else(|| "账号无凭证：请先导入本机登录态或手工录入 AK/SK".to_string())?;
    if rec.usable() {
        return Ok(rec);
    }
    // 熔断：needs_relogin 已标记 → 直接失败（失效凭证不可能自愈）
    if account.get("needs_relogin").and_then(Value::as_bool).unwrap_or(false) {
        let reason = account
            .get("relogin_reason")
            .and_then(Value::as_str)
            .unwrap_or("凭证已失效");
        return Err(format!("凭证已失效（{reason}），请在账号管理重新导入"));
    }
    match rec.credential_source.as_str() {
        // 桌面端登录态导入：绝不消费 refresh_token（红线）；过期时重读
        // state.vscdb 对齐客户端续期结果；仍过期则如实报错
        "local_import" => {
            if let Some(session) = read_local_session() {
                let fresh = session.credential(rec.credential_source.clone());
                if fresh.usable() {
                    let _ = crate::store::docs::codearts_token_put(&db, &id, &fresh.to_json());
                    crate::fs_utils::app_log(&st.data_dir, "[codearts] 登录态重读成功，凭证已对齐");
                    return Ok(fresh);
                }
                crate::fs_utils::app_log(&st.data_dir, "[codearts] 登录态重读未对齐（客户端同样过期），沿用现有凭证");
            } else {
                crate::fs_utils::app_log(&st.data_dir, "[codearts] 登录态重读未执行（本机无可用登录态），沿用现有凭证");
            }
            Err("STS 凭据已过期且本机登录态无法对齐，请在码道客户端/插件重新登录后扫描导入".into())
        }
        // 手工 AK/SK：SecurityToken 过期但 AK/SK 长效——去掉过期 STS token 直接用
        // （AK/SK 直签可行；上游按无 token 请求计）；失败由调用方如实落库
        "aksk" => {
            let bare = StsCredential {
                security_token: String::new(),
                expires_at_ms: 0,
                ..rec.clone()
            };
            let _ = crate::store::docs::codearts_token_put(&db, &id, &bare.to_json());
            Ok(bare)
        }
        // OAuth 独立授权链：互斥刷新 + 成功立即落库 + 失效熔断
        _ => refresh_and_persist(st, &id, &rec),
    }
}

/// OAuth refresh_token 刷新（仅 oauth 通道独立链；三防线同 minimax：
/// 全局互斥 + 互斥内二次校验 + 成功无条件立即落库 / invalid 熔断）。
fn refresh_and_persist(st: &AppState, id: &str, rec: &StsCredential) -> Result<StsCredential, String> {
    static REFRESH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = REFRESH_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let db = crate::store::db(&st.data_dir);
    // 互斥内二次校验：等待期间其他路径可能已完成刷新
    if let Some(latest) = crate::store::docs::codearts_token_get(&db, id) {
        let latest = StsCredential::from_json(&latest);
        if latest.usable() && latest.access_key != rec.access_key {
            return Ok(latest);
        }
    }
    if rec.refresh_token.is_empty() {
        return Err("无 refresh_token：请在账号管理重新授权".into());
    }
    let exchanged = crate::tasks::codearts_oauth::refresh_token_grant(rec)?;
    let fresh = StsCredential {
        access_key: exchanged.access,
        secret_key: exchanged.secret,
        security_token: exchanged.security_token,
        expires_at_ms: exchanged.expires_at_ms,
        ..rec.clone()
    };
    // 三防线②：刷新成功无条件立即落库
    crate::store::docs::codearts_token_put(&db, id, &fresh.to_json())
        .map_err(|e| format!("刷新后的凭证落库失败：{e}"))?;
    crate::fs_utils::app_log(&st.data_dir, "[codearts] 凭证刷新成功并已落库");
    Ok(fresh)
}

/// 标记账号 needs_relogin（熔断；重新授权 / 移除重加可清）。
pub fn mark_needs_relogin(db: &crate::store::Store, id: &str, reason: &str) -> Result<(), String> {
    let mut pool = crate::store::docs::codearts_pool_load(db);
    let Some(accounts) = pool.get_mut("accounts").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for acc in accounts.iter_mut() {
        if acc.get("id").and_then(Value::as_str) == Some(id) {
            if acc.get("needs_relogin").and_then(Value::as_bool).unwrap_or(false) {
                return Ok(());
            }
            acc["needs_relogin"] = json!(true);
            acc["relogin_reason"] = json!(reason);
            return crate::store::docs::codearts_pool_save(db, &pool);
        }
    }
    Ok(())
}

// ── caller-identity 查身份（账号归属校验 + 命名）──────────────────────────

/// 身份查询结果（脱敏返回：仅 id 与名称）
#[derive(Clone, Debug, Default)]
pub struct CallerIdentity {
    pub user_id: String,
    pub user_name: String,
    pub domain_id: String,
    pub domain_name: String,
}

impl CallerIdentity {
    /// 展示名：IAM 用户名 → 域名 → 用户 id 截断
    pub fn display_name(&self) -> String {
        if !self.user_name.is_empty() {
            self.user_name.clone()
        } else if !self.domain_name.is_empty() {
            self.domain_name.clone()
        } else {
            format!("IAM {}", &self.user_id[..self.user_id.len().min(8)])
        }
    }
}

/// GET {sts}/v5/caller-identity（SDK-HMAC 签名，最小头集合：无身份头矩阵）。
/// 端点在 STS 域（VENU-5 §2.1）；失败返回 Err（调用方降级用会话内嵌身份或 AK 前缀命名）。
pub fn fetch_caller_identity(cred: &StsCredential) -> Result<CallerIdentity, String> {
    let path = "/v5/caller-identity";
    let mut headers: Vec<(String, String)> = vec![(
        "x-sdk-date".to_string(),
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string(),
    )];
    if !cred.security_token.is_empty() {
        headers.push(("x-security-token".into(), cred.security_token.clone()));
    }
    let (authorization, _, _) =
        sign_request("GET", path, &[], &headers, "", &cred.access_key, &cred.secret_key);
    let agent = crate::tasks::http_agent(20);
    let mut request = agent.get(&format!("{STS_BASE}{path}"));
    for (name, value) in &headers {
        request = request.set(name, value);
    }
    request = request.set("Authorization", &authorization);
    let response = request.call();
    let (status, parsed, raw) = match response {
        Ok(resp) => {
            let raw = resp.into_string().unwrap_or_default();
            (200, serde_json::from_str::<Value>(&raw).ok(), raw)
        }
        Err(ureq::Error::Status(code, resp)) => {
            let raw = resp.into_string().unwrap_or_default();
            (code, serde_json::from_str::<Value>(&raw).ok(), raw)
        }
        Err(e) => (0, None, e.to_string()),
    };
    if status != 200 {
        return Err(format!("caller-identity 查询失败（HTTP {status}）：{}", excerpt(&raw)));
    }
    let value = parsed.ok_or_else(|| format!("caller-identity 响应不是 JSON：{}", excerpt(&raw)))?;
    let s = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    Ok(CallerIdentity {
        user_id: s(&value, "user_id"),
        user_name: s(&value, "user_name"),
        domain_id: s(&value, "domain_id"),
        domain_name: s(&value, "domain_name"),
    })
}

// ── 本机桌面端登录态导入（通道 A，只读，绝不写回）─────────────────────────

/// 解析出的本机会话（脱敏展示仅取 expires / 身份；凭证本体走 credential()）
pub struct LocalSession {
    pub access_key: String,
    pub secret_key: String,
    pub security_token: String,
    pub refresh_token: String,
    pub expires_at_ms: i64,
    /// 会话 JSON 内嵌身份（可能为空——空则由 caller-identity 兜底）
    pub identity_name: String,
    /// 数据目录路径（展示用）
    pub data_dir: String,
}

impl LocalSession {
    /// 凭证三元组 → STS 凭证记录（local_import 通道；绝不主动刷新，过期重读本机文件）
    pub fn credential(&self, source: String) -> StsCredential {
        StsCredential {
            access_key: self.access_key.clone(),
            secret_key: self.secret_key.clone(),
            security_token: self.security_token.clone(),
            // 红线：本机导入凭证不消费 refresh_token——这里仅保存副本供展示，
            // 刷新路径（ensure_fresh_credential）对 local_import 恒走重读而非刷新
            refresh_token: self.refresh_token.clone(),
            expires_at_ms: self.expires_at_ms,
            credential_source: source,
            session_seed: md5_hex(format!("{}|{}", self.access_key, self.secret_key).as_bytes()),
            ..Default::default()
        }
    }
}

/// CodeArts 客户端数据目录探测候选（旧 3.9.3 基准；顺序即优先级）
pub fn data_dir_candidates() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let base = std::path::PathBuf::from(appdata);
        out.push(base.join("codearts-agent"));
        out.push(base.join("CodeArts Agent"));
        out.push(base.join("CodeArtsCode"));
    }
    for var in ["USERPROFILE", "HOME"] {
        if let Some(home) = std::env::var_os(var) {
            out.push(std::path::PathBuf::from(home).join(".codearts-agent"));
        }
    }
    out
}

/// Local State（数据目录根）→ os_crypt.encrypted_key → base64 → DPAPI → AES 密钥
/// （与 Qoder IDE 存储同构，qoder/ide_store.rs 同款链路；非 Windows 由调用方拦截）
#[cfg(windows)]
fn local_state_aes_key(data_dir: &std::path::Path) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let lp = data_dir.join("Local State");
    let raw = std::fs::read_to_string(&lp).map_err(|e| format!("Local State 读取失败: {e}"))?;
    let state: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("Local State 解析失败: {e}"))?;
    let b64 = state
        .get("os_crypt")
        .and_then(|v| v.get("encrypted_key"))
        .and_then(Value::as_str)
        .ok_or("Local State 无 os_crypt.encrypted_key")?;
    let wrapped = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("encrypted_key base64 解码失败: {e}"))?;
    if wrapped.len() < 5 || &wrapped[..5] != b"DPAPI" {
        return Err("encrypted_key 前缀非 DPAPI".into());
    }
    crate::vault::dpapi::unprotect(&wrapped[5..])
}

/// v10 密文解密：'v10'/'v11' + nonce(12) + ct + tag(16) → AES-256-GCM
#[cfg(windows)]
fn decrypt_v10(enc: &[u8], key: &[u8]) -> Option<String> {
    if enc.len() < 19 || (&enc[..3] != b"v10" && &enc[..3] != b"v11") {
        return None;
    }
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let plain = cipher
        .decrypt(Nonce::from_slice(&enc[3..15]), &enc[15..])
        .ok()?;
    String::from_utf8(plain).ok()
}

/// secret:// 值形态①：TEXT JSON `{"type":"Buffer","data":[...]}` → 字节
#[cfg(windows)]
fn parse_buffer(text: &str) -> Option<Vec<u8>> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    v.get("data")?.as_array().map(|a| {
        a.iter()
            .filter_map(|x| x.as_u64().map(|n| n as u8))
            .collect::<Vec<u8>>()
    })
}

/// secret:// 值形态②：base64 串（形态漂移兜底）
#[cfg(windows)]
fn parse_b64(text: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let t = text.trim();
    if t.starts_with('{') {
        return None;
    }
    base64::engine::general_purpose::STANDARD.decode(t).ok()
}

/// state.vscdb ItemTable 读全部 secret:// 键（旧 3.9.3 同款 SQL）。
/// 只读连接 + busy_timeout 1.5s；WAL 残留提示与 Qoder 同款（不误报「被占用」）。
#[cfg(windows)]
fn read_all_secret_entries(vscdb: &std::path::Path) -> Result<Vec<(String, String)>, String> {
    if !vscdb.is_file() {
        return Err(format!(
            "已找到数据目录 {}，但缺少 User\\globalStorage\\state.vscdb",
            vscdb.parent().and_then(|p| p.parent()).map(|p| p.display().to_string()).unwrap_or_default()
        ));
    }
    let wal_orphan = vscdb.with_file_name("state.vscdb-wal").is_file()
        && !vscdb.with_file_name("state.vscdb-shm").is_file();
    let wal_hint = if wal_orphan {
        "；检测到异常退出残留的 WAL 索引，重启一次客户端可自动修复"
    } else {
        ""
    };
    let conn = rusqlite::Connection::open_with_flags(vscdb, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("state.vscdb 打开失败（可能被客户端占用{wal_hint}）: {e}"))?;
    conn.busy_timeout(std::time::Duration::from_millis(1500))
        .map_err(|e| format!("state.vscdb busy_timeout 设置失败: {e}"))?;
    let mut stmt = conn
        .prepare("SELECT key, value FROM ItemTable WHERE key LIKE 'secret://%'")
        .map_err(|e| format!("state.vscdb 查询准备失败{wal_hint}: {e}"))?;
    let mut rows = stmt
        .query([])
        .map_err(|e| format!("state.vscdb 查询执行失败: {e}"))?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().map_err(|e| format!("state.vscdb 迭代失败: {e}"))? {
        let key: String = r.get(0).map_err(|e| format!("state.vscdb 键名读取失败: {e}"))?;
        let val: rusqlite::types::Value =
            r.get(1).map_err(|e| format!("state.vscdb 键值类型读取失败: {e}"))?;
        let text = match val {
            rusqlite::types::Value::Text(s) => s,
            rusqlite::types::Value::Blob(b) => String::from_utf8_lossy(&b).to_string(),
            _ => continue,
        };
        out.push((key, text));
    }
    Ok(out)
}

/// 会话 JSON 深度键扫描：`accessKey/secretKey/securityToken/refreshToken` 等
/// 键名形态漂移容忍（大写/下划线/嵌套归一后匹配）。
fn deep_find<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let norm = |s: &str| s.to_ascii_lowercase().replace(['_', '-'], "");
    let wanted: Vec<String> = keys.iter().map(|k| norm(k)).collect();
    deep_walk(v, &wanted, &norm, 0)
}

fn deep_walk<'a>(
    v: &'a Value,
    wanted: &[String],
    norm: &dyn Fn(&str) -> String,
    depth: usize,
) -> Option<&'a Value> {
    if depth > 8 {
        return None;
    }
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                if wanted.iter().any(|w| *w == norm(k)) {
                    let hit = match val {
                        Value::String(s) if !s.trim().is_empty() => Some(val),
                        Value::Number(_) => Some(val),
                        _ => None,
                    };
                    if hit.is_some() {
                        return hit;
                    }
                }
            }
            for (_k, val) in map {
                if let Some(found) = deep_walk(val, wanted, norm, depth + 1) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(arr) => arr.iter().find_map(|item| deep_walk(item, wanted, norm, depth + 1)),
        _ => None,
    }
}

/// 字符串值读取（数字/字符串形态兼容）
fn deep_find_string(v: &Value, keys: &[&str]) -> String {
    deep_find(v, keys)
        .and_then(|x| x.as_str().map(str::to_string).or_else(|| x.as_i64().map(|n| n.to_string())))
        .unwrap_or_default()
}

/// 时间值 → 毫秒：RFC3339 字符串 / 13 位毫秒 / 10 位秒（防秒当毫秒折算成 1970 年）
pub fn expiry_to_ms(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(if n >= 1_000_000_000_000 { n } else { n * 1000 });
    }
    let s = v.as_str()?.trim();
    if let Ok(n) = s.parse::<i64>() {
        return Some(if n >= 1_000_000_000_000 { n } else { n * 1000 });
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_millis());
    }
    None
}

/// 会话明文 JSON → LocalSession（缺 AK 或 SK 视为不可解析 → None）
fn parse_session_plain(plain: &str, data_dir: &str) -> Option<LocalSession> {
    let v: Value = serde_json::from_str(plain).ok()?;
    let access_key = deep_find_string(&v, &["accessKey", "ak", "access"]);
    let secret_key = deep_find_string(&v, &["secretKey", "sk", "secret"]);
    if access_key.trim().is_empty() || secret_key.trim().is_empty() {
        return None;
    }
    let security_token =
        deep_find_string(&v, &["securityToken", "securitytoken", "security_token", "sessionToken"]);
    let refresh_token = deep_find_string(&v, &["refreshToken", "refresh_token", "refreshToken2"]);
    let expires_at_ms = deep_find(&v, &["expiration", "expiresAt", "expireTime", "expiresAtMs", "expires_at"])
        .and_then(expiry_to_ms)
        .unwrap_or(0);
    let identity_name = deep_find_string(
        &v,
        &["userName", "user_name", "domainName", "displayName", "label", "name"],
    );
    Some(LocalSession {
        access_key: access_key.trim().to_string(),
        secret_key: secret_key.trim().to_string(),
        security_token: security_token.trim().to_string(),
        refresh_token: refresh_token.trim().to_string(),
        expires_at_ms,
        identity_name,
        data_dir: data_dir.to_string(),
    })
}

/// 扫描本机 CodeArts 桌面端登录态（只读；候选目录逐个探测）。
/// 优先精确键 HuaweiCloudSession，未命中再扫全部 secret:// 键（旧 3.9.3 同款）。
#[cfg(windows)]
pub fn read_local_session() -> Option<LocalSession> {
    for data_dir in data_dir_candidates() {
        if !data_dir.is_dir() {
            continue;
        }
        let Ok(key) = local_state_aes_key(&data_dir) else {
            continue;
        };
        let vscdb = data_dir.join("User").join("globalStorage").join("state.vscdb");
        let entries = match read_all_secret_entries(&vscdb) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let dir_str = data_dir.display().to_string();
        // 精确键优先
        if let Some((_, text)) = entries.iter().find(|(k, _)| *k == SESSION_VSCDB_KEY) {
            let bytes = parse_buffer(text).or_else(|| parse_b64(text));
            if let Some(bytes) = bytes {
                if let Some(plain) = decrypt_v10(&bytes, &key) {
                    if let Some(session) = parse_session_plain(&plain, &dir_str) {
                        return Some(session);
                    }
                }
            }
        }
        // 兜底：全部 secret:// 键逐个解密试解析（旧 3.9.3 同款 LIKE 'secret://%'）
        for (_, text) in &entries {
            let bytes = parse_buffer(text).or_else(|| parse_b64(text));
            let Some(bytes) = bytes else { continue };
            let Some(plain) = decrypt_v10(&bytes, &key) else { continue };
            if let Some(session) = parse_session_plain(&plain, &dir_str) {
                return Some(session);
            }
        }
    }
    None
}

#[cfg(not(windows))]
/// 非 Windows：桌面端登录态导入依赖 DPAPI（见模块头），返回 None 由调用方如实报错
pub fn read_local_session() -> Option<LocalSession> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 金测①（坑）：CanonicalURI 强制补尾斜杠 + 按段编码
    #[test]
    fn canonical_uri_forces_trailing_slash() {
        assert_eq!(canonical_uri("/v1/ops/claim"), "/v1/ops/claim/");
        assert_eq!(canonical_uri("/v1/ops/delivery"), "/v1/ops/delivery/");
        assert_eq!(canonical_uri("/snap-manager/v1/statistics/plugin"), "/snap-manager/v1/statistics/plugin/");
        // 已带尾斜杠不重复补
        assert_eq!(canonical_uri("/v1/ops/claim/"), "/v1/ops/claim/");
        // 根路径原样（不产生 "//"）
        assert_eq!(canonical_uri("/"), "/");
        // 全 URL 入参剥域名
        assert_eq!(
            canonical_uri("https://snap-access.cn-north-4.myhuaweicloud.com/v1/ops/claim"),
            "/v1/ops/claim/"
        );
    }

    /// 金测②（坑）：CanonicalHeaders 尾 `\n` + CanonicalRequest 空行
    #[test]
    fn canonical_request_has_blank_line_before_signed_headers() {
        let headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("x-sdk-date".to_string(), "20261121T000000Z".to_string()),
            ("x-security-token".to_string(), "st".to_string()),
        ];
        let names = signed_header_names(&headers);
        assert_eq!(names, vec!["content-type", "x-sdk-date", "x-security-token"]);
        let ch = canonical_headers(&headers, &names);
        assert_eq!(
            ch,
            "content-type:application/json\nx-sdk-date:20261121T000000Z\nx-security-token:st\n",
            "每行（含最后一行）尾 \\n"
        );
        let canonical = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            "GET",
            canonical_uri("/v1/ops/delivery"),
            canonical_query(&[("channel", "IDE")]),
            ch,
            names.join(";"),
            sha256_hex("".as_bytes()),
        );
        // SignedHeaders 前多一个空行（headers 块尾 \n + 补 \n = "\n\n"）
        assert!(
            canonical.contains("\n\ncontent-type;x-sdk-date"),
            "CanonicalRequest 应含空行：{canonical:?}"
        );
        // 固定输入 → 固定 CanonicalRequest 全文（算法漂移首红）
        assert_eq!(
            canonical,
            format!(
                "GET\n/v1/ops/delivery/\nchannel=IDE\ncontent-type:application/json\nx-sdk-date:20261121T000000Z\nx-security-token:st\n\ncontent-type;x-sdk-date;x-security-token\n{}",
                sha256_hex("".as_bytes())
            )
        );
    }

    /// 签名：固定输入 → 固定 Authorization（HMAC(SK, StringToSign) 固化）。
    /// host 由签名器补入（CanonicalHeaders 必含 host）；Authorization 用
    /// `SDK-HMAC-SHA256 Access=<AK>` 华为云 JS SDK 真实格式（非 AWS 风格 Credential）。
    #[test]
    fn sign_request_golden() {
        let headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("x-sdk-date".to_string(), "20261121T000000Z".to_string()),
        ];
        let (authorization, sdk_date, names) = sign_request(
            "GET",
            "/v1/ops/delivery",
            &[("channel", "IDE")],
            &headers,
            "",
            "AKTEST",
            "SKTEST",
        );
        assert_eq!(sdk_date, "20261121T000000Z");
        // host 由签名器自动补入并参与签名
        assert_eq!(names, vec!["content-type", "host", "x-sdk-date"]);
        // Authorization 结构：Access= 形态（真机探针证明 Credential= 形态恒 APIG.0301）
        assert!(authorization.starts_with("SDK-HMAC-SHA256 Access=AKTEST, SignedHeaders=content-type;host;x-sdk-date, Signature="));
        // 固定输入 → 固定签名（独立复算 HMAC 校验；注意 SignedHeaders 前空行）
        let canonical = format!(
            "GET\n/v1/ops/delivery/\nchannel=IDE\ncontent-type:application/json\nhost:snap-access.cn-north-4.myhuaweicloud.com\nx-sdk-date:20261121T000000Z\n\ncontent-type;host;x-sdk-date\n{}",
            sha256_hex("".as_bytes())
        );
        let sts = format!("SDK-HMAC-SHA256\n20261121T000000Z\n{}", sha256_hex(canonical.as_bytes()));
        let mut mac = Hmac::<Sha256>::new_from_slice(b"SKTEST").unwrap();
        mac.update(sts.as_bytes());
        let expected: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert!(authorization.ends_with(&expected), "签名应等于独立复算的 HMAC：{authorization}");
        // SK 变化 → 签名变化
        let (auth2, _, _) = sign_request("GET", "/v1/ops/delivery", &[("channel", "IDE")], &headers, "", "AKTEST", "SKOTHER");
        assert_ne!(authorization, auth2);
    }

    #[test]
    fn canonical_query_sorts_and_encodes() {
        assert_eq!(canonical_query(&[("channel", "IDE")]), "channel=IDE");
        assert_eq!(canonical_query(&[("b", "2"), ("a", "1")]), "a=1&b=2");
        assert_eq!(canonical_query(&[("k", "a b&c")]), "k=a%20b%26c");
        assert_eq!(canonical_query(&[]), "");
    }

    #[test]
    fn signed_header_names_rule() {
        // 全部请求头参与签名（CodeArts2API 同款：头映射里有什么签什么），
        // 小写去重排序
        let headers = vec![
            ("App-ID".to_string(), "x".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Agent-Type".to_string(), "PromptCenter".to_string()),
            ("x-ot-trace-id".to_string(), "t".to_string()),
            ("X-Security-Token".to_string(), "st".to_string()),
            ("created-time".to_string(), "now".to_string()),
            ("app-id".to_string(), "dup".to_string()),
        ];
        assert_eq!(
            signed_header_names(&headers),
            vec![
                "agent-type",
                "app-id",
                "content-type",
                "created-time",
                "x-ot-trace-id",
                "x-security-token"
            ]
        );
    }

    #[test]
    fn classify_maps_apig_errors_to_relogin() {
        assert!(classify_needs_relogin(401, "any"));
        assert!(classify_needs_relogin(400, r#"{"error_code":"APIG.0602","error_msg":"the security token has expired"}"#));
        assert!(classify_needs_relogin(403, "APIG.0301: signature invalid"));
        assert!(classify_needs_relogin(403, "APIG.0303"));
        assert!(!classify_needs_relogin(400, r#"{"code":40001}"#));
        assert!(!classify_needs_relogin(500, "internal error"));
        assert!(classify_needs_relogin(400, "Bad request: the security token has expired"));
    }

    #[test]
    fn parse_session_plain_tolerant_keys() {
        let s = parse_session_plain(
            r#"{"credentials":{"access":"AK1","secret":"SK1","securitytoken":"ST1","expiration":"2026-12-31T00:00:00Z"},"refresh_token":"rt1"}"#,
            "D:\\x",
        )
        .expect("嵌套 JSON 应解析");
        assert_eq!(s.access_key, "AK1");
        assert_eq!(s.secret_key, "SK1");
        assert_eq!(s.security_token, "ST1");
        assert_eq!(s.refresh_token, "rt1");
        assert_eq!(s.expires_at_ms, 1798675200000); // 2026-12-31T00:00:00Z
        // 缺 SK → None（不可解析，不当「未登录」静默跳过由调用方报错）
        assert!(parse_session_plain(r#"{"accessKey":"AK1"}"#, "D:\\x").is_none());
        assert!(parse_session_plain("not-json", "D:\\x").is_none());
    }

    #[test]
    fn expiry_to_ms_seconds_vs_millis() {
        let v = serde_json::json!(1791673619906i64);
        assert_eq!(expiry_to_ms(&v), Some(1791673619906));
        let v = serde_json::json!("1791673619");
        assert_eq!(expiry_to_ms(&v), Some(1791673619000), "10 位秒级不得折算成 1970 年");
        let v = serde_json::json!(1791673619.0);
        assert_eq!(expiry_to_ms(&v), None, "浮点字符串形态不接收（防脏数据）");
    }

    #[test]
    fn account_id_stable() {
        let a = account_id("AKAAAAAA1111", "SK1");
        let b = account_id("AKAAAAAA1111", "SK1");
        let c = account_id("AKAAAAAA2222", "SK1");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("ca-"));
    }

    #[test]
    fn sts_credential_json_roundtrip_and_usable() {
        let cred = StsCredential {
            access_key: "AK".into(),
            secret_key: "SK".into(),
            security_token: "ST".into(),
            expires_at_ms: chrono::Utc::now().timestamp_millis() + 3600_000,
            credential_source: "local_import".into(),
            session_seed: "seed".into(),
            ..Default::default()
        };
        assert!(cred.usable());
        let back = StsCredential::from_json(&cred.to_json());
        assert_eq!(back.access_key, "AK");
        assert_eq!(back.credential_source, "local_import");
        // 过期 → 不可用
        let expired = StsCredential { expires_at_ms: 1, ..cred.clone() };
        assert!(!expired.usable());
        // 无 SecurityToken（手工 AK/SK）→ 不过期概念
        let bare = StsCredential { security_token: String::new(), expires_at_ms: 0, ..cred };
        assert!(bare.usable());
    }

    /// 真机探针（#[ignore]，`cargo test codearts_live_probe -- --ignored --nocapture`）：
    /// 只读本机 codearts-agent 登录态 → caller-identity → GET /v1/ops/delivery（只读
    /// 不 claim），验证解密链与签名链。输出全程脱敏（不打印 AK/SK/token）。
    #[test]
    #[ignore]
    fn codearts_live_probe() {
        let Some(session) = read_local_session() else {
            println!("[probe] 本机无可用 CodeArts 登录态（未登录或解密失败），跳过");
            return;
        };
        println!(
            "[probe] 登录态就绪：expires_at_ms={} identity={}字 dir={}",
            session.expires_at_ms,
            session.identity_name.len(),
            session.data_dir
        );
        println!(
            "[probe] 凭证形态：AK={}字头{} SK={}字 STS={}字 RT={}字",
            session.access_key.len(),
            &session.access_key[..session.access_key.len().min(4)],
            session.secret_key.len(),
            session.security_token.len(),
            session.refresh_token.len(),
        );
        let cred = session.credential("local_import".into());
        match fetch_caller_identity(&cred) {
            Ok(id) => println!(
                "[probe] caller-identity ok：user={}字 domain={}字",
                id.user_name.len(),
                id.domain_name.len()
            ),
            Err(e) => println!("[probe] caller-identity 失败（端点或签名问题，不阻断）: {}", excerpt(&e)),
        }
        let (status, parsed, raw) =
            snap_request("GET", "/v1/ops/delivery", &[("channel", "IDE")], None, &cred, true);
        println!("[probe] delivery HTTP={status} raw_head={}", excerpt(&raw));
        if let Some(v) = &parsed {
            println!(
                "[probe] code={:?} items={:?}",
                v.get("code").and_then(Value::as_i64),
                v.pointer("/data/items").and_then(Value::as_array).map(|a| a.len())
            );
            // 活动明细（只打活动字段，不打凭证）：可领状态 / 触发模式 / 状态机
            if let Some(items) = v.pointer("/data/items").and_then(Value::as_array) {
                for item in items {
                    println!(
                        "[probe] item id={} type={} claimable={} status={} triggerMode={} amount={:?}",
                        item.get("campaignId").and_then(Value::as_i64).unwrap_or(0),
                        item.get("type").and_then(Value::as_str).unwrap_or("?"),
                        item.get("claimable").and_then(Value::as_bool).unwrap_or(false),
                        item.get("status").and_then(Value::as_str).unwrap_or("?"),
                        item.pointer("/extra/triggerMode").and_then(Value::as_str).unwrap_or("?"),
                        item.get("benefitAmount")
                    );
                }
            }
        }
    }

}
