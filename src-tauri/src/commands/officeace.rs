//! OfficeAce（华为果办）签到命令域：状态 / 签到轮次 / 结果查询。

use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use crate::fs_utils;
use crate::state::AppState;
use crate::tasks::officeace_checkin;

/// NDJSON 事件转发（对齐 qoder/wb 管线：emit 序列化后的 JSON 字符串）
fn emit_event(app: &AppHandle, ev: &Value) {
    if let Ok(line) = serde_json::to_string(ev) {
        let _ = app.emit("officeace-checkin-progress", &line);
    }
}

/// 本机登录态（只读；不含任何凭据）。
#[tauri::command]
pub fn officeace_status() -> officeace_checkin::OfficeAceStatus {
    officeace_checkin::status()
}

/// 发起签到（单账号本机登录态；进程内轮次锁互斥 + 工作线程 + exit 收尾）。
#[tauri::command(async)]
pub fn officeace_checkin_start(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    let round = officeace_checkin::try_acquire_round()?;
    let app2 = app.clone();
    let state2 = state.inner().clone();
    let spawned = std::thread::Builder::new()
        .name("officeace-checkin".into())
        .spawn(move || {
            let _guard = round;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                officeace_checkin::run_checkin_round(&state2, &mut |ev| emit_event(&app2, ev));
            }));
            let ok = result.is_ok();
            if !ok {
                fs_utils::app_log(&state2.data_dir, "OfficeAce 签到轮次线程 panic（已捕获，exit 终态 ok=false 下发）");
            }
            let line = serde_json::json!({ "type": "exit", "ok": ok }).to_string();
            crate::events::emit_logged(
                &app2,
                "officeace-checkin-progress",
                Value::String(line),
                Some(state2.data_dir.as_path()),
            );
        });
    if let Err(e) = spawned {
        return Err(format!("签到后台线程启动失败: {e}"));
    }
    Ok(())
}

/// 签到日志（90 天存储，UI 默认展示 30 天）。
#[tauri::command]
pub fn officeace_checkin_results(state: State<AppState>, days: Option<i64>) -> Result<Vec<Value>, String> {
    crate::store::docs::checkin_results_since(
        &crate::store::db(&state.data_dir),
        "officeace_checkin_results",
        days.unwrap_or(30).clamp(1, 90),
    )
}

/// 启动自动补签（开关 officeace_checkin_enabled；抢不到轮次锁幂等跳过）。
pub fn startup_auto_checkin(app: &AppHandle, state: &AppState) {
    if !state.settings().officeace_checkin_enabled {
        return;
    }
    let app2 = app.clone();
    let state2 = state.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(75));
        // 跨进程轮次锁在后台线程内获取（guard 不跨线程传递）
        let (Some(_round), _) = crate::tasks::qoder_common::CrossProcLock::try_acquire_ns(
            &state2.data_dir,
            "officeace",
            "round",
            0,
        ) else {
            return;
        };
        officeace_checkin::run_checkin_round(&state2, &mut |ev| emit_event(&app2, ev));
        let line = serde_json::json!({ "type": "exit", "ok": true }).to_string();
        let _ = app2.emit("officeace-checkin-progress", Value::String(line));
    });
}