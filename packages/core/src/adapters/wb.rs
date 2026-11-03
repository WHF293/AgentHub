//! WorkBuddy adapter —— 路径与资产探测（方案 §2.1 实测结论）。
//! MCP：~/.workbuddy/mcp.json {mcpServers:{name:{type,command,args,url,timeout,disabled}}}
//! Skills：~/.workbuddy/skills/<name>（逐项链）
//! Memory：~/.workbuddy/memory/<uid>_memory.md + user-<uid>-personal/MEMORY.md
//! 登录态：%LOCALAPPDATA%\CodeBuddyExtension\Data\Public\auth\workbuddy-desktop.info
// mcp/memory/settings 路径为 M3/M5/M6 预留
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::adapters::{
    agent_home, eq_path, link_status_for, AgentAdapter, AgentCaps, McpConfig, McpEntry, McpLive,
    SkillsLink,
};
use crate::fsutil::{self, LinkMode, Res};

pub fn dir() -> PathBuf {
    agent_home().join(".workbuddy")
}
pub fn skills_dir() -> PathBuf {
    dir().join("skills")
}
pub fn mcp_json() -> PathBuf {
    dir().join("mcp.json")
}
/// 第三方自定义模型（openai-compatible 写穿，WB 侧 1s debounce 热加载）：
/// 裸数组或 {models:[...], availableModels:[...]}，条目必须带 id
pub fn models_json() -> PathBuf {
    dir().join("models.json")
}
pub fn memory_dir() -> PathBuf {
    dir().join("memory")
}
pub fn settings_json() -> PathBuf {
    dir().join("settings.json")
}

/// 登录态文件（含加密 token，工具只做文件级复制，永不读内容 —— 方案安全红线）
pub fn auth_info() -> Option<PathBuf> {
    let local = std::env::var("LOCALAPPDATA").ok()?;
    let p = PathBuf::from(local)
        .join("CodeBuddyExtension")
        .join("Data")
        .join("Public")
        .join("auth")
        .join("workbuddy-desktop.info");
    p.exists().then_some(p)
}

/// 已登录的账号目录 user-<uid>-personal（memory 岛按账号分组展示）
pub fn user_dirs() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir()) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("user-") && n.ends_with("-personal") {
                out.push(n);
            }
        }
    }
    out.sort();
    out
}

/// WB 技能开关存储（storage/user-*-personal/scoped/*/skills-installed-store.json，
/// 面板「允许模型自动调用」开关落在这些文件的 disableModelInvocation 字段）
pub fn skill_toggle_stores() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(accounts) = std::fs::read_dir(dir().join("storage")) else {
        return out;
    };
    for acct in accounts.flatten() {
        let name = acct.file_name().to_string_lossy().to_string();
        if !(name.starts_with("user-") && name.ends_with("-personal")) {
            continue;
        }
        let Ok(scopes) = std::fs::read_dir(acct.path().join("scoped")) else {
            continue;
        };
        for scope in scopes.flatten() {
            let p = scope.path().join("skills-installed-store.json");
            if p.is_file() {
                out.push(p);
            }
        }
    }
    out
}

/// 安装探测：目录存在即视为已安装
pub fn installed() -> bool {
    dir().is_dir()
}

// ─── AgentAdapter 实现（可插拔） ───

/// WorkBuddy 适配器：skills 逐项链 + MCP mcp.json（disabled 字段）。
pub struct WorkBuddyAdapter;

impl AgentAdapter for WorkBuddyAdapter {
    fn id(&self) -> &'static str {
        "workbuddy"
    }
    fn display_name(&self) -> &'static str {
        "WorkBuddy"
    }
    fn desc(&self) -> &'static str {
        "skills 逐项链 / mcp.json / memory / 多账号槽位 / 积分额度"
    }
    fn caps(&self) -> AgentCaps {
        AgentCaps { skills: true, mcp: true }
    }
    fn installed(&self) -> bool {
        installed()
    }
    fn auth_found(&self) -> bool {
        auth_info().is_some()
    }

    fn skills_dir(&self) -> PathBuf {
        skills_dir()
    }
    fn skills_link(&self) -> SkillsLink {
        SkillsLink::PerSkill
    }

    /// WB 单 skill 建链：返回 "relinked" = 本次重建，"kept" = 本就指向中央仓库无需动作
    fn relink_skill(&self, name: &str, central: &Path, backups: &Path, mode: LinkMode) -> Res<String> {
        let central_skill = central.join(name);
        let target = skills_dir().join(name);
        if fsutil::is_link(&target) {
            if fsutil::link_target(&target)
                .map(|t| eq_path(&t.to_string_lossy(), &central_skill.to_string_lossy()))
                .unwrap_or(false)
            {
                return Ok("kept".into());
            }
            std::fs::remove_dir(&target).map_err(|e| format!("移除旧链接失败: {e}"))?;
        } else if target.exists() {
            fsutil::move_dir_to_backup(&target, backups)?;
        }
        std::fs::create_dir_all(skills_dir()).map_err(|e| e.to_string())?;
        fsutil::make_dir_link(&target, &central_skill, mode)?;
        Ok("relinked".into())
    }

    /// 整端确保 = 逐项链每个中央仓库 skill（迁移主流程直接循环 relink_skill，
    /// 此处返回本次整体动作描述供报告展示）
    fn ensure_skills_link(&self, central: &Path, backups: &Path, mode: LinkMode) -> Res<String> {
        let mut relinked = 0usize;
        let total = fsutil_read_skill_names(central).len();
        for name in fsutil_read_skill_names(central) {
            if self.relink_skill(&name, central, backups, mode)? == "relinked" {
                relinked += 1;
            }
        }
        Ok(format!("逐项链 {relinked}/{total}"))
    }

    fn skill_link_status(&self, name: &str, central: &Path) -> String {
        link_status_for(&skills_dir().join(name), central)
    }

    fn mcp_config(&self) -> McpConfig {
        McpConfig { path: mcp_json(), container: &["mcpServers"] }
    }

    /// WB mcpServers 条目：disabled=true 视作禁用
    fn mcp_entry_live(&self, entry: &Value) -> McpLive {
        if entry.get("disabled").and_then(Value::as_bool) == Some(true) {
            McpLive::Disabled
        } else {
            McpLive::Present
        }
    }

    /// 生成 WB mcpServers 子对象：托管字段以统一定义覆盖，非托管字段（timeout 等）保留。
    /// 规范型 streamableHttp 原样写 WB。
    fn to_mcp_entry(&self, e: &McpEntry, existing: Option<&Value>) -> Option<Value> {
        let mut entry = existing
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        entry.insert("type".into(), json!(e.kind));
        put_opt(&mut entry, "command", e.command.as_deref());
        if !e.args.is_empty() {
            entry.insert("args".into(), json!(e.args));
        } else {
            entry.remove("args");
        }
        put_opt(&mut entry, "url", e.url.as_deref());
        put_map(&mut entry, "env", &e.env);
        put_map(&mut entry, "headers", &e.headers);
        entry.insert("disabled".into(), json!(!e.enabled));
        Some(Value::Object(entry))
    }
}

/// 条目可选字符串写入（None 时移除，保持目标文件干净）
fn put_opt(entry: &mut serde_json::Map<String, Value>, key: &str, v: Option<&str>) {
    if let Some(v) = v {
        entry.insert(key.into(), json!(v));
    } else {
        entry.remove(key);
    }
}

fn put_map(
    entry: &mut serde_json::Map<String, Value>,
    key: &str,
    map: &std::collections::BTreeMap<String, String>,
) {
    if map.is_empty() {
        entry.remove(key);
    } else {
        entry.insert(key.into(), json!(map));
    }
}

/// 列出中央仓库下的 skill 目录名（整端确保用，避免 adapters ↔ migrate 循环依赖）
fn fsutil_read_skill_names(central: &Path) -> Vec<String> {
    crate::adapters::scan_skills_dir(central)
        .into_iter()
        .map(|i| i.name)
        .collect()
}
