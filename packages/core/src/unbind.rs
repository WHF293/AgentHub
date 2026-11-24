//! unbind —— 解绑引擎：消除各 Agent 对 ~/.agenthub 的运行时依赖。
//!
//! 目标（方案 §1 定版）：把「链接位」还原为各 Agent 自己的独立实体文件/目录，卸载
//! AgentHub 后各端不再出现技能/记忆异常。原则：
//! - 删除只针对链接本身；任何被替换的实体内容先归档到 backups/unbind-<stamp>/；
//! - 内容选择信息不丢：优先还原原目录最新备份（backups/ 根 `<stamp>_<name>`），
//!   无备份则用中央仓库内容落成实文件；
//! - MCP / provider 配置自包含（不引用 ~/.agenthub），保留现状不回滚；
//! - 解绑标志落 app_settings.unbound=1（幂等抢占），后台采集线程据此自动停止。
//!
//! 各端 skills 链还原经 AgentAdapter::unbind 钩子分发（引擎不感知具体端）；
//! 全局记忆 symlink 与 .ai/ 桥接块由 core 统一处理（memory_targets / instruction_file 钩子）。

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::adapters::{self, eq_path, UnbindOpts};
use crate::fsutil::{self, Res};
use crate::memory::{BLOCK_END, BLOCK_START};
use crate::store::{self, Store};

/// app_settings 键：解绑标志
pub const UNBOUND_KEY: &str = "unbound";

#[derive(Debug, Serialize)]
pub struct UnbindReport {
    pub steps: Vec<String>,
    pub failed: usize,
    pub backup_dir: String,
    /// 已处于解绑状态（幂等跳过）
    pub already_unbound: bool,
}

/// 是否已解绑（后台采集线程每次巡检读一次；未建表/未设置返回 false）
pub fn is_unbound(store: &Store) -> bool {
    store
        .with(|conn| store::setting_get(conn, UNBOUND_KEY))
        .map(|v| v.as_deref() == Some("1"))
        .unwrap_or(false)
}

/// 清除解绑标志（重新接管前调用）
pub fn clear_unbound(store: &Store) -> Res<()> {
    store.with(|conn| store::setting_set(conn, UNBOUND_KEY, "0"))
}

// ─── 共享还原助手（各插件 unbind 钩子复用） ───

/// link_path 是否为指向 target 的目录/文件链接
fn link_points_to(link_path: &Path, target: &Path) -> bool {
    fsutil::is_link(link_path)
        && fsutil::link_target(link_path)
            .map(|t| eq_path(&t.to_string_lossy(), &target.to_string_lossy()))
            .unwrap_or(false)
}

/// 移除链接（symlink-dir / junction 走 remove_dir，文件 symlink 走 remove_file）
fn remove_link(link_path: &Path) -> Res<()> {
    std::fs::remove_dir(link_path)
        .or_else(|_| std::fs::remove_file(link_path))
        .map_err(|e| format!("移除链接失败 {}: {e}", link_path.display()))
}

/// backups 根目录下找 `<stamp>_<name>`（stamp = YYYYMMDD_HHMMSS 共 15 字符）里
/// name 匹配的最新一份备份
pub fn newest_backup(backups_root: &Path, name: &str) -> Option<PathBuf> {
    let mut best: Option<(String, PathBuf)> = None;
    for entry in std::fs::read_dir(backups_root).ok()?.flatten() {
        let fname = entry.file_name().to_string_lossy().to_string();
        if fname.len() <= 16 || fname.as_bytes()[15] != b'_' || &fname[16..] != name {
            continue;
        }
        let stamp = fname[..15].to_string();
        if !stamp.chars().all(|c| c.is_ascii_digit() || c == '_') {
            continue;
        }
        if best.as_ref().map(|(s, _)| stamp > *s).unwrap_or(true) {
            best = Some((stamp, entry.path()));
        }
    }
    best.map(|(_, p)| p)
}

/// 把备份先归档到 archive_dir 再搬回 dest（跨卷 rename 失败降级 copy+删除备份件）
fn restore_backup(backup: &Path, dest: &Path, archive_dir: &Path) -> Res<()> {
    let archived = archive_dir.join(format!("restore-{}", backup.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "item".into())));
    if backup.is_dir() {
        fsutil::copy_dir_recursive(backup, &archived)?;
        if let Some(par) = dest.parent() {
            std::fs::create_dir_all(par).map_err(|e| e.to_string())?;
        }
        if std::fs::rename(backup, dest).is_err() {
            fsutil::copy_dir_recursive(backup, dest)?;
            std::fs::remove_dir_all(backup).map_err(|e| e.to_string())?;
        }
    } else {
        std::fs::create_dir_all(archive_dir).map_err(|e| e.to_string())?;
        std::fs::copy(backup, &archived).map_err(|e| format!("归档备份失败: {e}"))?;
        if let Some(par) = dest.parent() {
            std::fs::create_dir_all(par).map_err(|e| e.to_string())?;
        }
        std::fs::copy(backup, dest).map_err(|e| format!("还原备份失败: {e}"))?;
    }
    Ok(())
}

/// 整目录链还原（WholeDir 端用）：删链接 → 还原原目录最新备份 / 中央仓库 copy 成实目录。
/// 返回动作描述；非链接（实目录/不存在）保持原样不动。
pub fn unlink_restore_dir(
    link_path: &Path,
    central: &Path,
    backups_root: &Path,
    archive_dir: &Path,
) -> Res<String> {
    if !fsutil::is_link(link_path) {
        return Ok("非链接（实目录或不存在），保持原样".into());
    }
    if !link_points_to(link_path, central) {
        return Ok(format!(
            "链接指向别处（{}），不动",
            fsutil::link_target(link_path)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "?".into())
        ));
    }
    let dirname = link_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "skills".into());
    remove_link(link_path)?;
    if let Some(backup) = newest_backup(backups_root, &dirname) {
        restore_backup(&backup, link_path, archive_dir)?;
        return Ok("链接已移除，原目录已从备份还原".into());
    }
    if central.is_dir() {
        std::fs::create_dir_all(link_path.parent().unwrap_or(Path::new(".")))
            .map_err(|e| e.to_string())?;
        fsutil::copy_dir_recursive(central, link_path)?;
        return Ok("链接已移除，中央仓库内容已复制为实目录".into());
    }
    Ok("⚠ 链接已移除；无备份且中央仓库缺失，目录留空".into())
}

/// 逐项链还原（PerSkill 端用）：skills_dir 下每个指向中央仓库的链接单独还原。
/// 返回各 skill 的动作描述（保留端目录里的实目录/外链不动）。
pub fn unlink_restore_per_skill(
    skills_dir: &Path,
    central: &Path,
    backups_root: &Path,
    archive_dir: &Path,
) -> Res<Vec<String>> {
    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return Ok(vec!["skills 目录不存在，跳过".into()]);
    };
    let mut steps = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || !fsutil::is_link(&path) {
            continue; // 实目录 / 非链接不碰
        }
        let central_skill = central.join(&name);
        if !link_points_to(&path, &central_skill) {
            steps.push(format!("{name}: 链接指向别处，不动"));
            continue;
        }
        remove_link(&path)?;
        if let Some(backup) = newest_backup(backups_root, &name) {
            restore_backup(&backup, &path, archive_dir)?;
            steps.push(format!("{name}: 已从备份还原"));
        } else if central_skill.is_dir() {
            std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))
                .map_err(|e| e.to_string())?;
            fsutil::copy_dir_recursive(&central_skill, &path)?;
            steps.push(format!("{name}: 中央仓库内容已复制为实目录"));
        } else {
            steps.push(format!("{name}: ⚠ 无备份且中央仓库缺失，已移除链接留空"));
        }
    }
    if steps.is_empty() {
        steps.push("没有指向中央仓库的链接".into());
    }
    Ok(steps)
}

// ─── 主流程 ───

/// 解绑全流程：adapter 钩子（skills 链）→ 全局记忆 symlink → .ai/ 桥接块 → 标志落库。
/// 幂等：unbound 标志 INSERT 抢占，二次调用直接返回 already_unbound 报告。
pub fn unbind_all(store: &Store) -> Res<UnbindReport> {
    let archive_dir = store::backups_dir().join(format!("unbind-{}", fsutil::now_stamp()));
    // 标志抢占：changes()==0 说明已存在 → 幂等返回
    let already = store.with(|conn| {
        conn.execute(
            "INSERT OR IGNORE INTO app_settings(key, value, updated_at) VALUES (?1, '1', datetime('now','localtime'))",
            rusqlite::params![UNBOUND_KEY],
        )
        .map_err(|e| e.to_string())?;
        Ok(conn.changes() == 0)
    })?;
    if already {
        return Ok(UnbindReport {
            steps: vec!["已处于解绑状态（幂等，未重复执行文件动作）".into()],
            failed: 0,
            backup_dir: archive_dir.to_string_lossy().to_string(),
            already_unbound: true,
        });
    }
    let mut steps = vec![format!(
        "开始解绑：移除各 Agent 指向 ~/.agenthub 的链接并还原独立实体（归档 {}）",
        archive_dir.display()
    )];
    let opts = UnbindOpts {
        archive_dir: archive_dir.clone(),
    };
    let backups_root = store::backups_dir();
    let mut failed = 0usize;

    // 1) 各端 skills 链还原（adapter 钩子分发；未安装的端无残留可还原，跳过）
    for a in adapters::registry() {
        if !a.installed() {
            continue;
        }
        match a.unbind(store, &opts) {
            Ok(vs) if vs.is_empty() => {}
            Ok(vs) => steps.extend(vs),
            Err(e) => {
                failed += 1;
                steps.push(format!("⚠ {} skills 解绑失败: {e}", a.display_name()));
            }
        }
    }

    // 2) 全局记忆 symlink 还原：删链 → 真身全文写回独立实体（真身缺失降级 backups/memory 备份）
    let canonical = store::central_memory_md();
    let mem_backups = backups_root.join("memory");
    for a in adapters::registry() {
        if !a.installed() {
            continue;
        }
        for t in a.memory_targets() {
            let p = &t.path;
            if !fsutil::is_link(p) {
                continue; // 已是独立实体或不存在
            }
            if !link_points_to(p, &canonical) {
                steps.push(format!("⚠ {}: 记忆链接指向别处，不动", t.label));
                continue;
            }
            let r = (|| -> Res<String> {
                std::fs::remove_file(p).map_err(|e| format!("移除记忆链接失败: {e}"))?;
                if canonical.is_file() {
                    let body = std::fs::read(&canonical)
                        .map_err(|e| format!("读真身失败: {e}"))?;
                    if let Some(par) = p.parent() {
                        std::fs::create_dir_all(par).map_err(|e| e.to_string())?;
                    }
                    fsutil::atomic_write(p, &body)?;
                    Ok("记忆链接已移除，统一内容已写回独立文件".into())
                } else if let Some(backup) = newest_backup(&mem_backups, &p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()) {
                    restore_backup(&backup, p, &archive_dir)?;
                    Ok("记忆链接已移除，已从备份还原原文件".into())
                } else {
                    Ok("⚠ 记忆链接已移除；真身与备份均缺失，未写入实体文件".into())
                }
            })();
            match r {
                Ok(msg) => steps.push(format!("✓ {}: {msg}", t.label)),
                Err(e) => {
                    failed += 1;
                    steps.push(format!("⚠ {}: {e}", t.label));
                }
            }
        }
    }

    // 3) .ai/ 桥接块清理：已注册项目 × 各端指令文件，剔除 agent-hub 标记块
    let projects: Vec<String> = store
        .with(|conn| {
            let mut stmt = conn
                .prepare("SELECT path FROM projects")
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
        .unwrap_or_default();
    let mut bridged = 0usize;
    for proj in &projects {
        for a in adapters::registry() {
            if !a.installed() {
                continue;
            }
            if let Some((name, _)) = a.instruction_file() {
                let f = Path::new(proj).join(name);
                if !f.is_file() {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&f) else {
                    continue;
                };
                let (Some(s), Some(e_rel)) = (content.find(BLOCK_START), content.find(BLOCK_END))
                else {
                    continue;
                };
                if e_rel < s {
                    continue;
                }
                let mut out = String::new();
                out.push_str(content[..s].trim_end());
                out.push('\n');
                out.push_str(content[e_rel + BLOCK_END.len()..].trim_start_matches('\n'));
                if fsutil::backup_file(&f, &archive_dir.join("bridge")).is_ok()
                    && fsutil::atomic_write(&f, out.as_bytes()).is_ok()
                {
                    bridged += 1;
                } else {
                    failed += 1;
                    steps.push(format!("⚠ 桥接块清理失败：{}", f.display()));
                }
            }
        }
    }
    if bridged > 0 {
        steps.push(format!("✓ 已清理 {bridged} 处 .ai/ 桥接块（原文件已归档）"));
    }

    // 4) 收尾说明 + op_log
    steps.push("MCP / 模型配置自包含（不引用 ~/.agenthub），保留现状；后续同步已停止".into());
    steps.push("✓ 解绑完成：卸载 AgentHub 后各 Agent 不会再出现技能/记忆异常".into());
    store.with(|conn| {
        store::log_op(
            conn,
            "unbind.all",
            "~/.agenthub",
            serde_json::json!({ "steps": steps, "failed": failed }),
        );
        Ok(())
    })?;
    Ok(UnbindReport {
        steps,
        failed,
        backup_dir: archive_dir.to_string_lossy().to_string(),
        already_unbound: false,
    })
}

// ─── Tauri commands ───

#[tauri::command]
pub async fn unbind_status(state: tauri::State<'_, Store>) -> Result<bool, String> {
    Ok(is_unbound(&state))
}

/// 解绑（异步命令；引擎同步执行，IO 量级为各端 skills/memory 目录，秒级）
#[tauri::command]
pub async fn unbind_run(state: tauri::State<'_, Store>) -> Result<UnbindReport, String> {
    unbind_all(&state)
}

/// 重新接管：清解绑标志 → skills 迁移建链 → MCP 全量同步 → 全局记忆统一（全部幂等）
#[tauri::command]
pub async fn unbind_reclaim(state: tauri::State<'_, Store>) -> Result<Vec<String>, String> {
    if !is_unbound(&state) {
        return Err("当前未处于解绑状态，无需重新接管".into());
    }
    clear_unbound(&state)?;
    let mut steps = vec!["✓ 已清除解绑标志，开始重新接管".into()];
    match crate::migrate::migrate_skills(&state, None) {
        Ok(r) => steps.push(format!(
            "✓ skills 迁移：导入 {} 项 · {}",
            r.imported.len(),
            r.agents
                .iter()
                .map(|a| format!("{} {}", a.display_name, a.action))
                .collect::<Vec<_>>()
                .join("；")
        )),
        Err(e) => steps.push(format!("⚠ skills 迁移失败: {e}")),
    }
    match crate::mcp::sync_all(&state) {
        Ok(rs) => steps.push(format!(
            "✓ MCP 同步：{}",
            rs.iter()
                .map(|r| format!("{} {} 项", r.display_name, r.written))
                .collect::<Vec<_>>()
                .join("；")
        )),
        Err(e) => steps.push(format!("⚠ MCP 同步失败: {e}")),
    }
    match crate::memory::unify_global_memory(&state) {
        Ok(ss) => steps.extend(ss),
        Err(e) => steps.push(format!("⚠ 全局记忆统一失败: {e}")),
    }
    steps.push("✓ 重新接管完成".into());
    state.with(|conn| {
        store::log_op(conn, "unbind.reclaim", "~/.agenthub", serde_json::json!({}));
        Ok(())
    })?;
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "agenthub-unbind-test-{}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// 备份目录模拟 move_dir_to_backup 语义：<stamp>_<dirname>
    fn make_backup(backups: &Path, name: &str, content_dir: &Path) {
        let stamp = fsutil::now_stamp();
        let backup = backups.join(format!("{stamp}_{name}"));
        fsutil::copy_dir_recursive(content_dir, &backup).unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn whole_dir_link_restores_from_backup() {
        let root = tmp("whole");
        let central = root.join("central");
        let agent_skills = root.join("agent").join("skills");
        let backups = root.join("backups");
        fs::create_dir_all(central.join("foo")).unwrap();
        fs::write(central.join("foo").join("SKILL.md"), "central").unwrap();
        // 原目录 → 备份（迁移腾挪语义），原位换成链接
        fs::create_dir_all(agent_skills.join("foo")).unwrap();
        fs::write(agent_skills.join("foo").join("SKILL.md"), "orig").unwrap();
        make_backup(&backups, "skills", &agent_skills);
        fs::remove_dir_all(&agent_skills).unwrap();
        let mode = fsutil::detect_link_mode(&root);
        if mode == fsutil::LinkMode::Copy {
            let _ = fs::remove_dir_all(&root);
            return; // copy 模式本就无链接，解绑无意义
        }
        fsutil::make_dir_link(&agent_skills, &central, mode).unwrap();

        let archive = root.join("archive");
        let action =
            unlink_restore_dir(&agent_skills, &central, &backups, &archive).unwrap();
        assert!(action.contains("备份"), "动作描述应含『备份』：{action}");
        assert!(!fsutil::is_link(&agent_skills));
        assert_eq!(
            fs::read_to_string(agent_skills.join("foo").join("SKILL.md")).unwrap(),
            "orig"
        );
        // 归档留了还原前备份快照
        let archived_any = fs::read_dir(&archive)
            .map(|mut it| it.any(|e| e.is_ok()))
            .unwrap_or(false);
        assert!(archived_any, "还原前备份应已归档");
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn per_skill_link_falls_back_to_central_copy_without_backup() {
        let root = tmp("per");
        let central = root.join("central");
        let skills_dir = root.join("wb").join("skills");
        let backups = root.join("backups");
        for name in ["has-backup", "no-backup"] {
            fs::create_dir_all(central.join(name)).unwrap();
            fs::write(central.join(name).join("SKILL.md"), "central").unwrap();
        }
        fs::create_dir_all(&skills_dir).unwrap();
        let mode = fsutil::detect_link_mode(&root);
        if mode == fsutil::LinkMode::Copy {
            let _ = fs::remove_dir_all(&root);
            return;
        }
        fsutil::make_dir_link(&skills_dir.join("has-backup"), &central.join("has-backup"), mode)
            .unwrap();
        fsutil::make_dir_link(&skills_dir.join("no-backup"), &central.join("no-backup"), mode)
            .unwrap();
        // 只有 has-backup 有迁移备份
        let orig = root.join("orig-has-backup");
        fs::create_dir_all(&orig).unwrap();
        fs::write(orig.join("SKILL.md"), "orig").unwrap();
        make_backup(&backups, "has-backup", &orig);

        let archive = root.join("archive");
        let steps = unlink_restore_per_skill(&skills_dir, &central, &backups, &archive).unwrap();
        assert!(!fsutil::is_link(&skills_dir.join("has-backup")));
        assert!(!fsutil::is_link(&skills_dir.join("no-backup")));
        assert_eq!(
            fs::read_to_string(skills_dir.join("has-backup").join("SKILL.md")).unwrap(),
            "orig"
        );
        assert_eq!(
            fs::read_to_string(skills_dir.join("no-backup").join("SKILL.md")).unwrap(),
            "central"
        );
        let joined = steps.join("\n");
        assert!(joined.contains("has-backup: 已从备份还原"), "{joined}");
        assert!(joined.contains("no-backup: 中央仓库内容已复制"), "{joined}");
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn foreign_link_and_real_dir_untouched() {
        let root = tmp("foreign");
        let central = root.join("central");
        let skills_dir = root.join("wb").join("skills");
        let backups = root.join("backups");
        fs::create_dir_all(central.join("mine")).unwrap();
        fs::write(central.join("mine").join("SKILL.md"), "central").unwrap();
        fs::create_dir_all(&skills_dir).unwrap();
        let mode = fsutil::detect_link_mode(&root);
        if mode == fsutil::LinkMode::Copy {
            let _ = fs::remove_dir_all(&root);
            return;
        }
        // 指向别处的链接 + 实目录（diverged）
        let elsewhere = root.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fsutil::make_dir_link(&skills_dir.join("foreign"), &elsewhere, mode).unwrap();
        fs::create_dir_all(skills_dir.join("diverged")).unwrap();
        fs::write(skills_dir.join("diverged").join("SKILL.md"), "user").unwrap();

        let steps =
            unlink_restore_per_skill(&skills_dir, &central, &backups, &root.join("archive"))
                .unwrap();
        let joined = steps.join("\n");
        assert!(fsutil::is_link(&skills_dir.join("foreign")), "外链不应被动");
        assert!(joined.contains("foreign: 链接指向别处"), "{joined}");
        assert_eq!(
            fs::read_to_string(skills_dir.join("diverged").join("SKILL.md")).unwrap(),
            "user"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
