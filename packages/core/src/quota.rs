//! quota —— 额度/积分查询。
//!
//! ZCode：读 `~/.zcode/v2/config.json` 中 bigmodel coding-plan provider 的明文
//! apiKey（与 cc-switch 同源；Key 只存 ZCode 目标文件，不入库），调官方
//! `GET https://open.bigmodel.cn/api/monitor/usage/quota/limit` 拿 5 小时/周窗口
//! 剩余百分比（unit=3 → 5h，unit=6 → 周；响应实测 2026-11-01）。
//! WB：剩余积分 = 用户填写的每月总额度（~/.agenthub/keys/wb-credit-total）−
//! 本月已用（usage_events.credit，与官方账单同源）。

use serde::Serialize;
use tauri::State;

use chrono::{Local, TimeZone};

use crate::adapters::zcode;
use crate::fsutil::{self, Res};
use crate::store::{self, Store};

fn keys_dir() -> std::path::PathBuf {
    crate::store::hub_dir().join("keys")
}

fn wb_credit_total_file() -> std::path::PathBuf {
    keys_dir().join("wb-credit-total")
}

// ─── ZCode Coding Plan 额度 ───

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
        .map_err(|e| format!("请求官方额度接口失败: {}", crate::checkin::net_err(e)))?;
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
    crate::plugins::require_enabled(&state, "zcode")?;
    tauri::async_runtime::spawn_blocking(fetch_zcode_quota)
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}

// ─── WB 积分汇总 ───

#[derive(Debug, Serialize)]
pub struct WbCreditSummary {
    /// 本月已用积分（usage_events.credit 聚合，与官方账单同源）
    pub month_used: f64,
    /// 用户填写的每月总额度（0 = 未填写）
    pub monthly_total: f64,
    pub remaining: f64,
}

pub fn wb_credit_summary_inner(store: &Store) -> Res<WbCreditSummary> {
    let month_start = Local::now().format("%Y-%m-01 00:00:00").to_string();
    let month_used: f64 = store.with(|conn| {
        conn.query_row(
            "SELECT IFNULL(SUM(credit),0) FROM usage_events
             WHERE agent = 'workbuddy' AND ts >= ?1",
            rusqlite::params![month_start],
            |row| row.get::<_, Option<f64>>(0),
        )
        .map(|o| o.unwrap_or(0.0))
        .map_err(|e| e.to_string())
    })?;
    let monthly_total = fsutil::read_opt(&wb_credit_total_file())
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(0.0);
    let remaining = if monthly_total > 0.0 {
        (monthly_total - month_used).max(0.0)
    } else {
        0.0
    };
    Ok(WbCreditSummary {
        month_used,
        monthly_total,
        remaining,
    })
}

#[tauri::command]
pub async fn wb_credit_summary(state: State<'_, Store>) -> Result<WbCreditSummary, String> {
    crate::plugins::require_enabled(&state, "workbuddy")?;
    wb_credit_summary_inner(&state)
}

/// 按账户（uid）的积分用量：槽位卡片显示【当日使用 / 账户剩余】用
#[derive(Debug, Serialize)]
pub struct WbAccountCredit {
    pub uid: String,
    /// 当日已用积分（本地自然日）
    pub today_used: f64,
    /// 本月已用积分
    pub month_used: f64,
    /// 剩余 = 每月总额度 − 该账户本月已用（总额度未填时为 0）
    pub remaining: f64,
    /// 用户为本账户单独设置的当前积分快照（未设置 = null）
    pub balance: Option<f64>,
    /// 余额设置时间
    pub balance_set_at: String,
    /// 设置后的已用（剩余 = balance − used_since_set）
    pub used_since_set: f64,
}

#[tauri::command]
pub async fn wb_credit_by_account(state: State<'_, Store>) -> Result<Vec<WbAccountCredit>, String> {
    let day_start = Local::now().format("%Y-%m-%d 00:00:00").to_string();
    let month_start = Local::now().format("%Y-%m-01 00:00:00").to_string();
    let monthly_total = fsutil::read_opt(&wb_credit_total_file())
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(0.0);
    let rows: Vec<(String, f64, f64)> = state.with(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT account,
                        IFNULL(SUM(CASE WHEN ts >= ?1 THEN credit ELSE 0 END), 0),
                        IFNULL(SUM(CASE WHEN ts >= ?2 THEN credit ELSE 0 END), 0)
                 FROM usage_events
                 WHERE agent = 'workbuddy' AND account != ''
                 GROUP BY account",
            )
            .map_err(|e| e.to_string())?;
        let rs = stmt
            .query_map(rusqlite::params![day_start, month_start], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?, row.get::<_, f64>(2)?))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rs {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    })?;
    // 每账户手动设置的当前积分快照 + 设置后的已用
    let balances: Vec<(String, f64, String, f64)> = state.with(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT b.uid, b.balance, b.set_at,
                        IFNULL((SELECT SUM(credit) FROM usage_events e
                                WHERE e.agent='workbuddy' AND e.account=b.uid AND e.ts > b.set_at), 0)
                 FROM wb_credit_balance b",
            )
            .map_err(|e| e.to_string())?;
        let rs = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, f64>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rs {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    })?;
    let bmap: std::collections::HashMap<String, (f64, String, f64)> =
        balances.into_iter().map(|(u, b, t, u2)| (u, (b, t, u2))).collect();
    let mut out = Vec::new();
    for (uid, today_used, month_used) in rows {
        let (balance, set_at, used_since) = bmap.get(&uid).cloned().unwrap_or((0.0, String::new(), 0.0));
        let has_balance = bmap.contains_key(&uid);
        let remaining = if has_balance {
            (balance - used_since).max(0.0)
        } else if monthly_total > 0.0 {
            (monthly_total - month_used).max(0.0)
        } else {
            0.0
        };
        out.push(WbAccountCredit {
            remaining,
            uid,
            today_used,
            month_used,
            balance: if has_balance { Some(balance) } else { None },
            balance_set_at: set_at,
            used_since_set: used_since,
        });
    }
    // 已设余额但尚无用量的账户也要返回（卡片显示积分用）
    for (uid, (balance, set_at, used_since)) in bmap {
        if out.iter().any(|x: &WbAccountCredit| x.uid == uid) {
            continue;
        }
        out.push(WbAccountCredit {
            remaining: (balance - used_since).max(0.0),
            uid,
            today_used: 0.0,
            month_used: 0.0,
            balance: Some(balance),
            balance_set_at: set_at,
            used_since_set: used_since,
        });
    }
    Ok(out)
}

/// 每账户当前积分设置（0 或负数 = 清除该账户的余额设置）
#[derive(Debug, serde::Deserialize)]
pub struct WbCreditBalanceInput {
    pub uid: String,
    pub balance: f64,
}

#[tauri::command]
pub async fn wb_credit_set_balance(state: State<'_, Store>, input: WbCreditBalanceInput) -> Result<(), String> {
    let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    state.with(|conn| {
        if input.balance > 0.0 {
            conn.execute(
                "INSERT INTO wb_credit_balance(uid, balance, set_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(uid) DO UPDATE SET balance = ?2, set_at = ?3",
                rusqlite::params![input.uid, input.balance, now],
            )
            .map_err(|e| e.to_string())?;
        } else {
            conn.execute(
                "DELETE FROM wb_credit_balance WHERE uid = ?1",
                rusqlite::params![input.uid],
            )
            .map_err(|e| e.to_string())?;
        }
        store::log_op(
            conn,
            "wb.credit_balance",
            &input.uid,
            serde_json::json!({ "balance": input.balance }),
        );
        Ok(())
    })
}

#[derive(Debug, serde::Deserialize)]
pub struct WbCreditTotalInput {
    /// 每月积分总额度；0 或负数 = 清除
    pub total: f64,
}

#[tauri::command]
pub async fn wb_credit_set_total(state: State<'_, Store>, input: WbCreditTotalInput) -> Result<(), String> {
    crate::plugins::require_enabled(&state, "workbuddy")?;
    let _ = &state;
    let f = wb_credit_total_file();
    std::fs::create_dir_all(keys_dir()).map_err(|e| e.to_string())?;
    if input.total > 0.0 {
        fsutil::atomic_write(&f, format!("{}", input.total).as_bytes())
            .map_err(|e| format!("写入总额度失败: {e}"))?;
    } else if f.is_file() {
        std::fs::remove_file(&f).map_err(|e| e.to_string())?;
    }
    Ok(())
}
