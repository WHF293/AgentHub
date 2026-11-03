//! Claude Code 插件包：适配器 + 专属功能域（projects jsonl 用量采集 /
//! settings.json env 模型托管 / cc-switch 备份用量导入）。
//!
//! 接入点：src-tauri 组装层把 [`adapter::CLAUDE_ADAPTER`] 收进 core 注册表。

pub mod adapter;
pub mod ccsimport;
pub mod provider_claude;
pub mod usage_claude;
