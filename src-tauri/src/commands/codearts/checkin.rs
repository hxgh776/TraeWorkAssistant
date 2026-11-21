//! CodeArts 签到命令：发起轮次（NDJSON 事件）/ 结果查询 / 启动补签。

use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use crate::fs_utils;
use crate::state::AppState;
use crate::tasks::codearts_checkin;

/// NDJSON 事件转发（对齐 minimax/qoder/wb 管线：emit 序列化后的 JSON 字符串。
/// 事件名沿用现行仓库统一 `-progress` 后缀；旧 3.9.3 的 `codearts-checkin-line`
/// 是旧一代命名，不回退）。
fn emit_event(app: &AppHandle, ev: &Value) {
    if let Ok(line) = serde_json::to_string(ev) {
        let _ = app.emit("codearts-checkin-progress", &line);
    }
}

/// 发起签到轮次（可选 user_ids 收窄）。
/// 进度经 `codearts-checkin-progress` NDJSON 事件推送（type=start/account/done/exit）。
#[tauri::command(async)]
pub fn codearts_checkin_start(
    app: AppHandle,
    state: State<'_, AppState>,
    user_ids: Option<Vec<String>>,
) -> Result<(), String> {
    let ids = user_ids.unwrap_or_default();
    let round = codearts_checkin::try_acquire_round()?;
    let app2 = app.clone();
    let state2 = state.inner().clone();
    let spawned = std::thread::Builder::new()
        .name("codearts-checkin".into())
        .spawn(move || {
            let _guard = round;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                codearts_checkin::run_checkin_round(&state2, &ids, &mut |ev| emit_event(&app2, ev));
            }));
            let ok = result.is_ok();
            if !ok {
                fs_utils::app_log(&state2.data_dir, "[codearts] 签到轮次线程 panic（已捕获，exit 终态 ok=false 下发）");
            }
            let line = serde_json::json!({ "type": "exit", "ok": ok }).to_string();
            crate::events::emit_logged(
                &app2,
                "codearts-checkin-progress",
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
pub fn codearts_checkin_results(
    state: State<'_, AppState>,
    days: Option<i64>,
) -> Result<Vec<Value>, String> {
    crate::store::docs::checkin_results_since(
        &crate::store::db(&state.data_dir),
        "codearts_checkin_results",
        days.unwrap_or(30).clamp(1, 90),
    )
}

/// 启动自动补签（开关 codearts_checkin_enabled；抢不到跨进程轮次锁幂等跳过）。
pub fn startup_auto_checkin(app: &AppHandle, state: &AppState) {
    if !state.settings().codearts_checkin_enabled {
        return;
    }
    let app2 = app.clone();
    let state2 = state.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(85));
        // 跨进程轮次锁在后台线程内获取（guard 不跨线程传递）
        let (Some(_round), _) = crate::tasks::qoder_common::CrossProcLock::try_acquire_ns(
            &state2.data_dir,
            "codearts",
            "round",
            0,
        ) else {
            return;
        };
        codearts_checkin::run_checkin_round(&state2, &[], &mut |ev| emit_event(&app2, ev));
        let line = serde_json::json!({ "type": "exit", "ok": true }).to_string();
        let _ = app2.emit("codearts-checkin-progress", Value::String(line));
    });
}
