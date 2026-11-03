//! market —— M7 Skill 广场（内置多开源市场）。
//!
//! 内置市场（SKILL.md 目录约定，通用树扫描，同款逻辑适配任何 GitHub 仓库）：
//! - anthropics/skills   —— Claude 官方
//! - obra/superpowers    —— 社区最火的工程方法论技能集
//! - cexll/myclaude      —— 中文社区热门工作流增强技能集
//! 树接口一次拉全目录，SKILL.md 按 name 缓存到 ~/.agenthub/market-cache/<source>/<name>/。
//! WorkBuddy 云端市场无公开接口，按方案 §8.9 不再作为广场来源（本地技能已在中央仓库）。
//! 安装：缓存 → 复制入中央仓库 → WB 逐项链（zcode 整链自动生效）。

use serde::Serialize;
use serde_json::Value;

use crate::fsutil::{self, LinkMode, Res};
use crate::migrate;
use crate::store::{self, Store};

pub struct MarketSource {
    pub id: &'static str,
    pub name: &'static str,
    /// owner/repo
    pub repo: &'static str,
}

pub const SOURCES: [MarketSource; 3] = [
    MarketSource { id: "claude-official", name: "Claude 官方", repo: "anthropics/skills" },
    MarketSource { id: "superpowers", name: "Superpowers", repo: "obra/superpowers" },
    MarketSource { id: "myclaude", name: "MyClaude", repo: "cexll/myclaude" },
];

fn client() -> Res<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("HTTP 客户端构建失败: {e}"))
}

fn cache_dir(source: &str) -> std::path::PathBuf {
    store::hub_dir().join("market-cache").join(source)
}

#[derive(Debug, Serialize, Clone)]
pub struct MarketSkill {
    pub name: String,
    pub description: String,
    /// 市场来源 id（claude-official / superpowers / myclaude）
    pub source: String,
    pub source_name: String,
    /// 是否已安装进中央仓库
    pub installed: bool,
}

#[derive(Debug, Serialize)]
pub struct MarketCatalog {
    pub skills: Vec<MarketSkill>,
    /// 拉取失败的市场（单源失败不中断整体）
    pub warnings: Vec<String>,
}

/// 拉取一个仓库里所有含 SKILL.md 的目录（main 失败自动试 master）
fn repo_skill_dirs(http: &reqwest::blocking::Client, repo: &str) -> Res<Vec<String>> {
    let mut last_err = String::new();
    for branch in ["main", "master"] {
        let url = format!("https://api.github.com/repos/{repo}/git/trees/{branch}?recursive=1");
        let resp = http
            .get(&url)
            .header("User-Agent", "AgentHub")
            .send();
        match resp {
            Err(e) => {
                last_err = format!("请求失败: {e}");
                continue;
            }
            Ok(r) => {
                if !r.status().is_success() {
                    last_err = format!("HTTP {}", r.status().as_u16());
                    continue;
                }
                let tree: Value = match r.json() {
                    Ok(v) => v,
                    Err(e) => {
                        last_err = format!("JSON 解析失败: {e}");
                        continue;
                    }
                };
                let mut dirs = Vec::new();
                if let Some(items) = tree.get("tree").and_then(Value::as_array) {
                    for item in items {
                        let Some(path) = item.get("path").and_then(Value::as_str) else {
                            continue;
                        };
                        if let Some(dir) = path.strip_suffix("/SKILL.md") {
                            dirs.push(dir.to_string());
                        }
                    }
                }
                dirs.sort();
                return Ok(dirs);
            }
        }
    }
    Err(format!("树接口不可达（{last_err}）"))
}

fn refresh_repo(http: &reqwest::blocking::Client, src: &MarketSource) -> Res<Vec<MarketSkill>> {
    let cache = cache_dir(src.id);
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let dirs = repo_skill_dirs(http, src.repo)?;
    if dirs.is_empty() {
        return Err("仓库中未发现任何 SKILL.md".into());
    }
    let mut out: Vec<MarketSkill> = Vec::new();
    for dir in &dirs {
        let name = dir.rsplit('/').next().unwrap_or(dir).to_string();
        if out.iter().any(|s| s.name == name) {
            continue; // 同仓库名字去重
        }
        let skill_md = cache.join(&name).join("SKILL.md");
        if !skill_md.is_file() {
            let url = format!("https://raw.githubusercontent.com/{}/main/{dir}/SKILL.md", src.repo);
            let body = http
                .get(&url)
                .header("User-Agent", "AgentHub")
                .send()
                .and_then(|r| r.error_for_status())
                .and_then(|r| r.bytes())
                .map_err(|e| format!("拉取 {url} 失败: {e}"))?;
            std::fs::create_dir_all(cache.join(&name)).map_err(|e| e.to_string())?;
            std::fs::write(&skill_md, &body).map_err(|e| e.to_string())?;
        }
        let (fm_name, description) = fsutil::parse_skill_md(&skill_md);
        let installed = store::central_skills_dir().join(&name).is_dir();
        out.push(MarketSkill {
            name: fm_name.unwrap_or(name),
            description: description.unwrap_or_default(),
            source: src.id.to_string(),
            source_name: src.name.to_string(),
            installed,
        });
    }
    Ok(out)
}

/// 刷新全部市场（单源失败不中断，失败信息进 warnings）
pub fn refresh_all() -> MarketCatalog {
    let mut skills: Vec<MarketSkill> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    match client() {
        Err(e) => warnings.push(e),
        Ok(http) => {
            for src in SOURCES {
                match refresh_repo(&http, &src) {
                    Ok(list) => {
                        for s in list {
                            // 跨源同名：先到先得（官方优先）
                            if seen.insert(s.name.clone()) {
                                skills.push(s);
                            }
                        }
                    }
                    Err(e) => warnings.push(format!("{}（{}）：{e}", src.name, src.repo)),
                }
            }
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    MarketCatalog { skills, warnings }
}

/// 从市场安装（或更新）一个 skill：缓存 → 中央仓库 → 各启用插件端分发
pub fn install(store: &Store, name: &str) -> Res<()> {
    // 在任一来源缓存里找
    let src = SOURCES
        .iter()
        .map(|s| cache_dir(s.id).join(name))
        .find(|p| p.join("SKILL.md").is_file());
    let Some(src) = src else {
        return Err(format!("市场缓存中无「{name}」，请先刷新市场"));
    };
    let central = store::central_skills_dir();
    let dest = central.join(name);
    if dest.exists() {
        fsutil::move_dir_to_backup(&dest, &store::backups_dir())?;
    }
    fsutil::copy_dir_recursive(&src, &dest)?;

    // 各启用插件端按策略分发（PerSkill 逐项链 / WholeDir 整链天然生效）
    let mode = migrate::parse_link_mode(None);
    let skills_adapters: Vec<&'static dyn crate::adapters::AgentAdapter> =
        crate::plugins::enabled_adapters(store)?
            .into_iter()
            .filter(|a| a.caps().skills)
            .collect();
    for a in &skills_adapters {
        let _ = a.relink_skill(name, &central, &store::backups_dir(), mode);
    }

    let (_, description) = fsutil::parse_skill_md(&dest.join("SKILL.md"));
    store.with(|conn| {
        let id = store::upsert_skill(
            conn,
            name,
            &dest.to_string_lossy(),
            description.as_deref(),
            "market",
            "builtin-sources",
        )?;
        for a in &skills_adapters {
            let whole = a.skills_link() == crate::adapters::SkillsLink::WholeDir;
            let mode_str = if whole {
                format!("whole-{}", mode.as_str())
            } else {
                mode.as_str().to_string()
            };
            store::upsert_skill_link(
                conn,
                id,
                a.id(),
                &mode_str,
                &a.skills_dir().to_string_lossy(),
                "ok",
            )?;
        }
        store::log_op(conn, "market.install", name, serde_json::json!({}));
        Ok(())
    })?;
    Ok(())
}

// ─── Tauri commands（M7） ───

use tauri::State;

#[tauri::command]
pub fn market_refresh() -> Result<MarketCatalog, String> {
    Ok(refresh_all())
}

/// 安装后重算 installed 标记返回最新目录
#[tauri::command]
pub fn market_install(state: State<'_, Store>, name: String) -> Result<MarketCatalog, String> {
    install(&state, &name)?;
    Ok(refresh_all())
}

/// 供未来 copy 模式降级时使用
#[allow(dead_code)]
fn detect_mode() -> LinkMode {
    migrate::parse_link_mode(None)
}
