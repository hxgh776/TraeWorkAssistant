//! MiniMax 签到命令：发起轮次（NDJSON 事件）/ 结果查询 / 启动补签。

use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use crate::fs_utils;
use crate::state::AppState;
use crate::tasks::minimax_checkin;

/// NDJSON 事件转发（对齐 qoder/wb/officeace 管线：emit 序列化后的 JSON 字符串）
fn emit_event(app: &AppHandle, ev: &Value) {
    if let Ok(line) = serde_json::to_string(ev) {
        let _ = app.emit("minimax-checkin-progress", &line);
    }
}

/// 发起签到轮次（双区账号按 settings 区开关过滤；可选 region / user_ids 收窄）。
/// 进度经 `minimax-checkin-progress` NDJSON 事件推送（type=start/account/done/exit）。
#[tauri::command(async)]
pub fn minimax_checkin_start(
    app: AppHandle,
    state: State<'_, AppState>,
    region: Option<String>,
    user_ids: Option<Vec<String>>,
) -> Result<(), String> {
    let region_filter = match region.as_deref().map(str::trim) {
        Some("") | None => None,
        Some(r) => {
            if crate::tasks::minimax_common::Region::parse(r).is_none() {
                return Err(format!("无效 region：{r}"));
            }
            Some(r.to_string())
        }
    };
    let ids = user_ids.unwrap_or_default();
    let round = minimax_checkin::try_acquire_round()?;
    let app2 = app.clone();
    let state2 = state.inner().clone();
    let spawned = std::thread::Builder::new()
        .name("minimax-checkin".into())
        .spawn(move || {
            let _guard = round;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                minimax_checkin::run_checkin_round(
                    &state2,
                    region_filter.as_deref(),
                    &ids,
                    &mut |ev| emit_event(&app2, ev),
                );
            }));
            let ok = result.is_ok();
            if !ok {
                fs_utils::app_log(&state2.data_dir, "MiniMax 签到轮次线程 panic（已捕获，exit 终态 ok=false 下发）");
            }
            let line = serde_json::json!({ "type": "exit", "ok": ok }).to_string();
            crate::events::emit_logged(
                &app2,
                "minimax-checkin-progress",
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
pub fn minimax_checkin_results(
    state: State<'_, AppState>,
    days: Option<i64>,
) -> Result<Vec<Value>, String> {
    crate::store::docs::checkin_results_since(
        &crate::store::db(&state.data_dir),
        "minimax_checkin_results",
        days.unwrap_or(30).clamp(1, 90),
    )
}

/// 启动自动补签（开关 minimax_checkin_enabled；抢不到跨进程轮次锁幂等跳过）。
pub fn startup_auto_checkin(app: &AppHandle, state: &AppState) {
    if !state.settings().minimax_checkin_enabled {
        return;
    }
    let app2 = app.clone();
    let state2 = state.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(85));
        // 跨进程轮次锁在后台线程内获取（guard 不跨线程传递）
        let (Some(_round), _) = crate::tasks::qoder_common::CrossProcLock::try_acquire_ns(
            &state2.data_dir,
            "minimax",
            "round",
            0,
        ) else {
            return;
        };
        minimax_checkin::run_checkin_round(&state2, None, &[], &mut |ev| emit_event(&app2, ev));
        let line = serde_json::json!({ "type": "exit", "ok": true }).to_string();
        let _ = app2.emit("minimax-checkin-progress", Value::String(line));
    });
}
