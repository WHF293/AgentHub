//! usage —— M9 用量分析引擎（monorepo 后 = 纯引擎：互斥/进度/游标/聚合，无具体采集器）。
//!
//! 各端采集经 `AgentAdapter::collect_usage` 钩子分发进各插件包
//! （workbuddy → projects jsonl + 快照兜底；zcode → model_usage 全量账本；
//! claude → projects jsonl）。隐私红线：正文永不入库。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{Local, TimeZone};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::fsutil::Res;
use crate::store::Store;

/// 采集互斥：避免启动线程与前端刷新并发重复扫描
static COLLECTING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Serialize, Clone)]
pub struct CollectProgress {
    pub stage: String,
    pub done: usize,
    pub total: usize,
}

pub fn emit_progress(app: Option<&AppHandle>, stage: &str, done: usize, total: usize) {
    if let Some(app) = app {
        let _ = app.emit(
            "usage-collect-progress",
            CollectProgress {
                stage: stage.to_string(),
                done,
                total,
            },
        );
    }
}

pub fn ts_to_local(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

/// 递归枚举目录下全部 *.jsonl（各插件包 jsonl 采集共用）
pub fn collect_jsonl_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_jsonl_files(&p, out);
        } else if p.extension().map(|x| x == "jsonl").unwrap_or(false) {
            out.push(p);
        }
    }
}

/// 读增量游标：(byte_offset, mtime_ms)；无记录 = (0, 0)
pub fn cursor_get(store: &Store, file_path: &str) -> Res<(i64, i64)> {
    store.with(|conn| {
        Ok(conn
            .query_row(
                "SELECT byte_offset, IFNULL(mtime,'0') FROM usage_cursor WHERE file_path = ?1",
                rusqlite::params![file_path],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?.parse::<i64>().unwrap_or(0),
                    ))
                },
            )
            .unwrap_or((0, 0)))
    })
}

/// 写增量游标（upsert）
pub fn cursor_set(store: &Store, source: &str, file_path: &str, byte_offset: i64, mtime_ms: i64) -> Res<()> {
    store.with(|conn| {
        conn.execute(
            "INSERT INTO usage_cursor(source, file_path, byte_offset, mtime) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(file_path) DO UPDATE SET byte_offset = excluded.byte_offset, mtime = excluded.mtime",
            rusqlite::params![source, file_path, byte_offset, mtime_ms.to_string()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })
}

/// 采集入口：遍历已启用插件的 collect_usage 钩子，返回 (zcode新增, wb新增, claude新增)。
/// COLLECTING 互斥防重入；app 用于向前端推采集进度事件。
pub fn collect_all(store: &Store, app: Option<&AppHandle>) -> Res<(usize, usize, usize)> {
    if COLLECTING.swap(true, Ordering::Relaxed) {
        return Ok((0, 0, 0));
    }
    let result = do_collect_all(store, app);
    COLLECTING.store(false, Ordering::Relaxed);
    result
}

fn do_collect_all(store: &Store, app: Option<&AppHandle>) -> Res<(usize, usize, usize)> {
    // 插件门控：停用的 agent 适配器不采集（历史事件保留在库，重新启用后恢复增量）；
    // 旧来源清理随各插件采集器自清，引擎不感知具体 agent
    let enabled = crate::plugins::enabled_ids(store)?;
    let mut counts: std::collections::HashMap<&'static str, usize> =
        std::collections::HashMap::new();
    for a in crate::adapters::registry() {
        if !enabled.iter().any(|id| id == a.id()) {
            continue;
        }
        let n = a.collect_usage(store, app)?;
        counts.insert(a.id(), n);
    }
    // 固定三元组投影（前端/无头命令的消费契约）；未来新增插件在此追加
    Ok((
        counts.get("zcode").copied().unwrap_or(0),
        counts.get("workbuddy").copied().unwrap_or(0),
        counts.get("claude").copied().unwrap_or(0),
    ))
}

// ─── 聚合查询 ───

#[derive(Debug, Serialize)]
pub struct AgentTotal {
    pub agent: String,
    /// 账号 uid（'' = ZCode 单账号/未归属）
    pub account: String,
    pub calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    /// 官方逐请求积分（WB rawUsage.credit 与账单同源；ZCode 恒为 0）
    pub credit: f64,
}

#[derive(Debug, Serialize)]
pub struct ModelBreakdown {
    pub agent: String,
    pub account: String,
    pub model: String,
    pub calls: i64,
    pub tokens: i64,
}

#[derive(Debug, Serialize)]
pub struct CurvePoint {
    pub bucket: i64, // 0..47（每 30min）
    pub agent: String,
    pub account: String,
    pub tokens: i64,
}

#[derive(Debug, Serialize)]
pub struct HeatCell {
    pub date: String,
    pub agent: String,
    pub account: String,
    pub tokens: i64,
    pub calls: i64,
}

#[derive(Debug, Serialize)]
pub struct UsageOverview {
    pub range: String,
    pub totals: Vec<AgentTotal>,
    pub models: Vec<ModelBreakdown>,
    pub curve: Vec<CurvePoint>,
    pub heatmap: Vec<HeatCell>,
    /// 日历视图窗口（YYYY-MM-DD）
    pub cal_start: String,
    pub cal_end: String,
    pub today: String,
}

/// 区间解析：day / trailing:N / year / all / month:YYYY-MM
/// 返回 (数据起点, 数据终点, 日历起点, 日历终点)
struct Window {
    start: String,
    end: String,
    cal_start: String,
    cal_end: String,
    is_day: bool,
}

fn parse_range(range: &str) -> Window {
    let now = Local::now();
    let today = now.format("%Y-%m-%d").to_string();
    let far = "9999-12-31 23:59:59".to_string();
    let day_start = |d: chrono::DateTime<Local>| d.format("%Y-%m-%d 00:00:00").to_string();

    if let Some(m) = range.strip_prefix("month:") {
        // 历史月份：month:YYYY-MM（含上下边界）
        let parts: Vec<&str> = m.split('-').collect();
        if parts.len() == 2 {
            if let (Ok(y), Ok(mo)) = (parts[0].parse::<i32>(), parts[1].parse::<u32>()) {
                if (1..=12).contains(&mo) {
                    let start = format!("{y}-{mo:02}-01 00:00:00");
                    let (ny, nm) = if mo == 12 { (y + 1, 1) } else { (y, mo + 1) };
                    let end = format!("{ny}-{nm:02}-01 00:00:00");
                    let label = format!("{y}-{mo:02}");
                    return Window {
                        start,
                        end: end.clone(),
                        cal_start: label.clone(),
                        cal_end: label,
                        is_day: false,
                    };
                }
            }
        }
    }

    if let Some(y) = range.strip_prefix("year:") {
        // 历史年份：year:YYYY
        if let Ok(y) = y.parse::<i32>() {
            let start = format!("{y}-01-01 00:00:00");
            let end = format!("{}-01-01 00:00:00", y + 1);
            return Window {
                start,
                end: end.clone(),
                cal_start: format!("{y}-01-01"),
                cal_end: format!("{y}-12-31"),
                is_day: false,
            };
        }
    }

    if let Some(d) = range.strip_prefix("date:") {
        // 任意历史单日：date:YYYY-MM-DD（24h 曲线）
        let date = d.trim().to_string();
        if date.len() == 10 {
            let start = format!("{date} 00:00:00");
            let end_dt = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                .map(|x| x.succ_opt().map(|n| n.format("%Y-%m-%d 00:00:00").to_string()))
                .ok()
                .flatten();
            if let Some(end) = end_dt {
                let month = &date[..7];
                return Window {
                    start,
                    end,
                    cal_start: month.to_string(),
                    cal_end: month.to_string(),
                    is_day: true,
                };
            }
        }
    }

    if let Some(d) = range.strip_prefix("week:") {
        // 周视图：week:YYYY-MM-DD（该周周一），7 天窗口
        if let Ok(mon) = chrono::NaiveDate::parse_from_str(d.trim(), "%Y-%m-%d") {
            let start = mon.format("%Y-%m-%d 00:00:00").to_string();
            let end = (mon + chrono::Duration::days(7))
                .format("%Y-%m-%d 00:00:00")
                .to_string();
            return Window {
                start,
                end: end.clone(),
                cal_start: mon.format("%Y-%m-%d").to_string(),
                cal_end: (mon + chrono::Duration::days(6))
                    .format("%Y-%m-%d")
                    .to_string(),
                is_day: false,
            };
        }
    }

    let trailing = range
        .strip_prefix("trailing:")
        .and_then(|s| s.parse::<i64>().ok());

    match trailing {
        Some(n) if n > 0 => {
            let start = day_start(now - chrono::Duration::days(n));
            Window {
                start: start.clone(),
                end: far,
                cal_start: start[..10].to_string(),
                cal_end: today.clone(),
                is_day: false,
            }
        }
        _ => match range {
            "day" => Window {
                start: format!("{today} 00:00:00"),
                end: far,
                cal_start: now.format("%Y-%m").to_string(),
                cal_end: now.format("%Y-%m").to_string(),
                is_day: true,
            },
            "year" => {
                let start = now.format("%Y-01-01 00:00:00").to_string();
                Window {
                    start: start.clone(),
                    end: far,
                    cal_start: start[..10].to_string(),
                    cal_end: today.clone(),
                    is_day: false,
                }
            }
            "all" => Window {
                start: "2000-01-01 00:00:00".into(),
                end: far,
                cal_start: "2000-01-01".into(),
                cal_end: today.clone(),
                is_day: false,
            },
            _ => Window {
                start: format!("{today} 00:00:00"),
                end: far,
                cal_start: now.format("%Y-%m").to_string(),
                cal_end: now.format("%Y-%m").to_string(),
                is_day: true,
            },
        },
    }
}

pub fn overview(store: &Store, range: &str) -> Res<UsageOverview> {
    let w = parse_range(range);
    let today = Local::now().format("%Y-%m-%d").to_string();
    let far = "9999-12-31 23:59:59".to_string();

    store.with(|conn| {
        let mut totals = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT agent, account, COUNT(*), SUM(input_tokens), SUM(output_tokens), SUM(cache_read_tokens), SUM(credit)
             FROM usage_events WHERE ts >= ?1 AND ts < ?2 GROUP BY agent, account ORDER BY agent, account",
        )
        .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![w.start, w.end], |row| {
                Ok(AgentTotal {
                    agent: row.get(0)?,
                    account: row.get(1)?,
                    calls: row.get(2)?,
                    input_tokens: row.get(3)?,
                    output_tokens: row.get(4)?,
                    cache_read_tokens: row.get(5)?,
                    credit: row.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            totals.push(r.map_err(|e| e.to_string())?);
        }

        let mut models = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT agent, account, model, COUNT(*), SUM(input_tokens + output_tokens + cache_read_tokens)
             FROM usage_events WHERE ts >= ?1 AND ts < ?2 GROUP BY agent, account, model
             ORDER BY 5 DESC LIMIT 16",
        )
        .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![w.start, w.end], |row| {
                Ok(ModelBreakdown {
                    agent: row.get(0)?,
                    account: row.get(1)?,
                    model: row.get(2)?,
                    calls: row.get(3)?,
                    tokens: row.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            models.push(r.map_err(|e| e.to_string())?);
        }

        let mut curve = Vec::new();
        if w.is_day {
            let mut stmt = conn.prepare(
                "SELECT (CAST(strftime('%H', ts) AS INT) * 2 + CAST(strftime('%M', ts) AS INT) / 30) AS bucket,
                        agent, account, SUM(input_tokens + output_tokens + cache_read_tokens)
                 FROM usage_events WHERE ts >= ?1 AND ts < ?2 GROUP BY bucket, agent, account ORDER BY bucket",
            )
            .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params![w.start, w.end], |row| {
                    Ok(CurvePoint {
                        bucket: row.get(0)?,
                        agent: row.get(1)?,
                        account: row.get(2)?,
                        tokens: row.get(3)?,
                    })
                })
                .map_err(|e| e.to_string())?;
            for r in rows {
                curve.push(r.map_err(|e| e.to_string())?);
            }
        }

        // 日历窗口：day 保持「当前月」上下文，其余区间即数据窗口本身
        let (heat_start, heat_end) = if w.is_day {
            (format!("{}-01 00:00:00", w.cal_start), far.clone())
        } else {
            (w.start.clone(), w.end.clone())
        };

        let mut heatmap = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT date(ts), agent, account, SUM(input_tokens + output_tokens + cache_read_tokens), COUNT(*)
             FROM usage_events WHERE ts >= ?1 AND ts < ?2 GROUP BY date(ts), agent, account ORDER BY date(ts)",
        )
        .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![heat_start, heat_end], |row| {
                Ok(HeatCell {
                    date: row.get(0)?,
                    agent: row.get(1)?,
                    account: row.get(2)?,
                    tokens: row.get(3)?,
                    calls: row.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            heatmap.push(r.map_err(|e| e.to_string())?);
        }

        Ok(UsageOverview {
            range: range.to_string(),
            totals,
            models,
            curve,
            heatmap,
            cal_start: w.cal_start,
            cal_end: w.cal_end,
            today,
        })
    })
}

// ─── Tauri commands（M9） ───
// 全部 async：同步 command 在主线程执行，采集/聚合持锁时会把窗口卡成「未响应」

use tauri::State;

#[tauri::command]
pub async fn usage_collect(state: State<'_, Store>) -> Result<(usize, usize, usize), String> {
    collect_all(&state, None)
}

#[tauri::command]
pub async fn usage_overview(state: State<'_, Store>, range: String) -> Result<UsageOverview, String> {
    overview(&state, &range)
}

/// 日期 × Agent × 账号 × 模型 聚合矩阵（日历格子与 hover 明细的数据源）
#[derive(Debug, Serialize)]
pub struct MatrixCell {
    pub date: String,
    pub agent: String,
    /// 账号 uid（'' = ZCode 单账号/未归属）
    pub account: String,
    pub model: String,
    pub tokens: i64,
    pub calls: i64,
}

#[tauri::command]
pub async fn usage_matrix(state: State<'_, Store>) -> Result<Vec<MatrixCell>, String> {
    state.with(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT date(ts), agent, account, model,
                        SUM(input_tokens + output_tokens + cache_read_tokens), COUNT(*)
                 FROM usage_events GROUP BY date(ts), agent, account, model ORDER BY date(ts)",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(MatrixCell {
                    date: row.get(0)?,
                    agent: row.get(1)?,
                    account: row.get(2)?,
                    model: row.get(3)?,
                    tokens: row.get(4)?,
                    calls: row.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    })
}

/// 账号清单（筛选 chip 数据源）：全库按 agent+account 聚合
#[derive(Debug, Serialize)]
pub struct UsageAccount {
    pub agent: String,
    /// 账号 uid（'' = ZCode 单账号/未归属）
    pub account: String,
    pub calls: i64,
    pub tokens: i64,
}

#[tauri::command]
pub async fn usage_accounts(state: State<'_, Store>) -> Result<Vec<UsageAccount>, String> {
    state.with(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT agent, account, COUNT(*), SUM(input_tokens + output_tokens + cache_read_tokens)
                 FROM usage_events WHERE account != '' GROUP BY agent, account ORDER BY agent, account",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(UsageAccount {
                    agent: row.get(0)?,
                    account: row.get(1)?,
                    calls: row.get(2)?,
                    tokens: row.get(3)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    })
}
