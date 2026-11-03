//! ZCode 插件包：适配器 + 专属功能域（账号槽位 / Coding Plan 额度 / model_usage
//! 用量采集 / v2 providers 直写 / 插件自带技能展示）。
//!
//! 接入点：src-tauri 组装层把 [`adapter::ZCODE_ADAPTER`] 收进 core 注册表。

pub mod adapter;
pub mod provider_zcode;
pub mod quota_zcode;
pub mod skills;
pub mod usage_zcode;
pub mod zaccount;
