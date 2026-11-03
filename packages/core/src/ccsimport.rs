//! cc-switch SQL 备份导入 —— 把另一台机器 cc-switch「设置 → 导出 SQL 备份」里的
//! 用量数据并入 usage_events，Usage 岛按独立 agent（app_type，如 claude）展示。
//!
//! 备份两种形态都认：SQLite 二进制（文件头 `SQLite format 3\0`）或 .sql 文本 dump
//! （临时建库执行后读取，读完即删临时文件）。映射约定：
//! - 明细 `proxy_request_logs` → 逐条事件；request_id 加 `ccs:` 前缀幂等去重；
//!   ts 用 created_at（unix 秒）转本地时间，与 WB/ZCode 采集同格式
//! - 日汇总 `usage_daily_rollups`（30 天前的老明细只留这个）→ 按 request_count
//!   合成等量事件（token 均分 + 余数进前几个），保日聚合口径与请求数；
//!   request_id 用 `ccs-roll:<date>:<app>:<model>:<i>` 幂等
//! - usage_events 无 cache_creation 列 → 并入 input_tokens（与 WB 口径一致：
//!   输入含缓存写入，输入/命中/输出三列分列展示不受影响）
//! - provider 只取 providers 表的名称映射；settings_config（含 API Key）永不读取
//! - 与线上采集完全解耦：不写 usage_cursor、不碰插件门控，重跑安全（幂等跳过）

use std::collections::{HashMap, HashSet};
use std::io::Read;

use chrono::{Local, TimeZone};
use serde::Serialize;

use crate::fsutil::Res;
use crate::store::Store;

/// 事件来源标记（usage_events.source），与 rollout / session-snapshot 并列
const SOURCE: &str = "ccswitch-import";
/// request_id 前缀：明细与日汇总合成事件共用命名空间，重跑幂等的判定依据
const PREFIX_DETAIL: &str = "ccs:";
const PREFIX_ROLL: &str = "ccs-roll:";

#[derive(Serialize)]
pub struct AgentSum {
    pub agent: String,
    pub events: usize,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
}

#[derive(Serialize)]
pub struct ImportReport {
    /// 明细表总行数 / 本次新插入事件数
    pub detail_total: usize,
    pub detail_new: usize,
    /// 日汇总有效行数 / 合成事件数
    pub rollup_total: usize,
    pub rollup_new: usize,
    /// 两段数据的本地时间范围（无数据为 null）
    pub detail_range: (Option<String>, Option<String>),
    pub rollup_range: (Option<String>, Option<String>),
    /// 按 agent 汇总本次新导入的 token
    pub by_agent: Vec<AgentSum>,
}

/// 待插入事件（明细与 rollup 合成统一到这一层再落库）
struct Event {
    agent: String,
    ts: String,
    model: Option<String>,
    provider: Option<String>,
    session_id: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    request_id: String,
}

/// rusqlite / io 错误 → String（Res 错误类型），集中一处免得逐点 map_err
fn s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// 打开源备份：二进制 sqlite 直接开；.sql 文本 dump 建临时库执行（返回库连接 + 临时文件路径）
fn open_source(path: &str) -> Res<(rusqlite::Connection, Option<std::path::PathBuf>)> {
    let p = std::path::Path::new(path);
    if !p.is_file() {
        return Err(format!("备份文件不存在：{path}"));
    }
    let mut head = [0u8; 16];
    std::fs::File::open(p)
        .map_err(s)?
        .read_exact(&mut head)
        .map_err(|e| format!("读文件头失败：{e}"))?;
    if &head == b"SQLite format 3\0" {
        return Ok((rusqlite::Connection::open(p).map_err(|e| e.to_string())?, None));
    }
    // 文本 SQL dump：临时空库整批执行（dump 仅 CREATE/INSERT，无 dot 命令）
    let sql = std::fs::read_to_string(p).map_err(s)?;
    let tmp = std::env::temp_dir().join(format!(
        "agenthub-ccsimport-{}.db",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let conn = rusqlite::Connection::open(&tmp).map_err(|e| e.to_string())?;
    conn.execute_batch(&sql).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("SQL 备份执行失败（不是有效的 cc-switch SQL 导出？）：{e}")
    })?;
    Ok((conn, Some(tmp)))
}

/// 清理临时库（含 -wal/-shm 残留），尽力而为
fn cleanup(tmp: Option<std::path::PathBuf>) {
    if let Some(t) = tmp {
        for suffix in ["", "-wal", "-shm"] {
            let mut name = t.as_os_str().to_owned();
            name.push(suffix);
            let _ = std::fs::remove_file(std::path::PathBuf::from(name));
        }
    }
}

fn sec_to_local(secs: i64) -> Option<String> {
    Local
        .timestamp_opt(secs, 0)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
}

/// provider_id → 展示名：providers 表有名称用名称，`_session`（会话日志聚合桶）给可读文案，
/// 其余未知 id 原样保留
fn provider_name(map: &HashMap<String, String>, pid: &str) -> Option<String> {
    if pid.is_empty() {
        return None;
    }
    if pid == "_session" {
        return Some("会话日志".into());
    }
    Some(map.get(pid).cloned().unwrap_or_else(|| pid.to_string()))
}

/// token 均分：v 分 n 份，第 i 份 = 商 +（i 落在余数段 +1），保证总量精确守恒
fn split_token(v: i64, n: i64, i: i64) -> i64 {
    v / n + i64::from(i < v % n)
}

/// 导入入口：读备份 → 内存组装事件 → 幂等落库 usage_events，返回报告
pub fn import_backup(store: &Store, path: &str) -> Res<ImportReport> {
    let (src, tmp) = open_source(path)?;
    let report = do_import(store, &src);
    cleanup(tmp);
    report
}

fn do_import(store: &Store, src: &rusqlite::Connection) -> Res<ImportReport> {
    // provider 名称映射（只取 id/name 两列，settings_config 含 Key 永不读取）
    let mut providers: HashMap<String, String> = HashMap::new();
    if let Ok(mut stmt) = src.prepare("SELECT id, name FROM providers") {
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            if let (Ok(id), Ok(name)) = (r.get::<_, String>(0), r.get::<_, String>(1)) {
                providers.insert(id, name);
            }
        }
    }

    // ── 明细 → 逐条事件 ──
    let mut detail = Vec::new();
    {
        let mut stmt = src
            .prepare(
                "SELECT request_id, app_type, model, provider_id, session_id,
                        input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                        created_at
                 FROM proxy_request_logs",
            )
            .map_err(|_| "不是有效的 cc-switch SQL 备份：缺 proxy_request_logs 表".to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(s)? {
            let rid: String = r.get(0).map_err(s)?;
            let agent: String = r.get(1).map_err(s)?;
            let model: Option<String> = r.get(2).map_err(s)?;
            let pid: String = r.get(3).map_err(s)?;
            let session_id: Option<String> = r.get(4).map_err(s)?;
            let input: i64 = r.get(5).map_err(s)?;
            let output: i64 = r.get(6).map_err(s)?;
            let cache_read: i64 = r.get(7).map_err(s)?;
            let cache_creation: i64 = r.get(8).map_err(s)?;
            let created: i64 = r.get(9).map_err(s)?;
            if agent.is_empty() {
                continue;
            }
            let ts = match sec_to_local(created) {
                Some(t) => t,
                None => continue, // 时间戳非法，跳过（不入库就无法按日期聚合）
            };
            detail.push(Event {
                agent,
                ts,
                model: model.filter(|m| !m.trim().is_empty()),
                provider: provider_name(&providers, &pid),
                session_id,
                input_tokens: input + cache_creation, // 并入输入（WB 口径：输入含缓存写入）
                output_tokens: output,
                cache_read_tokens: cache_read,
                request_id: format!("{PREFIX_DETAIL}{rid}"),
            });
        }
    }

    // ── 日汇总 → 按 request_count 合成事件（30 天前的老数据只有这里） ──
    let mut rollup = Vec::new();
    {
        let mut stmt = src
            .prepare(
                "SELECT date, app_type, provider_id, model, request_model, pricing_model,
                        request_count,
                        input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens
                 FROM usage_daily_rollups",
            )
            .map_err(|_| "不是有效的 cc-switch SQL 备份：缺 usage_daily_rollups 表".to_string())?;
        let mut rows = stmt.query([]).map_err(s)?;
        while let Some(r) = rows.next().map_err(s)? {
            let date: String = r.get(0).map_err(s)?;
            let agent: String = r.get(1).map_err(s)?;
            let pid: String = r.get(2).map_err(s)?;
            let model: String = r.get(3).map_err(s)?;
            let request_model: String = r.get(4).map_err(s)?;
            let pricing_model: String = r.get(5).map_err(s)?;
            let count: i64 = r.get(6).map_err(s)?;
            let input: i64 = r.get(7).map_err(s)?;
            let output: i64 = r.get(8).map_err(s)?;
            let cache_read: i64 = r.get(9).map_err(s)?;
            let cache_creation: i64 = r.get(10).map_err(s)?;
            if agent.is_empty() || date.is_empty() {
                continue;
            }
            let input = input + cache_creation;
            if count <= 0 && input <= 0 && output <= 0 && cache_read <= 0 {
                continue; // 空行（如 codex 那行 0-token 记录）不合成事件
            }
            let n = count.max(1).min(5000); // 防御：异常大 count 也不无限膨胀
            let ts = format!("{date} 12:00:00"); // 落在汇总日本地正午，date(ts) 聚合归位
            let provider = provider_name(&providers, &pid);
            let model_opt = if model.trim().is_empty() { None } else { Some(model.clone()) };
            // 合成 id 用整行主键（date/app/provider/model/request_model/pricing_model）的
            // FNV 指纹：同日同模型多 provider 的行不会撞车丢数据
            let row_key = format!("{date}|{agent}|{pid}|{model}|{request_model}|{pricing_model}");
            let fp = crate::fsutil::fnv1a(&row_key);
            for i in 0..n {
                rollup.push(Event {
                    agent: agent.clone(),
                    ts: ts.clone(),
                    model: model_opt.clone(),
                    provider: provider.clone(),
                    session_id: None,
                    input_tokens: split_token(input, n, i),
                    output_tokens: split_token(output, n, i),
                    cache_read_tokens: split_token(cache_read, n, i),
                    request_id: format!("{PREFIX_ROLL}{fp}:{i}"),
                });
            }
        }
    }

    // ── 幂等落库：先查已存在的 ccs* 事件 id，只插新增 ──
    let mut existing: HashSet<String> = HashSet::new();
    store.with(|hub| {
        let mut stmt = hub
            .prepare("SELECT request_id FROM usage_events WHERE request_id LIKE 'ccs%'")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(s)?;
        while let Some(r) = rows.next().map_err(s)? {
            existing.insert(r.get::<_, String>(0).map_err(s)?);
        }
        Ok(())
    })?;

    let mut detail_new = 0usize;
    let mut rollup_new = 0usize;
    let mut by_agent: HashMap<String, AgentSum> = HashMap::new();
    store.with(|hub| {
        let tx = hub.unchecked_transaction().map_err(|e| e.to_string())?;
        for e in detail.iter().chain(rollup.iter()) {
            if existing.contains(&e.request_id) {
                continue;
            }
            tx.execute(
                "INSERT OR IGNORE INTO usage_events(agent, ts, model, provider, session_id, input_tokens, output_tokens, cache_read_tokens, source, request_id, account)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, '')",
                rusqlite::params![
                    e.agent,
                    e.ts,
                    e.model,
                    e.provider,
                    e.session_id,
                    e.input_tokens,
                    e.output_tokens,
                    e.cache_read_tokens,
                    SOURCE,
                    e.request_id
                ],
            )
            .map_err(|e| e.to_string())?;
            let is_new = tx.changes() > 0;
            let sum = by_agent.entry(e.agent.clone()).or_insert(AgentSum {
                agent: e.agent.clone(),
                events: 0,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
            });
            if is_new {
                sum.events += 1;
                sum.input_tokens += e.input_tokens;
                sum.output_tokens += e.output_tokens;
                sum.cache_read_tokens += e.cache_read_tokens;
                if e.request_id.starts_with(PREFIX_DETAIL) {
                    detail_new += 1;
                } else {
                    rollup_new += 1;
                }
            }
        }
        tx.commit().map_err(|e| e.to_string())
    })?;

    let range = |list: &[&Event]| -> (Option<String>, Option<String>) {
        let mut all: Vec<&str> = list.iter().map(|e| e.ts.as_str()).collect();
        all.sort();
        match all.as_slice() {
            [] => (None, None),
            [only] => (Some((*only).to_string()), Some((*only).to_string())),
            _ => (
                Some(all.first().unwrap().to_string()),
                Some(all.last().unwrap().to_string()),
            ),
        }
    };
    let detail_list: Vec<&Event> = detail.iter().collect();
    let rollup_list: Vec<&Event> = rollup.iter().collect();

    Ok(ImportReport {
        detail_total: detail.len(),
        detail_new,
        rollup_total: rollup_list.len(),
        rollup_new,
        detail_range: range(&detail_list),
        rollup_range: range(&rollup_list),
        by_agent: {
            let mut v: Vec<AgentSum> = by_agent.into_values().collect();
            v.sort_by(|a, b| b.events.cmp(&a.events));
            v
        },
    })
}
