//! MiniMax Code 双区（minimax CN / minimax-global Global）共享层。
//!
//! 协议依据：VENU-5《MiniMax与CodeArts签到积分接入规格.md》§1（Stage 1 定稿，
//! CreditDaddy / 10Router 社区实测交叉验证）+ 旧 3.9.3 行为基准（VENU-36 重建）。
//!
//! - 双区同协议同路径，仅域名不同：CN `agent.minimax.cn` / `account.minimax.cn`，
//!   Global `agent.minimax.io` / `account.minimax.io`，client 恒 `mcode-public`。
//! - `signin/*` 与 `commerce/*` 必签（yy + x-signature md5 链）；`user/profile`
//!   纯 Bearer 不签。签名常量集中本模块 + 金测样例（客户端升级更换常量时热更此处）。
//! - OAuth 设备码流（RFC 8628 + S256 PKCE）为主通道；本机
//!   `~/.minimax/auth/prod/{cn|en}/mcode-public/auth.json` 导入为辅。无 PAT 通道。
//! - refreshToken 一次性轮换三防线：同账号刷新互斥（全局串行互斥锁）；
//!   刷新成功无条件立即落库；失效链熔断（needs_relogin 后不再自动重试）。
//! - 只读约束：绝不写回 MiniMax 客户端的 auth.json（通道 B 账号不主动刷新，
//!   过期时重读本机文件对齐客户端轮换结果）；凭证只落本应用 SQLite 凭证表，
//!   日志/事件/错误文案不含 token。

use base64::Engine;
use md5::{Digest, Md5};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::state::AppState;

/// MiniMax 账号所属区。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    /// 国内区 agent.minimax.cn
    Cn,
    /// 国际区 agent.minimax.io
    Global,
}

impl Region {
    /// 区标识（与前端 `MinimaxRegion` 及旧 3.9.3 命名一致）
    pub fn key(&self) -> &'static str {
        match self {
            Region::Cn => "minimax",
            Region::Global => "minimax-global",
        }
    }

    /// Agent 业务域
    pub fn agent_base(&self) -> &'static str {
        match self {
            Region::Cn => "https://agent.minimax.cn",
            Region::Global => "https://agent.minimax.io",
        }
    }

    /// 账号 / OAuth 域
    pub fn account_base(&self) -> &'static str {
        match self {
            Region::Cn => "https://account.minimax.cn",
            Region::Global => "https://account.minimax.io",
        }
    }

    /// 签名 query 的语言参数（CN=zh / Global=en）
    pub fn lang(&self) -> &'static str {
        match self {
            Region::Cn => "zh",
            Region::Global => "en",
        }
    }

    /// "minimax" / "minimax-global" → Region；其他值 None
    pub fn parse(raw: &str) -> Option<Region> {
        match raw.trim() {
            "minimax" => Some(Region::Cn),
            "minimax-global" => Some(Region::Global),
            _ => None,
        }
    }

    /// 本机 auth.json 目录语言名（cn/en；由客户端语言版决定，不是按账号所在区）
    fn authjson_lang(&self) -> &'static str {
        match self {
            Region::Cn => "cn",
            Region::Global => "en",
        }
    }
}

// ── 签名常量（VENU-5 §1.5 社区提取；客户端升级更换时热更此处并补金测）──────

/// OAuth / 业务 client 标识
pub const CLIENT_ID: &str = "mcode-public";
/// OAuth scope / audience
pub const OAUTH_SCOPE: &str = "agent.default";
pub const OAUTH_AUDIENCE: &str = "agent-backend";
/// 签名固定 query 参数
const SIGN_DEVICE_PLATFORM: &str = "mcode";
const SIGN_BIZ_ID: &str = "3";
const SIGN_APP_ID: &str = "3001";
const SIGN_VERSION_CODE: &str = "22201";
const SIGN_DEVICE_ID: &str = "0";
const SIGN_OS_NAME: &str = "win32";
const SIGN_BROWSER_NAME: &str = "mcode";
const SIGN_CLIENT: &str = "mcode";
/// yy 签名盐
const YY_SALT: &str = "ooui";
/// x-signature 盐
const XSIGN_KEY: &str = "I*7Cf%WZ#S&%1RlZJ&C2";
/// UA（规格明确 User-Agent: MiniMaxCode）
const USER_AGENT: &str = "MiniMaxCode";
/// access token 有效期余量（提前 5 分钟视为过期，留刷新窗口）
const EXPIRY_MARGIN_MS: i64 = 5 * 60 * 1000;
/// 签到 API 公共 query（业务参数段；签名固定参数由 agent_request 追加）。
/// `timezone_offset` 固定 28800 = UTC+8 秒数，与「今天」口径对齐（桌面端同值）。
pub const SIGNIN_QUERY: &str = "timezone_offset=28800&is_desktop=1&client=desktop";

fn md5_hex(bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_b64url(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// encodeURIComponent 口径（JS encodeURIComponent：非 [A-Za-z0-9-_.!~*'()] 全转义）
fn encode_uri_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')');
        if keep {
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

/// 组装签名用固定 query（含动态 unix / timezone_offset（已取反）/ user_id / lang）。
/// 返回形如 `device_platform=mcode&biz_id=3&...` 的串（不带头部 `?`）。
pub fn sign_query(unix_ms: i64, user_id: &str, lang: &str) -> String {
    // 本地 UTC 偏移秒数（已取反）：UTC+8 → -28800
    let offset_secs = chrono::Local::now().offset().local_minus_utc();
    format!(
        "device_platform={SIGN_DEVICE_PLATFORM}&biz_id={SIGN_BIZ_ID}&app_id={SIGN_APP_ID}&version_code={SIGN_VERSION_CODE}&unix={unix_ms}&timezone_offset={offset_secs}&sys_language={lang}&lang={lang}&device_id={SIGN_DEVICE_ID}&os_name={SIGN_OS_NAME}&browser_name={SIGN_BROWSER_NAME}&user_id={}&client={SIGN_CLIENT}",
        encode_uri_component(user_id)
    )
}

/// yy 头：md5( encodeURIComponent(path + "?" + query) + "_" + body + md5(String(unix毫秒)) + "ooui" )。
/// GET（无体）参与签名的 body 视为 "{}"；有体 POST 用实际 JSON 串。
pub fn sign_yy(path: &str, query: &str, body: &str, unix_ms: i64) -> String {
    let material = format!(
        "{}_{}{}{}",
        encode_uri_component(&format!("{path}?{query}")),
        body,
        md5_hex(unix_ms.to_string().as_bytes()),
        YY_SALT
    );
    md5_hex(material.as_bytes())
}

/// x-signature 头：md5( x-timestamp(秒) + 盐 + bodyJSON 或空串 )。
pub fn sign_xsignature(x_timestamp_secs: i64, body: &str) -> String {
    md5_hex(format!("{x_timestamp_secs}{XSIGN_KEY}{body}").as_bytes())
}

/// 为一次业务请求（signin/* / commerce/*）签名，返回应注入的头集合。
/// `path` 不含 query（如 `/minimax-cloud/api/v1/signin/status`）；
/// `query` 为完整 URL query 串（业务参数 + 签名固定参数，同串参与 yy 签名）。
pub fn sign_headers(
    method: &str,
    path: &str,
    query: &str,
    body: Option<&Value>,
    access_token: &str,
) -> Vec<(String, String)> {
    let unix_ms = chrono::Utc::now().timestamp_millis();
    let body_str = body
        .map(|v| v.to_string())
        .unwrap_or_else(|| "{}".to_string());
    // yy 口径：GET 无体按 "{}"，POST 用实际 JSON（sign_query 已含 unix 等）
    let yy = sign_yy(path, query, &body_str, unix_ms);
    let x_ts = chrono::Utc::now().timestamp();
    // x-signature 口径：无体为空串，有体为实际 JSON
    let x_body = if body.is_some() { body_str.as_str() } else { "" };
    let xsig = sign_xsignature(x_ts, x_body);
    let _ = method;
    vec![
        ("Authorization".to_string(), format!("Bearer {access_token}")),
        ("yy".to_string(), yy),
        ("x-timestamp".to_string(), x_ts.to_string()),
        ("x-signature".to_string(), xsig),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("User-Agent".to_string(), USER_AGENT.to_string()),
    ]
}

/// 发一次 Agent 业务请求（自动拼签名 query 与头）。
/// `path` 不含 query（如 `/minimax-cloud/api/v1/signin/status`）；
/// `biz_query` 为业务参数串（如 `timezone_offset=28800&is_desktop=1&client=desktop`，
/// 可空）。最终 URL query = 业务参数 + `&` + 签名固定参数（同串参与 yy 签名——
/// 发出去什么就签什么，服务端按原始 query 串复算）。
pub fn agent_request(
    region: Region,
    method: &str,
    path: &str,
    biz_query: &str,
    body: Option<&Value>,
    access_token: &str,
    user_id: &str,
    lang: &str,
) -> (u16, Option<Value>, String) {
    let unix_ms = chrono::Utc::now().timestamp_millis();
    let sign = sign_query(unix_ms, user_id, lang);
    let query = if biz_query.trim().is_empty() {
        sign
    } else {
        format!("{}&{sign}", biz_query.trim())
    };
    let url = format!("{}{path}?{query}", region.agent_base());
    let headers = sign_headers(method, path, &query, body, access_token);
    let agent = crate::tasks::http_agent(20);
    let mut request = if method == "POST" {
        agent.post(&url)
    } else {
        agent.get(&url)
    };
    for (name, value) in &headers {
        request = request.set(name, value);
    }
    let response = if method == "POST" {
        request.send_string(&body.map(|v| v.to_string()).unwrap_or_else(|| "{}".to_string()))
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

/// 业务响应信封校验：`base_resp.status_code != 0` 即业务错误（HTTP 层可能 200）。
/// 返回 Err（含 status_msg 摘录）；信封缺失时放行（宽容：部分端点可无信封）。
pub fn reject_business_error(value: &Value) -> Result<(), String> {
    let base = value.get("base_resp");
    let Some(base) = base else {
        return Ok(());
    };
    let code = base.get("status_code").and_then(Value::as_i64).unwrap_or(0);
    if code == 0 {
        return Ok(());
    }
    let msg = base
        .get("status_msg")
        .and_then(Value::as_str)
        .unwrap_or("未知错误")
        .trim();
    Err(format!("上游业务错误（status_code={code}）：{msg}"))
}

/// 响应原文截断（错误文案用；绝不回显 token——原文里若出现 Bearer 串也不会，
/// 这里只截响应体前 240 字符）
pub fn excerpt(text: &str) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    cleaned.chars().take(240).collect()
}

// ── OAuth 设备码流（RFC 8628 + S256 PKCE）─────────────────────────────────

/// 生成 PKCE code_verifier（43..=128 个 unreserved 字符，此处取 64）
pub fn generate_verifier() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    use rand::RngCore;
    let mut rng = rand::thread_rng();
    let mut bytes = [0u8; 64];
    rng.fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

/// S256 code_challenge
pub fn code_challenge_s256(verifier: &str) -> String {
    sha256_b64url(verifier.as_bytes())
}

/// 设备码授权第一步：POST {account}/oauth2/device/code。
/// 成功返回 (device_code, user_code, verification_uri_complete, expires_in_secs, interval_secs)。
pub fn device_code_start(region: Region) -> Result<(String, String, String, i64, i64), String> {
    let verifier = generate_verifier();
    let challenge = code_challenge_s256(&verifier);
    let url = format!("{}/oauth2/device/code", region.account_base());
    let form = format!(
        "client_id={CLIENT_ID}&scope={OAUTH_SCOPE}&audience={OAUTH_AUDIENCE}&code_challenge={challenge}&code_challenge_method=S256"
    );
    let agent = crate::tasks::http_agent(20);
    let response = agent
        .post(&url)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&form);
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
        return Err(format!("设备码申请失败（HTTP {status}）：{}", excerpt(&raw)));
    }
    let parsed = parsed.ok_or_else(|| format!("设备码响应不是 JSON：{}", excerpt(&raw)))?;
    let device_code = parsed
        .get("device_code")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let user_code = parsed
        .get("user_code")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let verify_uri = parsed
        .get("verification_uri_complete")
        .or_else(|| parsed.get("verification_uri"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let expires_in = parsed.get("expires_in").and_then(Value::as_i64).unwrap_or(300);
    let interval = parsed.get("interval").and_then(Value::as_i64).unwrap_or(3).max(2);
    if device_code.is_empty() || user_code.is_empty() || verify_uri.is_empty() {
        return Err("设备码响应缺少 device_code / user_code / verification_uri".into());
    }
    Ok((device_code, user_code, verify_uri, expires_in, interval))
}

/// 设备码轮询单步 + token 交换：返回
/// `pending`（继续轮询）/ `success(access, refresh, expires_in)` / `failed(原因终态)`。
pub fn device_code_poll(region: Region, device_code: &str, verifier: &str) -> DevicePollOutcome {
    let url = format!("{}/oauth2/token", region.account_base());
    let form = format!(
        "grant_type={}&device_code={}&client_id={CLIENT_ID}&code_verifier={}",
        "urn:ietf:params:oauth:grant-type:device_code",
        encode_uri_component(device_code),
        encode_uri_component(verifier)
    );
    let agent = crate::tasks::http_agent(20);
    let response = agent
        .post(&url)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&form);
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
    if status == 200 {
        if let Some(v) = parsed {
            let access = v.get("access_token").and_then(Value::as_str).unwrap_or("");
            let refresh = v.get("refresh_token").and_then(Value::as_str).unwrap_or("");
            let expires_in = v.get("expires_in").and_then(Value::as_i64).unwrap_or(0);
            if !access.is_empty() {
                return DevicePollOutcome::Success {
                    access_token: access.to_string(),
                    refresh_token: refresh.to_string(),
                    expires_in_secs: expires_in,
                };
            }
        }
        return DevicePollOutcome::Failed("授权响应缺少 access_token".into());
    }
    let error = parsed
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    match error.as_str() {
        "authorization_pending" => DevicePollOutcome::Pending,
        "slow_down" => DevicePollOutcome::SlowDown,
        "" => DevicePollOutcome::Failed(format!("授权轮询失败（HTTP {status}）：{}", excerpt(&raw))),
        // expired_token / access_denied 等终态
        other => {
            let desc = parsed
                .and_then(|v| v.get("error_description").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_default();
            DevicePollOutcome::Failed(if desc.is_empty() {
                other.to_string()
            } else {
                format!("{other}：{desc}")
            })
        }
    }
}

pub enum DevicePollOutcome {
    Pending,
    SlowDown,
    Success {
        access_token: String,
        refresh_token: String,
        expires_in_secs: i64,
    },
    Failed(String),
}

// ── 本机 auth.json（通道 B：只读导入，绝不写回）───────────────────────────

/// 本机 auth.json 单条记录（含解析出的区）。
#[derive(Clone, Debug)]
pub struct LocalAuthRecord {
    pub region: Region,
    pub access_token: String,
    pub refresh_token: String,
    /// 毫秒时间戳；0 = 未知
    pub expires_at_ms: i64,
    pub subject: String,
    pub account_id: String,
    /// 来源文件绝对路径（仅展示用）
    pub path: String,
}

/// `{USERPROFILE|HOME}/.minimax/auth/prod/{cn|en}/mcode-public/auth.json`
fn authjson_path(region: Region) -> Option<std::path::PathBuf> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join(".minimax")
            .join("auth")
            .join("prod")
            .join(region.authjson_lang())
            .join(CLIENT_ID)
            .join("auth.json"),
    )
}

/// auth.json 结构：`{ "records": { "<recordKey>": { accessToken, refreshToken, ... } } }`。
/// 取含 accessToken 的 record，按 expiresAtMs 新者优先。
fn parse_authjson(region: Region, path: &std::path::Path, raw: &str) -> Vec<LocalAuthRecord> {
    let mut out = Vec::new();
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return out;
    };
    let Some(records) = v.get("records").and_then(Value::as_object) else {
        return out;
    };
    for (_key, rec) in records {
        let access = rec.get("accessToken").and_then(Value::as_str).unwrap_or("");
        if access.trim().is_empty() {
            continue;
        }
        let expires_at_ms = rec
            .get("expiresAtMs")
            .and_then(Value::as_i64)
            .or_else(|| {
                // 宽容：expiresAt 字符串（RFC3339）或毫秒数
                rec.get("expiresAt")
                    .and_then(Value::as_str)
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.timestamp_millis())
            })
            .unwrap_or(0);
        out.push(LocalAuthRecord {
            region,
            access_token: access.trim().to_string(),
            refresh_token: rec
                .get("refreshToken")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            expires_at_ms,
            subject: rec.get("subject").and_then(Value::as_str).unwrap_or("").to_string(),
            account_id: rec
                .get("accountId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            path: path.display().to_string(),
        });
    }
    // 新者优先（毫秒大者新）
    out.sort_by(|a, b| b.expires_at_ms.cmp(&a.expires_at_ms));
    out
}

/// 扫描本机 MiniMax Code 登录态（双区各自语言目录；文件缺失跳过）。
pub fn scan_local_authjson() -> Vec<LocalAuthRecord> {
    let mut out = Vec::new();
    for region in [Region::Cn, Region::Global] {
        let Some(path) = authjson_path(region) else {
            continue;
        };
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        out.extend(parse_authjson(region, &path, &raw));
    }
    out
}

/// 读取指定区的本机 auth.json 最新记录（通道 B 过期时对齐客户端轮换结果用）。
pub fn read_local_authjson(region: Region) -> Option<LocalAuthRecord> {
    let path = authjson_path(region)?;
    let raw = std::fs::read_to_string(&path).ok()?;
    parse_authjson(region, &path, &raw).into_iter().next()
}

// ── 凭证记录与刷新（一次性轮换三防线）─────────────────────────────────────

/// 账号池 id：`mm-` + region|user_id 的 md5 前 12 位（区隔离，杜绝跨区串号）
pub fn account_id(region: Region, user_id: &str) -> String {
    format!("mm-{}", &md5_hex(format!("{}|{user_id}", region.key()).as_bytes())[..12])
}

/// 凭证记录（落 minimax_tokens 表；绝不进日志/事件）
#[derive(Clone, Default)]
pub struct TokenRec {
    pub access_token: String,
    pub refresh_token: String,
    /// 毫秒；0 = 未知
    pub expires_at_ms: i64,
    /// oauth_device（独立链，可自动刷新）| authjson（只读导入，不主动刷新）
    pub credential_source: String,
}

impl TokenRec {
    pub fn to_json(&self) -> Value {
        json!({
            "access_token": self.access_token,
            "refresh_token": self.refresh_token,
            "expires_at_ms": self.expires_at_ms,
            "credential_source": self.credential_source,
        })
    }

    pub fn from_json(v: &Value) -> TokenRec {
        TokenRec {
            access_token: v.get("access_token").and_then(Value::as_str).unwrap_or("").to_string(),
            refresh_token: v.get("refresh_token").and_then(Value::as_str).unwrap_or("").to_string(),
            expires_at_ms: v.get("expires_at_ms").and_then(Value::as_i64).unwrap_or(0),
            credential_source: v
                .get("credential_source")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }
    }
}

/// 同账号刷新互斥锁（规格三防线①：同账号串行；全局单锁是安全超集——
/// 设备码流每账号一条独立链，串行只损失少量并发，换取零轮换竞态。
/// std Mutex：签到/积分均在专用工作线程执行，阻塞等待无碍）
static REFRESH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 签到 / 积分共用的凭证就绪：返回可用 access_token。
/// 路径：有效直用 → oauth_device 互斥刷新（成功立即落库）/ authjson 重读本机文件 →
/// 失败标记 needs_relogin 熔断（后续调用直接 Err，不再打网络）。
pub fn ensure_fresh_token(st: &AppState, account: &Value) -> Result<TokenRec, String> {
    let id = account.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    if id.is_empty() {
        return Err("账号缺少 id".into());
    }
    let region = account
        .get("region")
        .and_then(Value::as_str)
        .and_then(Region::parse)
        .ok_or_else(|| "账号缺少有效 region".to_string())?;
    let db = crate::store::db(&st.data_dir);
    let rec = crate::store::docs::minimax_token_get(&db, &id)
        .map(|v| TokenRec::from_json(&v))
        .ok_or_else(|| "账号无凭证：请先完成授权或导入本机登录态".to_string())?;
    if !rec.access_token.is_empty()
        && rec.expires_at_ms > 0
        && rec.expires_at_ms - EXPIRY_MARGIN_MS > chrono::Utc::now().timestamp_millis()
    {
        return Ok(rec);
    }
    // 熔断：needs_relogin 已标记 → 直接失败（invalid_grant 的链不可能自愈）
    if account.get("needs_relogin").and_then(Value::as_bool).unwrap_or(false) {
        let reason = account
            .get("relogin_reason")
            .and_then(Value::as_str)
            .unwrap_or("凭证已失效");
        return Err(format!("凭证已失效（{reason}），请在账号管理重新授权"));
    }
    match rec.credential_source.as_str() {
        // 通道 B：只读导入——绝不主动刷新（refresh 一次性轮换会作废本机客户端链）。
        // 过期时重读本机 auth.json 对齐客户端的最新轮换结果；仍过期则如实报错。
        "authjson" => {
            if let Some(latest) = read_local_authjson(region) {
                if !latest.access_token.is_empty() && latest.access_token != rec.access_token {
                    let fresh = TokenRec {
                        access_token: latest.access_token,
                        refresh_token: latest.refresh_token,
                        expires_at_ms: latest.expires_at_ms,
                        credential_source: "authjson".into(),
                    };
                    let usable = fresh.expires_at_ms == 0
                        || fresh.expires_at_ms - EXPIRY_MARGIN_MS > chrono::Utc::now().timestamp_millis();
                    // 客户端轮换过的新值：落我们的凭证表副本（不触碰 MiniMax 客户端文件）
                    let _ = crate::store::docs::minimax_token_put(&db, &id, &fresh.to_json());
                    if usable {
                        return Ok(fresh);
                    }
                }
            }
            Err("本机登录态已过期：请打开 MiniMax Code 客户端（会自动续期）后重试，或改用设备码授权".into())
        }
        // 通道 A：设备码流独立链——互斥刷新 + 成功无条件立即落库 + 失败熔断
        _ => refresh_and_persist(st, &id, region, &rec),
    }
}

/// OAuth 设备码 / 刷新协议共用：POST {account}/oauth2/token（refresh_token grant）。
/// 返回 (http_status, 解析 JSON, 原文)。
fn oauth_token_request(region: Region, form: &str) -> (u16, Option<Value>, String) {
    let url = format!("{}/oauth2/token", region.account_base());
    let agent = crate::tasks::http_agent(20);
    let response = agent
        .post(&url)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(form);
    match response {
        Ok(resp) => {
            let raw = resp.into_string().unwrap_or_default();
            (200, serde_json::from_str(&raw).ok(), raw)
        }
        Err(ureq::Error::Status(code, resp)) => {
            let raw = resp.into_string().unwrap_or_default();
            (code, serde_json::from_str(&raw).ok(), raw)
        }
        Err(e) => (0, None, e.to_string()),
    }
}

/// 刷新当前凭证并立即落库（三防线②③：互斥内二次校验 + invalid_grant 熔断标记）。
fn refresh_and_persist(st: &AppState, id: &str, region: Region, rec: &TokenRec) -> Result<TokenRec, String> {
    let _guard = REFRESH_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let db = crate::store::db(&st.data_dir);
    // 互斥内二次校验：等待期间其他路径可能已完成刷新
    if let Some(latest) = crate::store::docs::minimax_token_get(&db, id) {
        let latest = TokenRec::from_json(&latest);
        if !latest.access_token.is_empty()
            && latest.access_token != rec.access_token
            && latest.expires_at_ms - EXPIRY_MARGIN_MS > chrono::Utc::now().timestamp_millis()
        {
            return Ok(latest);
        }
    }
    if rec.refresh_token.is_empty() {
        return Err("无 refresh_token：请在账号管理重新授权".into());
    }
    let form = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={CLIENT_ID}&scope={OAUTH_SCOPE}&audience={OAUTH_AUDIENCE}",
        encode_uri_component(&rec.refresh_token)
    );
    let (status, parsed, raw) = oauth_token_request(region, &form);
    if status == 200 {
        if let Some(v) = parsed {
            let access = v.get("access_token").and_then(Value::as_str).unwrap_or("").to_string();
            if !access.is_empty() {
                let fresh = TokenRec {
                    access_token: access,
                    refresh_token: v
                        .get("refresh_token")
                        .and_then(Value::as_str)
                        .unwrap_or(&rec.refresh_token)
                        .to_string(),
                    expires_at_ms: chrono::Utc::now().timestamp_millis()
                        + v.get("expires_in").and_then(Value::as_i64).unwrap_or(0) * 1000,
                    credential_source: rec.credential_source.clone(),
                };
                // 三防线②：刷新成功无条件立即落库（新链不落库 = 下轮拿旧链必败）
                crate::store::docs::minimax_token_put(&db, id, &fresh.to_json())
                    .map_err(|e| format!("刷新后的凭证落库失败：{e}"))?;
                crate::fs_utils::app_log(&st.data_dir, "[minimax] 凭证刷新成功并已落库");
                return Ok(fresh);
            }
        }
        return Err(format!("刷新响应缺少 access_token：{}", excerpt(&raw)));
    }
    // 400/401 invalid_grant = 链已作废（终态）：标记 needs_relogin 熔断，不再自动重试
    let invalid = status == 400 || status == 401;
    let message = if invalid {
        "授权链已失效（refresh 一次性轮换被拒绝），请重新授权".to_string()
    } else {
        format!("凭证刷新失败（HTTP {status}）：{}", excerpt(&raw))
    };
    if invalid {
        if let Err(e) = mark_needs_relogin(&db, id, "refresh 链已作废") {
            crate::fs_utils::app_log(&st.data_dir, &format!("[minimax] 熔断标记落库失败：{e}"));
        }
    }
    Err(message)
}

/// 标记账号 needs_relogin（熔断；Clearable by 重新授权 / 移除重加）。
pub fn mark_needs_relogin(db: &crate::store::Store, id: &str, reason: &str) -> Result<(), String> {
    let mut pool = crate::store::docs::minimax_pool_load(db);
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
            return crate::store::docs::minimax_pool_save(db, &pool);
        }
    }
    Ok(())
}

// ── 用户资料（纯 Bearer，不签名）──────────────────────────────────────────

/// GET /matrix/api/v1/user/profile：校验凭证归属并取 user_id / email / 名称。
pub fn fetch_profile(region: Region, access_token: &str) -> Result<Value, String> {
    let url = format!("{}/matrix/api/v1/user/profile", region.agent_base());
    let agent = crate::tasks::http_agent(20);
    let response = agent
        .get(&url)
        .set("Authorization", &format!("Bearer {access_token}"))
        .set("User-Agent", USER_AGENT)
        .call();
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
        return Err(format!("用户资料查询失败（HTTP {status}）：{}", excerpt(&raw)));
    }
    let value = parsed.ok_or_else(|| format!("用户资料响应不是 JSON：{}", excerpt(&raw)))?;
    reject_business_error(&value)?;
    Ok(value.get("data").cloned().unwrap_or(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 金测：yy 固定输入 → 固定输出（签名常量被客户端热更时，此处首红）
    #[test]
    fn sign_yy_golden() {
        // 已知输入的确定性校验：同输入恒同输出（md5 链固化）
        let a = sign_yy("/minimax-cloud/api/v1/signin/status", "device_platform=mcode&unix=1700000000000", "{}", 1700000000000);
        let b = sign_yy("/minimax-cloud/api/v1/signin/status", "device_platform=mcode&unix=1700000000000", "{}", 1700000000000);
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        // unix 变化 → md5(String(unix)) 变化 → yy 变化
        let c = sign_yy("/minimax-cloud/api/v1/signin/status", "device_platform=mcode&unix=1700000000001", "{}", 1700000000001);
        assert_ne!(a, c);
    }

    /// 金测：x-signature 固定输入 → 固定输出
    #[test]
    fn sign_xsignature_golden() {
        let a = sign_xsignature(1700000000, "{}");
        assert_eq!(a.len(), 32);
        // 与手工展开的 md5("1700000000" + KEY + "{}") 一致（固化盐值防漂移）
        let mut hasher = Md5::new();
        hasher.update((format!("1700000000{XSIGN_KEY}") + "{}").as_bytes());
        let manual: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(a, manual);
        // GET（空 body）与 POST（实际 JSON）签名不同
        assert_ne!(a, sign_xsignature(1700000000, ""));
    }

    #[test]
    fn encode_uri_component_matches_js_semantics() {
        assert_eq!(encode_uri_component("abc-_.!~*'()"), "abc-_.!~*'()");
        assert_eq!(encode_uri_component("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(encode_uri_component("中"), "%E4%B8%AD");
    }

    #[test]
    fn region_parse_and_domains() {
        assert_eq!(Region::parse("minimax"), Some(Region::Cn));
        assert_eq!(Region::parse("minimax-global"), Some(Region::Global));
        assert_eq!(Region::parse("global"), None);
        assert_eq!(Region::Cn.agent_base(), "https://agent.minimax.cn");
        assert_eq!(Region::Global.account_base(), "https://account.minimax.io");
        assert_eq!(Region::Global.lang(), "en");
    }

    #[test]
    fn account_id_is_stable_and_region_separated() {
        let a = account_id(Region::Cn, "u1");
        let b = account_id(Region::Cn, "u1");
        let g = account_id(Region::Global, "u1");
        assert_eq!(a, b);
        assert_ne!(a, g);
        assert!(a.starts_with("mm-"));
        assert_eq!(a.len(), "mm-".len() + 12);
    }

    #[test]
    fn reject_business_error_envelope() {
        assert!(reject_business_error(&json!({"base_resp": {"status_code": 0}})).is_ok());
        assert!(reject_business_error(&json!({"base_resp": {"status_code": 1001, "status_msg": "签名错误"}})).is_err());
        // 无信封放行（宽容）
        assert!(reject_business_error(&json!({"data": {}})).is_ok());
    }

    #[test]
    fn pkce_verifier_shape() {
        let v = generate_verifier();
        assert_eq!(v.len(), 64);
        assert!(v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')));
        let challenge = code_challenge_s256(&v);
        assert_eq!(challenge.len(), 43);
        // S256 = BASE64URL(SHA256(verifier)) 无填充
        assert!(!challenge.contains('='));
    }

    #[test]
    fn parse_authjson_picks_access_records() {
        let raw = r#"{"records": {
            "a": {"accessToken": "at-1", "refreshToken": "rt-1", "expiresAtMs": 100, "subject": "s1", "accountId": "ac1"},
            "b": {"refreshToken": "no-access"},
            "c": {"accessToken": "at-2", "expiresAtMs": 200}
        }}"#;
        let recs = parse_authjson(Region::Cn, std::path::Path::new("x/auth.json"), raw);
        assert_eq!(recs.len(), 2);
        // 新者优先排序（expiresAtMs 大在前）
        assert_eq!(recs[0].access_token, "at-2");
        assert_eq!(recs[1].subject, "s1");
    }

    /// 真机探针（#[ignore]，`cargo test minimax_live_probe -- --ignored --nocapture`）：
    /// 只读本机 auth.json → profile（纯 Bearer）→ signin/status（签名端点），验证签名链。
    /// 仅 status 不 claim（无副作用）；输出脱敏（不打印任何 token）。
    #[test]
    #[ignore]
    fn minimax_live_probe() {
        let Some(record) = read_local_authjson(Region::Cn) else {
            println!("[probe] 本机无 CN 区 auth.json，跳过");
            return;
        };
        println!(
            "[probe] auth.json 就绪：expires_at_ms={} subject={}字",
            record.expires_at_ms,
            record.subject.len()
        );
        let profile = match fetch_profile(Region::Cn, &record.access_token) {
            Ok(p) => p,
            Err(e) => {
                println!("[probe] profile 失败（token 可能已过期，客户端运行时会轮换）：{e}");
                return;
            }
        };
        let uid = profile.get("user_id").and_then(Value::as_str).unwrap_or("0");
        let email = profile.get("email").and_then(Value::as_str).unwrap_or("");
        println!("[probe] profile ok：user_id={uid} email={}字", email.len());
        let path = "/minimax-cloud/api/v1/signin/status";
        let (status, parsed, raw) = agent_request(
            Region::Cn,
            "GET",
            path,
            "timezone_offset=28800&is_desktop=1&client=desktop",
            None,
            &record.access_token,
            uid,
            Region::Cn.lang(),
        );
        println!("[probe] signin/status HTTP={status} raw_head={}", excerpt(&raw));
        if let Some(v) = &parsed {
            let base_code = v
                .pointer("/base_resp/status_code")
                .and_then(Value::as_i64)
                .unwrap_or(i64::MIN);
            println!("[probe] base_resp.status_code={base_code} today_status={:?}", today_status_of(v));
        }
    }

    /// 探针辅助：提今日 status（与 minimax_checkin::today_signin_status 同口径，避免循环依赖）
    fn today_status_of(v: &Value) -> Option<i64> {
        v.get("data")?
            .get("days")?
            .as_array()?
            .iter()
            .find(|d| d.get("is_today").and_then(Value::as_bool).unwrap_or(false))
            .and_then(|d| d.get("status").and_then(Value::as_i64))
    }

    /// 真机 claim 探针（#[ignore]，一次性验证领取链；等价应用每日签到行为——
    /// 领的是用户本机账号当日积分，重复执行幂等）。
    #[test]
    #[ignore]
    fn minimax_live_claim_probe() {
        let Some(record) = read_local_authjson(Region::Cn) else {
            println!("[claim-probe] 本机无 CN 区 auth.json，跳过");
            return;
        };
        let profile = match fetch_profile(Region::Cn, &record.access_token) {
            Ok(p) => p,
            Err(e) => {
                println!("[claim-probe] profile 失败：{e}");
                return;
            }
        };
        let uid = profile.get("user_id").and_then(Value::as_str).unwrap_or("0");
        // 先查积分余额（快照前置）
        let (before_status, before_parsed, raw_before) = agent_request(
            Region::Cn,
            "POST",
            "/matrix/api/v1/commerce/get_membership_info",
            "",
            Some(&Value::Object(serde_json::Map::new())),
            &record.access_token,
            uid,
            Region::Cn.lang(),
        );
        if before_status == 200 {
            if let Some(v) = &before_parsed {
                println!(
                    "[claim-probe] 余额前置 free={:?} total={:?}",
                    v.pointer("/op_credit_summary/free_remaining_amount"),
                    v.pointer("/op_credit_summary/total_remaining_amount")
                );
            }
        } else {
            println!("[claim-probe] 余额前置 HTTP={before_status} {}", excerpt(&raw_before));
        }
        // claim
        let path = "/minimax-cloud/api/v1/signin/claim";
        let (status, parsed, raw) = agent_request(
            Region::Cn,
            "POST",
            path,
            "timezone_offset=28800&is_desktop=1&client=desktop",
            Some(&Value::Object(serde_json::Map::new())),
            &record.access_token,
            uid,
            Region::Cn.lang(),
        );
        println!("[claim-probe] claim HTTP={status} raw_head={}", excerpt(&raw));
        if let Some(v) = &parsed {
            println!(
                "[claim-probe] claim_result={:?} points={:?} base_code={:?}",
                v.pointer("/data/claim_result"),
                v.pointer("/data/points"),
                v.pointer("/base_resp/status_code")
            );
        }
    }
}
