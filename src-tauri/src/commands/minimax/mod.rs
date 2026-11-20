//! MiniMax Code 命令域（VENU-36 重建，旧 3.9.3 行为基准；形态对齐 commands/qoder/ 拆分）。
//!
//! ⚠ serde 命名约定：全部 snake_case，与前端 types.ts 严格对齐。
//! 凭证红线：access/refresh token 同密码——不进日志、不进 NDJSON、前端掩码展示；
//! 本机 auth.json 只读不写回（通道 B 不主动刷新，避免作废客户端一次性轮换链）。

mod accounts;
pub(crate) mod checkin;
pub(crate) mod common;
mod credits;
pub(crate) mod oauth;

pub use accounts::*;
pub use checkin::*;
pub use credits::*;
pub use oauth::*;
