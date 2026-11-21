//! CodeArts 凭证通道 B：本机浏览器 OAuth（授权码 + PKCE + DPoP）。
//!
//! 协议（VENU-5 §2.3 定稿，复刻自官方 `huaweicloud.authentication` 插件）：
//! 1. 生成 PKCE 对 + DPoP P-256 密钥对，127.0.0.1 随机端口（≥10000，门户强制下限）回调；
//! 2. 打开 `{portal}/authorize?...&client_id=codearts-agent&port=<port>&code_challenge=<S256>&...`；
//! 3. 浏览器登录后回调 `GET http://127.0.0.1:<port>/oauth/callback?code=<authorizationCode>`；
//! 4. 换凭据 `POST {sts}/v1/oauth2/tokens`（form，DPoP 头）→
//!    `{ credentials: { access, secret, securitytoken, expiration }, refresh_token }`。
//!
//! 与桌面端客户端的关系：本通道换取的是**本应用自建的独立授权链**（自有 DPoP
//! 密钥对），刷新只消费自己的 refresh_token，不影响桌面端登录（对齐 MiniMax
//! 设备码流的独立链设计）。refresh_token 单次轮换：刷新互斥 + 成功立即落库 +
//! 失效熔断（tasks/codearts_common::refresh_and_persist）。
//!
//! 浏览器授权需真人交互：协议实现 + PKCE/DPoP 结构有单测，端到端属「未实测」
//! 项（见 VENU-37 交付说明）；手工粘贴回调 URL 兜底（commands/codearts/credentials）。

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::codearts_common::{excerpt, random_hex, STS_BASE};

/// OAuth client 标识（门户公共 client，非机密）
pub const CLIENT_ID: &str = "codearts-agent";
/// 授权门户
pub const AUTHORIZE_PORTAL: &str = "https://codearts.huaweicloud.com/portal/authorize";
/// 换票端点
pub const TOKEN_ENDPOINT: &str = "/v1/oauth2/tokens";
/// 回调路径（与门户约定的 redirect_uri 组成部分）
pub const CALLBACK_PATH: &str = "/oauth/callback";
/// 门户强制回调端口下限
pub const MIN_PORT: u16 = 10000;
/// 授权会话有效期（浏览器交互窗口；超时丢弃）
pub const SESSION_TTL_MS: i64 = 10 * 60 * 1000;

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// PKCE code_verifier（48 字符 unreserved 集；S256 口径）
pub fn generate_verifier() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    // 96 个 hex 字符 → 48 字节 → 48 字符（模映射，均匀性满足 PKCE 熵要求）
    let bytes_hex = random_hex(96);
    let mut out = String::new();
    for chunk in bytes_hex.as_bytes().chunks(2) {
        let v = usize::from_str_radix(std::str::from_utf8(chunk).unwrap_or("0"), 16).unwrap_or(0);
        out.push(ALPHABET[v % ALPHABET.len()] as char);
    }
    out
}

/// S256 code_challenge（BASE64URL(SHA256(verifier))，无填充）
pub fn code_challenge_s256(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

/// 授权 URL 组装（参数按 VENU-5 §2.3；theme 固定 1、locale zh-cn）
pub fn authorize_url(port: u16, challenge: &str, ticket_id: &str) -> String {
    format!(
        "{AUTHORIZE_PORTAL}?theme=1&locale=zh-cn&uri_scheme=codearts-agent&client_id={CLIENT_ID}&port={port}&code_challenge={challenge}&code_challenge_method=SHA-256&ticket_id={ticket_id}&plugin-name=snap_AIIDE&plugin-version={}",
        super::codearts_common::PLUGIN_VERSION
    )
}

// ── DPoP（ES256 JWS proof，RFC 9449 口径）─────────────────────────────────

/// DPoP P-256 密钥对（私钥标量 hex 存凭证表；公钥坐标随 proof 头下发）
#[derive(Clone)]
pub struct DpopKey {
    /// 私钥标量（32 字节 hex）
    pub private_hex: String,
    /// JWK 公钥部分（kty/crv/x/y，b64url 坐标）
    pub public_jwk: Value,
}

impl DpopKey {
    /// 生成新密钥对（随机源 = 既有 rand 依赖，构造 SigningKey 前先取 32 随机字节）
    pub fn generate() -> Result<DpopKey, String> {
        let bytes_hex = random_hex(64);
        let mut bytes = [0u8; 32];
        for (i, chunk) in bytes_hex.as_bytes().chunks(2).enumerate() {
            bytes[i] = u8::from_str_radix(std::str::from_utf8(chunk).unwrap_or("0"), 16).unwrap_or(0);
        }
        let secret = p256::SecretKey::from_bytes(&p256::FieldBytes::from(bytes))
            .map_err(|e| format!("DPoP 密钥生成失败: {e}"))?;
        use p256::elliptic_curve::sec1::ToEncodedPoint as _;
        let encoded = secret.public_key().to_encoded_point(false);
        // 非压缩点：04 || x(32) || y(32)
        if encoded.len() != 65 {
            return Err("DPoP 公钥编码形态异常".into());
        }
        Ok(DpopKey {
            private_hex: bytes_hex,
            public_jwk: json!({
                "kty": "EC",
                "crv": "P-256",
                "x": b64url(&encoded.as_bytes()[1..33]),
                "y": b64url(&encoded.as_bytes()[33..65]),
            }),
        })
    }

    fn signing_key(&self) -> Result<p256::ecdsa::SigningKey, String> {
        let mut bytes = [0u8; 32];
        for (i, chunk) in self.private_hex.as_bytes().chunks(2).enumerate() {
            bytes[i] = u8::from_str_radix(std::str::from_utf8(chunk).unwrap_or("0"), 16).unwrap_or(0);
        }
        p256::ecdsa::SigningKey::from_bytes(&p256::FieldBytes::from(bytes))
            .map_err(|e| format!("DPoP 私钥解析失败: {e}"))
    }

    /// ES256 JWS proof（compact 序列化：header.payload.signature）
    pub fn proof(&self, htm: &str, htu: &str) -> Result<String, String> {
        let header = b64url(
            json!({ "typ": "dpop+jwt", "alg": "ES256", "jwk": self.public_jwk })
                .to_string()
                .as_bytes(),
        );
        let payload = b64url(
            json!({
                "jti": random_hex(24),
                "htm": htm,
                "htu": htu,
                "iat": chrono::Utc::now().timestamp(),
            })
            .to_string()
            .as_bytes(),
        );
        let signing_input = format!("{header}.{payload}");
        use p256::ecdsa::signature::Signer;
        let key = self.signing_key()?;
        let sig: p256::ecdsa::Signature = key.sign(signing_input.as_bytes());
        Ok(format!("{signing_input}.{}", b64url(&sig.to_bytes())))
    }
}

/// 从凭证表记录重建 DPoP 密钥（dpop_private_jwk 存 private_hex）
pub fn dpop_key_from_private_hex(private_hex: &str) -> Result<DpopKey, String> {
    if private_hex.trim().is_empty() {
        return Err("无 DPoP 密钥材料（oauth 通道凭证才可刷新）".into());
    }
    Ok(DpopKey {
        private_hex: private_hex.trim().to_string(),
        public_jwk: json!(null), // 重建时仅 proof 签名需要私钥；公钥从私钥推导
    })
}

// ── 换票（授权码 / refresh_token 两个 grant 共用端点与 DPoP 头）────────────

/// 换票成功产物（STS 三元组 + 刷新链）
pub struct ExchangedCredential {
    pub access: String,
    pub secret: String,
    pub security_token: String,
    pub refresh_token: String,
    pub expires_at_ms: i64,
}

/// DPoP 头重建公钥：私钥推导公钥坐标（凭证表只存 private_hex 省空间）
fn dpop_key_with_public_jwk(mut key: DpopKey) -> Result<DpopKey, String> {
    let signing = key.signing_key()?;
    let secret = p256::SecretKey::from(signing);
    use p256::elliptic_curve::sec1::ToEncodedPoint as _;
    let encoded = secret.public_key().to_encoded_point(false);
    if encoded.len() != 65 {
        return Err("DPoP 公钥编码形态异常".into());
    }
    key.public_jwk = json!({
        "kty": "EC",
        "crv": "P-256",
        "x": b64url(&encoded.as_bytes()[1..33]),
        "y": b64url(&encoded.as_bytes()[33..65]),
    });
    Ok(key)
}

/// POST {sts}/v1/oauth2/tokens（form-urlencoded + DPoP 头）。
/// 返回 (http_status, 解析 JSON, 原文)。
fn token_request(form: &str, dpop: &DpopKey) -> (u16, Option<Value>, String) {
    let proof = match dpop_key_with_public_jwk(dpop.clone()) {
        Ok(k) => match k.proof("POST", &format!("{STS_BASE}{TOKEN_ENDPOINT}")) {
            Ok(p) => p,
            Err(e) => return (0, None, e),
        },
        Err(e) => return (0, None, e),
    };
    let url = format!("{STS_BASE}{TOKEN_ENDPOINT}");
    let agent = crate::tasks::http_agent(20);
    let response = agent
        .post(&url)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .set("DPoP", &proof)
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

/// 解析换票响应体（credentials 三元组 + refresh_token）
fn parse_token_response(v: &Value) -> Result<ExchangedCredential, String> {
    let creds = v.get("credentials").cloned().unwrap_or(Value::Null);
    let s = |base: &Value, k: &str| base.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let access = s(&creds, "access");
    let secret = s(&creds, "secret");
    if access.is_empty() || secret.is_empty() {
        return Err("换票响应缺少 credentials.access/secret".into());
    }
    let expiration = creds
        .get("expiration")
        .cloned()
        .unwrap_or(Value::Null);
    let expires_at_ms = super::codearts_common::expiry_to_ms(&expiration).unwrap_or(0);
    Ok(ExchangedCredential {
        access,
        secret,
        security_token: s(&creds, "securitytoken"),
        refresh_token: s(v, "refresh_token"),
        expires_at_ms,
    })
}

/// 授权码换票（通道 B 第 4 步）
pub fn exchange_code(code: &str, verifier: &str, redirect_uri: &str, dpop: &DpopKey) -> Result<ExchangedCredential, String> {
    let form = format!(
        "client_id={CLIENT_ID}&code={}&code_verifier={}&grant_type=authorization_code&redirect_uri={}",
        urlencode(code),
        urlencode(verifier),
        urlencode(redirect_uri),
    );
    let (status, parsed, raw) = token_request(&form, dpop);
    if status != 200 {
        return Err(format!("授权码换票失败（HTTP {status}）：{}", excerpt(&raw)));
    }
    let v = parsed.ok_or_else(|| format!("换票响应不是 JSON：{}", excerpt(&raw)))?;
    parse_token_response(&v)
}

/// refresh_token 刷新（仅 oauth 通道独立链；规格：grant + DPoP 头 + 原 code_verifier 材料）
pub fn refresh_token_grant(rec: &super::codearts_common::StsCredential) -> Result<ExchangedCredential, String> {
    let dpop = dpop_key_from_private_hex(&rec.dpop_private_jwk)?;
    let form = format!(
        "client_id={CLIENT_ID}&grant_type=refresh_token&refresh_token={}&code_verifier={}",
        urlencode(&rec.refresh_token),
        urlencode(&rec.code_verifier),
    );
    let (status, parsed, raw) = token_request(&form, &dpop);
    if status == 400 || status == 401 {
        // invalid_grant：链已作废（终态）——调用方落 needs_relogin 熔断
        return Err(format!("授权链已失效（refresh 一次性轮换被拒绝，HTTP {status}），请重新授权"));
    }
    if status != 200 {
        return Err(format!("凭证刷新失败（HTTP {status}）：{}", excerpt(&raw)));
    }
    let v = parsed.ok_or_else(|| format!("刷新响应不是 JSON：{}", excerpt(&raw)))?;
    parse_token_response(&v)
}

fn urlencode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_verifier_shape_and_challenge() {
        let v = generate_verifier();
        assert_eq!(v.len(), 48);
        assert!(v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')));
        let challenge = code_challenge_s256(&v);
        assert_eq!(challenge.len(), 43);
        assert!(!challenge.contains('='));
        assert!(!challenge.contains('+') && !challenge.contains('/'));
        // 同输入恒同 challenge；异输入不同
        assert_eq!(challenge, code_challenge_s256(&v));
        assert_ne!(challenge, code_challenge_s256(&generate_verifier()));
    }

    #[test]
    fn dpop_key_roundtrip_and_proof_shape() {
        let key = DpopKey::generate().expect("密钥生成");
        assert_eq!(key.private_hex.len(), 64);
        assert_eq!(key.public_jwk.get("kty").and_then(Value::as_str), Some("EC"));
        assert_eq!(key.public_jwk.get("crv").and_then(Value::as_str), Some("P-256"));
        // x/y 为 b64url 坐标（各 43 字符 = 32 字节无填充）
        assert_eq!(key.public_jwk.get("x").and_then(Value::as_str).map(str::len), Some(43));
        let proof = key.proof("POST", &format!("{STS_BASE}{TOKEN_ENDPOINT}")).expect("proof");
        let parts: Vec<&str> = proof.split('.').collect();
        assert_eq!(parts.len(), 3, "JWS compact 三段");
        // header 可解且含 typ/alg/jwk
        let header = String::from_utf8(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(parts[0])
                .unwrap(),
        )
        .unwrap();
        assert!(header.contains("dpop+jwt") && header.contains("ES256"));
        let payload = String::from_utf8(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(parts[1])
                .unwrap(),
        )
        .unwrap();
        assert!(payload.contains("\"htm\":\"POST\"") && payload.contains("\"jti\""));
        // 重建路径：私钥 hex → 带公钥的 key → 仍可签
        let rebuilt = dpop_key_with_public_jwk(dpop_key_from_private_hex(&key.private_hex).unwrap()).unwrap();
        assert_eq!(rebuilt.public_jwk, key.public_jwk, "公钥坐标应从私钥稳定推导");
        assert!(rebuilt.proof("POST", "https://x").is_ok());
    }

    #[test]
    fn authorize_url_contains_contract_params() {
        let url = authorize_url(12345, "CHAL", "abcd1234");
        assert!(url.starts_with(AUTHORIZE_PORTAL));
        assert!(url.contains("client_id=codearts-agent"));
        assert!(url.contains("port=12345"));
        assert!(url.contains("code_challenge=CHAL"));
        assert!(url.contains("code_challenge_method=SHA-256"));
        assert!(url.contains("ticket_id=abcd1234"));
        assert!(url.contains("plugin-name=snap_AIIDE"));
        assert!(url.contains("uri_scheme=codearts-agent"));
    }

    #[test]
    fn parse_token_response_reads_triple_and_refresh() {
        let v = json!({
            "credentials": { "access": "AK", "secret": "SK", "securitytoken": "ST", "expiration": "2026-12-31T00:00:00Z" },
            "refresh_token": "rt"
        });
        let c = parse_token_response(&v).unwrap();
        assert_eq!(c.access, "AK");
        assert_eq!(c.secret, "SK");
        assert_eq!(c.security_token, "ST");
        assert_eq!(c.refresh_token, "rt");
        assert_eq!(c.expires_at_ms, 1798675200000); // 2026-12-31T00:00:00Z
        // 缺 credentials → Err
        assert!(parse_token_response(&json!({"refresh_token": "rt"})).is_err());
        // 缺 refresh_token 容忍（空串）
        let c2 = parse_token_response(&json!({"credentials": {"access": "a", "secret": "b"}})).unwrap();
        assert_eq!(c2.refresh_token, "");
    }
}
