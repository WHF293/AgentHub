//! plugins —— 可插拔适配器的启用/停用状态管理（Plugin 岛数据层）。
//!
//! 语义（用户定版）：
//! - 每个注册在 adapters::registry() 里的 agent 适配器 = 一个「插件」；
//! - **首次运行**（plugin_state 表为空）自动探测本机安装了哪些 agent，安装了的默认启用；
//! - 之后一切以用户手动开关为准并持久化，后续启动只读配置，不再自动改状态
//!   （后装 agent 由用户在 Plugin 岛手动启用）；
//! - 共享引擎（skills 迁移 / MCP 同步 / usage 采集）只对已启用插件生效。

use serde::Serialize;
use tauri::State;

use crate::adapters::{self, AgentAdapter};
use crate::fsutil::Res;
use crate::store::{self, Store};

/// 插件状态行（plugin_state）
#[derive(Debug, Serialize)]
pub struct PluginInfo {
    /// 适配器稳定 id（= agent id）
    pub agent: String,
    pub display_name: String,
    /// 一句话简介
    pub desc: String,
    /// 用户配置的启用/停用
    pub enabled: bool,
    /// 本机是否探测到该 agent 安装（仅提示用；停用的插件不影响探测）
    pub installed: bool,
    /// 能力域（skills / mcp）
    pub caps: adapters::AgentCaps,
    /// 状态来源：auto = 首次运行按安装探测自动启用；manual = 用户手动设置
    pub source: String,
}

/// 确保状态表已初始化：空表 = 首次运行，按安装探测自动启用；
/// 非空表也要给「新注册的适配器」补缺行（升级新增插件后 list() 才有行可读），幂等
fn ensure_initialized(conn: &rusqlite::Connection) -> Res<()> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM plugin_state", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    let first_run = count == 0;
    let mut detected: Vec<&'static str> = Vec::new();
    for a in adapters::registry() {
        // 新补行的默认态按安装探测（已启用/停用的存量行不受影响，OR IGNORE 跳过）
        let enabled = a.installed();
        let n = conn
            .execute(
                "INSERT OR IGNORE INTO plugin_state(agent, enabled, source, updated_at)
                 VALUES (?1, ?2, 'auto', datetime('now','localtime'))",
                rusqlite::params![a.id(), enabled as i64],
            )
            .map_err(|e| e.to_string())?;
        if n > 0 && enabled {
            detected.push(a.id());
        }
    }
    if first_run {
        store::log_op(
            conn,
            "plugins.init",
            "plugin_state",
            serde_json::json!({
                "detected": detected,
            }),
        );
    }
    Ok(())
}

/// 全部插件（注册表全量 × 状态）
pub fn list(store: &Store) -> Res<Vec<PluginInfo>> {
    store.with(|conn| {
        ensure_initialized(conn)?;
        let mut out = Vec::new();
        for a in adapters::registry() {
            let (enabled, source) = conn
                .query_row(
                    "SELECT enabled, IFNULL(source,'manual') FROM plugin_state WHERE agent = ?1",
                    rusqlite::params![a.id()],
                    |row| Ok((row.get::<_, i64>(0)? != 0, row.get::<_, String>(1)?)),
                )
                .map_err(|e| e.to_string())?;
            out.push(PluginInfo {
                agent: a.id().to_string(),
                display_name: a.display_name().to_string(),
                desc: a.desc().to_string(),
                enabled,
                installed: a.installed(),
                caps: a.caps(),
                source,
            });
        }
        Ok(out)
    })
}

/// 用户手动开关（持久化，后续启动只读此配置）
pub fn set_enabled(store: &Store, agent: &str, enabled: bool) -> Res<()> {
    let Some(a) = adapters::by_id(agent) else {
        return Err(format!("未知适配器：{agent}"));
    };
    store.with(|conn| {
        ensure_initialized(conn)?;
        let n = conn
            .execute(
                "UPDATE plugin_state SET enabled = ?2, source = 'manual', updated_at = datetime('now','localtime') WHERE agent = ?1",
                rusqlite::params![agent, enabled as i64],
            )
            .map_err(|e| e.to_string())?;
        if n == 0 {
            conn.execute(
                "INSERT INTO plugin_state(agent, enabled, source, updated_at)
                 VALUES (?1, ?2, 'manual', datetime('now','localtime'))",
                rusqlite::params![agent, enabled as i64],
            )
            .map_err(|e| e.to_string())?;
        }
        store::log_op(conn, "plugins.toggle", agent, serde_json::json!({ "enabled": enabled, "display": a.display_name() }));
        Ok(())
    })
}

/// 已启用插件的适配器列表（共享引擎的门控入口；调用方再按 caps 过滤能力域）
pub fn enabled_adapters(store: &Store) -> Res<Vec<&'static dyn AgentAdapter>> {
    let ids = enabled_ids(store)?;
    Ok(adapters::registry()
        .iter()
        .copied()
        .filter(|a| ids.iter().any(|id| id == a.id()))
        .collect())
}

/// 已启用插件 id 集合
pub fn enabled_ids(store: &Store) -> Res<Vec<String>> {
    store.with(|conn| {
        ensure_initialized(conn)?;
        let mut stmt = conn
            .prepare("SELECT agent FROM plugin_state WHERE enabled = 1")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    })
}

/// 门控断言：agent 插件必须已启用，否则报错（各岛写操作与「唤起 agent」类命令的统一入口）。
/// 读操作（总览/列表）不强制走这里 —— 停用端的数据仍可读出用于置灰展示。
pub fn require_enabled(store: &Store, agent: &str) -> Result<(), String> {
    let Some(a) = adapters::by_id(agent) else {
        return Err(format!("未知适配器：{agent}"));
    };
    if enabled_ids(store)?.iter().any(|id| id == agent) {
        Ok(())
    } else {
        Err(format!(
            "{} 插件已停用（到 Plugin 岛开启后可用）",
            a.display_name()
        ))
    }
}

// ─── Tauri commands（Plugin 岛） ───

#[derive(Serialize)]
pub struct PluginListPayload {
    pub items: Vec<PluginInfo>,
}

#[tauri::command]
pub fn plugin_list(state: State<'_, Store>) -> Result<PluginListPayload, String> {
    Ok(PluginListPayload { items: list(&state)? })
}

#[tauri::command]
pub fn plugin_toggle(state: State<'_, Store>, agent: String, enabled: bool) -> Result<(), String> {
    set_enabled(&state, &agent, enabled)
}
