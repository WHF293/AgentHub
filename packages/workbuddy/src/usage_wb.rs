//! usage_wb —— WB 用量采集：projects jsonl 请求级真实 usage + 会话快照差分兜底 + 账号回填。
//! （原 core/usage.rs WB 段平移；入口由 AgentAdapter::collect_usage 分发）
//!
//! 实测（2026-10-31，参考 workbuddy-token-dashboard skill）：
//! ~/.workbuddy/projects/**/*.jsonl 每条记录的 providerData.rawUsage / usage 是
//! 请求级真实 usage（rawUsage.credit = 官方逐请求积分，与 App 账单同源）。
//! 口径：input = prompt - 缓存命中（未命中输入），cache_read = prompt_cache_hit_tokens，
//!       output = completion_tokens；合计 = total_tokens。

use std::path::Path;

use rusqlite::OpenFlags;
use tauri::AppHandle;

use agenthub_core::fsutil::Res;
use agenthub_core::store::Store;
use agenthub_core::usage::{collect_jsonl_files, cursor_get, cursor_set, emit_progress, ts_to_local};

use crate::adapter;

/// 一次性清理：旧 session-snapshot 快照来源被 projects 请求级账本取代
/// （request_id 体系不同，不清会双计）
fn cleanup_legacy(store: &Store) -> Res<()> {
    store.with(|conn| {
        conn.execute(
            "DELETE FROM usage_events WHERE agent = 'workbuddy' AND source = 'session-snapshot'",
            [],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })
}

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
    let db = adapter::dir().join("workbuddy.db");
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
            let request_id = agenthub_core::fsutil::fnv1a(&format!(
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

fn collect_wb_projects(store: &Store, app: Option<&AppHandle>) -> Res<usize> {
    let root = adapter::dir().join("projects");
    if !root.is_dir() {
        return Ok(0);
    }
    let accounts = wb_session_accounts();
    let mut files = Vec::new();
    collect_jsonl_files(&root, &mut files);
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
        let (last_offset, last_mtime) = cursor_get(store, &fp)?;
        if mtime == last_mtime && last_offset >= file_len {
            emit_progress(app, "workbuddy", i + 1, total);
            continue;
        }
        let start = if mtime == last_mtime { last_offset } else { 0 };
        let (events, new_offset) = collect_wb_jsonl_file(p, start, &accounts)?;
        total_inserted += insert_wb_events(store, &events)?;
        cursor_set(store, "wb-projects", &fp, new_offset, mtime)?;
        emit_progress(app, "workbuddy", i + 1, total);
    }
    Ok(total_inserted)
}

// ─── WorkBuddy 会话快照差分（projects 日志缺失时的兜底） ───

fn collect_wb_snapshot(store: &Store) -> Res<usize> {
    let db = adapter::dir().join("workbuddy.db");
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

/// 采集入口（AgentAdapter::collect_usage 分发到此）：projects 优先，缺失走快照兜底，末了账号回填
pub fn collect(store: &Store, app: Option<&AppHandle>) -> Res<usize> {
    cleanup_legacy(store)?;
    let has_projects = adapter::dir().join("projects").is_dir();
    let n = if has_projects {
        collect_wb_projects(store, app)?
    } else {
        collect_wb_snapshot(store)?
    };
    backfill_wb_accounts(store)?;
    Ok(n)
}
