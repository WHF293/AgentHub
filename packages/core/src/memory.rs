//! memory —— M6 记忆管理：全局记忆（用户级，可统一为双端共写）+ 项目记忆 .ai/ 桥接。
//!
//! 全局：WB user-<uid>-personal/MEMORY.md 与 ZCode AGENTS.md 可编辑（写前备份）；
//!       「统一全局记忆」后真身 ~/.agenthub/MEMORY.md，双端路径为指向它的文件符号链接，
//!       用户手动改 / 任一 Agent 保存全局记忆落的都是同一份（unify_global_memory，幂等）。
//! 项目：.ai/ 统一记忆 + 指令文件幂等桥接块（init / sync 漂移检测 / consolidate 蒸馏），
//!       方案 §4 定版；不碰各 App 内部记忆目录。

use std::path::{Path, PathBuf};

use chrono::TimeZone;
use serde::Serialize;

use crate::adapters::{agent_home, eq_path, MemoryTarget};
use crate::fsutil::{self, LinkMode, Res};
use crate::store::{self, Store};

pub const BLOCK_START: &str = "<!-- agent-hub:start -->";
pub const BLOCK_END: &str = "<!-- agent-hub:end -->";
pub const BRIDGE_BLOCK: &str = "<!-- agent-hub:start -->
本项目所有 AI agent 共享记忆位于 `.ai/`：
- 开始任务前，先读 `.ai/MEMORY.md` 和 `.ai/memory/` 下与任务相关的记忆文件
- 完成实质工作后，记入 `.ai/memory/`（按主题归档，同一主题追加进已有文件，不要按日期重复建文件）：
  - 文件名规范：`YYYY-MM-DD-memoryTitle.md`，如 `2026-11-26-antd组件库踩坑记录.md`
  - 同一文件可能被多个 agent 先后补充，每条记录必须标注记录人与时间：
    格式 `- [YYYY-MM-DD HH:MM @agent名] 记录内容`，如 `- [2026-11-26 14:30 @claude] ...`
- 稳定的项目约定写入 `.ai/PROJECT.md`
<!-- agent-hub:end -->";

/// FNV-1a 64：零依赖稳定 hash（桥接块漂移指纹）
fn fnv1a(data: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

// ─── 全局记忆 ───

#[derive(Debug, Serialize)]
pub struct MemoryFile {
    pub id: String,
    pub label: String,
    pub path: String,
    pub agent: String,
    /// ZCode AGENTS.md 默认只读
    pub readonly_default: bool,
    pub size: u64,
    pub updated: String,
}

fn file_meta(p: &Path, id: &str, label: &str, agent: &str, ro: bool) -> Option<MemoryFile> {
    let md = std::fs::metadata(p).ok()?;
    let updated = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| {
            chrono::Local
                .timestamp_opt(d.as_secs() as i64, 0)
                .single()
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    Some(MemoryFile {
        id: id.to_string(),
        label: label.to_string(),
        path: p.to_string_lossy().to_string(),
        agent: agent.to_string(),
        readonly_default: ro,
        size: md.len(),
        updated,
    })
}

/// path 是否已是指向 canonical 的文件符号链接
fn points_to(path: &Path, canonical: &Path) -> bool {
    fsutil::is_link(path)
        && fsutil::link_target(path)
            .map(|t| eq_path(&t.to_string_lossy(), &canonical.to_string_lossy()))
            .unwrap_or(false)
}

// ─── Agent 特定全局记忆（v0.7.7：<agent>-global-memory.md，避免跨 agent 上下文互扰） ───

pub const AGENT_LINKS_START: &str = "<!-- agent-hub:agent-links:start -->";
pub const AGENT_LINKS_END: &str = "<!-- agent-hub:agent-links:end -->";

/// agent 特定记忆真身路径：~/.agenthub/<agent>-global-memory.md
pub fn central_agent_memory_md(agent: &str) -> PathBuf {
    store::hub_dir().join(format!("{agent}-global-memory.md"))
}

/// MEMORY.md 中的链接区（幂等标记块，unify/ensure 时 upsert）
fn agent_links_block() -> String {
    let mut lines = vec![
        AGENT_LINKS_START.to_string(),
        "## Agent 特定全局记忆（仅对应 agent 按需读取，勿随本文件整体注入上下文）".to_string(),
        String::new(),
    ];
    for a in crate::adapters::registry() {
        lines.push(format!(
            "- {}：`~/.agenthub/{}-global-memory.md`",
            a.display_name(),
            a.id()
        ));
    }
    lines.push(AGENT_LINKS_END.to_string());
    lines.join("\n")
}

fn upsert_agent_links(content: &str) -> String {
    let block = agent_links_block();
    if let (Some(s), Some(e_rel)) = (content.find(AGENT_LINKS_START), content.find(AGENT_LINKS_END)) {
        if e_rel >= s {
            let mut out = String::new();
            out.push_str(&content[..s]);
            out.push_str(&block);
            out.push_str(&content[e_rel + AGENT_LINKS_END.len()..]);
            return out;
        }
    }
    let mut out = content.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&block);
    out.push('\n');
    out
}

fn agent_memory_stub(display_name: &str) -> String {
    format!(
        "# {display_name} 全局记忆（agent 特定）\n\n\
         > 仅 {display_name} 需要读取的记忆放这里；跨 agent 通用记忆在 ~/.agenthub/MEMORY.md。\n\
         > 由 AgentHub 创建于 {}。\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    )
}

/// 幂等落地 agent 特定记忆：为每个已启用插件创建缺失的 <agent>-global-memory.md，
/// 并向真身 MEMORY.md upsert 链接区（真身不存在则跳过链接区，unify 时会补）。
pub fn ensure_agent_memory_files(store: &Store) -> Res<Vec<String>> {
    let mut steps = Vec::new();
    let enabled = crate::plugins::active_ids(store)?;
    for a in crate::adapters::registry() {
        if !enabled.iter().any(|id| id == a.id()) {
            continue;
        }
        let p = central_agent_memory_md(a.id());
        if !p.is_file() {
            if let Some(par) = p.parent() {
                std::fs::create_dir_all(par).map_err(|e| e.to_string())?;
            }
            fsutil::atomic_write(&p, agent_memory_stub(a.display_name()).as_bytes())?;
            steps.push(format!("创建 {}-global-memory.md（{} 特定记忆真身）", a.id(), a.display_name()));
        }
    }
    let canonical = store::central_memory_md();
    if canonical.is_file() {
        let content = std::fs::read_to_string(&canonical).unwrap_or_default();
        let has_block =
            content.contains(AGENT_LINKS_START) && content.contains(AGENT_LINKS_END);
        let all_listed = crate::adapters::registry()
            .iter()
            .all(|a| content.contains(&format!("{}-global-memory.md", a.id())));
        if !has_block || !all_listed {
            let backup_dir = store::backups_dir().join("memory");
            fsutil::backup_file(&canonical, &backup_dir).ok();
            let new_content = upsert_agent_links(&content);
            fsutil::atomic_write(&canonical, new_content.as_bytes())?;
            steps.push("MEMORY.md 链接区已更新（agent 特定记忆入口）".into());
        }
    }
    store.with(|conn| {
        store::log_op(
            conn,
            "memory.agent_files_ensure",
            "~/.agenthub",
            serde_json::json!({ "steps": steps }),
        );
        Ok(())
    })?;
    Ok(steps)
}

/// 全局记忆文件列表：hub 真身恒显；agent 特定记忆（已启用插件）次之；
/// 各端残留文件仅在该端插件启用时列出
/// （目标清单由各插件经 AgentAdapter::memory_targets 提供，core 不感知具体路径）
pub fn global_files(store: &Store) -> Vec<MemoryFile> {
    let enabled = crate::plugins::active_ids(store).unwrap_or_default();
    let on = |agent: &str| enabled.iter().any(|id| id == agent);
    let mut out = Vec::new();
    let canonical = store::central_memory_md();
    // 统一真身 ~/.agenthub/MEMORY.md：存在即排最前（各端共写唯一入口）
    if canonical.is_file() {
        if let Some(f) = file_meta(
            &canonical,
            "hub-memory",
            "全局记忆 · MEMORY.md（各启用 Agent 共用）",
            "hub",
            false,
        ) {
            out.push(f);
        }
    }
    // agent 特定记忆真身：<agent>-global-memory.md（缺失则幂等创建，页面加载即就绪）
    if let Ok(_) = std::fs::metadata(store::hub_dir()) {
        let _ = ensure_agent_memory_files(store);
    }
    for a in crate::adapters::registry() {
        if !on(a.id()) {
            continue;
        }
        let p = central_agent_memory_md(a.id());
        if p.is_file() {
            if let Some(f) = file_meta(
                &p,
                &format!("{}-agent-memory", a.id()),
                &format!(
                    "{} 全局记忆 · {}-global-memory.md（仅该 Agent 读取）",
                    a.display_name(),
                    a.id()
                ),
                a.id(),
                false,
            ) {
                out.push(f);
            }
        }
    }
    // 各端用户级记忆：已建链指向真身的不单列（就是真身本身），未统一的照旧列出便于发现残留
    for a in crate::adapters::registry() {
        if !on(a.id()) {
            continue;
        }
        for t in a.memory_targets() {
            if t.path.is_file() && !points_to(&t.path, &canonical) {
                if let Some(f) = file_meta(&t.path, &t.id, &t.label, a.id(), t.readonly_default) {
                    out.push(f);
                }
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    // 真身条目置顶（stable 排序，其余保持 id 序）
    out.sort_by_key(|f| f.agent != "hub");
    out
}

/// 路径安全：只允许各 agent 数据根（插件 data_dirs 钩子提供）、~/.agenthub 与已注册项目目录
fn allowed_root_of(path: &Path, store: &Store) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![store::hub_dir()];
    for a in crate::adapters::registry() {
        candidates.extend(a.data_dirs());
    }
    // canonicalize 对尚不存在的新文件会失败：退化为其父目录 canonicalize + 文件名
    let canon = |p: &Path| {
        std::fs::canonicalize(p).unwrap_or_else(|_| {
            p.parent()
                .and_then(|par| std::fs::canonicalize(par).ok())
                .map(|par| par.join(p.file_name().unwrap_or_default()))
                .unwrap_or_else(|| p.to_path_buf())
        })
    };
    let target = canon(path);
    for root in &candidates {
        if target.starts_with(canon(root)) {
            return Some(root.clone());
        }
    }
    // 注册项目目录
    let ok = store
        .with(|conn| {
            let mut stmt = conn
                .prepare("SELECT path FROM projects")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            let mut paths = Vec::new();
            for r in rows {
                paths.push(r.map_err(|e| e.to_string())?);
            }
            Ok(paths)
        })
        .unwrap_or_default();
    for p in ok {
        if target.starts_with(canon(Path::new(&p))) {
            return Some(PathBuf::from(p));
        }
    }
    None
}

pub fn read_global(store: &Store, path: &str) -> Res<String> {
    let p = Path::new(path);
    if allowed_root_of(p, store).is_none() {
        return Err(format!("路径不在允许范围：{path}"));
    }
    std::fs::read_to_string(p).map_err(|e| format!("读取失败: {e}"))
}

#[derive(Debug, serde::Deserialize)]
pub struct WriteMemoryInput {
    pub path: String,
    pub content: String,
    /// ZCode AGENTS.md 需显式解锁
    pub allow_zcode_agents: bool,
}

pub fn write_global(store: &Store, input: &WriteMemoryInput) -> Res<()> {
    let raw = Path::new(&input.path);
    let root = allowed_root_of(raw, store).ok_or_else(|| format!("路径不在允许范围：{}", input.path))?;
    // 归属判定：目标落在哪个插件的数据根就要求哪个插件启用（hub 真身恒可写）
    let enabled = crate::plugins::active_ids(store)?;
    let mut owner: Option<&'static dyn crate::adapters::AgentAdapter> = None;
    for a in crate::adapters::registry() {
        if a.data_dirs().iter().any(|d| raw.starts_with(d)) {
            owner = Some(*a);
            break;
        }
    }
    if let Some(a) = owner {
        if !enabled.iter().any(|id| id == a.id()) {
            return Err(format!(
                "{} 插件已停用，不能写入其记忆文件（到 Plugin 岛开启）",
                a.display_name()
            ));
        }
        // 只读守卫（如 ZCode AGENTS.md 需显式解锁；unlock 语义沿前端字段 allow_zcode_agents）
        a.memory_write_guard(raw, input.allow_zcode_agents)?;
    }
    let _ = root;
    // 统一建链的路径解析到真身再写：atomic_write 的 tmp+rename 会把符号链接替换成实体文件
    let p: PathBuf = if fsutil::is_link(raw) {
        fsutil::link_target(raw).unwrap_or_else(|| raw.to_path_buf())
    } else {
        raw.to_path_buf()
    };
    let backup_dir = store::backups_dir().join("memory");
    if p.exists() {
        fsutil::backup_file(&p, &backup_dir)?;
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fsutil::atomic_write(&p, input.content.as_bytes())?;
    store.with(|conn| {
        store::log_op(
            conn,
            "memory.write",
            &input.path,
            serde_json::json!({ "bytes": input.content.len() }),
        );
        Ok(())
    })
}

/// 统一全局记忆：真身 ~/.agenthub/MEMORY.md，双端用户级路径替换为指向它的文件符号链接，
/// 此后用户手动改 / ZCode 写 AGENTS.md / WB 写 user-personal MEMORY.md 落的都是同一份。
/// 幂等：真身已存在则沿用不覆盖；已建链跳过；断链或指向别处的重建。返回步骤日志（⚠ 项修复后重跑即可）。
pub fn unify_global_memory(store: &Store) -> Res<Vec<String>> {
    let mut steps = Vec::new();
    // 能力预检：单文件链接仅 symlink 可行（junction 仅目录、copy 会内容分叉），不行就整体不动
    if fsutil::detect_link_mode(&store::hub_dir()) == LinkMode::Copy {
        return Err("本机无法创建符号链接（需开发者模式或管理员权限），未做任何改动".into());
    }
    let canonical = store::central_memory_md();
    // 插件门控：只对已启用端的记忆目标做种子合并与建链（目标清单来自 memory_targets 钩子）
    let enabled = crate::plugins::active_ids(store)?;
    let targets_all: Vec<(&'static dyn crate::adapters::AgentAdapter, Vec<MemoryTarget>)> =
        crate::adapters::registry()
            .iter()
            .filter(|a| enabled.iter().any(|id| id == a.id()))
            .map(|a| (*a, a.memory_targets()))
            .collect();
    // 种子：真身不存在时合并各来源（注册表顺序，即 WB → ZCode → Claude）
    if !canonical.is_file() {
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        let mut body = format!(
            "# MEMORY.md — 全局记忆（各启用 Agent 统一共用）\n\n\
             > 由 AgentHub「统一全局记忆」生成于 {stamp}；原文件已备份至 ~/.agenthub/backups/memory/。\n\
             > 本文件是各启用端唯一真身，各端用户级记忆文件均为指向它的符号链接，\n\
             > 任一端保存全局记忆都写入这里。\n\n"
        );
        let mut sources = 0usize;
        for (_, targets) in &targets_all {
            for t in targets {
                if t.path.is_file() {
                    sources += 1;
                    body.push_str(&format!(
                        "<!-- 来源: {} -->\n{}\n\n---\n\n",
                        t.path.display(),
                        std::fs::read_to_string(&t.path).unwrap_or_default()
                    ));
                }
            }
        }
        fsutil::atomic_write(&canonical, body.as_bytes())?;
        steps.push(format!(
            "✓ 生成统一真身 {}（合并 {sources} 份来源）",
            canonical.display()
        ));
    } else {
        steps.push(format!("✓ 真身已存在，沿用：{}", canonical.display()));
    }
    // agent 特定记忆：创建缺失的 <agent>-global-memory.md + 真身链接区 upsert
    match ensure_agent_memory_files(store) {
        Ok(agent_steps) => steps.extend(agent_steps.into_iter().map(|s| format!("✓ {s}"))),
        Err(e) => steps.push(format!("⚠ agent 特定记忆落地失败：{e}")),
    }
    // 目标：各启用端的用户级记忆路径（没有的也建链，获得统一记忆）
    let mut link_targets: Vec<(PathBuf, String)> = Vec::new();
    for (_, targets) in &targets_all {
        for t in targets {
            link_targets.push((t.path.clone(), t.label.clone()));
        }
    }
    let backup_dir = store::backups_dir().join("memory");
    let mut failed = 0usize;
    for (p, name) in link_targets.iter() {
        if points_to(p, &canonical) {
            steps.push(format!("✓ {name} 已指向真身，跳过"));
            continue;
        }
        // 先备份现有内容，失败则不动原文件
        let backup = if p.is_file() {
            match fsutil::backup_file(p, &backup_dir) {
                Ok(b) => Some(b),
                Err(e) => {
                    failed += 1;
                    steps.push(format!("⚠ {name} 备份失败，跳过：{e}"));
                    continue;
                }
            }
        } else {
            None
        };
        // 腾挪链接位
        if p.exists() || fsutil::is_link(p) {
            if let Err(e) = std::fs::remove_file(p) {
                failed += 1;
                steps.push(format!("⚠ {name} 移除旧文件失败，跳过：{e}"));
                continue;
            }
        }
        if let Some(par) = p.parent() {
            let _ = std::fs::create_dir_all(par);
        }
        match fsutil::make_file_link(p, &canonical) {
            Ok(()) => steps.push(format!("✓ {name} → {} 建链完成", canonical.display())),
            Err(e) => {
                failed += 1;
                // 建链失败尽量还原原文件，不留破绽
                if let Some(b) = &backup {
                    let _ = std::fs::copy(b, p);
                }
                steps.push(format!("⚠ {name} 建链失败（已还原原文件）：{e}"));
            }
        }
    }
    if link_targets.is_empty() {
        steps.push("没有已启用的 agent 插件，未建任何链接（到 Plugin 岛开启后重跑）".into());
    } else if failed > 0 {
        steps.push(format!("⚠ 统一完成但有 {failed} 项失败，修复后重跑即可（幂等）"));
    } else {
        steps.push("✓ 统一完成：各启用端保存全局记忆均写入同一份".into());
    }
    store.with(|conn| {
        store::log_op(
            conn,
            "memory.unify",
            &canonical.to_string_lossy(),
            serde_json::json!({ "targets": link_targets.len(), "failed": failed }),
        );
        Ok(())
    })?;
    Ok(steps)
}

// ─── 项目记忆 .ai/ 桥接 ───

#[derive(Debug, Serialize)]
pub struct BridgeStatus {
    pub file: String,
    pub injected: bool,
    pub drifted: bool,
}

#[derive(Debug, Serialize)]
pub struct ProjectInfo {
    pub path: String,
    pub name: String,
    /// .ai/ 是否就绪
    pub ai_ready: bool,
    pub bridges: Vec<BridgeStatus>,
    /// 蒸馏候选条数（各 agent 自动沉淀中未入 .ai 的）
    pub consolidate_candidates: usize,
}

fn project_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

fn instruction_files(path: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    // 指令文件清单来自各插件 instruction_file 钩子（如 AGENTS.md→zcode、CLAUDE.md→claude）
    for a in crate::adapters::registry() {
        if let Some((name, agent)) = a.instruction_file() {
            let p = path.join(name);
            if p.is_file() {
                out.push((agent.to_string(), p));
            }
        }
    }
    out
}

fn upsert_block(content: &str) -> (String, bool) {
    // 幂等：有标记替换标记内，无标记追加
    if let (Some(s), Some(e_rel)) = (content.find(BLOCK_START), content.find(BLOCK_END)) {
        if e_rel >= s {
            let mut out = String::new();
            out.push_str(&content[..s]);
            out.push_str(BRIDGE_BLOCK);
            out.push_str(&content[e_rel + BLOCK_END.len()..]);
            return (out, true);
        }
    }
    let mut out = content.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(BRIDGE_BLOCK);
    out.push('\n');
    (out, false)
}

pub fn overview_projects(store: &Store) -> Res<Vec<ProjectInfo>> {
    // 项目注册表为用户手动导入制（v2.4 起不再自动识别）；此处只做清理：
    // 删除缓存目录 / 已不存在的路径 / canonicalize 后重复的行
    store.with(|conn| {
        let mut stmt = conn
            .prepare("SELECT id, path FROM projects")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        let mut all: Vec<(i64, String)> = Vec::new();
        for r in rows {
            all.push(r.map_err(|e| e.to_string())?);
        }
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (id, path) in all {
            let pb = Path::new(&path);
            let junk = path.contains("workbuddy-cache")
                || path.contains("\\.cache\\")
                || path.contains("/.cache/")
                || !pb.is_dir();
            let canon = std::fs::canonicalize(pb)
                .unwrap_or_else(|_| pb.to_path_buf())
                .to_string_lossy()
                .to_lowercase();
            let dup = !seen.insert(canon);
            if junk || dup {
                let _ = conn.execute("DELETE FROM projects WHERE id = ?1", rusqlite::params![id]);
            }
        }
        Ok(())
    })?;
    store.with(|conn| {
        let mut stmt = conn
            .prepare("SELECT id, path FROM projects ORDER BY id DESC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            let (id, path) = r.map_err(|e| e.to_string())?;
            let root = Path::new(&path);
            let ai = root.join(".ai");
            let ai_ready = ai.join("MEMORY.md").is_file();
            let mut bridges = Vec::new();
            for (agent, file) in instruction_files(root) {
                let content = std::fs::read_to_string(&file).unwrap_or_default();
                let injected = content.contains(BLOCK_START);
                let expected: Option<String> = conn
                    .query_row(
                        "SELECT block_hash FROM bridge_targets WHERE project_id = ?1 AND file_path = ?2",
                        rusqlite::params![id, file.to_string_lossy()],
                        |row| row.get(0),
                    )
                    .ok();
                let drifted = injected
                    && expected
                        .as_ref()
                        .map(|h| *h != fnv1a(&content))
                        .unwrap_or(false);
                bridges.push(BridgeStatus {
                    file: file.to_string_lossy().to_string(),
                    injected,
                    drifted,
                });
                let _ = agent;
            }
            // 蒸馏候选：项目 .workbuddy/memory/*.md 中未入 .ai/MEMORY.md 的行数（粗估）
            let mut candidates = 0usize;
            let wb_mem_dir = root.join(".workbuddy").join("memory");
            if let Ok(entries) = std::fs::read_dir(&wb_mem_dir) {
                let existing = std::fs::read_to_string(ai.join("MEMORY.md")).unwrap_or_default();
                let existing_lines: std::collections::HashSet<&str> =
                    existing.lines().map(str::trim).collect();
                for e in entries.flatten() {
                    if let Ok(c) = std::fs::read_to_string(e.path()) {
                        for line in c.lines() {
                            let t = line.trim();
                            if t.len() > 8 && !existing_lines.contains(t) && !t.starts_with('#') {
                                candidates += 1;
                            }
                        }
                    }
                }
            }
            out.push(ProjectInfo {
                path: path.clone(),
                name: project_name(&path),
                ai_ready,
                bridges,
                consolidate_candidates: candidates,
            });
        }
        Ok(out)
    })
}

/// init：创建 .ai/ 结构 + 幂等注入桥接块到已存在的指令文件
pub fn init_project_bridge(store: &Store, path: &str) -> Res<Vec<String>> {
    let root = Path::new(path);
    if !root.is_dir() {
        return Err(format!("项目目录不存在：{path}"));
    }
    let mut steps = Vec::new();
    let ai = root.join(".ai");
    for (name, content) in [
        ("PROJECT.md", "# PROJECT\n\n（技术栈 / 目录结构 / 常用命令 / 规范 —— 稳定事实写这里）\n"),
        ("MEMORY.md", "# MEMORY\n\n（关键决策与踩坑 —— 由各 agent 与蒸馏共同维护）\n"),
    ] {
        let p = ai.join(name);
        if !p.is_file() {
            std::fs::create_dir_all(&ai).map_err(|e| e.to_string())?;
            fsutil::atomic_write(&p, content.as_bytes())?;
            steps.push(format!("创建 .ai/{name}"));
        }
    }
    let mem_dir = ai.join("memory");
    if !mem_dir.is_dir() {
        std::fs::create_dir_all(&mem_dir).map_err(|e| e.to_string())?;
        steps.push("创建 .ai/memory/".into());
    }
    // 记忆文件按主题归档（YYYY-MM-DD-memoryTitle.md），由各 agent 首次记录时创建，
    // init 不再预建按日命名的日志文件

    let project_id: i64 = store.with(|conn| {
        conn.execute(
            "INSERT INTO projects(path, name) VALUES (?1, ?2)
             ON CONFLICT(path) DO UPDATE SET name = excluded.name",
            rusqlite::params![path, project_name(path)],
        )
        .map_err(|e| e.to_string())?;
        Ok(conn.query_row(
            "SELECT id FROM projects WHERE path = ?1",
            rusqlite::params![path],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?)
    })?;

    let files = instruction_files(root);
    if files.is_empty() {
        steps.push("未找到 AGENTS.md / CLAUDE.md —— 桥接块未注入（可手动创建指令文件后重新 init）".into());
    }
    for (agent, file) in files {
        let content = std::fs::read_to_string(&file).unwrap_or_default();
        let (new_content, replaced) = upsert_block(&content);
        let backup_dir = store::backups_dir().join("bridge");
        fsutil::backup_file(&file, &backup_dir).ok();
        fsutil::atomic_write(&file, new_content.as_bytes())?;
        let hash = fnv1a(&new_content);
        store.with(|conn| {
            conn.execute(
                "INSERT INTO bridge_targets(project_id, agent, file_path, block_hash, injected_at)
                 VALUES (?1, ?2, ?3, ?4, datetime('now','localtime'))
                 ON CONFLICT(project_id, file_path) DO UPDATE SET
                   agent = excluded.agent, block_hash = excluded.block_hash,
                   injected_at = excluded.injected_at",
                rusqlite::params![project_id, agent, file.to_string_lossy(), hash],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })?;
        steps.push(format!(
            "注入 {} {}",
            file.file_name().unwrap_or_default().to_string_lossy(),
            if replaced { "（替换标记块）" } else { "（追加）" }
        ));
    }
    store.with(|conn| {
        store::log_op(conn, "project.init", path, serde_json::json!({ "steps": steps }));
        Ok(())
    })?;
    Ok(steps)
}

/// sync：漂移检测并恢复标记块
pub fn sync_project_bridge(store: &Store, path: &str) -> Res<Vec<String>> {
    let root = Path::new(path);
    let mut steps = Vec::new();
    let project_id: Option<i64> = store.with(|conn| {
        Ok(conn
            .query_row(
                "SELECT id FROM projects WHERE path = ?1",
                rusqlite::params![path],
                |row| row.get(0),
            )
            .ok())
    })?;
    let Some(project_id) = project_id else {
        return Err("项目未注册，请先 init".into());
    };
    for (_, file) in instruction_files(root) {
        let content = std::fs::read_to_string(&file).unwrap_or_default();
        if !content.contains(BLOCK_START) {
            steps.push(format!("{}：无桥接块（可 init 注入）", file.file_name().unwrap_or_default().to_string_lossy()));
            continue;
        }
        let hash_now = fnv1a(&content);
        let expected: Option<String> = store.with(|conn| {
            Ok(conn
                .query_row(
                    "SELECT block_hash FROM bridge_targets WHERE project_id = ?1 AND file_path = ?2",
                    rusqlite::params![project_id, file.to_string_lossy()],
                    |row| row.get(0),
                )
                .ok())
        })?;
        if Some(hash_now.clone()) == expected {
            steps.push(format!("{}：桥接块完好", file.file_name().unwrap_or_default().to_string_lossy()));
        } else {
            let (new_content, _) = upsert_block(&content);
            fsutil::backup_file(&file, &store::backups_dir().join("bridge")).ok();
            fsutil::atomic_write(&file, new_content.as_bytes())?;
            let hash = fnv1a(&new_content);
            store.with(|conn| {
                conn.execute(
                    "UPDATE bridge_targets SET block_hash = ?3, injected_at = datetime('now','localtime')
                     WHERE project_id = ?1 AND file_path = ?2",
                    rusqlite::params![project_id, file.to_string_lossy(), hash],
                )
                .map_err(|e| e.to_string())?;
                Ok(())
            })?;
            steps.push(format!(
                "{}：检测到漂移，已恢复标记块",
                file.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }
    store.with(|conn| {
        store::log_op(conn, "project.sync", path, serde_json::json!({ "steps": steps }));
        Ok(())
    })?;
    Ok(steps)
}

/// consolidate：读取项目 .workbuddy/memory/*.md（各 agent 自动沉淀）→ 行级去重 → 蒸馏进 .ai/MEMORY.md
pub fn consolidate_project_bridge(store: &Store, path: &str) -> Res<Vec<String>> {
    let root = Path::new(path);
    let ai = root.join(".ai");
    let memory_md = ai.join("MEMORY.md");
    if !memory_md.is_file() {
        return Err("项目尚未 init（.ai/MEMORY.md 不存在）".into());
    }
    let existing = std::fs::read_to_string(&memory_md).unwrap_or_default();
    let mut existing_lines: std::collections::HashSet<String> =
        existing.lines().map(|l| l.trim().to_string()).collect();
    let mut added: Vec<String> = Vec::new();
    let wb_mem_dir = root.join(".workbuddy").join("memory");
    if let Ok(entries) = std::fs::read_dir(&wb_mem_dir) {
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().map(|x| x == "md").unwrap_or(false)).collect();
        files.sort();
        for f in files {
            let Ok(content) = std::fs::read_to_string(&f) else { continue };
            let day = f
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut section: Vec<String> = Vec::new();
            for line in content.lines() {
                let t = line.trim();
                if t.len() > 8 && !t.starts_with('#') && !existing_lines.contains(t) {
                    // 源行已是 bullet 则不加前缀，避免 "- - "
                    section.push(if t.starts_with("- ") {
                        t.to_string()
                    } else {
                        format!("- {t}")
                    });
                    existing_lines.insert(t.to_string());
                }
            }
            if !section.is_empty() {
                added.push(format!(
                    "\n## 蒸馏 · {day} · 来源: workbuddy\n{}\n",
                    section.join("\n")
                ));
            }
        }
    }
    let mut steps = Vec::new();
    if added.is_empty() {
        steps.push("没有新的蒸馏候选（已全部入 .ai/MEMORY.md）".into());
    } else {
        let mut out = existing;
        if !out.ends_with('\n') {
            out.push('\n');
        }
        for a in &added {
            out.push_str(a);
        }
        fsutil::backup_file(&memory_md, &store::backups_dir().join("memory")).ok();
        fsutil::atomic_write(&memory_md, out.as_bytes())?;
        steps.push(format!("已蒸馏 {} 个来源段 → .ai/MEMORY.md（均标注来源与日期）", added.len()));
    }
    store.with(|conn| {
        store::log_op(
            conn,
            "project.consolidate",
            path,
            serde_json::json!({ "steps": steps }),
        );
        Ok(())
    })?;
    Ok(steps)
}

// ─── 项目记忆文件管理（v2.5 用户导入制：import / list / delete / rename） ───

#[derive(Debug, Serialize)]
pub struct MemoryListItem {
    /// 文件名（如 2026-11-26-antd组件库踩坑记录.md）
    pub name: String,
    pub path: String,
    pub size: u64,
    pub updated: String,
}

/// 项目的 .ai/memory 目录路径
fn ai_memory_dir(project: &str) -> PathBuf {
    Path::new(project).join(".ai").join("memory")
}

/// 路径守卫：必须是某个已注册项目 .ai/memory 目录下的直接子文件（防越权删改任意文件）
fn ensure_in_ai_memory(store: &Store, path: &Path) -> Res<()> {
    let target = std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_lowercase();
    let ok = store
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
        })?
        .into_iter()
        .any(|proj| {
            let dir = std::fs::canonicalize(ai_memory_dir(&proj))
                .unwrap_or_else(|_| ai_memory_dir(&proj))
                .to_string_lossy()
                .to_lowercase();
            target.starts_with(&format!("{dir}\\"))
                || target.starts_with(&format!("{dir}/"))
        });
    if !ok {
        return Err("路径不在任何已导入项目的 .ai/memory 目录内，拒绝操作".into());
    }
    Ok(())
}

/// 导入项目（用户手动选择目录注册；幂等，重复导入按 upsert 更新名称）
pub fn import_project(store: &Store, path: &str) -> Res<Vec<String>> {
    let root = Path::new(path);
    if !root.is_dir() {
        return Err(format!("目录不存在：{path}"));
    }
    let mut steps = Vec::new();
    store.with(|conn| {
        conn.execute(
            "INSERT INTO projects(path, name) VALUES (?1, ?2)
             ON CONFLICT(path) DO UPDATE SET name = excluded.name",
            rusqlite::params![path, project_name(path)],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })?;
    steps.push(format!("已导入项目：{}", project_name(path)));
    if ai_memory_dir(path).is_dir() {
        steps.push("检测到 .ai/memory/，展开即可管理记忆文件".into());
    } else {
        steps.push("该项目暂无 .ai/memory/ —— 可先执行 init 创建 .ai/ 结构".into());
    }
    store.with(|conn| {
        store::log_op(conn, "project.import", path, serde_json::json!({ "steps": steps }));
        Ok(())
    })?;
    Ok(steps)
}

/// 列出项目 .ai/memory/ 下的全部 md 文件（按文件名排序）
pub fn list_memory_files(store: &Store, project: &str) -> Res<Vec<MemoryListItem>> {
    ensure_in_ai_memory_dir(store, project)?;
    let dir = ai_memory_dir(project);
    let mut items: Vec<MemoryListItem> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_file() || p.extension().map(|x| x != "md").unwrap_or(true) {
                continue;
            }
            let md = e.metadata().map_err(|e| e.to_string())?;
            let updated = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|d| {
                    chrono::Local
                        .timestamp_opt(d.as_secs() as i64, 0)
                        .single()
                        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                })
                .unwrap_or_default();
            items.push(MemoryListItem {
                name: p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                path: p.to_string_lossy().to_string(),
                size: md.len(),
                updated,
            });
        }
    }
    items.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(items)
}

/// 项目目录必须已注册（列表入口守卫，与文件级守卫互补）
fn ensure_in_ai_memory_dir(store: &Store, project: &str) -> Res<()> {
    let registered = store.with(|conn| {
        Ok(conn
            .query_row(
                "SELECT id FROM projects WHERE path = ?1",
                rusqlite::params![project],
                |row| row.get::<_, i64>(0),
            )
            .is_ok())
    })?;
    if !registered {
        return Err("项目未导入（注册表无此目录）".into());
    }
    Ok(())
}

/// 删除 .ai/memory 下的记忆文件：先备份到 backups/memory/ 再移除（防误删可回滚）
pub fn delete_memory_file(store: &Store, path: &str) -> Res<String> {
    let p = Path::new(path);
    if !p.is_file() {
        return Err(format!("文件不存在：{path}"));
    }
    ensure_in_ai_memory(store, p)?;
    let backup = fsutil::backup_file(p, &store::backups_dir().join("memory"))?;
    std::fs::remove_file(p).map_err(|e| format!("删除失败: {e}"))?;
    store.with(|conn| {
        store::log_op(conn, "memory.file_delete", path, serde_json::json!({ "backup": backup.display().to_string() }));
        Ok(())
    })?;
    Ok(backup.display().to_string())
}

/// 重命名 .ai/memory 下的记忆文件：同目录内改名，强制 .md 后缀，目标重名拒绝
pub fn rename_memory_file(store: &Store, path: &str, name: &str) -> Res<String> {
    let p = Path::new(path);
    if !p.is_file() {
        return Err(format!("文件不存在：{path}"));
    }
    ensure_in_ai_memory(store, p)?;
    let name = name.trim().trim_end_matches(".md").to_string();
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
    {
        return Err(format!("非法文件名：{name}"));
    }
    let new_path = p
        .parent()
        .ok_or_else(|| "无法定位父目录".to_string())?
        .join(format!("{name}.md"));
    if new_path.exists() {
        return Err(format!("目标已存在：{}", new_path.display()));
    }
    std::fs::rename(p, &new_path).map_err(|e| format!("重命名失败: {e}"))?;
    store.with(|conn| {
        store::log_op(
            conn,
            "memory.file_rename",
            path,
            serde_json::json!({ "new": new_path.to_string_lossy() }),
        );
        Ok(())
    })?;
    Ok(new_path.to_string_lossy().to_string())
}

// ─── Tauri commands（M6） ───

use tauri::State;

#[tauri::command]
pub async fn memory_global_list(state: State<'_, Store>) -> Result<Vec<MemoryFile>, String> {
    Ok(global_files(&state))
}

#[tauri::command]
pub async fn memory_read(state: State<'_, Store>, path: String) -> Result<String, String> {
    read_global(&state, &path)
}

#[tauri::command]
pub async fn memory_write(state: State<'_, Store>, input: WriteMemoryInput) -> Result<(), String> {
    write_global(&state, &input)
}

#[tauri::command]
pub async fn memory_global_unify(state: State<'_, Store>) -> Result<Vec<String>, String> {
    unify_global_memory(&state)
}

#[tauri::command]
pub async fn projects_overview(state: State<'_, Store>) -> Result<Vec<ProjectInfo>, String> {
    overview_projects(&state)
}

#[tauri::command]
pub async fn project_init(state: State<'_, Store>, path: String) -> Result<Vec<String>, String> {
    init_project_bridge(&state, &path)
}

#[tauri::command]
pub async fn project_sync(state: State<'_, Store>, path: String) -> Result<Vec<String>, String> {
    sync_project_bridge(&state, &path)
}

#[tauri::command]
pub async fn project_consolidate(state: State<'_, Store>, path: String) -> Result<Vec<String>, String> {
    consolidate_project_bridge(&state, &path)
}

/// 导入项目（用户手动选择目录；v2.5 起项目注册表为纯手动制）
#[tauri::command]
pub async fn project_import(state: State<'_, Store>, path: String) -> Result<Vec<String>, String> {
    import_project(&state, &path)
}

/// 列出项目 .ai/memory/ 下的记忆文件
#[tauri::command]
pub async fn memory_files_list(state: State<'_, Store>, project: String) -> Result<Vec<MemoryListItem>, String> {
    list_memory_files(&state, &project)
}

/// 删除记忆文件（先备份到 backups/memory/）
#[tauri::command]
pub async fn memory_file_delete(state: State<'_, Store>, path: String) -> Result<String, String> {
    delete_memory_file(&state, &path)
}

/// 重命名记忆文件（同目录内；自动补 .md 后缀；重名拒绝）
#[tauri::command]
pub async fn memory_file_rename(state: State<'_, Store>, path: String, name: String) -> Result<String, String> {
    rename_memory_file(&state, &path, &name)
}

/// 供路径校验单元测试使用
#[allow(dead_code)]
fn home_for_test() -> PathBuf {
    agent_home()
}
