//! usage —— M9 用量分析引擎。
//!
//! 双源采集（方案 §2.7 / 隐私红线：正文永不入库）：
//! - ZCode：`~/.zcode/cli/rollout/model-io-sess*.jsonl` 逐次流水，按字节偏移增量解析
//!   （实测字段：requestId 去重键、completedAt ISO、model.modelId/providerId、
//!   response.usage.{inputTokens,outputTokens,cacheReadTokens} —— camelCase）
//! - WorkBuddy：`~/.workbuddy/workbuddy.db` 只读连接，`session_usage.used` 快照差分
//!   （sessions.model 提供模型；无逐次拆分 → source='session-snapshot'，UI 注记「会话级」）

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{Local, TimeZone};
use rusqlite::OpenFlags;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::adapters::{claude, wb, zcode};
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

fn emit_progress(app: Option<&AppHandle>, stage: &str, done: usize, total: usize) {
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

fn ts_to_local(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

// ─── ZCode 采集：db.sqlite model_usage 全量账本 ───
// 实测（2026-10-31）：rollout jsonl 只保留近期 3 个会话文件；完整账本在
// ~/.zcode/cli/db/db.sqlite 的 model_usage 表（含 input/output/reasoning/cache 拆分、
// completed_at、model/provider），SUM(input_tokens)=18.87 亿 与官方「累计 Token 18.9 亿」吻合。
// 全表扫描 + request_id(=model_usage.id) 幂等去重，无需游标。

fn collect_zcode(store: &Store, app: Option<&AppHandle>) -> Res<usize> {
    let db = zcode::dir().join("cli").join("db").join("db.sqlite");
    if !db.is_file() {
        return Ok(0);
    }
    let conn = rusqlite::Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("只读打开 zcode db.sqlite 失败: {e}"))?;

    let mut stmt = conn
        .prepare(
            "SELECT id, completed_at, IFNULL(model_id,''), IFNULL(provider_id,''),
                    IFNULL(session_id,''), IFNULL(input_tokens,0), IFNULL(output_tokens,0),
                    IFNULL(cache_read_input_tokens,0), IFNULL(cache_creation_input_tokens,0),
                    IFNULL(status,'')
             FROM model_usage",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, String>(9)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut records: Vec<(String, i64, String, String, String, i64, i64, i64, i64, String)> =
        Vec::new();
    for r in rows {
        records.push(r.map_err(|e| e.to_string())?);
    }
    drop(stmt);

    // 分批短锁插入（每批 400 条），批次间让出互斥锁给 UI 查询，避免页面卡死
    let total = records.len();
    let mut inserted = 0;
    for (bi, chunk) in records.chunks(400).enumerate() {
        inserted += store.with(|hub| {
            let mut n = 0;
            for (id, completed_at, model, provider, session_id, inp, out, cache_read, cache_write, _status) in
                chunk
            {
                if *completed_at <= 0 {
                    continue;
                }
                let r = hub.execute(
                    "INSERT OR IGNORE INTO usage_events(agent, ts, model, provider, session_id, input_tokens, output_tokens, cache_read_tokens, source, request_id)
                     VALUES ('zcode', ?1, ?2, ?3, ?4, ?5, ?6, ?7, 'model-usage', ?8)",
                    rusqlite::params![
                        ts_to_local(*completed_at),
                        model,
                        provider,
                        session_id,
                        inp,
                        out,
                        cache_read + cache_write,
                        id
                    ],
                );
                if matches!(r, Ok(c) if c > 0) {
                    n += 1;
                }
            }
            Ok(n)
        })?;
        emit_progress(app, "zcode", (bi + 1) * 400.min(total), total);
    }
    Ok(inserted)
}

// ─── ZCode rollout 增量解析（已被 model_usage 全量账本取代，保留兜底） ───

#[allow(dead_code)]
fn collect_zcode_rollout(store: &Store) -> Res<usize> {
    let dir = zcode::rollout_dir();
    let mut inserted = 0;
    if !dir.is_dir() {
        return Ok(0);
    }
    let entries: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            name.starts_with("model-io-sess") && name.ends_with(".jsonl")
        })
        .collect();

    for path in entries {
        let fp = path.to_string_lossy().to_string();
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let file_len = meta.len() as i64;

        let (last_offset, last_mtime) = store.with(|conn| {
            Ok(conn
                .query_row(
                    "SELECT byte_offset, IFNULL(mtime,'0') FROM usage_cursor WHERE file_path = ?1",
                    rusqlite::params![fp],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?
                                .parse::<i64>()
                                .unwrap_or(0),
                        ))
                    },
                )
                .unwrap_or((0, 0)))
        })?;
        // 未变化跳过；mtime 变了但游标越界（文件轮转/重写）则整文件重扫，靠 request_id 去重
        if mtime == last_mtime && last_offset >= file_len {
            continue;
        }
        let start = if mtime == last_mtime { last_offset } else { 0 };

        let Ok(mut file) = std::fs::File::open(&path) else { continue };
        if start > 0 {
            file.seek(SeekFrom::Start(start as u64)).map_err(|e| e.to_string())?;
        }
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).map_err(|e| e.to_string())?;

        // 只消费完整行：末行无换行符则留在游标外，待下次补读
        let text_start = start as usize;
        let full = &buf;
        let mut consumed = 0usize;
        let mut lines_bytes: Vec<&[u8]> = Vec::new();
        let mut pos = 0usize;
        while let Some(nl) = full[pos..].iter().position(|&b| b == b'\n') {
            let line = &full[pos..pos + nl];
            lines_bytes.push(line);
            pos += nl + 1;
            consumed = pos;
        }
        let new_offset = (text_start + consumed) as i64;

        inserted += store.with(|conn| {
            let mut n = 0;
            for line in lines_bytes {
                if line.len() > 2 * 1024 * 1024 {
                    continue; // 异常大行防护
                }
                let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
                    continue;
                };
                let Some(request_id) = v.get("requestId").and_then(|x| x.as_str()) else {
                    continue;
                };
                let completed = v
                    .get("completedAt")
                    .and_then(|x| x.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|t| t.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string());
                let Some(ts) = completed else { continue };
                let model = v
                    .get("model")
                    .and_then(|m| m.get("modelId"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                let provider = v
                    .get("model")
                    .and_then(|m| m.get("providerId"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                let usage = v.get("response").and_then(|r| r.get("usage"));
                let inp = usage
                    .and_then(|u| u.get("inputTokens"))
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0) as i64;
                let out = usage
                    .and_then(|u| u.get("outputTokens"))
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0) as i64;
                let cache = usage
                    .and_then(|u| u.get("cacheReadTokens"))
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0) as i64;
                // 正文（request.messages / response.text 等）绝不入库：只取上面字段
                let r = conn.execute(
                    "INSERT OR IGNORE INTO usage_events(agent, ts, model, provider, session_id, input_tokens, output_tokens, cache_read_tokens, source, request_id)
                     VALUES ('zcode', ?1, ?2, ?3, ?4, ?5, ?6, ?7, 'rollout', ?8)",
                    rusqlite::params![
                        ts,
                        model,
                        provider,
                        v.get("sessionId").and_then(|x| x.as_str()).unwrap_or(""),
                        inp,
                        out,
                        cache,
                        request_id
                    ],
                );
                if matches!(r, Ok(c) if c > 0) {
                    n += 1;
                }
            }
            Ok(n)
        })?;

        store.with(|conn| {
            conn.execute(
                "INSERT INTO usage_cursor(source, file_path, byte_offset, mtime) VALUES ('rollout', ?1, ?2, ?3)
                 ON CONFLICT(file_path) DO UPDATE SET byte_offset = excluded.byte_offset, mtime = excluded.mtime",
                rusqlite::params![fp, new_offset, mtime.to_string()],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })?;
    }
    Ok(inserted)
}

// ─── WorkBuddy 采集 v2：projects jsonl 请求级真实 usage ───
// 实测（2026-10-31，参考 workbuddy-token-dashboard skill）：
// ~/.workbuddy/projects/**/*.jsonl 每条记录的 providerData.rawUsage / usage 是
// 请求级真实 usage（rawUsage.credit = 官方逐请求积分，与 App 账单同源）。
// 口径：input = prompt - 缓存命中（未命中输入），cache_read = prompt_cache_hit_tokens，
//       output = completion_tokens；合计 = total_tokens。

struct WbEvent {
    ts_ms: i64,
    model: String,
    session: String,
    /// 账号 uid（sessions.user_id 关联；查不到 = ''）
    account: String,
    inp: i64,
    ot: i64,
    ch: i64,
    credit: f64,
    request_id: String,
}

/// WB 多账号归属映射：session_id → user_id（只读 workbuddy.db，仅取元数据）
fn wb_session_accounts() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let db = wb::dir().join("workbuddy.db");
    if !db.is_file() {
        return map;
    }
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return map;
    };
    let Ok(mut stmt) =
        conn.prepare("SELECT id, user_id FROM sessions WHERE IFNULL(user_id,'') != ''")
    else {
        return map;
    };
    if let Ok(rows) = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) {
        for r in rows.flatten() {
            map.insert(r.0, r.1);
        }
    }
    map
}

/// 纯解析不持锁：读一个 jsonl 增量段，返回解析出的事件与新偏移
fn collect_wb_jsonl_file(
    path: &Path,
    offset: i64,
    accounts: &std::collections::HashMap<String, String>,
) -> Res<(Vec<WbEvent>, i64)> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Ok((Vec::new(), offset));
    };
    use std::io::{Read, Seek};
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset as u64))
            .map_err(|e| e.to_string())?;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let text_start = offset as usize;
    let mut consumed = 0usize;
    let mut pos = 0usize;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut events: Vec<WbEvent> = Vec::new();

    {
        while let Some(nl) = buf[pos..].iter().position(|&b| b == b'\n') {
            let line = &buf[pos..pos + nl];
            pos += nl + 1;
            consumed = pos;
            let Ok(lstr) = std::str::from_utf8(line) else {
                continue;
            };
            if !lstr.contains("rawUsage") && !lstr.contains("totalTokens") {
                continue; // 快速预筛，绝大多数行是正文/工具记录
            }
            if line.len() > 4 * 1024 * 1024 {
                continue;
            }
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
                continue;
            };
            let Some(pd) = v.get("providerData").and_then(|x| x.as_object()) else {
                continue;
            };
            let ru = pd.get("rawUsage").and_then(|x| x.as_object());
            let u = pd.get("usage").and_then(|x| x.as_object());
            let get_i = |obj: Option<&serde_json::Map<String, serde_json::Value>>,
                         keys: &[&str]|
             -> Option<i64> {
                for k in keys {
                    if let Some(o) = obj {
                        if let Some(x) = o.get(*k) {
                            if let Some(n) = x.as_i64() {
                                return Some(n);
                            }
                            if let Some(n) = x.as_f64() {
                                return Some(n as i64);
                            }
                        }
                    }
                }
                None
            };
            let tt = get_i(ru, &["total_tokens"]).or_else(|| get_i(u, &["totalTokens"]));
            let Some(tt) = tt else { continue };
            let it = get_i(ru, &["prompt_tokens"]).or_else(|| get_i(u, &["inputTokens"])).unwrap_or(0);
            let ot = get_i(ru, &["completion_tokens"])
                .or_else(|| get_i(u, &["outputTokens"]))
                .unwrap_or(0);
            let ch = get_i(ru, &["prompt_cache_hit_tokens"])
                .or_else(|| {
                    get_i(
                        ru,
                        &["prompt_tokens_details"], // 嵌套 cached_tokens 由下方取
                    )
                })
                .unwrap_or(0);
            // 嵌套兜底：prompt_tokens_details.cached_tokens
            let ch = if ch > 0 {
                ch
            } else {
                ru.and_then(|r| r.get("prompt_tokens_details"))
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(|x| x.as_i64())
                    .unwrap_or(0)
            };
            let credit = ru
                .and_then(|r| r.get("credit"))
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0);
            let model = pd
                .get("model")
                .or_else(|| pd.get("requestModelId"))
                .or_else(|| pd.get("requestModelName"))
                .and_then(|x| x.as_str())
                .unwrap_or("unknown")
                .to_string();
            let ts_ms = v.get("timestamp").and_then(|x| x.as_i64()).unwrap_or(0);
            if ts_ms <= 0 {
                continue;
            }
            let request_id = crate::fsutil::fnv1a(&format!(
                "{stem}|{model}|{ts_ms}|{tt}|{it}|{ot}|{credit}"
            ));
            let session = v
                .get("sessionId")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            events.push(WbEvent {
                ts_ms,
                model,
                account: accounts.get(&session).cloned().unwrap_or_default(),
                session,
                inp: (it - ch).max(0),
                ot,
                ch,
                credit,
                request_id,
            });
        }
        Ok((events, (text_start + consumed) as i64))
    }
}

/// 分批短锁插入（每批 400 条），批次间让出互斥锁给 UI 查询
fn insert_wb_events(store: &Store, events: &[WbEvent]) -> Res<usize> {
    let mut inserted = 0;
    for chunk in events.chunks(400) {
        inserted += store.with(|hub| {
            let mut n = 0;
            for e in chunk {
                let r = hub.execute(
                    "INSERT OR IGNORE INTO usage_events(agent, ts, model, provider, session_id, input_tokens, output_tokens, cache_read_tokens, credit, source, request_id, account)
                     VALUES ('workbuddy', ?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, 'wb-projects', ?8, ?9)",
                    rusqlite::params![
                        ts_to_local(e.ts_ms),
                        e.model,
                        e.session,
                        e.inp,
                        e.ot,
                        e.ch,
                        e.credit,
                        e.request_id,
                        e.account
                    ],
                );
                if matches!(r, Ok(c) if c > 0) {
                    n += 1;
                }
            }
            Ok(n)
        })?;
    }
    Ok(inserted)
}

fn collect_wb_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_wb_files(&p, out);
        } else if p.extension().map(|x| x == "jsonl").unwrap_or(false) {
            out.push(p);
        }
    }
}

fn collect_wb_projects(store: &Store, app: Option<&AppHandle>) -> Res<usize> {
    let root = wb::dir().join("projects");
    if !root.is_dir() {
        return Ok(0);
    }
    let accounts = wb_session_accounts();
    let mut files = Vec::new();
    collect_wb_files(&root, &mut files);
    let total = files.len();
    let mut total_inserted = 0;
    for (i, p) in files.iter().enumerate() {
        let fp = p.to_string_lossy().to_string();
        let meta = match p.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let file_len = meta.len() as i64;
        let (last_offset, last_mtime) = store.with(|conn| {
            Ok(conn
                .query_row(
                    "SELECT byte_offset, IFNULL(mtime,'0') FROM usage_cursor WHERE file_path = ?1",
                    rusqlite::params![fp],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?
                                .parse::<i64>()
                                .unwrap_or(0),
                        ))
                    },
                )
                .unwrap_or((0, 0)))
        })?;
        if mtime == last_mtime && last_offset >= file_len {
            emit_progress(app, "workbuddy", i + 1, total);
            continue;
        }
        let start = if mtime == last_mtime { last_offset } else { 0 };
        let (events, new_offset) = collect_wb_jsonl_file(p, start, &accounts)?;
        total_inserted += insert_wb_events(store, &events)?;
        store.with(|conn| {
            conn.execute(
                "INSERT INTO usage_cursor(source, file_path, byte_offset, mtime) VALUES ('wb-projects', ?1, ?2, ?3)
                 ON CONFLICT(file_path) DO UPDATE SET byte_offset = excluded.byte_offset, mtime = excluded.mtime",
                rusqlite::params![fp, new_offset, mtime.to_string()],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })?;
        emit_progress(app, "workbuddy", i + 1, total);
    }
    Ok(total_inserted)
}

// ─── Claude Code 采集：~/.claude/projects/**/*.jsonl 逐请求 message.usage ───
// 每条 assistant 记录：message.usage.{input_tokens, output_tokens,
// cache_read_input_tokens, cache_creation_input_tokens} + message.model + timestamp(RFC3339)。
// 口径（与 ccsimport 一致）：input = input_tokens + cache_creation（缓存写入并入输入），
// cache_read = cache_read；账号域不存在，account 恒 ''。

struct ClaudeEvent {
    ts: String,
    model: String,
    session: String,
    inp: i64,
    ot: i64,
    ch: i64,
    request_id: String,
}

/// 纯解析不持锁：读一个 claude jsonl 增量段，返回解析出的事件与新偏移
fn collect_claude_jsonl_file(path: &Path, offset: i64) -> Res<(Vec<ClaudeEvent>, i64)> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Ok((Vec::new(), offset));
    };
    file.seek(SeekFrom::Start(offset.max(0) as u64))
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let text_start = offset as usize;
    let mut consumed = 0usize;
    let mut pos = 0usize;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut events: Vec<ClaudeEvent> = Vec::new();

    while let Some(nl) = buf[pos..].iter().position(|&b| b == b'\n') {
        let line = &buf[pos..pos + nl];
        pos += nl + 1;
        consumed = pos;
        let Ok(lstr) = std::str::from_utf8(line) else {
            continue;
        };
        if !lstr.contains("\"usage\"") {
            continue; // 快速预筛，绝大多数行是正文/工具记录
        }
        if line.len() > 4 * 1024 * 1024 {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        let Some(msg) = v.get("message").and_then(|x| x.as_object()) else {
            continue;
        };
        let Some(u) = msg.get("usage").and_then(|x| x.as_object()) else {
            continue;
        };
        let get_i = |keys: &[&str]| -> i64 {
            for k in keys {
                match u.get(*k) {
                    Some(serde_json::Value::Number(n)) => {
                        if let Some(x) = n.as_i64() {
                            return x;
                        }
                        if let Some(x) = n.as_f64() {
                            return x as i64;
                        }
                    }
                    _ => {}
                }
            }
            0
        };
        let it = get_i(&["input_tokens"]);
        let cc = get_i(&["cache_creation_input_tokens"]);
        let ot = get_i(&["output_tokens"]);
        let ch = get_i(&["cache_read_input_tokens"]);
        if it + cc + ot + ch <= 0 {
            continue; // 无 token 的 usage 行不产生事件
        }
        let model = msg
            .get("model")
            .or_else(|| v.get("model"))
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
            .to_string();
        let ts_raw = v.get("timestamp").and_then(|x| x.as_str()).unwrap_or("");
        let Ok(t) = chrono::DateTime::parse_from_rfc3339(ts_raw) else {
            continue; // 无时间戳无法按日聚合，跳过
        };
        let ts = t.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string();
        let session = v
            .get("sessionId")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let inp = it + cc;
        let request_id = crate::fsutil::fnv1a(&format!(
            "{stem}|{model}|{}|{inp}|{ot}|{ch}",
            t.timestamp_millis()
        ));
        events.push(ClaudeEvent {
            ts,
            model,
            session,
            inp,
            ot,
            ch,
            request_id,
        });
    }
    Ok((events, (text_start + consumed) as i64))
}

/// 分批短锁插入（每批 400 条），批次间让出互斥锁给 UI 查询
fn insert_claude_events(store: &Store, events: &[ClaudeEvent]) -> Res<usize> {
    let mut inserted = 0;
    for chunk in events.chunks(400) {
        inserted += store.with(|hub| {
            let mut n = 0;
            for e in chunk {
                let r = hub.execute(
                    "INSERT OR IGNORE INTO usage_events(agent, ts, model, provider, session_id, input_tokens, output_tokens, cache_read_tokens, credit, source, request_id, account)
                     VALUES ('claude', ?1, ?2, NULL, ?3, ?4, ?5, ?6, 0, 'claude-projects', ?7, '')",
                    rusqlite::params![e.ts, e.model, e.session, e.inp, e.ot, e.ch, e.request_id],
                );
                if matches!(r, Ok(c) if c > 0) {
                    n += 1;
                }
            }
            Ok(n)
        })?;
    }
    Ok(inserted)
}

fn collect_claude_projects(store: &Store, app: Option<&AppHandle>) -> Res<usize> {
    let root = claude::projects_dir();
    if !root.is_dir() {
        return Ok(0);
    }
    let mut files = Vec::new();
    collect_wb_files(&root, &mut files);
    let total = files.len();
    let mut total_inserted = 0;
    for (i, p) in files.iter().enumerate() {
        let fp = p.to_string_lossy().to_string();
        let meta = match p.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let file_len = meta.len() as i64;
        let (last_offset, last_mtime) = store.with(|conn| {
            Ok(conn
                .query_row(
                    "SELECT byte_offset, IFNULL(mtime,'0') FROM usage_cursor WHERE file_path = ?1",
                    rusqlite::params![fp],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?
                                .parse::<i64>()
                                .unwrap_or(0),
                        ))
                    },
                )
                .unwrap_or((0, 0)))
        })?;
        if mtime == last_mtime && last_offset >= file_len {
            emit_progress(app, "claude", i + 1, total);
            continue;
        }
        let start = if mtime == last_mtime { last_offset } else { 0 };
        let (events, new_offset) = collect_claude_jsonl_file(p, start)?;
        total_inserted += insert_claude_events(store, &events)?;
        store.with(|conn| {
            conn.execute(
                "INSERT INTO usage_cursor(source, file_path, byte_offset, mtime) VALUES ('claude-projects', ?1, ?2, ?3)
                 ON CONFLICT(file_path) DO UPDATE SET byte_offset = excluded.byte_offset, mtime = excluded.mtime",
                rusqlite::params![fp, new_offset, mtime.to_string()],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })?;
        emit_progress(app, "claude", i + 1, total);
    }
    Ok(total_inserted)
}

// ─── WorkBuddy 会话快照差分（projects 日志缺失时的兜底） ───

fn collect_wb_snapshot(store: &Store) -> Res<usize> {
    let db = wb::dir().join("workbuddy.db");
    if !db.is_file() {
        return Ok(0);
    }
    let conn = rusqlite::Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("只读打开 workbuddy.db 失败: {e}"))?;

    let mut stmt = conn
        .prepare(
            "SELECT u.session_id, IFNULL(s.model,''), IFNULL(u.used,0), u.updated_at,
                    IFNULL(s.user_id,'')
             FROM session_usage u LEFT JOIN sessions s ON s.id = u.session_id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut sessions: Vec<(String, String, i64, i64, String)> = Vec::new();
    for r in rows {
        sessions.push(r.map_err(|e| e.to_string())?);
    }
    drop(stmt);
    // 显式关闭只读连接（作用域结束自动 drop）

    let mut inserted = 0;
    store.with(|hub| {
        for (session_id, model, used, updated_ms, account) in sessions {
            let last_used: i64 = hub
                .query_row(
                    "SELECT last_used FROM wb_session_seen WHERE session_id = ?1",
                    rusqlite::params![session_id],
                    |row| row.get(0),
                )
                .unwrap_or(-1);
            if used <= last_used {
                continue; // 无增长（含快照回退）不产生事件
            }
            let diff = if last_used < 0 { used } else { used - last_used };
            if diff > 0 {
                hub.execute(
                    "INSERT INTO usage_events(agent, ts, model, provider, session_id, input_tokens, output_tokens, cache_read_tokens, source, request_id, account)
                     VALUES ('workbuddy', ?1, ?2, NULL, ?3, ?4, 0, 0, 'session-snapshot', NULL, ?5)",
                    rusqlite::params![ts_to_local(updated_ms), model, session_id, diff, account],
                )
                .map_err(|e| e.to_string())?;
                inserted += 1;
            }
            hub.execute(
                "INSERT INTO wb_session_seen(session_id, last_used, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(session_id) DO UPDATE SET last_used = excluded.last_used, updated_at = excluded.updated_at",
                rusqlite::params![session_id, used, ts_to_local(updated_ms)],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    })?;
    Ok(inserted)
}

/// 采集入口：先 ZCode 后 WB 再 Claude，返回 (zcode新增, wb新增, claude新增)。
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
    // 一次性迁移：rollout（ZCode）与 session-snapshot（WB）来源事件分别被
    // model_usage 全量账本与 projects 请求级账本取代（request_id 体系不同，不清会双计）
    store.with(|conn| {
        conn.execute(
            "DELETE FROM usage_events WHERE agent = 'zcode' AND source = 'rollout'",
            [],
        )
        .map_err(|e| e.to_string())?;
        conn.execute(
            "DELETE FROM usage_events WHERE agent = 'workbuddy' AND source = 'session-snapshot'",
            [],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })?;
    // 插件门控：停用的 agent 适配器不采集（历史事件保留在库，重新启用后恢复增量）
    let enabled = crate::plugins::enabled_ids(store)?;
    let zc_on = enabled.iter().any(|id| id == "zcode");
    let wb_on = enabled.iter().any(|id| id == "workbuddy");
    let cl_on = enabled.iter().any(|id| id == "claude");
    let z = if zc_on { collect_zcode(store, app)? } else { 0 };
    let w = if !wb_on {
        0
    } else {
        let has_projects = crate::adapters::wb::dir().join("projects").is_dir();
        if has_projects {
            collect_wb_projects(store, app)?
        } else {
            collect_wb_snapshot(store)?
        }
    };
    let c = if cl_on {
        collect_claude_projects(store, app)?
    } else {
        0
    };
    backfill_wb_accounts(store)?;
    Ok((z, w, c))
}

/// 存量 WB 事件账号回填：session_id → sessions.user_id（幂等，只补 account='' 的行）
fn backfill_wb_accounts(store: &Store) -> Res<usize> {
    let accounts = wb_session_accounts();
    if accounts.is_empty() {
        return Ok(0);
    }
    let mut updated = 0;
    store.with(|conn| {
        for (session, uid) in &accounts {
            updated += conn
                .execute(
                    "UPDATE usage_events SET account = ?2
                     WHERE agent = 'workbuddy' AND session_id = ?1 AND account = ''",
                    rusqlite::params![session, uid],
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(updated)
    })
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
