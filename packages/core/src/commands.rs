//! commands —— Tauri IPC 命令层（M1/M2：状态探测、迁移、skills 总览、重建链接）。
//!
//! 可插拔：本文件不出现具体 agent 名 —— 状态与分发全部经 adapters::registry() +
//! plugins 启用配置驱动；WB 专属特性（自动调用开关）显式标注 wb_auto。

use std::collections::HashMap;

use serde::Serialize;
use tauri::State;

use crate::adapters::{self, wb, zcode, SkillsLink};
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
    let skills_adapters = plugins::enabled_adapters(&store_state)?
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
    let wb_auto = wb_auto_map();
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
            wb_auto: wb_auto.get(&info.name).copied(),
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
    if !plugins::enabled_ids(&store_state)?.iter().any(|id| id == &agent) {
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

/// WB 端技能「允许模型自动调用」映射（skills-installed-store.json 的
/// disableModelInvocation 取反）；WB 未收录的技能不在表里
fn wb_auto_map() -> HashMap<String, bool> {
    let mut map = HashMap::new();
    for path in wb::skill_toggle_stores() {
        let Some(bytes) = fsutil::read_opt(&path) else { continue };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
        let Some(obj) = v.as_object() else { continue };
        for node in obj.values() {
            let Some(data) = node.get("data").and_then(|d| d.as_array()) else { continue };
            for e in data {
                let Some(name) = e.get("name").and_then(|n| n.as_str()) else { continue };
                let auto = !e
                    .get("disableModelInvocation")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(false);
                map.insert(name.to_string(), auto);
            }
        }
    }
    map
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
    sync_wb_skill_invocation(&store_state, &name, enabled)
}

/// WB 开关同步：写全部账号×scope 的 skills-installed-store.json
fn sync_wb_skill_invocation(
    store_state: &Store,
    name: &str,
    enabled: bool,
) -> Result<String, String> {
    let stores = wb::skill_toggle_stores();
    if stores.is_empty() {
        return Ok("WB 未安装或无技能存储，仅更新 AgentHub 记录".into());
    }
    let backups = store::backups_dir().join("wb-skill-toggle");
    let mut touched = 0usize;
    let mut seen = false;
    for path in &stores {
        let Some(bytes) = fsutil::read_opt(path) else { continue };
        let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
        let mut changed = false;
        if let Some(obj) = v.as_object_mut() {
            for node in obj.values_mut() {
                let Some(data) = node.get_mut("data").and_then(|d| d.as_array_mut()) else {
                    continue;
                };
                for e in data.iter_mut() {
                    if e.get("name").and_then(|n| n.as_str()) != Some(name) {
                        continue;
                    }
                    seen = true;
                    if e.get("disableModelInvocation").and_then(|b| b.as_bool()) != Some(!enabled) {
                        if let Some(em) = e.as_object_mut() {
                            em.insert(
                                "disableModelInvocation".into(),
                                serde_json::json!(!enabled),
                            );
                            changed = true;
                        }
                    }
                }
            }
        }
        if changed {
            fsutil::backup_file(path, &backups)?;
            fsutil::atomic_write(
                path,
                serde_json::to_vec_pretty(&v)
                    .map_err(|e| e.to_string())?
                    .as_slice(),
            )?;
            touched += 1;
        }
    }
    store_state.with(|conn| {
        store::log_op(
            conn,
            "skill.wb_toggle",
            name,
            serde_json::json!({ "enabled": enabled, "touched": touched }),
        );
        Ok(())
    })?;
    if !seen {
        return Ok(format!(
            "WB 存储未收录「{name}」（先在 WB 打开一次技能面板再试），本次仅更新 AgentHub 记录"
        ));
    }
    Ok(format!(
        "已同步 WB：「{name}」自动调用 {}（更新 {touched} 处存储，原文件已备份）",
        if enabled { "开" } else { "关" }
    ))
}

#[derive(Debug, Serialize)]
pub struct PluginSkillItem {
    /// 运行时技能名（plugin:目录名，如 github:commit）
    pub name: String,
    pub plugin: String,
    pub plugin_version: String,
    pub description: Option<String>,
    /// 插件本身是否在 cli/config.json enabledPlugins 中启用
    pub plugin_enabled: bool,
}

/// 取 YAML frontmatter 字段；折叠标记（> / >- / |）续收缩进行
fn fm_get(fm: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    let mut acc: Option<String> = None;
    for line in fm.lines() {
        if let Some(v) = line.strip_prefix(&prefix) {
            acc = Some(v.trim().trim_start_matches(['>', '-']).trim().to_string());
        } else if acc.is_some() {
            if line.starts_with(' ') {
                let t = line.trim();
                if !t.is_empty() {
                    let cur = acc.unwrap_or_default();
                    acc = Some(if cur.is_empty() {
                        t.to_string()
                    } else {
                        format!("{cur} {t}")
                    });
                }
            } else if !line.trim().is_empty() {
                break;
            }
        }
    }
    acc.filter(|s| !s.is_empty())
}

/// ZCode 插件自带技能（只读展示）：插件体系不经中央仓库分发，AgentHub 仅列出全貌
#[tauri::command]
pub fn zcode_plugin_skills() -> Result<Vec<PluginSkillItem>, String> {
    let mut out: Vec<PluginSkillItem> = Vec::new();
    let Some(bytes) = fsutil::read_opt(&zcode::installed_plugins_json()) else {
        return Ok(out);
    };
    let v: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("installed_plugins.json 解析失败: {e}"))?;
    let Some(plugins) = v.get("plugins").and_then(|x| x.as_array()) else {
        return Ok(out);
    };
    // 插件启用状态来自 cli/config.json plugins.enabledPlugins
    let enabled_map: HashMap<String, bool> = fsutil::read_opt(&zcode::cli_config())
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|cfg| {
            cfg.get("plugins")
                .and_then(|p| p.get("enabledPlugins"))
                .and_then(|m| m.as_object())
                .cloned()
        })
        .map(|m| {
            m.into_iter()
                .map(|(k, val)| (k, val.as_bool().unwrap_or(false)))
                .collect()
        })
        .unwrap_or_default();
    for p in plugins {
        let id = p.get("id").and_then(|x| x.as_str()).unwrap_or("");
        let name = p.get("name").and_then(|x| x.as_str()).unwrap_or("");
        let version = p.get("version").and_then(|x| x.as_str()).unwrap_or("");
        let install = p.get("installPath").and_then(|x| x.as_str()).unwrap_or("");
        if id.is_empty() || name.is_empty() || install.is_empty() {
            continue;
        }
        let install_path = std::path::PathBuf::from(install);
        // manifest 里的 skills 相对路径（缺省 ./skills）
        let skills_rel = fsutil::read_opt(&install_path.join(".claude-plugin").join("plugin.json"))
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|m| {
                m.get("skills")
                    .and_then(|s| s.as_str())
                    .map(|s| s.trim_start_matches("./").to_string())
            })
            .unwrap_or_else(|| "skills".into());
        let Ok(dirs) = std::fs::read_dir(install_path.join(skills_rel)) else {
            continue;
        };
        for d in dirs.flatten() {
            if !d.path().is_dir() {
                continue;
            }
            let dir_name = d.file_name().to_string_lossy().to_string();
            let Some(raw) = fsutil::read_opt(&d.path().join("SKILL.md")) else {
                continue;
            };
            let content = String::from_utf8_lossy(&raw);
            let fm = content
                .strip_prefix("---")
                .and_then(|rest| rest.find("\n---").map(|end| &rest[..end]))
                .unwrap_or("");
            out.push(PluginSkillItem {
                name: format!("{name}:{dir_name}"),
                plugin: name.to_string(),
                plugin_version: version.to_string(),
                description: fm_get(fm, "description"),
                plugin_enabled: enabled_map.get(id).copied().unwrap_or(false),
            });
        }
    }
    out.sort_by(|a, b| a.plugin.cmp(&b.plugin).then(a.name.cmp(&b.name)));
    Ok(out)
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
    for a in plugins::enabled_adapters(&store_state)?
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
