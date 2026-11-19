//! OfficeAce（华为果办 / OfficeClaw）每日签到。
//!
//! 协议依据（离线实测，2026-11-16 本机 OfficeAce 1.8.1）：
//! - 本机登录态：`<安装根>\data\storage.sqlite`（`oc_kv_string` 表，
//!   键前缀 `office-claw:oc:session:user:`）→ `providerStateRef`
//!   （`wincred://OfficeAce/auth-sessions/<provider>/<org>%3A<user>/provider-state`）。
//! - `CredRead` 该 target 得 manifest（UTF-16LE JSON：`encoding=base64`、
//!   `storageId`、`chunkCount`），再逐块读 `<target>/chunks/<storageId>/<n>`，
//!   按序拼接 base64 → 解码 → provider-state JSON。
//! - provider-state 内含 `modelInfo.model_api_url_base`（模型网关主机名）与
//!   `credential` / `credentialV3`（临时 AK/SK/STS/项目号，约 2 小时有效）。
//! - 签到：控制面 `POST https://<网关主机>/v1/subscription/bonus/claim`，
//!   body 必须是 `{}`（无体会 500 `OfficeAce.11020001`），带
//!   `x-subscription-type: v2`，签名算法 **V11-HMAC-SHA256**（region
//!   `cn-southwest-2`、service `apic`）。
//! - 幂等由上游保证：一天一个活动只领一次，重复领不报错（返回同一份列表）。
//!
//! 只读约束：本模块只读本机存储与 Windows 凭据管理器，**不写回**。
//! 临时凭证过期时如实报错并提示打开 OfficeAce 桌面端刷新登录态——不消费
//! 一次性 refresh token（桌面端与它共用同一份登录态，写入会互相破坏）。

use base64::Engine;
use hmac::{Hmac, Mac};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::state::AppState;

const SESSION_PREFIX: &str = "office-claw:oc:session:user:";
const CLAIM_PATH: &str = "/v1/subscription/bonus/claim";
const SUBSCRIPTION_PATH: &str = "/v1/subscription";
const SIGN_REGION: &str = "cn-southwest-2";
const SCOPE_SERVICE: &str = "apic";
const EXPIRY_MARGIN_MS: i64 = 30 * 60 * 1000;
const EMPTY_BODY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// 登录态/凭证过期时的可执行指引（预检与上游归类共用同一口径）。
const RELOGIN_HINT: &str = "请打开 OfficeAce 客户端重新登录后再签到";

/// 本机登录态（脱敏视图；凭证字段不参与序列化）。
#[derive(Serialize, Clone, Default)]
pub struct OfficeAceStatus {
    pub installed: bool,
    pub logged_in: bool,
    pub user_id: String,
    pub name: String,
    /// 会话到期时刻（ISO 8601，来自本机 session 记录）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_expires_at: Option<String>,
    /// 登录会话是否已过期（与签到预检同口径；过期时 logged_in=false）
    pub session_expired: bool,
    /// 控制面临时凭证到期时刻（Unix 毫秒；0 = 未知）
    pub credential_expires_at: Option<i64>,
    /// 凭证是否仍在有效期内（含 30 分钟余量；登录会话过期时一并视为不新鲜）
    pub credential_fresh: bool,
    /// 网关主机（不含 scheme 与路径）
    pub gateway_host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 内存态凭据（绝不落盘、不进日志/事件）。
#[derive(Clone, Default)]
struct ControlCredential {
    access_key_id: String,
    secret_access_key: String,
    security_token: String,
    project_id: String,
    expires_at_ms: i64,
}

#[derive(Clone, Default)]
struct LocalAccount {
    user_id: String,
    name: String,
    session_expires_at: Option<String>,
    gateway_host: String,
    control: ControlCredential,
}

// ── 本机存储读取 ────────────────────────────────────────────────────────────

fn install_root() -> Option<std::path::PathBuf> {
    let local = std::env::var("LOCALAPPDATA").ok()?;
    let root = std::path::PathBuf::from(local).join("Programs").join("OfficeAce");
    if root.join("data").join("storage.sqlite").is_file() {
        Some(root)
    } else {
        None
    }
}

fn storage_db_path() -> Option<std::path::PathBuf> {
    Some(install_root()?.join("data").join("storage.sqlite"))
}

/// 读最新的登录会话记录（同账号可能残留多份，按 lastSeenAt/createdAt 取最新）。
fn read_session(db_path: &std::path::Path) -> Result<Option<(String, Value)>, String> {
    use rusqlite::{Connection, OpenFlags};
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("打开 OfficeAce 本地状态库失败：{e}"))?;
    let mut stmt = conn
        .prepare("select key, value from oc_kv_string where key like ?1 order by key")
        .map_err(|e| format!("读取 OfficeAce 登录记录失败：{e}"))?;
    let rows = stmt
        .query_map([format!("{SESSION_PREFIX}%")], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| format!("读取 OfficeAce 登录记录失败：{e}"))?;
    let mut best: Option<(String, Value, String)> = None;
    for row in rows.flatten() {
        let (key, raw) = row;
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let stamp = value
            .get("lastSeenAt")
            .or_else(|| value.get("createdAt"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let key_of = format!("{stamp}|{key}");
        let better = best
            .as_ref()
            .map(|(current, _, _)| key_of > *current)
            .unwrap_or(true);
        if better {
            best = Some((key_of, value, key));
        }
    }
    Ok(best.map(|(_, value, key)| (key, value)))
}

#[cfg(windows)]
fn wincred_read(target: &str) -> Option<String> {
    use windows_sys::Win32::Security::Credentials::{
        CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
    };
    let wide: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    let mut raw: *mut CREDENTIALW = std::ptr::null_mut();
    let ok = unsafe { CredReadW(wide.as_ptr(), CRED_TYPE_GENERIC, 0, &mut raw) };
    if ok == 0 || raw.is_null() {
        return None;
    }
    let cred = unsafe { &*raw };
    let bytes = unsafe {
        std::slice::from_raw_parts(cred.CredentialBlob, cred.CredentialBlobSize as usize)
    }
    .to_vec();
    unsafe { CredFree(raw as *mut _) };
    Some(utf16le_to_string(&bytes))
}

#[cfg(not(windows))]
fn wincred_read(_target: &str) -> Option<String> {
    None
}

fn utf16le_to_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units).trim_end_matches('\0').to_string()
}

/// 解码 Windows 凭据管理器里的 provider-state（manifest + 分块）。
fn decode_provider_state(provider_ref: &str) -> Result<Value, String> {
    let target = provider_ref
        .trim()
        .strip_prefix("wincred://")
        .unwrap_or(provider_ref.trim())
        .trim_end_matches('/')
        .to_string();
    if target.is_empty() {
        return Err("登录记录缺少 providerStateRef".into());
    }
    let manifest_text = wincred_read(&target)
        .ok_or_else(|| "读取 OfficeAce 登录凭据失败（Windows 凭据管理器未找到 provider-state）".to_string())?;
    let manifest: Value =
        serde_json::from_str(&manifest_text).map_err(|e| format!("provider-state manifest 解析失败：{e}"))?;
    let storage_id = manifest
        .get("storageId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let chunk_count = manifest
        .get("chunkCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if storage_id.is_empty() || chunk_count == 0 || chunk_count > 64 {
        return Err("provider-state manifest 字段异常（storageId/chunkCount）".into());
    }
    let mut encoded = String::new();
    for index in 0..chunk_count {
        let chunk_target = format!("{target}/chunks/{storage_id}/{index}");
        let part = wincred_read(&chunk_target)
            .ok_or_else(|| format!("读取 OfficeAce 凭据点分块失败（第 {index} 块）"))?;
        encoded.push_str(part.trim());
    }
    let decoded = decode_base64_lenient(&encoded)
        .ok_or_else(|| "OfficeAce 登录凭据 base64 解码失败".to_string())?;
    serde_json::from_slice::<Value>(&decoded).map_err(|e| format!("OfficeAce 登录凭据 JSON 解析失败：{e}"))
}

fn decode_base64_lenient(text: &str) -> Option<Vec<u8>> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let engines: [base64::engine::GeneralPurpose; 2] = [
        base64::engine::general_purpose::STANDARD,
        base64::engine::general_purpose::STANDARD_NO_PAD,
    ];
    for engine in engines {
        if let Ok(bytes) = engine.decode(&cleaned) {
            return Some(bytes);
        }
    }
    let mut padded = cleaned.clone();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    base64::engine::general_purpose::STANDARD
        .decode(&padded)
        .ok()
}

// ── 凭据解析 ────────────────────────────────────────────────────────────────

fn text_of(v: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = v.get(*key).and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

fn number_of(v: &Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        let Some(raw) = v.get(*key) else { continue };
        if let Some(n) = raw.as_i64() {
            return Some(normalize_ms(n));
        }
        if let Some(f) = raw.as_f64() {
            return Some(normalize_ms(f as i64));
        }
        if let Some(text) = raw.as_str() {
            if let Some(ms) = parse_time_ms(text) {
                return Some(ms);
            }
            if let Ok(n) = text.trim().parse::<i64>() {
                return Some(normalize_ms(n));
            }
        }
    }
    None
}

/// 数字时间戳归一：> 1e12 视为毫秒，否则视为秒。
fn normalize_ms(value: i64) -> i64 {
    if value > 1_000_000_000_000 {
        value
    } else if value > 0 {
        value * 1000
    } else {
        0
    }
}

/// 时间字段宽容解析：RFC3339 → 毫秒；纯数字按「秒 / 毫秒」判定。
fn parse_time_ms(text: &str) -> Option<i64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(n) = trimmed.parse::<i64>() {
        return Some(if n > 1_000_000_000_000 { n } else { n * 1000 });
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.timestamp_millis());
    }
    None
}

/// 登录会话记录的 `expiresAt` 是否已过期：过期返回到期时刻（毫秒）。
/// 字段缺失 / 无法解析时返回 None（不判过期——宁可不拦，不能误拦新会话）。
fn session_expired_at(session_expires_at: Option<&str>, now: i64) -> Option<i64> {
    let text = session_expires_at.map(str::trim).filter(|s| !s.is_empty())?;
    let ms = parse_time_ms(text)?;
    if ms > 0 && ms <= now {
        Some(ms)
    } else {
        None
    }
}

/// 过期时刻本地展示：`YYYY-MM-DD HH:MM`（与用户在本机看到的时间一致）。
fn local_time_text(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| ms.to_string())
}

/// provider-state JSON → 本机账号（缺关键字段时返回可读错误）。
fn account_from_state(state: &Value) -> Result<LocalAccount, String> {
    let ps = state.get("providerState").unwrap_or(state);
    let user_id = text_of(ps, &["userId", "user_id", "accountId", "account_id"]);
    let name = text_of(ps, &["userName", "user_name", "displayName", "nickname"]);
    let model_info = ps.get("modelInfo").or_else(|| ps.get("model_info"));
    let host_raw = model_info
        .and_then(|m| m.get("model_api_url_base"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let gateway_host = host_raw
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();
    if gateway_host.is_empty() {
        return Err("OfficeAce 登录态缺少模型网关地址（model_api_url_base）".into());
    }
    let mut control = ControlCredential::default();
    for src in [ps.get("credential"), ps.get("credentialV3")] {
        let Some(src) = src else { continue };
        if control.access_key_id.is_empty() {
            control.access_key_id = text_of(src, &["access", "access_key_id", "accessKeyId"]);
        }
        if control.secret_access_key.is_empty() {
            control.secret_access_key = text_of(src, &["secret", "secret_access_key", "secretAccessKey"]);
        }
        if control.security_token.is_empty() {
            control.security_token = text_of(src, &["sts_token", "security_token", "securityToken"]);
        }
        if control.project_id.is_empty() {
            control.project_id = text_of(src, &["project_id", "projectId"]);
        }
        if control.expires_at_ms == 0 {
            control.expires_at_ms =
                number_of(src, &["expires_at", "expiresAt", "expiration"]).unwrap_or(0);
        }
    }
    if control.access_key_id.is_empty() || control.secret_access_key.is_empty() {
        return Err("OfficeAce 登录态缺少控制面临时凭据（AK/SK）：请在桌面端重新登录一次".into());
    }
    Ok(LocalAccount {
        user_id: if user_id.is_empty() {
            "officeace-local".to_string()
        } else {
            user_id
        },
        name: if name.is_empty() {
            "OfficeAce 账号".to_string()
        } else {
            name
        },
        session_expires_at: ps
            .get("expires_at")
            .or_else(|| ps.get("expiresAt"))
            .and_then(Value::as_str)
            .map(str::to_string),
        gateway_host,
        control,
    })
}

/// 本机登录账号（无安装 / 未登录时返回 None；解析错误进 Err）。
fn local_account() -> Result<Option<(LocalAccount, Option<String>)>, String> {
    let Some(db_path) = storage_db_path() else {
        return Ok(None);
    };
    let Some((_key, session)) = read_session(&db_path)? else {
        return Ok(None);
    };
    let provider_ref = session
        .get("providerStateRef")
        .and_then(Value::as_str)
        .unwrap_or("");
    let state = decode_provider_state(provider_ref)?;
    let account = account_from_state(&state)?;
    let session_expires_at = session
        .get("expiresAt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(account.session_expires_at.clone());
    Ok(Some((account, session_expires_at)))
}

pub fn status() -> OfficeAceStatus {
    let installed = install_root().is_some();
    if !installed {
        return OfficeAceStatus {
            installed: false,
            error: Some("未检测到 OfficeAce 桌面端（%LOCALAPPDATA%\\Programs\\OfficeAce）".into()),
            ..Default::default()
        };
    }
    match local_account() {
        Ok(Some((account, session_expires_at))) => {
            let session_expired_ms = session_expired_at(session_expires_at.as_deref(), now_ms());
            let session_expired = session_expired_ms.is_some();
            let fresh = !session_expired
                && account.control.expires_at_ms > 0
                && account.control.expires_at_ms - EXPIRY_MARGIN_MS > now_ms();
            OfficeAceStatus {
                installed: true,
                // 登录会话已过期 = 登录态失效：与签到预检同口径，
                // 前端据此显示重新登录指引而不是「凭证临期」
                logged_in: !session_expired,
                user_id: account.user_id,
                name: account.name,
                session_expires_at,
                session_expired,
                credential_expires_at: Some(account.control.expires_at_ms).filter(|v| *v > 0),
                credential_fresh: fresh,
                gateway_host: account.gateway_host,
                error: session_expired_ms.map(|ms| {
                    format!("OfficeAce 登录态已于 {} 过期，{RELOGIN_HINT}", local_time_text(ms))
                }),
            }
        }
        Ok(None) => OfficeAceStatus {
            installed: true,
            error: Some("OfficeAce 桌面端未登录：请打开客户端完成登录".into()),
            ..Default::default()
        },
        Err(e) => OfficeAceStatus {
            installed: true,
            error: Some(e),
            ..Default::default()
        },
    }
}

// ── V11-HMAC-SHA256 签名 ────────────────────────────────────────────────────

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC 接受任意长度密钥");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    hmac_sha256(key, message)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
}

fn encode_component(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len());
    for byte in bytes {
        if unreserved(*byte) {
            out.push(*byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    out
}

/// 路径按段解码再编码，末尾**强制补斜杠**（OfficeAce 签名器口径）。
fn canonical_uri(path: &str) -> String {
    let path = if path.is_empty() { "/" } else { path };
    let escaped = path
        .split('/')
        .map(|segment| encode_component(segment.as_bytes()))
        .collect::<Vec<_>>()
        .join("/");
    if escaped.ends_with('/') {
        escaped
    } else {
        format!("{escaped}/")
    }
}

fn authority_of(url: &str) -> Result<String, String> {
    let after_scheme = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .ok_or_else(|| format!("地址缺少 scheme：{url}"))?;
    let host_end = after_scheme.find(['/', '?', '#']).unwrap_or(after_scheme.len());
    Ok(after_scheme[..host_end].rsplit('@').next().unwrap_or("").to_string())
}

fn path_of(url: &str) -> String {
    let Some(after_scheme) = url.split_once("://").map(|(_, rest)| rest) else {
        return "/".to_string();
    };
    let start = after_scheme.find('/').unwrap_or(after_scheme.len());
    after_scheme[start..]
        .split(['?', '#'])
        .next()
        .unwrap_or("/")
        .to_string()
}

/// 给一次控制面请求签名，返回应发出去的头集合（入参头 + 注入项 + Authorization）。
fn sign_v11(
    method: &str,
    url: &str,
    body: &[u8],
    extra_headers: &[(&str, &str)],
    credential: &ControlCredential,
) -> Result<Vec<(String, String)>, String> {
    if credential.access_key_id.is_empty() || credential.secret_access_key.is_empty() {
        return Err("OfficeAce 控制面凭据不完整（AK/SK 缺失）".into());
    }
    let date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let mut headers: Vec<(String, String)> = extra_headers
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    headers.push(("X-Sdk-Date".to_string(), date.clone()));
    headers.push(("Host".to_string(), authority_of(url)?));
    if !credential.security_token.is_empty() {
        headers.push(("X-Security-Token".to_string(), credential.security_token.clone()));
    }
    if !credential.project_id.is_empty() {
        headers.push(("X-Project-ID".to_string(), credential.project_id.clone()));
    }
    let mut canonical: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| {
            (
                name.to_ascii_lowercase(),
                value.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect();
    canonical.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical_headers: String = canonical
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect();
    let signed_headers = canonical
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let payload_hash = if body.is_empty() {
        EMPTY_BODY_SHA256.to_string()
    } else {
        sha256_hex(body)
    };
    let canonical_request = [
        method.to_ascii_uppercase(),
        canonical_uri(&path_of(url)),
        String::new(),
        canonical_headers,
        signed_headers.clone(),
        payload_hash,
    ]
    .join("\n");
    let scope = format!("{}/{SIGN_REGION}/{SCOPE_SERVICE}", &date[..8.min(date.len())]);
    let string_to_sign = [
        "V11-HMAC-SHA256",
        &date,
        &scope,
        &sha256_hex(canonical_request.as_bytes()),
    ]
    .join("\n");
    let step1 = hmac_sha256(
        credential.access_key_id.as_bytes(),
        credential.secret_access_key.as_bytes(),
    );
    let mut seed = scope.clone().into_bytes();
    seed.push(1);
    let derived = &hmac_sha256(&step1, &seed)[..32];
    let signing_key_hex: String = derived.iter().map(|byte| format!("{byte:02x}")).collect();
    let signature = hmac_sha256_hex(signing_key_hex.as_bytes(), string_to_sign.as_bytes());
    let authorization = format!(
        "V11-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        credential.access_key_id
    );
    headers.push(("Authorization".to_string(), authorization));
    Ok(headers)
}

// ── 控制面请求 ──────────────────────────────────────────────────────────────

fn control_request(
    account: &LocalAccount,
    method: &str,
    path: &str,
    body: Option<&Value>,
    extra: &[(&str, &str)],
) -> (u16, Option<Value>, String) {
    let url = format!("https://{}{path}", account.gateway_host);
    let payload = body
        .map(|value| value.to_string().into_bytes())
        .unwrap_or_default();
    let mut headers = extra.to_vec();
    if body.is_some() {
        headers.push(("Content-Type", "application/json"));
    }
    let signed = match sign_v11(method, &url, &payload, &headers, &account.control) {
        Ok(headers) => headers,
        Err(e) => return (0, None, e),
    };
    let agent = crate::tasks::http_agent(20);
    let mut request = if method == "POST" {
        agent.post(&url)
    } else {
        agent.get(&url)
    };
    for (name, value) in &signed {
        // Host 由 ureq 依 URL 自动写入（与签名里用的 authority 同值），不重复设置
        if name.eq_ignore_ascii_case("host") {
            continue;
        }
        request = request.set(name, value);
    }
    let response = if method == "POST" {
        request.send_string(&String::from_utf8_lossy(&payload))
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

/// 上游 HTTP 400 响应体是否表示凭证/登录态过期。APIG 网关对过期 security
/// token 返回 400（而非 401），body 带 `APIG.0602` /
/// `security token has expired`（大小写不敏感）。
fn is_token_expired_response(status: u16, raw: &str) -> bool {
    if status != 400 {
        return false;
    }
    let lower = raw.to_lowercase();
    lower.contains("apig.0602") || lower.contains("security token has expired")
}

/// 错误文案是否属于「凭证/登录态过期」类（`upstream_failure` 归类后的文案
/// 自带同一标记）。
fn is_token_expired_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("apig.0602") || lower.contains("security token has expired")
}

/// 非 2xx 上游响应 → 错误文案。400 + APIG.0602 / security token has expired
/// 归类为「凭证/登录态过期」，文案自带 `{RELOGIN_HINT}`，调用方不得再追加
/// 「请稍后重试」；其余状态码保持通用文案口径。
fn upstream_failure(stage: &str, status: u16, raw: &str) -> String {
    if is_token_expired_response(status, raw) {
        return format!(
            "{stage}失败（HTTP 400）：上游判定登录态/临时凭证已过期（APIG.0602 security token has expired）；{RELOGIN_HINT}"
        );
    }
    format!("{stage}失败（HTTP {status}）：{}", excerpt(raw))
}

fn fetch_subscription(account: &LocalAccount) -> Result<Value, String> {
    let (status, parsed, raw) = control_request(
        account,
        "GET",
        SUBSCRIPTION_PATH,
        None,
        &[("x-subscription-type", "v2")],
    );
    if status == 200 {
        let value = parsed.ok_or_else(|| format!("订阅响应不是 JSON：{}", excerpt(&raw)))?;
        reject_error_payload(&value)?;
        return Ok(value);
    }
    Err(upstream_failure("订阅查询", status, &raw))
}

fn claim_bonus(account: &LocalAccount) -> Result<Value, String> {
    let payload = json!({});
    let (status, parsed, raw) = control_request(
        account,
        "POST",
        CLAIM_PATH,
        Some(&payload),
        &[("x-subscription-type", "v2")],
    );
    if status == 200 {
        let value = parsed.ok_or_else(|| format!("签到响应不是 JSON：{}", excerpt(&raw)))?;
        reject_error_payload(&value)?;
        return Ok(value);
    }
    Err(upstream_failure("签到请求", status, &raw))
}

/// 伪 200 信封防御：HTTP 200 但体内带 error / 非 0 code 时按失败处理（否则会
/// 被误判成「今日已签」）。
fn reject_error_payload(value: &Value) -> Result<(), String> {
    let code = value
        .get("code")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
        })
        .unwrap_or(0);
    let error_field = value.get("error").filter(|v| !v.is_null());
    let error_text = error_field
        .and_then(|v| {
            v.as_str().map(str::to_string).or_else(|| {
                v.get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
        })
        .unwrap_or_default();
    let has_error = error_text.trim().len() > 0 || error_field.map(|v| v.is_object()).unwrap_or(false);
    if code != 0 || has_error {
        return Err(format!(
            "上游返回错误（code={code}）：{}",
            if error_text.trim().is_empty() {
                "未知错误"
            } else {
                error_text.trim()
            }
        ));
    }
    Ok(())
}

fn excerpt(text: &str) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    cleaned.chars().take(240).collect()
}

// ── 积分解析 ────────────────────────────────────────────────────────────────

fn num(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Number(number)) => number.as_f64().unwrap_or(0.0),
        Some(Value::String(text)) => text.trim().parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn bonuses_of(subscription: &Value) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    let Some(items) = subscription.get("bonus_skus").and_then(Value::as_array) else {
        return out;
    };
    for item in items {
        let activity = text_of(item, &["activity_id", "cbc_resource_id", "activity_name"]);
        if activity.is_empty() {
            continue;
        }
        let total = num(item.get("points"));
        let used = num(item.get("current_value"));
        out.push((activity, (total - used).max(0.0)));
    }
    out
}

fn gained_between(before: &Value, after: &Value) -> f64 {
    let previous = bonuses_of(before);
    bonuses_of(after)
        .into_iter()
        .map(|(activity, remaining)| {
            match previous
                .iter()
                .find(|(name, _)| name == &activity)
                .map(|(_, value)| *value)
            {
                Some(prev) => (remaining - prev).max(0.0),
                None => remaining,
            }
        })
        .sum()
}

// ── 签到轮次 ────────────────────────────────────────────────────────────────

#[derive(Serialize, Clone)]
pub struct OfficeAceCheckinRecord {
    pub date: String,
    pub time: String,
    pub time_ms: i64,
    pub user_id: String,
    pub name: String,
    pub status: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reward: Option<f64>,
}

/// 签到轮次全局锁（应用内调度器与 UI 手动触发互斥；CLI 另用跨进程互斥体）。
static OFFICEACE_ROUND_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) fn try_acquire_round() -> Result<tokio::sync::MutexGuard<'static, ()>, String> {
    OFFICEACE_ROUND_LOCK
        .try_lock()
        .map_err(|_| "已有 OfficeAce 签到任务在执行中，请等待当前轮次完成".to_string())
}

/// 签到预检：本机登录会话（session `expiresAt`）或控制面临时凭证
/// （STS `expires_at_ms`）已过期时返回 fail 文案（含过期时刻与
/// `{RELOGIN_HINT}`），调用方直接落 fail、不得再发上游请求。
/// 字段缺失 / 无法解析时不拦截（避免把新会话误判成过期）。
fn login_state_expired_reason(
    account: &LocalAccount,
    session_expires_at: Option<&str>,
) -> Option<String> {
    let now = now_ms();
    if let Some(expired_ms) = session_expired_at(session_expires_at, now) {
        return Some(format!(
            "OfficeAce 登录态已于 {} 过期，{RELOGIN_HINT}",
            local_time_text(expired_ms)
        ));
    }
    if account.control.expires_at_ms > 0 && account.control.expires_at_ms <= now {
        return Some(format!(
            "OfficeAce 临时凭证已于 {} 过期，请打开 OfficeAce 客户端刷新登录态后重试（签到复用桌面端登录态，凭证约 2 小时有效）",
            local_time_text(account.control.expires_at_ms)
        ));
    }
    None
}

/// claim 失败 → 用户可见文案。凭证/登录态过期类（上游 400 APIG.0602 /
/// security token has expired）自带重新登录指引，**不追加**「请稍后重试」；
/// 其余保持现有口径（401/403 → 临时凭证提示，其他 → 请稍后重试）。
fn claim_failure_message(error_text: String) -> String {
    if is_token_expired_error(&error_text) {
        return error_text;
    }
    let hint = if error_text.contains("HTTP 401") || error_text.contains("HTTP 403") {
        "临时凭证已过期：请打开并登录 OfficeAce 桌面端后重试（签到复用桌面端登录态，凭证约 2 小时有效）"
    } else {
        "请稍后重试"
    };
    format!("{error_text}；{hint}")
}

/// 执行一轮签到（当前本机登录账号；单账号）。
pub fn run_checkin_round(state: &AppState, emit: &mut dyn FnMut(&Value)) -> Value {
    emit(&json!({ "type": "start", "total": 1 }));
    let mut ok = 0i64;
    let mut already = 0i64;
    let mut failed = 0i64;
    let (status, message, reward, uid, name) = match local_account() {
        Ok(Some((account, session_expires_at))) => {
            // 会话/凭证过期预检：过期直接落 fail（含过期时刻与指引），
            // 不再向上游发注定失败的请求（此前会 400 APIG.0602）
            if let Some(reason) = login_state_expired_reason(&account, session_expires_at.as_deref()) {
                failed = 1;
                (
                    "fail".to_string(),
                    reason,
                    None,
                    account.user_id.clone(),
                    account.name.clone(),
                )
            } else {
                let before = fetch_subscription(&account).ok();
                match claim_bonus(&account) {
                    Ok(claim) => {
                        let gained = before.as_ref().map(|b| gained_between(b, &claim));
                        let reward = gained.map(|v| v.floor()).filter(|v| *v > 0.0);
                        if let Some(points) = reward {
                            ok = 1;
                            (
                                "success".to_string(),
                                format!("签到成功 +{points} 积分"),
                                reward,
                                account.user_id.clone(),
                                account.name.clone(),
                            )
                        } else {
                            already = 1;
                            (
                                "already".to_string(),
                                "今日奖励已领取（上游幂等返回，无新增积分）".to_string(),
                                None,
                                account.user_id.clone(),
                                account.name.clone(),
                            )
                        }
                    }
                    Err(e) => {
                        failed = 1;
                        (
                            "fail".to_string(),
                            claim_failure_message(e),
                            None,
                            account.user_id.clone(),
                            account.name.clone(),
                        )
                    }
                }
            }
        }
        Ok(None) => {
            failed = 1;
            (
                "fail".to_string(),
                "未检测到 OfficeAce 本机登录态：请先安装并登录 OfficeAce 桌面端".to_string(),
                None,
                String::new(),
                String::new(),
            )
        }
        Err(e) => {
            failed = 1;
            (
                "fail".to_string(),
                e,
                None,
                String::new(),
                String::new(),
            )
        }
    };

    emit(&json!({
        "type": "account",
        "index": 1,
        "user_id": uid.clone(),
        "name": name.clone(),
        "status": status.clone(),
        "message": message.clone(),
        "reward": reward,
    }));

    let now = chrono::Local::now();
    let record = OfficeAceCheckinRecord {
        date: now.format("%Y-%m-%d").to_string(),
        time: now.format("%H:%M:%S").to_string(),
        time_ms: now.timestamp_millis(),
        user_id: if uid.is_empty() { "officeace-local".to_string() } else { uid },
        name,
        status,
        message,
        reward,
    };
    if let Ok(value) = serde_json::to_value(&record) {
        if let Err(e) =
            crate::store::docs::officeace_checkin_results_upsert(&crate::store::db(&state.data_dir), &value)
        {
            crate::fs_utils::app_log(&state.data_dir, &format!("OfficeAce 签到结果落库失败：{e}"));
        }
    }
    if failed > 0 {
        crate::fs_utils::app_log(
            &state.data_dir,
            &format!("OfficeAce 签到失败：{}", record.message),
        );
    }

    let done = json!({
        "type": "done",
        "ok": ok,
        "already": already,
        "failed": failed,
    });
    emit(&done);
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ms_scales_seconds_and_keeps_millis() {
        assert_eq!(normalize_ms(0), 0);
        assert_eq!(normalize_ms(1_763_300_000), 1_763_300_000_000);
        assert_eq!(normalize_ms(1_763_300_000_000), 1_763_300_000_000);
    }

    #[test]
    fn account_from_state_reads_gateway_and_credentials() {
        let state = json!({
            "providerState": {
                "userId": "org:user",
                "userName": "测试账号",
                "modelInfo": {
                    "model_api_url_base": "https://modelgw-0006.example.com/v1"
                },
                "credential": {
                    "access": "AK",
                    "secret": "SK",
                    "sts_token": "STS",
                    "project_id": "PID",
                    "expires_at": "2026-11-16T17:00:00Z"
                }
            }
        });
        let account = account_from_state(&state).expect("应能解析");
        assert_eq!(account.gateway_host, "modelgw-0006.example.com");
        assert_eq!(account.control.access_key_id, "AK");
        assert_eq!(account.control.project_id, "PID");
        assert!(account.control.expires_at_ms > 0);
    }

    #[test]
    fn gained_between_uses_remaining_delta() {
        // points = 累计额度，current_value = 已用；剩余 = points - current_value
        let before = json!({ "bonus_skus": [
            { "activity_id": "daily", "points": "100", "current_value": "100" }
        ]});
        let after = json!({ "bonus_skus": [
            { "activity_id": "daily", "points": "200", "current_value": "100" }
        ]});
        assert_eq!(gained_between(&before, &after), 100.0);
        // 首次见到的活动按 remaining 计，不按 total 虚高
        let fresh = json!({ "bonus_skus": [
            { "activity_name": "新人礼", "points": "500", "current_value": "300" }
        ]});
        assert_eq!(gained_between(&json!({}), &fresh), 200.0);
    }

    #[test]
    fn reject_error_payload_flags_business_code() {
        assert!(reject_error_payload(&json!({ "code": 1005 })).is_err());
        assert!(reject_error_payload(&json!({ "error": "denied" })).is_err());
        assert!(reject_error_payload(&json!({ "bonus_skus": [] })).is_ok());
    }

    #[test]
    fn canonical_uri_forces_trailing_slash() {
        assert_eq!(canonical_uri("/v1/subscription/bonus/claim"), "/v1/subscription/bonus/claim/");
        assert_eq!(canonical_uri("/"), "/");
    }

    fn account_with_sts(expires_at_ms: i64) -> LocalAccount {
        LocalAccount {
            user_id: "org:user".into(),
            name: "测试账号".into(),
            session_expires_at: None,
            gateway_host: "modelgw.example.com".into(),
            control: ControlCredential {
                access_key_id: "AK".into(),
                secret_access_key: "SK".into(),
                security_token: "STS".into(),
                project_id: "PID".into(),
                expires_at_ms,
            },
        }
    }

    #[test]
    fn login_state_expired_reason_flags_expired_session_with_time_and_hint() {
        // 场景还原：本机 session expiresAt = 2026-11-17T15:43:43Z（已过期）
        let expired = parse_time_ms("2026-11-17T15:43:43Z").expect("应能解析");
        let account = account_with_sts(now_ms() + 60 * 60 * 1000);
        let reason = login_state_expired_reason(&account, Some("2026-11-17T15:43:43Z"))
            .expect("会话已过期应拦截");
        assert!(reason.contains("OfficeAce 登录态已于"));
        assert!(reason.contains(&local_time_text(expired)));
        assert!(reason.contains(RELOGIN_HINT));
        assert!(!reason.contains("请稍后重试"));

        // 会话未过期、STS 未过期 → 不拦截
        {
            use chrono::TimeZone;
            let future = chrono::Local
                .timestamp_millis_opt(now_ms() + 60 * 60 * 1000)
                .single()
                .expect("应能构造未来时刻");
            assert!(login_state_expired_reason(&account, Some(&future.to_rfc3339())).is_none());
        }
        assert!(login_state_expired_reason(&account, None).is_none());

        // 字段缺失 / 无法解析时不判过期（不得误拦新会话）
        assert!(login_state_expired_reason(&account, Some("")).is_none());
        assert!(login_state_expired_reason(&account, Some("not-a-time")).is_none());
    }

    #[test]
    fn login_state_expired_reason_flags_expired_sts_token() {
        // 会话字段缺失但 STS 已过期：同样预检拦截（含过期时刻与指引）
        let account = account_with_sts(now_ms() - 60 * 1000);
        let reason =
            login_state_expired_reason(&account, None).expect("STS 已过期应拦截");
        assert!(reason.contains("临时凭证已于"));
        assert!(reason.contains(&local_time_text(now_ms() - 60 * 1000)));
        assert!(reason.contains("刷新登录态"));
    }

    #[test]
    fn upstream_failure_classifies_apig_token_expiry_400() {
        let raw = r#"{"error_msg":"Bad request: the security token has expired","error_code":"APIG.0602","request_id":"a605aec5094d152e5e9dbed4cdb455b8"}"#;
        assert!(is_token_expired_response(400, raw));
        let e = upstream_failure("签到请求", 400, raw);
        assert!(is_token_expired_error(&e));
        assert!(e.contains("已过期"));
        assert!(e.contains(RELOGIN_HINT));
        assert!(!e.contains("请稍后重试"));

        // 大小写不敏感
        assert!(is_token_expired_response(400, r#"{"error_code":"apig.0602"}"#));
        assert!(is_token_expired_response(
            400,
            r#"{"error_msg":"Security Token Has Expired"}"#
        ));

        // 非 400 状态码即使带同标记也不归类（401/403 走原有临时凭证口径）
        assert!(!is_token_expired_response(401, raw));
        assert!(!is_token_expired_response(500, raw));
    }

    #[test]
    fn upstream_failure_keeps_generic_copy_for_other_400_codes() {
        // 其他 400 错误码：不归类，仍走通用文案（请稍后重试）
        let raw = r#"{"error_code":"OfficeAce.11020001","error_msg":"body must be {}"}"#;
        assert!(!is_token_expired_response(400, raw));
        let e = upstream_failure("签到请求", 400, raw);
        assert!(!is_token_expired_error(&e));
        assert!(e.contains("HTTP 400"));
        assert!(e.contains("OfficeAce.11020001"));
        assert_eq!(claim_failure_message(e.clone()), format!("{e}；请稍后重试"));
    }

    #[test]
    fn claim_failure_message_routes_expired_and_generic_errors() {
        // 归类文案（经 upstream_failure 组合，自带标记）：不再追加「请稍后重试」
        let raw = r#"{"error_msg":"Bad request: the security token has expired","error_code":"APIG.0602"}"#;
        let expired = upstream_failure("签到请求", 400, raw);
        assert!(is_token_expired_error(&expired));
        assert_eq!(claim_failure_message(expired.clone()), expired);
        assert!(expired.contains(RELOGIN_HINT));
        assert!(!expired.contains("请稍后重试"));

        // 401/403：保持现有「临时凭证已过期」口径
        let unauthorized =
            "签到请求失败（HTTP 401）：unauthorized".to_string();
        let message = claim_failure_message(unauthorized);
        assert!(message.contains("临时凭证已过期"));
        assert!(!message.contains("请稍后重试"));
    }

    #[test]
    fn local_time_text_renders_local_calendar_shape() {
        let ms = parse_time_ms("2026-11-17T15:43:43Z").expect("应能解析");
        let text = local_time_text(ms);
        assert_eq!(text.len(), 16, "YYYY-MM-DD HH:MM 共 16 字符");
        assert_eq!(&text[4..5], "-");
        assert_eq!(&text[7..8], "-");
        assert_eq!(&text[10..11], " ");
        assert_eq!(&text[13..14], ":");
        // 超出可表示范围回退为原始毫秒值
        assert_eq!(local_time_text(i64::MAX), i64::MAX.to_string());
    }
}