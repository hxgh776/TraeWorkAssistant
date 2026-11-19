//! LobsterAI（网易有道龙虾）签到命令域：状态 / 签到轮次 / 结果查询。

use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use crate::fs_utils;
use crate::state::AppState;
use crate::tasks::lobsterai_checkin;

fn emit_event(app: &AppHandle, ev: &Value) {
    if let Ok(line) = serde_json::to_string(ev) {
        let _ = app.emit("lobsterai-checkin-progress", &line);
    }
}

#[tauri::command]
pub fn lobsterai_status() -> lobsterai_checkin::LobsterAiStatus {
    lobsterai_checkin::status()
}

/// 积分 / 连签数据（只读 client-activities context，与签到同一份本机登录态）。
/// 上游不可达或登录态缺失时返回 Err——前端展示错误，禁止用 0 冒充；
/// 上游未下发余额字段时 balance 缺省（前端显示「未知」）。
/// 不写回、不落盘凭据、错误文案不含令牌。
#[tauri::command(async)]
pub fn lobsterai_quota() -> Result<lobsterai_checkin::LobsterAiQuota, String> {
    lobsterai_checkin::quota()
}

#[tauri::command(async)]
pub fn lobsterai_checkin_start(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    let round = lobsterai_checkin::try_acquire_round()?;
    let app2 = app.clone();
    let state2 = state.inner().clone();
    let spawned = std::thread::Builder::new()
        .name("lobsterai-checkin".into())
        .spawn(move || {
            let _guard = round;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                lobsterai_checkin::run_checkin_round(&state2, &mut |ev| emit_event(&app2, ev));
            }));
            let ok = result.is_ok();
            if !ok {
                fs_utils::app_log(&state2.data_dir, "LobsterAI 签到轮次线程 panic（已捕获，exit 终态 ok=false 下发）");
            }
            let line = serde_json::json!({ "type": "exit", "ok": ok }).to_string();
            crate::events::emit_logged(
                &app2,
                "lobsterai-checkin-progress",
                Value::String(line),
                Some(state2.data_dir.as_path()),
            );
        });
    if let Err(e) = spawned {
        return Err(format!("签到后台线程启动失败: {e}"));
    }
    Ok(())
}

#[tauri::command]
pub fn lobsterai_checkin_results(state: State<AppState>, days: Option<i64>) -> Result<Vec<Value>, String> {
    crate::store::docs::checkin_results_since(
        &crate::store::db(&state.data_dir),
        "lobsterai_checkin_results",
        days.unwrap_or(30).clamp(1, 90),
    )
}

/// 启动自动补签（开关 lobsterai_checkin_enabled；抢不到轮次锁幂等跳过）。
pub fn startup_auto_checkin(app: &AppHandle, state: &AppState) {
    if !state.settings().lobsterai_checkin_enabled {
        return;
    }
    let app2 = app.clone();
    let state2 = state.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(75));
        // 跨进程轮次锁在后台线程内获取（guard 不跨线程传递）
        let (Some(_round), _) = crate::tasks::qoder_common::CrossProcLock::try_acquire_ns(
            &state2.data_dir,
            "lobsterai",
            "round",
            0,
        ) else {
            return;
        };
        lobsterai_checkin::run_checkin_round(&state2, &mut |ev| emit_event(&app2, ev));
        let line = serde_json::json!({ "type": "exit", "ok": true }).to_string();
        let _ = app2.emit("lobsterai-checkin-progress", Value::String(line));
    });
}