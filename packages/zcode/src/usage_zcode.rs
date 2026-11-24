//! usage_zcode —— ZCode 用量采集：db.sqlite model_usage 全量账本（原 core/usage.rs ZCode 段平移）。
//!
//! 实测（2026-10-31）：rollout jsonl 只保留近期 3 个会话文件；完整账本在
//! ~/.zcode/cli/db/db.sqlite 的 model_usage 表（含 input/output/reasoning/cache 拆分、
//! completed_at、model/provider），SUM(input_tokens)=18.87 亿 与官方「累计 Token 18.9 亿」吻合。
//! 行数游标快检 + 全表扫描 + request_id(=model_usage.id) 幂等去重：
//! model_usage 是 append-only 账本，行数不变 = 无新数据（60s 巡检零成本跳过）；
//! 行数变化才全表重扫，INSERT OR IGNORE 只落新增。

use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::OpenFlags;
use tauri::AppHandle;

use agenthub_core::fsutil::Res;
use agenthub_core::store::Store;
use agenthub_core::usage::{cursor_get, cursor_set, emit_progress, ts_to_local};

use crate::adapter;

/// 一次性清理：旧 rollout 来源被 model_usage 全量账本取代（request_id 体系不同，不清会双计）。
/// Once 守卫 —— 历史遗留数据首轮清完就不再跑（否则每 60s 巡检都对 usage_events 全表 DELETE 扫描）
static CLEAN_DONE: AtomicBool = AtomicBool::new(false);

fn cleanup_legacy(store: &Store) -> agenthub_core::fsutil::Res<()> {
    if CLEAN_DONE.load(Ordering::Relaxed) {
        return Ok(());
    }
    store.with(|conn| {
        conn.execute(
            "DELETE FROM usage_events WHERE agent = 'zcode' AND source = 'rollout'",
            [],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })?;
    // 成功才置位（失败下次巡检重试，避免遗留 rollout 数据双计）
    CLEAN_DONE.store(true, Ordering::Relaxed);
    Ok(())
}

/// 行数游标键：model_usage 是 append-only 账本，行数不变 = 无新数据，跳过全表扫描
const COUNT_CURSOR_KEY: &str = "zcode:model_usage";

fn collect_zcode(store: &Store, app: Option<&AppHandle>) -> Res<usize> {
    let db = adapter::dir().join("cli").join("db").join("db.sqlite");
    if !db.is_file() {
        return Ok(0);
    }
    let conn = rusqlite::Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("只读打开 zcode db.sqlite 失败: {e}"))?;

    // 行数游标快检：append-only 账本行数不变 = 无新数据，直接返回（60s 巡检零成本）。
    // 行数变化才进全表扫描，靠 request_id(=id) 幂等去重只落新增。
    let row_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM model_usage", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let (last_count, _) = cursor_get(store, COUNT_CURSOR_KEY)?;
    if row_count == last_count && last_count > 0 {
        return Ok(0);
    }

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
        // 修优先级 bug：原 (bi+1) * 400.min(total) 在 total<400*批数 时 done 会超过 total
        emit_progress(app, "zcode", ((bi + 1) * 400).min(total), total);
    }
    // 扫描成功才推进行数游标（失败下次重扫，幂等去重兜底）
    cursor_set(store, "model-usage", COUNT_CURSOR_KEY, row_count, 0)?;
    Ok(inserted)
}

// ─── ZCode rollout 增量解析（已被 model_usage 全量账本取代，保留兜底） ───

#[allow(dead_code)]
fn collect_zcode_rollout(store: &Store) -> Res<usize> {
    use std::io::{Read, Seek, SeekFrom};
    let dir = adapter::rollout_dir();
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
        // append-only 增量续扫（同 usage_wb）：mtime 变化从上次偏移继续，仅文件变短才全量重扫
        let start = if last_offset > 0 && file_len >= last_offset {
            last_offset
        } else {
            0
        };

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
                    .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string());
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

/// 采集入口（AgentAdapter::collect_usage 分发到此）
pub fn collect(store: &Store, app: Option<&AppHandle>) -> Res<usize> {
    cleanup_legacy(store)?;
    collect_zcode(store, app)
}
