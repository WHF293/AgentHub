//! quota_zcode —— ZCode Coding Plan 官方额度（原 core/quota.rs ZCode 段平移）。
//!
//! 读 `~/.zcode/v2/config.json` 中 bigmodel coding-plan provider 的明文 apiKey
//! （与 cc-switch 同源；Key 只存 ZCode 目标文件，不入库），调官方
//! `GET https://open.bigmodel.cn/api/monitor/usage/quota/limit` 拿 5 小时/周窗口
//! 剩余百分比（unit=3 → 5h，unit=6 → 周；响应实测 2026-11-01）。

use serde::Serialize;

use chrono::{Local, TimeZone};

use agenthub_core::fsutil::{self, Res};
use agenthub_core::store::Store;

use crate::adapter as zcode;

#[derive(Debug, Serialize, Clone, Default)]
pub struct QuotaWindow {
    /// 已用百分比 0-100
    pub used_pct: i64,
    pub used: i64,
    pub total: i64,
    pub remaining: i64,
    /// 窗口重置时间（本地化文本；5h 窗口可能缺失）
    pub reset_at: String,
}

#[derive(Debug, Serialize)]
pub struct ZcodeQuota {
    pub plan_level: String,
    pub h5: QuotaWindow,
    pub week: QuotaWindow,
}

/// 从 ZCode v2/config.json 解析 coding-plan 明文 Key（不落库、不展示）
fn resolve_coding_plan_key() -> Option<String> {
    let bytes = fsutil::read_opt(&zcode::v2_config())?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let providers = v.get("provider")?.as_object()?;
    // 优先官方 coding-plan 条目；兜底任意 baseURL 指向 bigmodel 且带非空 apiKey 的 provider
    let mut fallback: Option<String> = None;
    for (id, p) in providers {
        let opts = p.get("options").and_then(|o| o.as_object());
        let Some(opts) = opts else { continue };
        let key = opts
            .get("apiKey")
            .and_then(|k| k.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let Some(key) = key else { continue };
        if id.contains("bigmodel-coding-plan") {
            return Some(key);
        }
        let base = opts
            .get("baseURL")
            .or_else(|| opts.get("baseUrl"))
            .and_then(|b| b.as_str())
            .unwrap_or("");
        if base.contains("bigmodel.cn") && fallback.is_none() {
            fallback = Some(key);
        }
    }
    fallback
}

fn parse_window(l: &serde_json::Value) -> QuotaWindow {
    let get_i = |k: &str| l.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
    let reset_ms = l.get("nextResetTime").and_then(|x| x.as_i64()).unwrap_or(0);
    QuotaWindow {
        used_pct: get_i("percentage"),
        used: get_i("currentValue"),
        total: get_i("usage"),
        remaining: get_i("remaining"),
        reset_at: if reset_ms > 0 {
            Local
                .timestamp_millis_opt(reset_ms)
                .single()
                .map(|t| t.format("%m-%d %H:%M").to_string())
                .unwrap_or_default()
        } else {
            String::new()
        },
    }
}

/// 阻塞网络请求（调用方须放在 spawn_blocking）
fn fetch_zcode_quota() -> Res<ZcodeQuota> {
    let key = resolve_coding_plan_key()
        .ok_or("未在 ~/.zcode/v2/config.json 找到 Coding Plan apiKey（请确认 ZCode 已登录套餐）")?;
    let url = "https://open.bigmodel.cn/api/monitor/usage/quota/limit";
    let resp = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        // 固定国内域名直连即达；禁用代理避免继承会话级 proxy 环境变量导致发送失败
        .no_proxy()
        .build()
        .map_err(|e| e.to_string())?
        .get(url)
        .bearer_auth(&key)
        .header("Accept", "application/json")
        .send()
        .map_err(|e| format!("请求官方额度接口失败: {}", fsutil::net_err(e)))?;
    if !resp.status().is_success() {
        return Err(format!("官方额度接口返回 {}", resp.status()));
    }
    let body: serde_json::Value = resp.json().map_err(|e| format!("解析响应失败: {e}"))?;
    if body.get("code").and_then(|c| c.as_i64()) != Some(200) {
        return Err(format!(
            "官方额度接口错误: {}",
            body.get("msg").and_then(|m| m.as_str()).unwrap_or("未知")
        ));
    }
    let data = body.get("data").ok_or("响应缺少 data")?;
    let limits = data
        .get("limits")
        .and_then(|l| l.as_array())
        .ok_or("响应缺少 limits")?;
    // unit=3 → 5h 窗口；unit=6 → 周窗口（nextResetTime 兜底：最早重置=5h）
    let mut h5: Option<QuotaWindow> = None;
    let mut week: Option<QuotaWindow> = None;
    for l in limits {
        let unit = l.get("unit").and_then(|x| x.as_i64());
        let w = parse_window(l);
        match unit {
            Some(3) if h5.is_none() => h5 = Some(w),
            Some(6) if week.is_none() => week = Some(w),
            _ => {}
        }
    }
    if h5.is_none() || week.is_none() {
        // 兜底：按 reset 时间排序（缺失排最后），最早的当 5h
        let mut sorted: Vec<&serde_json::Value> = limits.iter().collect();
        sorted.sort_by_key(|l| {
            l.get("nextResetTime")
                .and_then(|x| x.as_i64())
                .unwrap_or(i64::MAX)
        });
        if h5.is_none() {
            if let Some(l) = sorted.first() {
                h5 = Some(parse_window(l));
            }
        }
        if week.is_none() {
            if let Some(l) = sorted.last() {
                week = Some(parse_window(l));
            }
        }
    }
    Ok(ZcodeQuota {
        plan_level: data
            .get("level")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        h5: h5.ok_or("响应中未找到 5 小时窗口")?,
        week: week.ok_or("响应中未找到周窗口")?,
    })
}

#[tauri::command]
pub async fn zcode_quota(state: tauri::State<'_, Store>) -> Result<ZcodeQuota, String> {
    agenthub_core::plugins::require_enabled(&state, "zcode")?;
    tauri::async_runtime::spawn_blocking(fetch_zcode_quota)
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
