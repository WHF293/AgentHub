//! usage_claude —— Claude Code 用量采集：~/.claude/projects/**/*.jsonl 逐请求 message.usage
//! （原 core/usage.rs claude 段平移；入口由 AgentAdapter::collect_usage 分发）。
//!
//! 每条 assistant 记录：message.usage.{input_tokens, output_tokens,
//! cache_read_input_tokens, cache_creation_input_tokens} + message.model + timestamp(RFC3339)。
//! 口径（与 ccsimport 一致）：input = input_tokens + cache_creation（缓存写入并入输入），
//! cache_read = cache_read；账号域不存在，account 恒 ''。

use std::path::Path;

use chrono::Local;
use tauri::AppHandle;

use agenthub_core::fsutil::Res;
use agenthub_core::store::Store;
use agenthub_core::usage::{collect_jsonl_files, cursor_get, cursor_set, emit_progress};

use crate::adapter;

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
    use std::io::{Read, Seek, SeekFrom};
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
        let request_id = agenthub_core::fsutil::fnv1a(&format!(
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

/// 采集入口（AgentAdapter::collect_usage 分发到此）
pub fn collect(store: &Store, app: Option<&AppHandle>) -> Res<usize> {
    let root = adapter::projects_dir();
    if !root.is_dir() {
        return Ok(0);
    }
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
            emit_progress(app, "claude", i + 1, total);
            continue;
        }
        let start = if mtime == last_mtime { last_offset } else { 0 };
        let (events, new_offset) = collect_claude_jsonl_file(p, start)?;
        total_inserted += insert_claude_events(store, &events)?;
        cursor_set(store, "claude-projects", &fp, new_offset, mtime)?;
        emit_progress(app, "claude", i + 1, total);
    }
    Ok(total_inserted)
}
