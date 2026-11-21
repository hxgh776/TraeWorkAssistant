//! CodeArts 命令域（VENU-37 按旧 3.9.3 基准重建；形态对齐 commands/minimax/ 拆分）。
//!
//! ⚠ serde 命名约定：全部 snake_case，与前端 types.ts 严格对齐。
//! 凭证红线：AK/SK/SecurityToken/refresh_token 同密码——不进日志、不进 NDJSON、
//! 前端掩码展示；state.vscdb 只读不写回；local_import / aksk 通道不消费
//! refresh_token（单次轮转会作废桌面端登录链，issue 硬约束）。

mod accounts;
pub(crate) mod checkin;
pub(crate) mod common;
pub(crate) mod credits;
mod credentials;

pub use accounts::*;
pub use checkin::*;
pub use credits::*;
pub use credentials::*;
