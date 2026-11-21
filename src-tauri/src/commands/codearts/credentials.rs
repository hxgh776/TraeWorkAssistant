//! CodeArts 凭证通道命令：桌面端登录态扫描/导入（通道 A）、手工 AK/SK（通道 C）、
//! 浏览器 OAuth（通道 B：授权码 + PKCE + DPoP，本机回环回调 + 手工粘贴兜底）。
//!
//! 通道 A（主，旧 3.9.3 同款）：只读 `%APPDATA%\codearts-agent` 家族目录的
//! `Local State`（DPAPI 主密钥）+ `User\globalStorage\state.vscdb`（secret:// 键），
//! 解密出华为云 STS 三元组建档入池。**绝不写回该库**；过期会话拒绝入池（导入即
//! needs_relogin 没有意义），提示在码道客户端重新登录后重扫。
//!
//! 通道 C（兜底）：手工 IAM AK/SK（不过期，高级用户）；与旧 3.9.3 命令
//! `codearts_account_import_manual`（params accessKey/secretKey/securityToken/expiresAtMs）同名同参。
//!
//! 通道 B：本应用自建独立授权链（自有 DPoP 密钥），刷新只消费自己的
//! refresh_token，不影响桌面端登录。回环回调 5 分钟空闲自动收口；
//! 浏览器授权需真人交互——协议有单测，端到端属「未实测」（VENU-37 交付说明）。

use serde_json::{json, Value};
use tauri::State;

use super::common::{account_view, with_pool};
use crate::state::AppState;
use crate::tasks::codearts_common::{account_id, fetch_caller_identity, read_local_session, StsCredential};
use crate::tasks::codearts_oauth::{
    authorize_url, code_challenge_s256, dpop_key_from_private_hex, exchange_code, generate_verifier,
    MIN_PORT, SESSION_TTL_MS,
};

/// 回调授权码槽（start 与 poll/complete 之间传递；start 重建时整体替换）
type CallbackCode = std::sync::Arc<std::sync::Mutex<Option<String>>>;

// ── 通道 A：本机登录态 ───────────────────────────────────────────────────

/// 扫描本机 CodeArts 桌面端登录态（只读；脱敏摘要，不含 AK/SK）。
#[tauri::command(async)]
pub fn codearts_local_scan() -> Value {
    match read_local_session() {
        Some(session) => {
            let expired =
                session.expires_at_ms > 0 && session.expires_at_ms <= chrono::Utc::now().timestamp_millis();
            json!({
                "found": true,
                "data_dir": session.data_dir,
                "identity_name": session.identity_name,
                "expires_at_ms": session.expires_at_ms,
                "expired": expired,
            })
        }
        None => json!({
            "found": false,
            "reason": "未找到 CodeArts 客户端数据目录，或 state.vscdb 中无可解析的华为云登录态（可能未登录）",
        }),
    }
}

/// 导入本机登录态（幂等：同凭证稳定同 id，重复 = 更新）。过期会话拒绝入池。
#[tauri::command(async)]
pub fn codearts_local_import(state: State<'_, AppState>) -> Result<Value, String> {
    #[cfg(windows)]
    {
        let Some(session) = read_local_session() else {
            return Err("state.vscdb 中未找到可解析的华为云登录态（可能未登录）——请先在码道客户端/VS Code 插件登录，或改用手工 AK/SK 导入".into());
        };
        let expired = session.expires_at_ms > 0
            && session.expires_at_ms <= chrono::Utc::now().timestamp_millis();
        if expired {
            return Err("STS 凭据已过期，请在码道客户端/插件重新登录后扫描导入".into());
        }
        let cred = session.credential("local_import".into());
        import_credential(&state, cred)
    }
    #[cfg(not(windows))]
    {
        let _ = &state;
        Err("桌面端登录态导入仅支持 Windows（DPAPI + AES-GCM）".into())
    }
}

// ── 通道 C：手工 AK/SK ───────────────────────────────────────────────────

/// 手工导入 IAM AK/SK（旧 3.9.3 命令同名同参；expiresAtMs 0/缺省 = 永久不过期）。
#[tauri::command(async)]
pub fn codearts_account_import_manual(
    state: State<'_, AppState>,
    #[allow(non_snake_case)] accessKey: String,
    #[allow(non_snake_case)] secretKey: String,
    #[allow(non_snake_case)] securityToken: Option<String>,
    #[allow(non_snake_case)] expiresAtMs: Option<i64>,
) -> Result<Value, String> {
    let ak = accessKey.trim();
    let sk = secretKey.trim();
    if ak.is_empty() || sk.is_empty() {
        return Err("AccessKey 与 SecretKey 均不能为空".into());
    }
    let cred = StsCredential {
        access_key: ak.to_string(),
        secret_key: sk.to_string(),
        security_token: securityToken.unwrap_or_default().trim().to_string(),
        expires_at_ms: expiresAtMs.unwrap_or(0),
        credential_source: "aksk".into(),
        session_seed: crate::tasks::codearts_common::random_hex(24),
        ..Default::default()
    };
    import_credential(&state, cred)
}

// ── 建档入池（三通道共用；caller-identity 校验归属 + 命名）────────────────

/// 凭证 → 建档入池（身份查询失败降级会话内嵌身份/AK 前缀命名，不阻断导入）。
/// 返回账号视图（脱敏）。
fn import_credential(state: &AppState, cred: StsCredential) -> Result<Value, String> {
    let id = account_id(&cred.access_key, &cred.secret_key);
    // 身份归属：caller-identity 优先（端点/签名问题降级，不阻断导入）
    let identity = fetch_caller_identity(&cred).ok();
    let display = identity
        .as_ref()
        .map(|i| i.display_name())
        .unwrap_or_else(|| format!("华为云 {}", &cred.access_key[..cred.access_key.len().min(6)]));
    let user_id = identity
        .map(|i| if i.user_id.is_empty() { i.domain_id } else { i.user_id })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("ak-{}", &id[3..]));
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let merged = with_pool(state, |pool| {
        // 池结构兜底：无 accounts 数组时重建
        if !pool.get("accounts").map(Value::is_array).unwrap_or(false) {
            pool["accounts"] = json!([]);
        }
        let accounts = pool
            .get_mut("accounts")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| "账号池结构异常".to_string())?;
        let mut was_merged = false;
        if let Some(acc) = accounts
            .iter_mut()
            .find(|a| a.get("id").and_then(Value::as_str) == Some(id.as_str()))
        {
            // 同账号合并：更新身份字段与凭证来源，保留改名 / 备注 / 积分缓存
            if acc.get("name").and_then(Value::as_str).map(|n| n.is_empty()).unwrap_or(true) {
                acc["name"] = json!(display);
            }
            acc["user_id"] = json!(user_id);
            acc["credential_source"] = json!(cred.credential_source);
            acc["needs_relogin"] = json!(false);
            acc["relogin_reason"] = json!("");
            acc["auth_saved_at"] = json!(now);
            was_merged = true;
        } else {
            accounts.push(json!({
                "id": id,
                "user_id": user_id,
                "name": display,
                "note": "",
                "credential_source": cred.credential_source,
                "added_at": now,
                "auth_saved_at": now,
                "needs_relogin": false,
                "relogin_reason": "",
            }));
        }
        Ok(was_merged)
    })?;
    // 凭证落库（绝不进日志）
    crate::store::docs::codearts_token_put(&crate::store::db(&state.data_dir), &id, &cred.to_json())?;
    crate::fs_utils::app_log(
        &state.data_dir,
        &format!(
            "[codearts] 账号{}（{}）凭证已保存（来源 {}）",
            if merged { "更新" } else { "导入" },
            display,
            cred.credential_source
        ),
    );
    let mut view = account_view(
        &crate::store::docs::codearts_pool_load(&crate::store::db(&state.data_dir))
            .get("accounts")
            .and_then(Value::as_array)
            .and_then(|arr| {
                arr.iter()
                    .find(|a| a.get("id").and_then(Value::as_str) == Some(id.as_str()))
                    .cloned()
            })
            .ok_or_else(|| "建档后读取账号失败".to_string())?,
        Some(&cred.to_json()),
    );
    view["_merged"] = json!(merged);
    Ok(view)
}

// ── 通道 B：浏览器 OAuth（授权码 + PKCE + DPoP）──────────────────────────

/// 内存中的 OAuth 授权会话（单会话；重复 start 覆盖旧的）。
/// 授权码经 CODE_SLOT 传递（回环线程写入），不入会话。
struct OauthSession {
    verifier: String,
    dpop_private_hex: String,
    redirect_uri: String,
    port: u16,
    created_ms: i64,
}

static OAUTH_SESSION: std::sync::Mutex<Option<OauthSession>> = std::sync::Mutex::new(None);

fn session_slot() -> std::sync::MutexGuard<'static, Option<OauthSession>> {
    OAUTH_SESSION.lock().unwrap_or_else(|p| p.into_inner())
}

/// 回环回调句柄（shutdown 信号 + 线程句柄；oauth_start 建立会被话覆盖/超时收口）
struct LoopbackGuard {
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

static LOOPBACK: std::sync::Mutex<Option<LoopbackGuard>> = std::sync::Mutex::new(None);

/// 停掉进行中的回环监听（幂等）
fn stop_loopback() {
    let guard = LOOPBACK.lock().unwrap_or_else(|p| p.into_inner()).take();
    if let Some(g) = guard {
        g.shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
        // 不 join：监听线程阻塞在 accept，进程退出时随线程回收；shutdown 位防新回调
    }
}

/// 在 127.0.0.1 随机端口（≥10000，门户强制下限）起短生命周期回调监听，
/// 捕获 `/oauth/callback?code=...` 后写入代码槽。5 分钟空闲自动收口。
/// 返回实际绑定端口（0 = 分配失败）。
fn start_loopback(code_slot: CallbackCode) -> u16 {
    use rand::Rng;
    use std::io::{Read, Write};
    // 随机端口尝试若干次（bind 失败换端口）
    for _ in 0..20 {
        let candidate: u16 = rand::thread_rng().gen_range(MIN_PORT..u16::MAX);
        let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", candidate)) else {
            continue;
        };
        let port = listener
            .local_addr()
            .map(|a| a.port())
            .unwrap_or(candidate);
        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shutdown_thread = shutdown.clone();
        let handle = std::thread::Builder::new()
            .name("codearts-oauth-loopback".into())
            .spawn(move || {
                // 5 分钟空闲收口（与授权会话 TTL 一致）
                let _ = listener.set_nonblocking(true);
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(SESSION_TTL_MS as u64);
                loop {
                    if shutdown_thread.load(std::sync::atomic::Ordering::SeqCst)
                        || std::time::Instant::now() > deadline
                    {
                        return;
                    }
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                            let mut buf = [0u8; 2048];
                            let read = stream.read(&mut buf).unwrap_or(0);
                            let req = String::from_utf8_lossy(&buf[..read]).to_string();
                            let first_line = req.lines().next().unwrap_or("").to_string();
                            // "GET /oauth/callback?code=... HTTP/1.1"
                            let path = first_line.split_whitespace().nth(1).unwrap_or("").to_string();
                            let mut captured: Option<String> = None;
                            if let Some((path_part, query)) = path.split_once('?') {
                                if path_part == crate::tasks::codearts_oauth::CALLBACK_PATH {
                                    for pair in query.split('&') {
                                        if let Some((k, v)) = pair.split_once('=') {
                                            if k == "code" && !v.is_empty() {
                                                captured = Some(v.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                            let body = if captured.is_some() {
                                "<html><body><h3>授权完成</h3><p>请回到 AI Work 助手继续操作，本页可以关闭。</p></body></html>"
                            } else {
                                "<html><body><h3>未捕获授权码</h3><p>请回到 AI Work 助手改用「粘贴回调地址」。</p></body></html>"
                            };
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                body.len(),
                                body
                            );
                            let _ = stream.write_all(response.as_bytes());
                            if let Some(code) = captured {
                                *code_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(code);
                                return;
                            }
                        }
                        Err(_) => {
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                    }
                }
            });
        {
            let mut slot = LOOPBACK.lock().unwrap_or_else(|p| p.into_inner());
            *slot = Some(LoopbackGuard {
                shutdown,
                handle: handle.ok(),
            });
        }
        return port;
    }
    0
}

/// OAuth 授权起点：生成 PKCE + DPoP 密钥对，起回环回调，返回授权链接（前端打开浏览器）。
#[tauri::command(async)]
pub fn codearts_oauth_start() -> Result<Value, String> {
    // 重建会话（覆盖旧会话前先停旧监听）
    stop_loopback();
    let verifier = generate_verifier();
    let challenge = code_challenge_s256(&verifier);
    let dpop = crate::tasks::codearts_oauth::DpopKey::generate()?;
    let code_slot: CallbackCode = std::sync::Arc::new(std::sync::Mutex::new(None));
    let port = start_loopback(code_slot.clone());
    if port < MIN_PORT {
        return Err("本机回环回调端口分配失败，请重试".into());
    }
    let redirect_uri = format!("http://127.0.0.1:{port}{}", crate::tasks::codearts_oauth::CALLBACK_PATH);
    let ticket = crate::tasks::codearts_common::random_hex(32);
    let url = authorize_url(port, &challenge, &ticket);
    *session_slot() = Some(OauthSession {
        verifier,
        dpop_private_hex: dpop.private_hex,
        redirect_uri,
        port,
        created_ms: chrono::Utc::now().timestamp_millis(),
    });
    // 代码槽挂到会话外静态槽（poll 读取；会话覆盖时一并重置）
    *CODE_SLOT.lock().unwrap_or_else(|p| p.into_inner()) = Some(code_slot);
    Ok(json!({
        "authorize_url": url,
        "port": port,
        "expires_in_secs": SESSION_TTL_MS / 1000,
    }))
}

static CODE_SLOT: std::sync::Mutex<Option<CallbackCode>> = std::sync::Mutex::new(None);

/// OAuth 轮询单步（前端每 2s 调用）：
/// 返回 { status: pending | success | failed, message?, account? }。
#[tauri::command(async)]
pub fn codearts_oauth_poll(state: State<'_, AppState>) -> Result<Value, String> {
    let code = {
        let slot = CODE_SLOT.lock().unwrap_or_else(|p| p.into_inner());
        slot.as_ref()
            .and_then(|s| s.lock().unwrap_or_else(|p| p.into_inner()).clone())
    };
    let (verifier, dpop_hex, redirect_uri, expired) = {
        let slot = session_slot();
        let Some(s) = slot.as_ref() else {
            return Err("无进行中的 OAuth 授权，请先点「开始授权」".into());
        };
        let expired = chrono::Utc::now().timestamp_millis() > s.created_ms + SESSION_TTL_MS;
        (
            s.verifier.clone(),
            s.dpop_private_hex.clone(),
            s.redirect_uri.clone(),
            expired,
        )
    };
    if expired {
        stop_loopback();
        *session_slot() = None;
        return Ok(json!({ "status": "failed", "message": "授权已超时（10 分钟有效），请重新开始" }));
    }
    let Some(code) = code else {
        return Ok(json!({ "status": "pending" }));
    };
    // 拿到授权码 → 换票 → 建档
    stop_loopback();
    let dpop = dpop_key_from_private_hex(&dpop_hex)?;
    let exchanged = exchange_code(&code, &verifier, &redirect_uri, &dpop)
        .map_err(|e| {
            // 终态失败：清会话（授权码一次性）
            *session_slot() = None;
            e
        })?;
    let cred = StsCredential {
        access_key: exchanged.access,
        secret_key: exchanged.secret,
        security_token: exchanged.security_token,
        refresh_token: exchanged.refresh_token,
        code_verifier: verifier,
        dpop_private_jwk: dpop_hex,
        redirect_uri,
        expires_at_ms: exchanged.expires_at_ms,
        credential_source: "oauth".into(),
        session_seed: crate::tasks::codearts_common::random_hex(24),
    };
    let account = import_credential(&state, cred)?;
    *session_slot() = None;
    Ok(json!({ "status": "success", "account": account }))
}

/// 取消进行中的 OAuth 授权（无会话时静默成功）。
#[tauri::command]
pub fn codearts_oauth_cancel() {
    stop_loopback();
    *session_slot() = None;
    *CODE_SLOT.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// 手工粘贴回调地址兜底（回环未捕获时：把浏览器最终停在的地址整段粘贴回来）。
#[tauri::command(async)]
pub fn codearts_oauth_complete_url(state: State<'_, AppState>, url: String) -> Result<Value, String> {
    let code = url
        .split(&['?', '&'][..])
        .find_map(|pair| pair.strip_prefix("code="))
        .map(|c| c.split('&').next().unwrap_or("").to_string())
        .filter(|c| !c.is_empty())
        .ok_or_else(|| "回调地址中未找到 code 参数".to_string())?;
    let (verifier, dpop_hex, redirect_uri) = {
        let slot = session_slot();
        let Some(s) = slot.as_ref() else {
            return Err("无进行中的 OAuth 授权，请先点「开始授权」".into());
        };
        (s.verifier.clone(), s.dpop_private_hex.clone(), s.redirect_uri.clone())
    };
    stop_loopback();
    let dpop = dpop_key_from_private_hex(&dpop_hex)?;
    let exchanged = exchange_code(&code, &verifier, &redirect_uri, &dpop).map_err(|e| {
        *session_slot() = None;
        e
    })?;
    let cred = StsCredential {
        access_key: exchanged.access,
        secret_key: exchanged.secret,
        security_token: exchanged.security_token,
        refresh_token: exchanged.refresh_token,
        code_verifier: verifier,
        dpop_private_jwk: dpop_hex,
        redirect_uri,
        expires_at_ms: exchanged.expires_at_ms,
        credential_source: "oauth".into(),
        session_seed: crate::tasks::codearts_common::random_hex(24),
    };
    let account = import_credential(&state, cred)?;
    *session_slot() = None;
    Ok(json!({ "status": "success", "account": account }))
}

/// 凭证就绪预检（账号管理页「重新检测」：读凭证表判定可用性，不发网络）。
#[tauri::command]
pub fn codearts_credential_status(state: State<'_, AppState>, id: String) -> Value {
    let db = crate::store::db(&state.data_dir);
    let Some(rec) = crate::store::docs::codearts_token_get(&db, &id) else {
        return json!({ "found": false, "usable": false, "reason": "无凭证记录" });
    };
    let cred = StsCredential::from_json(&rec);
    let usable = cred.usable();
    json!({
        "found": true,
        "usable": usable,
        "expires_at_ms": cred.expires_at_ms,
        "credential_source": cred.credential_source,
        // 无凭证本体下发；可刷新通道（oauth）提示
        "refreshable": cred.credential_source == "oauth" && !cred.refresh_token.is_empty(),
    })
}

// ensure_fresh_credential 的刷新链路在 tasks/codearts_common；本命令域预检（凭证状态）不触发网络。
