//! migrate —— skills 迁移与分发引擎：从各启用 agent 的现有 skill 目录导入中央仓库并重建链接。
//!
//! 流程（方案 §2 / §8.11）：扫描各启用插件端 skills 目录 → 并集导入 ~/.agenthub/skills
//! → 按各端分发策略建链（逐项链 / 整目录链，由 AgentAdapter 声明）。所有被替换的实目录先备份。
//! 引擎不含具体 agent 逻辑 —— 全部经 AgentAdapter trait 分发。

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::adapters::{self, SkillsLink};
use crate::fsutil::{self, LinkMode, Res};
use crate::plugins;
use crate::store::{self, Store};

fn central_str() -> String {
    store::central_skills_dir().to_string_lossy().to_string()
}

pub fn parse_link_mode(pref: Option<&str>) -> LinkMode {
    match pref {
        Some("symlink") => LinkMode::Symlink,
        Some("junction") => LinkMode::Junction,
        Some("copy") => LinkMode::Copy,
        _ => fsutil::detect_link_mode(&store::hub_dir()),
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct ImportedSkill {
    pub name: String,
    /// 来源 agent（多个用 + 连接）
    pub from: String,
    /// 多端并存时取了哪一侧（更新者胜出）
    pub took: String,
}

/// 单 agent 端的迁移/分发结果
#[derive(Debug, Serialize)]
pub struct AgentMigrateReport {
    pub agent: String,
    pub display_name: String,
    /// 整端动作：PerSkill 端 = "逐项链 relinked/total"；WholeDir 端 = 整链动作
    /// （already-linked / relinked / replaced-with-link / created）
    pub action: String,
    pub relinked: Vec<String>,
    pub kept: Vec<String>,
    pub failed: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct MigrateReport {
    pub central_dir: String,
    pub link_mode: String,
    pub imported: Vec<ImportedSkill>,
    /// 中央仓库已有且未变化的数量（幂等重跑时跳过复制）
    pub skipped_existing: usize,
    pub import_warnings: Vec<String>,
    /// 各启用插件端的结果（注册表顺序）
    pub agents: Vec<AgentMigrateReport>,
    pub warnings: Vec<String>,
}

/// 单个 skill 的候选（来自各端扫描）
struct Candidate {
    agent: &'static str,
    info: adapters::SkillDirInfo,
}

/// M1 主流程：导入 → 按各端策略建链 → 全程 op_log
pub fn migrate_skills(store: &Store, pref: Option<&str>) -> Res<MigrateReport> {
    store::ensure_hub_dirs()?;
    let backups = store::backups_dir();
    let mode = parse_link_mode(pref);
    let central = store::central_skills_dir();
    let mut warnings: Vec<String> = Vec::new();

    let skills_adapters = plugins::enabled_adapters(store)?
        .into_iter()
        .filter(|a| a.caps().skills)
        .collect::<Vec<_>>();

    // 1) 扫描各启用端（只导入实目录；软链与断链不导入）
    let mut candidates: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
    for a in &skills_adapters {
        for info in adapters::scan_skills_dir(&a.skills_dir()) {
            if info.importable {
                candidates
                    .entry(info.name.clone())
                    .or_default()
                    .push(Candidate { agent: a.id(), info });
            } else if info.is_link {
                warnings.push(format!(
                    "{} 侧「{}」是软链（→ {}），跳过导入",
                    a.display_name(),
                    info.name,
                    info.link_target.as_deref().unwrap_or("?")
                ));
            }
        }
    }

    // 2) 并集导入中央仓库（多端并存取更新者；已有且未变化则跳过复制）
    let mut imported: Vec<ImportedSkill> = Vec::new();
    let mut skipped_existing: usize = 0;
    let mut import_warnings: Vec<String> = Vec::new();
    store.with(|conn| {
        for (name, cands) in &candidates {
            let best = cands
                .iter()
                .max_by_key(|c| c.info.mtime_ms)
                .expect("candidates 非空");
            let dest = central.join(name);
            let dest_mtime_ms = std::fs::metadata(&dest)
                .ok()
                .and_then(|md| md.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let need_copy = !dest.is_dir() || best.info.mtime_ms > dest_mtime_ms;
            if need_copy {
                if dest.exists() {
                    fsutil::move_dir_to_backup(&dest, &backups).map_err(|e| {
                        format!("中央仓库既有「{name}」备份失败: {e}")
                    })?;
                }
                if let Err(e) = fsutil::copy_dir_recursive(Path::new(&best.info.path), &dest) {
                    import_warnings.push(format!("「{name}」导入失败: {e}"));
                    continue;
                }
            }
            let (_, description) = fsutil::parse_skill_md(&dest.join("SKILL.md"));
            let desc = description.or(best.info.description.clone());
            let sources: Vec<&str> = cands.iter().map(|c| c.agent).collect();
            let id = store::upsert_skill(
                conn,
                name,
                &dest.to_string_lossy(),
                desc.as_deref(),
                "migrate",
                &sources.join("+"),
            )?;
            for agent in &sources {
                store::upsert_skill_link(conn, id, agent, mode.as_str(), "", "pending")?;
            }
            if need_copy {
                let from = sources.join("+");
                let took = if cands.len() > 1 {
                    format!("{}（更新者）", best.agent)
                } else {
                    best.agent.to_string()
                };
                imported.push(ImportedSkill {
                    name: name.clone(),
                    from,
                    took,
                });
            } else {
                skipped_existing += 1;
            }
        }
        store::log_op(
            conn,
            "skills.import",
            &central.to_string_lossy(),
            serde_json::json!({
                "imported": imported.len(),
                "skipped": skipped_existing,
                "warnings": import_warnings,
            }),
        );
        Ok(())
    })?;

    // 3) 各启用端按策略建链
    let central_list = adapters::scan_skills_dir(&central);
    let mut agent_reports: Vec<AgentMigrateReport> = Vec::new();
    for a in &skills_adapters {
        let mut report = AgentMigrateReport {
            agent: a.id().to_string(),
            display_name: a.display_name().to_string(),
            action: String::new(),
            relinked: Vec::new(),
            kept: Vec::new(),
            failed: Vec::new(),
        };
        match a.skills_link() {
            SkillsLink::PerSkill => {
                for info in &central_list {
                    match a.relink_skill(&info.name, &central, &backups, mode) {
                        Ok(action) if action == "relinked" => report.relinked.push(info.name.clone()),
                        Ok(_) => report.kept.push(info.name.clone()),
                        Err(e) => {
                            report.failed.push(info.name.clone());
                            warnings.push(format!(
                                "{}「{}」建链失败: {e}",
                                a.display_name(),
                                info.name
                            ));
                            continue;
                        }
                    }
                }
                report.action = format!("逐项链 {}/{}", report.relinked.len(), central_list.len());
            }
            SkillsLink::WholeDir => match a.ensure_skills_link(&central, &backups, mode) {
                Ok(action) => report.action = action,
                Err(e) => {
                    report.action = format!("failed:{e}");
                    warnings.push(format!("{} 整目录链失败: {e}", a.display_name()));
                }
            },
        }
        // 落库链接状态
        let whole = a.skills_link() == SkillsLink::WholeDir;
        store.with(|conn| {
            for info in &central_list {
                if let Ok(id) = fetch_skill_id(conn, &info.name) {
                    let broken = report.failed.contains(&info.name)
                        || report.action.starts_with("failed");
                    let target = if whole {
                        a.skills_dir().to_string_lossy().to_string()
                    } else {
                        a.skills_dir().join(&info.name).to_string_lossy().to_string()
                    };
                    let mode_str = if whole {
                        format!("whole-{}", mode.as_str())
                    } else {
                        mode.as_str().to_string()
                    };
                    let _ = store::upsert_skill_link(
                        conn,
                        id,
                        a.id(),
                        &mode_str,
                        &target,
                        if broken { "broken" } else { "ok" },
                    );
                }
            }
            Ok(())
        })?;
        agent_reports.push(report);
    }

    store.with(|conn| {
        store::log_op(
            conn,
            "skills.migrate",
            &central.to_string_lossy(),
            serde_json::json!({
                "imported": imported.len(),
                "skipped": skipped_existing,
                "per_agent": agent_reports.iter().map(|r| serde_json::json!({
                    "agent": r.agent,
                    "action": r.action,
                    "relinked": r.relinked.len(),
                    "kept": r.kept.len(),
                    "failed": r.failed.len(),
                })).collect::<Vec<_>>(),
                "link_mode": mode.as_str(),
            }),
        );
        Ok(())
    })?;

    Ok(MigrateReport {
        central_dir: central_str(),
        link_mode: mode.as_str().to_string(),
        imported,
        skipped_existing,
        import_warnings,
        agents: agent_reports,
        warnings,
    })
}

fn fetch_skill_id(conn: &rusqlite::Connection, name: &str) -> Res<i64> {
    conn.query_row(
        "SELECT id FROM skills WHERE name = ?1",
        rusqlite::params![name],
        |row| row.get::<_, i64>(0),
    )
    .map_err(|e| e.to_string())
}
