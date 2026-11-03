//! store —— SQLite 单一事实源（~/.agenthub/agenthub.db），借鉴 cc-switch v4。
//! schema v1：skills / skill_links / skill_sources / mcp_servers / providers /
//! wb_slots / projects / bridge_targets / usage_events / usage_cursor / op_log。

use std::sync::Mutex;

use rusqlite::Connection;

use crate::fsutil::Res;

pub struct Store {
    conn: Mutex<Connection>,
}

/// 中央存储根目录 ~/.agenthub
pub fn hub_dir() -> std::path::PathBuf {
    crate::adapters::agent_home().join(".agenthub")
}
pub fn central_skills_dir() -> std::path::PathBuf {
    hub_dir().join("skills")
}
/// 全局记忆统一真身（双端符号链接共同指向）
pub fn central_memory_md() -> std::path::PathBuf {
    hub_dir().join("MEMORY.md")
}
pub fn backups_dir() -> std::path::PathBuf {
    hub_dir().join("backups")
}
pub fn db_path() -> std::path::PathBuf {
    hub_dir().join("agenthub.db")
}

impl Store {
    /// 打开（目录不存在则初始化）并执行迁移
    pub fn open() -> Res<Self> {
        let db = db_path();
        std::fs::create_dir_all(db.parent().unwrap()).map_err(|e| e.to_string())?;
        let conn = Connection::open(&db).map_err(|e| e.to_string())?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| e.to_string())?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| e.to_string())?;
        let store = Store {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    /// 读写锁内执行
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> Res<T>) -> Res<T> {
        let guard = self.conn.lock().map_err(|e| e.to_string())?;
        f(&guard)
    }

    fn migrate(&self) -> Res<()> {
        self.with(|conn| {
            conn.execute_batch(SCHEMA_V1)
                .map_err(|e| format!("schema 迁移失败: {e}"))?;
            // v1.1：mcp_servers 补 headers_json 列（远程 MCP 鉴权头）
            let has_headers: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('mcp_servers') WHERE name = 'headers_json'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if has_headers == 0 {
                conn.execute_batch("ALTER TABLE mcp_servers ADD COLUMN headers_json TEXT")
                    .map_err(|e| format!("headers_json 列迁移失败: {e}"))?;
            }
            // v1.2：wb_slots.id 由 INTEGER 自增改为 TEXT 语义主键（槽位 id 形如 slot-…/legacy-…）
            let slot_pk_type: String = conn
                .query_row(
                    "SELECT IFNULL(type,'') FROM pragma_table_info('wb_slots') WHERE pk = 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or_default();
            if slot_pk_type.eq_ignore_ascii_case("integer") {
                conn.execute_batch(
                    "ALTER TABLE wb_slots RENAME TO wb_slots_old;
                     CREATE TABLE wb_slots (
                       id TEXT PRIMARY KEY,
                       label TEXT NOT NULL,
                       uid TEXT,
                       uin TEXT,
                       slot_dir TEXT NOT NULL,
                       saved_at TEXT
                     );
                     DROP TABLE wb_slots_old;",
                )
                .map_err(|e| format!("wb_slots 主键迁移失败: {e}"))?;
            }
            // v1.3：WB 会话快照差分状态（usage_events 去重的上游记账）
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS wb_session_seen (
                   session_id TEXT PRIMARY KEY,
                   last_used INTEGER NOT NULL,
                   updated_at TEXT
                 );",
            )
            .map_err(|e| format!("wb_session_seen 建表失败: {e}"))?;
            // v1.4：官方逐请求积分（WB rawUsage.credit 与账单同源）
            let has_credit: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('usage_events') WHERE name = 'credit'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if has_credit == 0 {
                conn.execute_batch("ALTER TABLE usage_events ADD COLUMN credit REAL NOT NULL DEFAULT 0")
                    .map_err(|e| format!("credit 列迁移失败: {e}"))?;
            }
            // v1.6：用量事件账号归属（WB 多账号：sessions.user_id 关联；'' = 未归属/ZCode 单账号）
            let has_account: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('usage_events') WHERE name = 'account'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if has_account == 0 {
                conn.execute_batch("ALTER TABLE usage_events ADD COLUMN account TEXT NOT NULL DEFAULT ''")
                    .map_err(|e| format!("account 列迁移失败: {e}"))?;
            }
            // v1.7：ZCode 账号槽位（credentials.json 快照，登录态文件级复制）
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS zcode_slots (
                   id TEXT PRIMARY KEY,
                   label TEXT NOT NULL,
                   slot_dir TEXT NOT NULL,
                   saved_at TEXT
                 );",
            )
            .map_err(|e| format!("zcode_slots 建表失败: {e}"))?;
            // v1.8：MCP 统一定义的启用态改为按 agent 通用 JSON 列
            // （{"workbuddy":true,"zcode":false,...}；NULL = 旧数据未迁移，读取时回退
            // enabled_wb/enabled_zcode 旧列；旧列保留作回滚保险，新写入不再更新）
            let has_enabled_json: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('mcp_servers') WHERE name = 'enabled_json'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if has_enabled_json == 0 {
                conn.execute_batch("ALTER TABLE mcp_servers ADD COLUMN enabled_json TEXT")
                    .map_err(|e| format!("enabled_json 列迁移失败: {e}"))?;
            }
            // v1.9：可插拔适配器启用状态（首次运行自动探测，之后以用户手动配置为准）
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS plugin_state (
                   agent TEXT PRIMARY KEY,
                   enabled INTEGER NOT NULL,
                   source TEXT NOT NULL DEFAULT 'manual',
                   updated_at TEXT
                 );",
            )
            .map_err(|e| format!("plugin_state 建表失败: {e}"))?;
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS wb_model_entries (
                   entry_id TEXT PRIMARY KEY,
                   provider_id TEXT NOT NULL,
                   updated_at TEXT
                 );",
            )
            .map_err(|e| format!("wb_model_entries 建表失败: {e}"))?;
            // v1.10：WB 每账户积分余额 + 每日签到记录（Account 岛积分设置 / 每日领取 100 积分）
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS wb_checkins (
                   uid TEXT PRIMARY KEY,
                   last_date TEXT NOT NULL,
                   last_credit REAL NOT NULL DEFAULT 0,
                   claimed_at TEXT
                 );
                 CREATE TABLE IF NOT EXISTS wb_credit_balance (
                   uid TEXT PRIMARY KEY,
                   balance REAL NOT NULL,
                   set_at TEXT NOT NULL
                 );",
            )
            .map_err(|e| format!("wb_checkins/wb_credit_balance 建表失败: {e}"))?;
            // v2.0：WB 跨账号会话复制关联组（sessions.rs P1；P2 增量同步在此基础上扩展）
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS wb_session_links (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   group_key TEXT NOT NULL,
                   source_session_id TEXT NOT NULL,
                   source_uid TEXT NOT NULL DEFAULT '',
                   target_session_id TEXT NOT NULL,
                   target_uid TEXT NOT NULL DEFAULT '',
                   status TEXT NOT NULL DEFAULT 'copied',
                   created_at TEXT
                 );",
            )
            .map_err(|e| format!("wb_session_links 建表失败: {e}"))?;
            // 内置双市场（WB 市场为占位：接口探测未果前降级本地扫描）
            conn.execute(
                "INSERT OR IGNORE INTO skill_sources(id, name, type, url, enabled) VALUES
                 (1, 'Claude 官方', 'market', 'https://github.com/anthropics/skills', 1),
                 (2, 'WorkBuddy 市场', 'market', '', 1)",
                [],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
    }
}

pub fn log_op(conn: &Connection, action: &str, target: &str, detail: serde_json::Value) {
    let _ = conn.execute(
        "INSERT INTO op_log(action, target, detail_json, created_at) VALUES (?1, ?2, ?3, datetime('now','localtime'))",
        rusqlite::params![action, target, detail.to_string()],
    );
}

// ─── WB models.json 托管清单（M4 双端写穿） ───

/// 某 provider 在 WB 侧托管的条目 id 列表
pub fn wb_entries_for(conn: &Connection, provider_id: &str) -> Res<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT entry_id FROM wb_model_entries WHERE provider_id = ?1 ORDER BY entry_id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![provider_id], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// 全部托管映射 (provider_id, entry_id)，overview 归属判定用
pub fn wb_entries_all(conn: &Connection) -> Res<Vec<(String, String)>> {
    let mut stmt = conn
        .prepare("SELECT provider_id, entry_id FROM wb_model_entries ORDER BY provider_id, entry_id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// 重写某 provider 的托管清单（同步成功后调用）
pub fn wb_entries_replace(conn: &Connection, provider_id: &str, ids: &[String]) -> Res<()> {
    conn.execute(
        "DELETE FROM wb_model_entries WHERE provider_id = ?1",
        rusqlite::params![provider_id],
    )
    .map_err(|e| e.to_string())?;
    for id in ids {
        conn.execute(
            "INSERT OR REPLACE INTO wb_model_entries(entry_id, provider_id, updated_at)
             VALUES (?1, ?2, datetime('now','localtime'))",
            rusqlite::params![id, provider_id],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 删除某 provider 的全部托管记录，返回删除条数
pub fn wb_entries_remove_provider(conn: &Connection, provider_id: &str) -> Res<usize> {
    conn.execute(
        "DELETE FROM wb_model_entries WHERE provider_id = ?1",
        rusqlite::params![provider_id],
    )
    .map_err(|e| e.to_string())
}

pub fn upsert_skill(
    conn: &Connection,
    name: &str,
    path: &str,
    description: Option<&str>,
    source: &str,
    source_ref: &str,
) -> Res<i64> {
    conn.execute(
        "INSERT INTO skills(name, path, description, source, source_ref, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now','localtime'))
         ON CONFLICT(name) DO UPDATE SET
           path = excluded.path,
           description = excluded.description,
           source = excluded.source,
           source_ref = excluded.source_ref,
           updated_at = excluded.updated_at",
        rusqlite::params![name, path, description, source, source_ref],
    )
    .map_err(|e| e.to_string())?;
    // ON CONFLICT 走 UPDATE 路径时 last_insert_rowid() 不可靠，显式回查
    conn.query_row(
        "SELECT id FROM skills WHERE name = ?1",
        rusqlite::params![name],
        |row| row.get(0),
    )
    .map_err(|e| e.to_string())
}

pub fn upsert_skill_link(
    conn: &Connection,
    skill_id: i64,
    agent: &str,
    mode: &str,
    target_path: &str,
    status: &str,
) -> Res<()> {
    conn.execute(
        "INSERT INTO skill_links(skill_id, agent, mode, target_path, status, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now','localtime'))
         ON CONFLICT(skill_id, agent) DO UPDATE SET
           mode = excluded.mode,
           target_path = excluded.target_path,
           status = excluded.status,
           updated_at = excluded.updated_at",
        rusqlite::params![skill_id, agent, mode, target_path, status],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// schema v1 —— 与开发方案 §4 数据模型一致
const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS skills (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT UNIQUE NOT NULL,
  version TEXT,
  description TEXT,
  path TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  source TEXT NOT NULL DEFAULT 'local',
  source_ref TEXT,
  updated_at TEXT
);
CREATE TABLE IF NOT EXISTS skill_links (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  skill_id INTEGER NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
  agent TEXT NOT NULL,
  mode TEXT NOT NULL,
  target_path TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'ok',
  updated_at TEXT,
  UNIQUE(skill_id, agent)
);
CREATE TABLE IF NOT EXISTS skill_sources (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  type TEXT NOT NULL,
  url TEXT,
  ref TEXT,
  enabled INTEGER NOT NULL DEFAULT 1,
  last_sync_at TEXT
);
CREATE TABLE IF NOT EXISTS mcp_servers (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT UNIQUE NOT NULL,
  type TEXT NOT NULL DEFAULT 'stdio',
  command TEXT,
  args_json TEXT,
  url TEXT,
  env_json TEXT,
  enabled_wb INTEGER NOT NULL DEFAULT 1,
  enabled_zcode INTEGER NOT NULL DEFAULT 1,
  updated_at TEXT
);
CREATE TABLE IF NOT EXISTS providers (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT UNIQUE NOT NULL,
  kind TEXT NOT NULL,
  base_url TEXT,
  api_key_enc TEXT,
  models_json TEXT,
  models_url TEXT,
  enabled_wb INTEGER NOT NULL DEFAULT 0,
  enabled_zcode INTEGER NOT NULL DEFAULT 1,
  updated_at TEXT
);
CREATE TABLE IF NOT EXISTS wb_slots (
  id TEXT PRIMARY KEY,
  label TEXT NOT NULL,
  uid TEXT,
  uin TEXT,
  slot_dir TEXT NOT NULL,
  saved_at TEXT
);
CREATE TABLE IF NOT EXISTS projects (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  path TEXT UNIQUE NOT NULL,
  name TEXT,
  ai_initialized INTEGER NOT NULL DEFAULT 0,
  last_sync_at TEXT
);
CREATE TABLE IF NOT EXISTS bridge_targets (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  agent TEXT NOT NULL,
  file_path TEXT NOT NULL,
  block_hash TEXT,
  injected_at TEXT,
  UNIQUE(project_id, file_path)
);
CREATE TABLE IF NOT EXISTS usage_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  agent TEXT NOT NULL,
  ts TEXT NOT NULL,
  model TEXT,
  provider TEXT,
  session_id TEXT,
  input_tokens INTEGER NOT NULL DEFAULT 0,
  output_tokens INTEGER NOT NULL DEFAULT 0,
  cache_read_tokens INTEGER NOT NULL DEFAULT 0,
  source TEXT NOT NULL DEFAULT 'rollout',
  request_id TEXT UNIQUE
);
CREATE INDEX IF NOT EXISTS idx_usage_events_ts ON usage_events(ts);
CREATE TABLE IF NOT EXISTS usage_cursor (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source TEXT NOT NULL,
  file_path TEXT UNIQUE NOT NULL,
  byte_offset INTEGER NOT NULL DEFAULT 0,
  mtime TEXT
);
CREATE TABLE IF NOT EXISTS op_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  action TEXT NOT NULL,
  target TEXT,
  detail_json TEXT,
  created_at TEXT
);
"#;

/// 校验中央 skill 目录存在性（供命令层使用）
pub fn ensure_hub_dirs() -> Res<()> {
    std::fs::create_dir_all(central_skills_dir()).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(backups_dir()).map_err(|e| e.to_string())?;
    Ok(())
}
