//! agenthub-core —— AgentHub 共享内核。
//!
//! 承载：AgentAdapter trait 与注册表、SQLite 单一事实源、插件生命周期管理器、
//! Skills/MCP/用量引擎、第三方模型编排、Memory/Skill 市场/在线更新、各域 IPC 命令。
//! P2 起各 agent 专属域逐包拆入 packages/<id>（本 crate 只留 trait/引擎/编排）。

pub mod account;
pub mod adapters;
pub mod ccsimport;
pub mod checkin;
pub mod commands;
pub mod fsutil;
pub mod mcp;
pub mod market;
pub mod memory;
pub mod migrate;
pub mod modelfetch;
pub mod plugins;
pub mod providers;
pub mod quota;
pub mod sessions;
pub mod store;
pub mod update;
pub mod usage;
pub mod zaccount;

use std::sync::atomic::AtomicBool;

/// 关闭行为：true = 关到托盘（默认），false = 直接退出（设置岛可切换）。
/// 设置命令在 commands::set_close_to_tray，壳层 on_window_event 读此标记。
pub static CLOSE_TO_TRAY: AtomicBool = AtomicBool::new(true);
