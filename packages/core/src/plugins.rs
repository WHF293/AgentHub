//! plugins —— 可插拔适配器的「安装 / 启停」生命周期管理（Plugin 岛数据层）。
//!
//! 语义（monorepo 插件化定版，VS Code 内置扩展同款）：
//! - 每个注册在 adapters::registry() 里的 agent 适配器 = 一个「插件」，内置随应用分发；
//! - 首次运行：全部置 installed=1（默认安装），enabled 按本机安装探测（装了的启用）；
//! - **卸载** = installed=0（联动 enabled=0）：全岛隐藏 + 门控全关，**数据保留**（usage
//!   历史 / 目标文件配置不删），可随时重装；
//! - **重装** = installed=1 + enabled=1；
//! - 启停 = enabled 开关（installed 不变）；
//! - 共享引擎统一门控 = active（installed && enabled），入口 [`active_ids`] / [`active_adapters`]。

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
    /// 安装态（默认 1；卸载 = 0，全岛隐藏 + 门控全关，数据保留）
    pub installed: bool,
    /// 本机是否探测到该 agent 安装（仅提示用；停用的插件不影响探测）
    pub installed_probe: bool,
    /// 能力域（skills / mcp）
    pub caps: adapters::AgentCaps,
    /// 状态来源：auto = 首次运行/新装按安装探测；manual = 用户手动设置
    pub source: String,
}

/// 确保状态表已初始化：空表 = 首次运行，全部置已安装、enabled 按安装探测；
/// 非空表也要给「新注册的适配器」补缺行（升级新增插件后 list() 才有行可读），幂等
fn ensure_initialized(conn: &rusqlite::Connection) -> Res<()> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM plugin_state", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    let first_run = count == 0;
    let mut detected: Vec<&'static str> = Vec::new();
    for a in adapters::registry() {
        // 新补行：installed=1（默认全装），enabled 按本机安装探测；存量行 OR IGNORE 跳过
        let probe = a.installed();
        let n = conn
            .execute(
                "INSERT OR IGNORE INTO plugin_state(agent, enabled, installed, source, updated_at)
                 VALUES (?1, ?2, 1, 'auto', datetime('now','localtime'))",
                rusqlite::params![a.id(), probe as i64],
            )
            .map_err(|e| e.to_string())?;
        if n > 0 && probe {
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
            let (enabled, installed, source) = conn
                .query_row(
                    "SELECT enabled, IFNULL(installed,1), IFNULL(source,'manual')
                     FROM plugin_state WHERE agent = ?1",
                    rusqlite::params![a.id()],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)? != 0,
                            row.get::<_, i64>(1)? != 0,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .map_err(|e| e.to_string())?;
            out.push(PluginInfo {
                agent: a.id().to_string(),
                display_name: a.display_name().to_string(),
                desc: a.desc().to_string(),
                enabled,
                installed,
                installed_probe: a.installed(),
                caps: a.caps(),
                source,
            });
        }
        Ok(out)
    })
}

/// 用户手动启停（持久化，后续启动只读此配置）。启用已卸载的插件 = 先安装。
pub fn set_enabled(store: &Store, agent: &str, enabled: bool) -> Res<()> {
    let Some(a) = adapters::by_id(agent) else {
        return Err(format!("未知适配器：{agent}"));
    };
    store.with(|conn| {
        ensure_initialized(conn)?;
        let (cur_enabled, cur_installed): (i64, i64) = conn
            .query_row(
                "SELECT enabled, IFNULL(installed,1) FROM plugin_state WHERE agent = ?1",
                rusqlite::params![agent],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| e.to_string())?;
        if enabled && cur_installed == 0 {
            return Err(format!("{} 插件已卸载，请先在 Plugin 岛重新安装", a.display_name()));
        }
        if cur_enabled == enabled as i64 {
            return Ok(()); // 幂等：状态未变不写
        }
        conn.execute(
            "UPDATE plugin_state SET enabled = ?2, source = 'manual', updated_at = datetime('now','localtime') WHERE agent = ?1",
            rusqlite::params![agent, enabled as i64],
        )
        .map_err(|e| e.to_string())?;
        store::log_op(conn, "plugins.toggle", agent, serde_json::json!({ "enabled": enabled, "display": a.display_name() }));
        Ok(())
    })
}

/// 安装 / 卸载（VS Code 语义）：卸载联动停用；重装恢复启用。
/// 数据红线：不删 usage_events / 目标文件配置，仅改状态。
pub fn set_installed(store: &Store, agent: &str, installed: bool) -> Res<()> {
    let Some(a) = adapters::by_id(agent) else {
        return Err(format!("未知适配器：{agent}"));
    };
    store.with(|conn| {
        ensure_initialized(conn)?;
        let cur_installed: i64 = conn
            .query_row(
                "SELECT IFNULL(installed,1) FROM plugin_state WHERE agent = ?1",
                rusqlite::params![agent],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if cur_installed == installed as i64 {
            return Ok(()); // 幂等：状态未变不写
        }
        conn.execute(
            "UPDATE plugin_state SET installed = ?2, enabled = ?3, source = 'manual', updated_at = datetime('now','localtime') WHERE agent = ?1",
            rusqlite::params![agent, installed as i64, installed as i64],
        )
        .map_err(|e| e.to_string())?;
        store::log_op(
            conn,
            if installed { "plugins.install" } else { "plugins.uninstall" },
            agent,
            serde_json::json!({ "display": a.display_name() }),
        );
        Ok(())
    })
}

/// 已安装且已启用插件的适配器列表（共享引擎统一门控；调用方再按 caps 过滤能力域）
pub fn active_adapters(store: &Store) -> Res<Vec<&'static dyn AgentAdapter>> {
    let ids = active_ids(store)?;
    Ok(adapters::registry()
        .iter()
        .copied()
        .filter(|a| ids.iter().any(|id| id == a.id()))
        .collect())
}

/// 已安装且已启用的插件 id 集合（active = installed && enabled）
pub fn active_ids(store: &Store) -> Res<Vec<String>> {
    store.with(|conn| {
        ensure_initialized(conn)?;
        let mut stmt = conn
            .prepare("SELECT agent FROM plugin_state WHERE IFNULL(installed,1) = 1 AND enabled = 1")
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

/// 门控断言：agent 插件必须已安装且已启用（各岛写操作与「唤起 agent」类命令的统一入口）。
/// 读操作（总览/列表）不强制走这里 —— 停用端的数据仍可读出用于置灰展示。
pub fn require_enabled(store: &Store, agent: &str) -> Result<(), String> {
    let Some(a) = adapters::by_id(agent) else {
        return Err(format!("未知适配器：{agent}"));
    };
    store.with(|conn| {
        ensure_initialized(conn)?;
        let (installed, enabled): (i64, i64) = conn
            .query_row(
                "SELECT IFNULL(installed,1), enabled FROM plugin_state WHERE agent = ?1",
                rusqlite::params![agent],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| e.to_string())?;
        if installed == 0 {
            return Err(format!("{} 插件已卸载（到 Plugin 岛重新安装后可用）", a.display_name()));
        }
        if enabled == 0 {
            return Err(format!("{} 插件已停用（到 Plugin 岛开启后可用）", a.display_name()));
        }
        Ok(())
    })
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

#[tauri::command]
pub fn plugin_install(state: State<'_, Store>, agent: String) -> Result<(), String> {
    set_installed(&state, &agent, true)
}

#[tauri::command]
pub fn plugin_uninstall(state: State<'_, Store>, agent: String) -> Result<(), String> {
    set_installed(&state, &agent, false)
}
