//! quota_wb —— WB 积分汇总（原 core/quota.rs WB 段平移）。
//!
//! 剩余积分 = 用户填写的每月总额度（~/.agenthub/keys/wb-credit-total）−
//! 本月已用（usage_events.credit，与官方账单同源）。

use serde::Serialize;
use tauri::State;

use chrono::Local;

use agenthub_core::fsutil::{self, Res};
use agenthub_core::store::{self, Store};

fn keys_dir() -> std::path::PathBuf {
    agenthub_core::store::hub_dir().join("keys")
}

fn wb_credit_total_file() -> std::path::PathBuf {
    keys_dir().join("wb-credit-total")
}

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
    agenthub_core::plugins::require_enabled(&state, "workbuddy")?;
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
    agenthub_core::plugins::require_enabled(&state, "workbuddy")?;
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
