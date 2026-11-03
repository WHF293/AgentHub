//! WorkBuddy 插件包：适配器 + 专属功能域（多账号槽位 / 会话复制 / 签到 / 积分 /
//! projects jsonl 用量采集 / models.json 写穿）。
//!
//! 接入点：src-tauri 组装层把 [`adapter::WORKBUDDY`] 收进 core 注册表；
//! 跨域联动（provider 保存后的 models.json 写穿）由组装桥调用 [`provider_wb`]。

pub mod account;
pub mod adapter;
pub mod checkin;
pub mod provider_wb;
pub mod quota_wb;
pub mod sessions;
pub mod usage_wb;
