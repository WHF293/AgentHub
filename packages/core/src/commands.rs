//! commands —— Tauri IPC 命令层（M1/M2：状态探测、迁移、skills 总览、重建链接）。
//!
//! 可插拔：本文件不出现具体 agent 名 —— 状态与分发全部经 adapters::registry() +
//! plugins 启用配置驱动；WB 专属特性（自动调用开关）显式标注 wb_auto。

use std::collections::HashMap;

use serde::Serialize;
use tauri::State;

use crate::adapters::{self, SkillsLink};
use crate::fsutil;
use crate::migrate;
use crate::plugins;
use crate::store::{self, Store};

/// 单 agent 探测状态（app_status.agents 数组元素）
#[derive(Serialize)]
pub struct AgentStatus {
    pub agent: String,
    pub display_name: String,
    pub desc: String,
    /// 插件启用态（plugin_state，用户配置优先）
    pub plugin_enabled: bool,
    /// 本机安装探测
    pub installed: bool,
    pub caps: adapters::AgentCaps,
    pub skills_dir: String,
    pub skills_dir_is_link: bool,
    pub skills_dir_target: Option<String>,
    pub skills_count: usize,
    /// 登录态文件探测（无此概念的端恒 false）
    pub auth_found: bool,
}

#[derive(Serialize)]
pub struct AppStatus {
    pub hub_initialized: bool,
    pub central_skills_count: usize,
    pub link_mode: String,
    /// 全部已注册适配器（含停用插件，前端按 plugin_enabled 过滤展示）
    pub agents: Vec<AgentStatus>,
}

fn agent_status(a: &'static dyn adapters::AgentAdapter, plugin_enabled: bool) -> AgentStatus {
    let skills_dir = if a.caps().skills { a.skills_dir() } else { std::path::PathBuf::new() };
    let is_link = fsutil::is_link(&skills_dir);
    let skills_count = if is_link || skills_dir.is_dir() {
        adapters::scan_skills_dir(&skills_dir).len()
    } else {
        0
    };
    AgentStatus {
        agent: a.id().to_string(),
        display_name: a.display_name().to_string(),
        desc: a.desc().to_string(),
        plugin_enabled,
        installed: a.installed(),
        caps: a.caps(),
        skills_dir: skills_dir.to_string_lossy().to_string(),
        skills_dir_is_link: is_link,
        skills_dir_target: fsutil::link_target(&skills_dir).map(|p| p.to_string_lossy().to_string()),
        skills_count,
        auth_found: a.auth_found(),
    }
}

/// 启动探测：中央仓库 / 各适配器安装态与插件启用态 / 链接策略
#[tauri::command]
pub fn app_status(store_state: State<'_, Store>) -> AppStatus {
    store::ensure_hub_dirs().ok();
    let plugin_enabled = |id: &str| {
        plugins::list(&store_state)
            .ok()
            .and_then(|l| l.into_iter().find(|p| p.agent == id))
            .map(|p| p.enabled)
            .unwrap_or(false)
    };
    AppStatus {
        hub_initialized: store::db_path().exists(),
        central_skills_count: adapters::scan_skills_dir(&store::central_skills_dir()).len(),
        link_mode: migrate::parse_link_mode(None).as_str().to_string(),
        agents: adapters::registry()
            .iter()
            .map(|a| agent_status(*a, plugin_enabled(a.id())))
            .collect(),
    }
}

/// M1 一键迁移：各启用端 skills 并集导入中央仓库 → 按各端策略建链
#[tauri::command]
pub fn run_skills_migration(
    store_state: State<'_, Store>,
    mode: Option<String>,
) -> Result<migrate::MigrateReport, String> {
    migrate::migrate_skills(&store_state, mode.as_deref())
}

/// skill 每行在每启用端的状态：linked / diverged（实目录）/ broken（断链）/ missing
#[derive(Serialize)]
pub struct SkillAgentState {
    pub agent: String,
    pub display_name: String,
    pub status: String,
    /// 该端是否处于「整目录链」模式（此时单 skill 状态恒为 linked）
    pub whole_link: bool,
}

#[derive(Serialize)]
pub struct SkillOverviewItem {
    pub name: String,
    pub description: Option<String>,
    pub source: String,
    pub source_ref: Option<String>,
    pub enabled: bool,
    /// 各启用插件端的状态（数组顺序 = 注册表顺序）
    pub agents: Vec<SkillAgentState>,
    /// WB 端「允许模型自动调用」实测（None = WB 存储未收录；WB 专属特性）
    pub wb_auto: Option<bool>,
}

#[tauri::command]
pub fn skills_overview(store_state: State<'_, Store>) -> Result<Vec<SkillOverviewItem>, String> {
    store::ensure_hub_dirs()?;
    let central = store::central_skills_dir();
    let central_list = adapters::scan_skills_dir(&central);
    let skills_adapters = plugins::active_adapters(&store_state)?
        .into_iter()
        .filter(|a| a.caps().skills)
        .collect::<Vec<_>>();

    // DB 元数据（enabled / source / source_ref）
    let (enabled_map, source_map, srcref_map) = store_state.with(|conn| {
        let mut stmt = conn
            .prepare("SELECT name, IFNULL(enabled,1), IFNULL(source,''), source_ref FROM skills")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? != 0,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut en = HashMap::new();
        let mut src = HashMap::new();
        let mut srf = HashMap::new();
        for r in rows {
            let (n, e, s, rf) = r.map_err(|e| e.to_string())?;
            en.insert(n.clone(), e);
            src.insert(n.clone(), s);
            srf.insert(n, rf);
        }
        Ok((en, src, srf))
    })?;

    let mut items = Vec::new();
    // 各端「允许模型自动调用」实测聚合（钩子聚合，core 不感知具体端）
    let mut auto_map: HashMap<String, bool> = HashMap::new();
    for a in adapters::registry() {
        for (k, v) in a.skill_auto_map() {
            auto_map.entry(k).or_insert(v);
        }
    }
    for info in central_list {
        let central_skill = central.join(&info.name);
        let mut agent_states = Vec::new();
        for a in &skills_adapters {
            agent_states.push(SkillAgentState {
                agent: a.id().to_string(),
                display_name: a.display_name().to_string(),
                status: a.skill_link_status(&info.name, &central_skill),
                whole_link: a.skills_link() == SkillsLink::WholeDir
                    && a.whole_link_active(&central),
            });
        }
        items.push(SkillOverviewItem {
            name: info.name.clone(),
            description: info.description.clone(),
            enabled: *enabled_map.get(&info.name).unwrap_or(&true),
            source: source_map.get(&info.name).cloned().unwrap_or_default(),
            source_ref: srcref_map.get(&info.name).cloned().flatten(),
            agents: agent_states,
            wb_auto: auto_map.get(&info.name).copied(),
        });
    }
    Ok(items)
}

/// 单 skill 在单端重建链接（断链修复 / 分发）；端必须为已启用插件
#[tauri::command]
pub fn relink_skill(
    store_state: State<'_, Store>,
    name: String,
    agent: String,
) -> Result<String, String> {
    let Some(a) = adapters::by_id(&agent) else {
        return Err(format!("未知 agent：{agent}"));
    };
    if !plugins::active_ids(&store_state)?.iter().any(|id| id == &agent) {
        return Err(format!("{} 插件未启用（Plugin 岛开启后可分发）", a.display_name()));
    }
    if !a.caps().skills {
        return Err(format!("{} 不支持 skills 能力域", a.display_name()));
    }
    let central = store::central_skills_dir();
    let central_skill = central.join(&name);
    if !central_skill.is_dir() {
        return Err(format!("中央仓库无此 skill：{name}"));
    }
    let mode = migrate::parse_link_mode(None);
    let backups = store::backups_dir();
    let action = a.relink_skill(&name, &central, &backups, mode)?;
    if action.starts_with("failed") {
        return Err(action);
    }
    // 落库 + op_log
    let whole = a.skills_link() == SkillsLink::WholeDir;
    store_state.with(|conn| {
        let id: Option<i64> = conn
            .query_row(
                "SELECT id FROM skills WHERE name = ?1",
                rusqlite::params![name],
                |row| row.get(0),
            )
            .ok();
        if let Some(id) = id {
            let target = if whole {
                a.skills_dir().to_string_lossy().to_string()
            } else {
                a.skills_dir().join(&name).to_string_lossy().to_string()
            };
            let mode_str = if whole {
                format!("whole-{}", mode.as_str())
            } else {
                mode.as_str().to_string()
            };
            store::upsert_skill_link(conn, id, &agent, &mode_str, &target, "ok")?;
        }
        store::log_op(
            conn,
            "skill.relink",
            &name,
            serde_json::json!({ "agent": agent, "action": action }),
        );
        Ok(())
    })?;
    Ok("linked".to_string())
}

/// 启停 skill：DB 状态 + 写穿 WB「允许模型自动调用」（备份 → 原子写）。
/// ZCode 无 per-skill 禁用机制，不写 ZCode 侧；返回同步结果消息。
#[tauri::command]
pub fn set_skill_enabled(
    store_state: State<'_, Store>,
    name: String,
    enabled: bool,
) -> Result<String, String> {
    store_state.with(|conn| {
        conn.execute(
            "UPDATE skills SET enabled = ?2, updated_at = datetime('now','localtime') WHERE name = ?1",
            rusqlite::params![name, enabled as i64],
        )
        .map_err(|e| e.to_string())?;
        store::log_op(
            conn,
            "skill.set_enabled",
            &name,
            serde_json::json!({ "enabled": enabled }),
        );
        Ok(())
    })?;
    // 端侧联动聚合（如 WB 写穿自身 skills-installed-store.json），core 不感知具体端
    let mut notes: Vec<String> = Vec::new();
    for a in adapters::registry() {
        if let Some(msg) = a.on_skill_enabled(&store_state, &name, enabled)? {
            notes.push(msg);
        }
    }
    if notes.is_empty() {
        Ok("已更新 AgentHub 记录".into())
    } else {
        Ok(notes.join("；"))
    }
}
/// skill 引用移除：链接直接删；实目录（diverged，可能含未同步内容）先备份再移除
fn remove_skill_ref(target: &std::path::Path, backups: &std::path::Path) -> Result<(), String> {
    if fsutil::is_link(target) {
        std::fs::remove_dir(target).map_err(|e| format!("移除链接失败 {}: {e}", target.display()))?;
    } else if target.is_dir() {
        fsutil::move_dir_to_backup(target, backups)?;
    }
    Ok(())
}

/// 删除 skill：各启用端引用移除 + 中央仓库源文件删除前自动备份 + DB 行清理
#[tauri::command]
pub fn skill_delete(store_state: State<'_, Store>, name: String) -> Result<(), String> {
    let central = store::central_skills_dir();
    let central_skill = central.join(&name);
    if !central_skill.is_dir() {
        return Err(format!("中央仓库无此 skill：{name}"));
    }
    let backups = store::backups_dir();

    // 各启用插件端的引用（WholeDir 整链端：skills 目录即中央仓库本体，随源一并消失，跳过单条引用）
    for a in plugins::active_adapters(&store_state)?
        .into_iter()
        .filter(|a| a.caps().skills)
    {
        if a.skills_link() == SkillsLink::WholeDir && a.whole_link_active(&central) {
            continue;
        }
        remove_skill_ref(&a.skills_dir().join(&name), &backups)?;
    }

    // 中央仓库源文件 → 备份目录（删除语义，留快照兜底）
    fsutil::move_dir_to_backup(&central_skill, &backups)?;

    store_state.with(|conn| {
        conn.execute(
            "DELETE FROM skill_links WHERE skill_id = (SELECT id FROM skills WHERE name = ?1)",
            rusqlite::params![name],
        )
        .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM skills WHERE name = ?1", rusqlite::params![name])
            .map_err(|e| e.to_string())?;
        store::log_op(conn, "skill.delete", &name, serde_json::json!({}));
        Ok(())
    })
}

/// 最近操作日志（总览岛「最近操作」行）
#[derive(Serialize)]
pub struct OpLogRow {
    pub id: i64,
    pub action: String,
    pub target: Option<String>,
    pub detail_json: Option<String>,
    pub created_at: Option<String>,
}

#[tauri::command]
pub fn recent_op_log(store_state: State<'_, Store>, limit: i64) -> Result<Vec<OpLogRow>, String> {
    store_state.with(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT id, action, target, detail_json, created_at FROM op_log
                 ORDER BY id DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![limit.clamp(1, 200)], |row| {
                Ok(OpLogRow {
                    id: row.get(0)?,
                    action: row.get(1)?,
                    target: row.get(2)?,
                    detail_json: row.get(3)?,
                    created_at: row.get(4)?,
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

/// 关闭行为切换：true = 关到托盘（默认），false = 直接退出（设置岛）
#[tauri::command]
pub fn set_close_to_tray(enabled: bool) {
    crate::CLOSE_TO_TRAY.store(enabled, std::sync::atomic::Ordering::Relaxed);
}
