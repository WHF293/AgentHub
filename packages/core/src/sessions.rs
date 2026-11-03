//! sessions —— WB 跨账号会话复制（P1）。
//!
//! 原理（2026-11-01 实测定版，见 .ai/开发方案/2026-11-01-WB跨账号会话复制与关联同步方案.md）：
//! - 正文：`~/.workbuddy/projects/<cwd-slug>/<sessionId>.jsonl`，元数据无账号绑定
//! - 索引：`~/.workbuddy/workbuddy.db` sessions 表，`user_id` 列 = 账号 uid（侧边栏按它过滤）
//! - 云端映射：`edge-sync-mapping-*.db` 不由本工具写，WB 自身 edge-sync 启动时自动补
//!
//! 复制 = 复制 jsonl（逐行 serde_json 改写顶层 sessionId，不做全文 replace）+
//! sessions 表按目标库列取交集 INSERT（id/user_id/created_at/updated_at 覆盖，
//! synchronous=FULL，INSERT 非 REPLACE，Claw 工作区拒绝）。
//! 仅在 WB 关闭窗口执行（切号链路内调用），绝不热写。

use std::path::{Path, PathBuf};

use rusqlite::types::Value as SqlValue;
use rusqlite::OpenFlags;
use serde::Serialize;

use crate::adapters::wb;
use crate::fsutil::Res;
use crate::store::{self, Store};

/// Claw 工作区（绑定账号渠道，禁止复制）—— 判定与 workbuddy-switch 一致：cwd 末段 == "claw"
fn is_claw_workspace(cwd: &str) -> bool {
    cwd.trim()
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case("claw"))
}

fn data_root() -> PathBuf {
    wb::dir()
}

fn wb_db() -> PathBuf {
    data_root().join("workbuddy.db")
}

fn projects_dir() -> PathBuf {
    data_root().join("projects")
}

/// 生成 uuid-v4 形态的新会话 id（无 uuid crate：时间纳秒 + pid + 计数器混合散列）
fn new_session_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let mut acc: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    acc ^= (std::process::id() as u64) << 32;
    acc = acc.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    acc ^= (COUNTER.fetch_add(1, Ordering::Relaxed) as u64) << 17;
    let a = acc as u128;
    let b = (acc.rotate_left(29) as u128) << 64 | (acc as u128).rotate_left(13) as u128;
    let n = a | b;
    // v4：第 13 位高 4 bit = 0100；variant：第 17 字节高 2 bit = 10
    let hex = format!("{:032x}", n);
    let bytes: Vec<char> = hex.chars().collect();
    let f = |s: &[char]| s.iter().collect::<String>();
    format!(
        "{}-{}-4{}-8{}-{}",
        f(&bytes[0..8]),
        f(&bytes[8..12]),
        f(&bytes[13..16]),
        f(&bytes[17..20]),
        f(&bytes[20..32])
    )
}

fn now_ms() -> i64 {
    chrono::Local::now().timestamp_millis()
}

fn open_db(path: &Path, read_only: bool) -> Result<rusqlite::Connection, String> {
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let conn = rusqlite::Connection::open_with_flags(path, flags)
        .map_err(|e| format!("打开 workbuddy.db 失败: {e}"))?;
    if !read_only {
        // 复制写入必须可靠持久（对标 workbuddy-switch：synchronous ≥ FULL）
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(|e| e.to_string())?;
    }
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    Ok(conn)
}

fn table_exists(conn: &rusqlite::Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |row| row.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// 定位会话正文：扫描 projects/*/ 查 <sessionId>.jsonl（不依赖 slug 算法，实测更稳）
fn find_project_jsonl(cid: &str) -> Option<PathBuf> {
    let projects = projects_dir();
    let direct = projects.join(format!("{cid}.jsonl"));
    if direct.is_file() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(&projects).ok()?.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let p = entry.path().join(format!("{cid}.jsonl"));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// cwd 的项目展示名：末段（WB 侧栏「空间」同款）
fn project_name(cwd: &str) -> String {
    cwd.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_string()
}

// ─── 会话列表 ───

#[derive(Debug, Serialize, Clone)]
pub struct WbSessionInfo {
    pub id: String,
    pub title: String,
    pub custom_title: Option<String>,
    pub cwd: String,
    pub project: String,
    pub status: String,
    pub model: Option<String>,
    pub updated_at: i64,
    /// 正文 jsonl 是否存在于本机（不存在则无法复制）
    pub jsonl_ok: bool,
    /// Claw 工作区会话（禁止复制）
    pub claw: bool,
    /// 已复制过副本（存在跨账号关联）
    pub linked: bool,
}

/// 列出某账号未删除会话（uid 为空 = 未登录，返回空列表）
pub fn list_sessions(store: &Store, uid: &str) -> Res<Vec<WbSessionInfo>> {
    if uid.is_empty() {
        return Ok(Vec::new());
    }
    let db = wb_db();
    if !db.is_file() {
        return Ok(Vec::new());
    }
    let conn = open_db(&db, true).map_err(|e| e)?;
    if !table_exists(&conn, "sessions") {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT id, IFNULL(title,''), custom_title, IFNULL(cwd,''), IFNULL(status,''), \
             IFNULL(model,''), IFNULL(updated_at,0) FROM sessions \
             WHERE user_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC LIMIT 100",
        )
        .map_err(|e| e.to_string())?;
    let mut rows = stmt
        .query(rusqlite::params![uid])
        .map_err(|e| e.to_string())?;
    // 已建关联的源会话 id 集合
    let linked: std::collections::HashSet<String> = store
        .with(|conn| {
            let mut set = std::collections::HashSet::new();
            if table_exists(conn, "wb_session_links") {
                let mut st = conn
                    .prepare("SELECT DISTINCT source_session_id FROM wb_session_links")
                    .map_err(|e| e.to_string())?;
                let mut rows = st.query([]).map_err(|e| e.to_string())?;
                while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                    let v: String = r.get(0).map_err(|e| e.to_string())?;
                    set.insert(v);
                }
            }
            Ok(set)
        })
        .unwrap_or_default();
    let mut out = Vec::new();
    while let Some(r) = rows.next().map_err(|e| e.to_string())? {
        let g = |i: usize| -> Result<rusqlite::types::Value, String> {
            r.get(i).map_err(|e| e.to_string())
        };
        let id = match g(0)? {
            rusqlite::types::Value::Text(s) => s,
            _ => String::new(),
        };
        let cwd = match g(3)? {
            rusqlite::types::Value::Text(s) => s,
            _ => String::new(),
        };
        let title = match g(1)? {
            rusqlite::types::Value::Text(s) => s,
            _ => String::new(),
        };
        let custom_title = match g(2)? {
            rusqlite::types::Value::Text(s) => Some(s),
            _ => None,
        };
        let status = match g(4)? {
            rusqlite::types::Value::Text(s) => s,
            _ => String::new(),
        };
        let model_raw = match g(5)? {
            rusqlite::types::Value::Text(s) => s,
            _ => String::new(),
        };
        let updated_at = match g(6)? {
            rusqlite::types::Value::Integer(n) => n,
            _ => 0,
        };
        out.push(WbSessionInfo {
            jsonl_ok: find_project_jsonl(&id).is_some(),
            claw: is_claw_workspace(&cwd),
            linked: linked.contains(&id),
            id,
            title,
            custom_title,
            project: project_name(&cwd),
            cwd: cwd.clone(),
            status,
            model: (!model_raw.is_empty()).then_some(model_raw),
            updated_at,
        });
    }
    Ok(out)
}

// ─── 复制 ───

#[derive(Debug, Serialize, Clone)]
pub struct SessionCopyResult {
    pub source_id: String,
    pub new_id: String,
    pub title: String,
    /// copied / skipped / failed
    pub status: String,
    pub detail: String,
}

fn read_source_row(
    conn: &rusqlite::Connection,
    cid: &str,
) -> Result<Option<Vec<(String, SqlValue)>>, String> {
    let mut stmt = conn
        .prepare("SELECT * FROM sessions WHERE id = ?1")
        .map_err(|e| e.to_string())?;
    let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query(rusqlite::params![cid]).map_err(|e| e.to_string())?;
    let Some(row) = rows.next().map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let mut out = Vec::with_capacity(cols.len());
    for (i, name) in cols.iter().enumerate() {
        let v: SqlValue = row.get(i).map_err(|e| e.to_string())?;
        out.push((name.clone(), v));
    }
    Ok(Some(out))
}

/// 正文改写：逐行 serde_json 解析，仅替换顶层 sessionId 字段（不做全文 replace，
/// 避免对话内容中恰好出现源会话 id 被误伤 —— 对 workbuddy-switch 的改进点）
fn rewrite_body(src: &Path, dest: &Path, old_id: &str, new_id: &str) -> Res<()> {
    let text = std::fs::read_to_string(src).map_err(|e| format!("读取正文失败: {e}"))?;
    let mut out = String::with_capacity(text.len() + 64);
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(mut v) => {
                if v.get("sessionId").and_then(|s| s.as_str()) == Some(old_id) {
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert("sessionId".into(), serde_json::Value::String(new_id.into()));
                    }
                }
                out.push_str(&serde_json::to_string(&v).map_err(|e| e.to_string())?);
            }
            // 解析失败的行原样保留（正文完整性优先）
            Err(_) => out.push_str(line),
        }
        out.push('\n');
    }
    // 原子写：tmp + rename（项目约定）
    let tmp = dest.with_extension(format!("jsonl.tmp-{}", std::process::id()));
    std::fs::write(&tmp, out.as_bytes()).map_err(|e| format!("写临时文件失败: {e}"))?;
    std::fs::rename(&tmp, dest).map_err(|e| format!("写正文失败: {e}"))?;
    Ok(())
}

/// 复制单个会话。conn = workbuddy.db 读写连接（调用方保证 WB 已关闭）。
#[allow(clippy::too_many_arguments)]
fn copy_one(
    conn: &rusqlite::Connection,
    src_jsonl: &Path,
    src_row: &[(String, SqlValue)],
    old_id: &str,
    target_uid: &str,
    backup_dir: &Path,
) -> Result<String, String> {
    // Claw 工作区拒绝
    let cwd = src_row
        .iter()
        .find(|(n, _)| n == "cwd")
        .and_then(|(_, v)| match v {
            SqlValue::Text(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_default();
    if is_claw_workspace(&cwd) {
        return Err("Claw 工作区会话绑定账号渠道，不支持复制".into());
    }
    let new_id = new_session_id();

    // 1) 备份正文到 session-<ts>/<source_id>.jsonl（每次复制前，可溯）
    let _ = std::fs::create_dir_all(backup_dir);
    let _ = std::fs::copy(src_jsonl, backup_dir.join(format!("{old_id}.jsonl")));

    // 2) 复制 + 改写正文（同目录 = 同项目归属）
    let parent: PathBuf = src_jsonl
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(projects_dir);
    let dest_jsonl = parent.join(format!("{new_id}.jsonl"));
    rewrite_body(src_jsonl, &dest_jsonl, old_id, &new_id)?;

    // 3) 附属文件：会话工作区目录 + file-rollback.ndjson
    let src_dir = parent.join(old_id);
    if src_dir.is_dir() {
        let _ = fs_extra_copy_dir(&src_dir, &parent.join(&new_id));
    }
    let rb = parent.join(format!("{old_id}.file-rollback.ndjson"));
    if rb.is_file() {
        let _ = std::fs::copy(&rb, parent.join(format!("{new_id}.file-rollback.ndjson")));
    }

    // 4) sessions 表按目标列取交集 INSERT（id/user_id/created_at/updated_at 覆盖）
    let target_cols: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info('sessions')")
            .map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        rows.flatten().collect()
    };
    let mut cols: Vec<String> = Vec::with_capacity(target_cols.len());
    let mut vals: Vec<SqlValue> = Vec::with_capacity(target_cols.len());
    for col in &target_cols {
        let Some((_, value)) = src_row.iter().find(|(n, _)| n == col) else {
            continue;
        };
        let mut value = value.clone();
        match col.as_str() {
            "id" => value = SqlValue::Text(new_id.clone()),
            "user_id" => value = SqlValue::Text(target_uid.to_string()),
            "created_at" | "updated_at" => value = SqlValue::Integer(now_ms()),
            "deleted_at" => value = SqlValue::Null,
            _ => {}
        }
        cols.push(col.clone());
        vals.push(value);
    }
    let placeholders = cols.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "INSERT INTO sessions ({}) VALUES ({})",
        cols.join(", "),
        placeholders
    );
    conn.execute(&sql, rusqlite::params_from_iter(vals.iter()))
        .map_err(|e| format!("会话索引写入失败: {e}"))?;
    Ok(new_id)
}

/// 目录复制（会话工作区目录；失败不致命，降级忽略）
fn fs_extra_copy_dir(src: &Path, dest: &Path) -> Res<()> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
        let p = e.path();
        let d = dest.join(e.file_name());
        if p.is_dir() {
            fs_extra_copy_dir(&p, &d)?;
        } else {
            let _ = std::fs::copy(&p, &d);
        }
    }
    Ok(())
}

/// 批量复制会话到目标账号。必须在 WB 关闭窗口调用（切号链路内）。
pub fn copy_sessions(
    store: &Store,
    ids: &[String],
    target_uid: &str,
    backup_dir: &Path,
) -> Vec<SessionCopyResult> {
    let mut results = Vec::new();
    if ids.is_empty() || target_uid.is_empty() {
        return results;
    }
    if !wb_db().is_file() {
        return ids
            .iter()
            .map(|id| SessionCopyResult {
                source_id: id.clone(),
                new_id: String::new(),
                title: String::new(),
                status: "failed".into(),
                detail: "未找到 workbuddy.db".into(),
            })
            .collect();
    }
    let Ok(conn) = open_db(&wb_db(), false) else {
        return ids
            .iter()
            .map(|id| SessionCopyResult {
                source_id: id.clone(),
                new_id: String::new(),
                title: String::new(),
                status: "failed".into(),
                detail: "workbuddy.db 无法打开（WB 可能未完全退出）".into(),
            })
            .collect();
    };
    if !table_exists(&conn, "sessions") {
        return ids
            .iter()
            .map(|id| SessionCopyResult {
                source_id: id.clone(),
                new_id: String::new(),
                title: String::new(),
                status: "failed".into(),
                detail: "sessions 表不存在".into(),
            })
            .collect();
    }
    for id in ids {
        let title: String = conn
            .query_row(
                "SELECT IFNULL(COALESCE(custom_title, title),'') FROM sessions WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap_or_default();
        let mut result = |status: &str, detail: String, new_id: &str| {
            results.push(SessionCopyResult {
                source_id: id.clone(),
                new_id: new_id.to_string(),
                title: title.clone(),
                status: status.into(),
                detail,
            });
        };
        let Some(src_jsonl) = find_project_jsonl(id) else {
            result("skipped", "未找到会话正文 jsonl".into(), "");
            continue;
        };
        let Ok(Some(src_row)) = read_source_row(&conn, id) else {
            result("skipped", "sessions 表无此会话行".into(), "");
            continue;
        };
        match copy_one(&conn, &src_jsonl, &src_row, id, target_uid, backup_dir) {
            Ok(new_id) => {
                result("copied", String::new(), &new_id);
                // 关联组入 agenthub.db（group_key = 源会话 id；P2 在此基础上做增量同步）
                let source_uid = src_row
                    .iter()
                    .find(|(n, _)| n == "user_id")
                    .and_then(|(_, v)| match v {
                        SqlValue::Text(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let gid = new_id.clone();
                let src = id.clone();
                let tuid = target_uid.to_string();
                let _ = store.with(|c| {
                    c.execute(
                        "INSERT INTO wb_session_links(source_session_id, source_uid, target_session_id, target_uid, group_key, status, created_at) \
                         VALUES (?1, ?2, ?3, ?4, ?1, 'copied', datetime('now','localtime'))",
                        rusqlite::params![src, source_uid, gid, tuid],
                    )
                    .map_err(|e| e.to_string())?;
                    Ok(())
                });
            }
            Err(e) => result("failed", e, ""),
        }
    }
    let _ = store.with(|c| {
        store::log_op(
            c,
            "wb.session_copy",
            &format!("{} 个会话", ids.len()),
            serde_json::json!({ "results": results }),
        );
        Ok(())
    });
    results
}

/// 复制前的整库备份（workbuddy.db + wal/shm），返回备份目录
pub fn backup_db(dest_dir: &Path) -> Res<()> {
    std::fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
    for suffix in ["", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{}", wb_db().display(), suffix));
        if p.is_file() {
            let _ = std::fs::copy(
                &p,
                dest_dir.join(format!("workbuddy.db{}", suffix)),
            );
        }
    }
    Ok(())
}

// ─── Tauri commands ───

use tauri::State;

#[tauri::command]
pub fn wb_session_list(state: State<'_, Store>, uid: String) -> Result<Vec<WbSessionInfo>, String> {
    crate::plugins::require_enabled(&state, "workbuddy")?;
    list_sessions(&state, &uid)
}

// ─── 独立复制（Account 岛卡片按钮，一次复制给多个 WB 账号） ───

#[derive(Debug, serde::Deserialize)]
pub struct SessionCopyInput {
    /// 源会话 id（同一来源账号）
    pub session_ids: Vec<String>,
    /// 目标账号 uid 列表（可多选，逐账号各复制一份）
    pub target_uids: Vec<String>,
    /// WB 运行中时是否允许强制结束（绝不静默强杀红线）
    pub confirm_kill: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct TargetCopyGroup {
    pub target_uid: String,
    pub results: Vec<SessionCopyResult>,
}

#[derive(Debug, Serialize, Default, Clone)]
pub struct SessionCopyReport {
    pub ok: bool,
    /// WB 正在运行且未勾选强杀（前端据此显示勾选项后重试）
    pub need_kill: bool,
    /// 本次是否关闭了 WB
    pub killed: bool,
    /// 复制完成后是否重启了 WB（复制前在运行才重启，还原现场）
    pub restarted: bool,
    pub targets: Vec<TargetCopyGroup>,
    pub error: Option<String>,
}

/// 独立复制命令：与切号链路解耦，直接把源账号会话复制给 1..N 个目标账号。
/// 复制要写 workbuddy.db，绝不热写 —— WB 运行中需 confirm_kill 先关闭，完成后重启还原。
#[tauri::command]
pub fn wb_session_copy(
    state: State<'_, Store>,
    input: SessionCopyInput,
) -> Result<SessionCopyReport, String> {
    crate::plugins::require_enabled(&state, "workbuddy")?;
    let mut report = SessionCopyReport::default();

    if input.session_ids.is_empty() {
        report.error = Some("未勾选要复制的会话".into());
        return Ok(report);
    }
    if input.target_uids.is_empty() {
        report.error = Some("未勾选目标账号".into());
        return Ok(report);
    }
    if crate::account::process_running() {
        if !input.confirm_kill {
            report.need_kill = true;
            report.error = Some(
                "WorkBuddy.exe 正在运行 —— 复制会话需要先完全退出 WB，或勾选「强制结束」后重试"
                    .into(),
            );
            return Ok(report);
        }
        crate::account::kill_process().map_err(|e| e.to_string())?;
        std::thread::sleep(std::time::Duration::from_millis(1500));
        report.killed = true;
    }

    // 复制前整库备份一次（多目标共用同一备份目录，可溯）
    let sess_backup =
        crate::store::backups_dir().join(format!("session-{}", crate::fsutil::now_stamp()));
    if let Err(e) = backup_db(&sess_backup) {
        report.error = Some(format!("workbuddy.db 备份失败（{e}），已中止复制"));
        restart_wb_if_killed(&mut report);
        return Ok(report);
    }
    for uid in &input.target_uids {
        let results = copy_sessions(&state, &input.session_ids, uid, &sess_backup);
        report.targets.push(TargetCopyGroup {
            target_uid: uid.clone(),
            results,
        });
    }
    let any_copied = report
        .targets
        .iter()
        .any(|t| t.results.iter().any(|r| r.status == "copied"));
    let any_failed = report
        .targets
        .iter()
        .any(|t| t.results.iter().any(|r| r.status == "failed"));
    report.ok = any_copied && !any_failed;
    restart_wb_if_killed(&mut report);
    Ok(report)
}

/// 复制前 WB 在运行才会走到 killed —— 完成后重启还原现场（失败仅留痕不报错）
fn restart_wb_if_killed(report: &mut SessionCopyReport) {
    if report.killed && Path::new(crate::account::WB_EXE).is_file() {
        if std::process::Command::new(crate::account::WB_EXE)
            .spawn()
            .is_ok()
        {
            report.restarted = true;
        }
    }
}
