//! ZCode adapter —— 路径与资产探测（方案 §2.2 实测结论）。
//! MCP：~/.zcode/cli/config.json {plugins, mcp:{servers:{name:{type,command,args,enabled?}}}}
//! Skills：~/.zcode/skills（整目录软链，目标由 AgentHub 重建为 ~/.agenthub/skills）
//! 第三方模型：~/.zcode/v2/config.json provider.<id>:{name,kind,options,models}
//! Memory：~/.zcode/AGENTS.md + 各 workspace
// cli/v2 配置路径为 M3/M4/M9 预留
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::adapters::{
    agent_home, eq_path, link_status_for, AgentAdapter, AgentCaps, McpConfig, McpEntry, McpLive,
    SkillsLink,
};
use crate::fsutil::{self, LinkMode, Res};

pub fn dir() -> PathBuf {
    agent_home().join(".zcode")
}
pub fn skills_dir() -> PathBuf {
    dir().join("skills")
}
pub fn cli_config() -> PathBuf {
    dir().join("cli").join("config.json")
}
pub fn v2_config() -> PathBuf {
    dir().join("v2").join("config.json")
}
pub fn v2_setting() -> PathBuf {
    dir().join("v2").join("setting.json")
}
pub fn rollout_dir() -> PathBuf {
    dir().join("cli").join("rollout")
}
pub fn agents_md() -> PathBuf {
    dir().join("AGENTS.md")
}
pub fn plugins_dir() -> PathBuf {
    dir().join("cli").join("plugins")
}
/// 插件注册清单（含 installPath/version/source）
pub fn installed_plugins_json() -> PathBuf {
    plugins_dir().join("installed_plugins.json")
}

pub fn installed() -> bool {
    dir().is_dir()
}

// ─── AgentAdapter 实现（可插拔） ───

/// ZCode 适配器：skills 整目录链 + MCP cli/config.json（enabled 字段，http 类型翻译）。
pub struct ZcodeAdapter;

impl AgentAdapter for ZcodeAdapter {
    fn id(&self) -> &'static str {
        "zcode"
    }
    fn display_name(&self) -> &'static str {
        "ZCode"
    }
    fn desc(&self) -> &'static str {
        "skills 整目录链 / cli config MCP / v2 providers / 账号槽位 / 官方额度"
    }
    fn caps(&self) -> AgentCaps {
        AgentCaps { skills: true, mcp: true }
    }
    fn installed(&self) -> bool {
        installed()
    }

    fn skills_dir(&self) -> PathBuf {
        skills_dir()
    }
    fn skills_link(&self) -> SkillsLink {
        SkillsLink::WholeDir
    }

    /// WholeDir 端：单 skill 分发 = 整目录链确保（中央仓库变动天然生效）
    fn relink_skill(&self, _name: &str, central: &Path, backups: &Path, mode: LinkMode) -> Res<String> {
        self.ensure_skills_link(central, backups, mode)
    }

    /// ZCode 整目录链：~/.zcode/skills → ~/.agenthub/skills
    fn ensure_skills_link(&self, central: &Path, backups: &Path, mode: LinkMode) -> Res<String> {
        let zdir = skills_dir();
        let central_str = central.to_string_lossy().to_string();
        if fsutil::is_link(&zdir) {
            if fsutil::link_target(&zdir)
                .map(|t| eq_path(&t.to_string_lossy(), &central_str))
                .unwrap_or(false)
            {
                return Ok("already-linked".into());
            }
            std::fs::remove_dir(&zdir).map_err(|e| format!("移除旧整链失败: {e}"))?;
            fsutil::make_dir_link(&zdir, central, mode)?;
            return Ok("relinked".into());
        }
        if zdir.is_dir() {
            // 防御：确认目录内所有实目录 skill 都已入中央仓库再腾挪
            for info in crate::adapters::scan_skills_dir(&zdir) {
                if info.importable && !central.join(&info.name).is_dir() {
                    let dest = central.join(&info.name);
                    fsutil::copy_dir_recursive(Path::new(&info.path), &dest)
                        .map_err(|e| format!("补导「{}」失败: {e}", info.name))?;
                }
            }
            fsutil::move_dir_to_backup(&zdir, backups)?;
            fsutil::make_dir_link(&zdir, central, mode)?;
            return Ok("replaced-with-link".into());
        }
        std::fs::create_dir_all(zdir.parent().unwrap()).map_err(|e| e.to_string())?;
        fsutil::make_dir_link(&zdir, central, mode)?;
        Ok("created".into())
    }

    /// 整链生效时单 skill 恒为 linked
    fn skill_link_status(&self, name: &str, central: &Path) -> String {
        if self.whole_link_active(central) {
            "linked".into()
        } else {
            link_status_for(&skills_dir().join(name), central)
        }
    }

    fn whole_link_active(&self, central: &Path) -> bool {
        let zdir = skills_dir();
        fsutil::is_link(&zdir)
            && fsutil::link_target(&zdir)
                .map(|t| eq_path(&t.to_string_lossy(), &central.to_string_lossy()))
                .unwrap_or(false)
    }

    fn mcp_config(&self) -> McpConfig {
        McpConfig { path: cli_config(), container: &["mcp", "servers"] }
    }

    /// ZCode mcp.servers 条目：enabled=false 视作禁用
    fn mcp_entry_live(&self, entry: &Value) -> McpLive {
        match entry.get("enabled") {
            Some(Value::Bool(false)) => McpLive::Disabled,
            _ => McpLive::Present,
        }
    }

    /// 收编类型名归一：ZCode 的 http → 规范型 streamableHttp（写回时翻译回去）
    fn canonical_kind(&self, kind: &str) -> String {
        if kind == "http" {
            "streamableHttp".into()
        } else {
            kind.into()
        }
    }

    /// 生成 ZCode mcp.servers 子对象。类型翻译：streamableHttp → http（ZCode 命名）
    fn to_mcp_entry(&self, e: &McpEntry, existing: Option<&Value>) -> Option<Value> {
        let zc_type = if e.kind == "streamableHttp" {
            "http"
        } else {
            e.kind.as_str()
        };
        let mut entry = existing
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        entry.insert("type".into(), json!(zc_type));
        if let Some(cmd) = &e.command {
            entry.insert("command".into(), json!(cmd));
        } else {
            entry.remove("command");
        }
        if !e.args.is_empty() {
            entry.insert("args".into(), json!(e.args));
        } else {
            entry.remove("args");
        }
        if let Some(url) = &e.url {
            entry.insert("url".into(), json!(url));
        } else {
            entry.remove("url");
        }
        if !e.env.is_empty() {
            entry.insert("env".into(), json!(e.env));
        } else {
            entry.remove("env");
        }
        if !e.headers.is_empty() {
            entry.insert("headers".into(), json!(e.headers));
        } else {
            entry.remove("headers");
        }
        entry.insert("enabled".into(), json!(e.enabled));
        Some(Value::Object(entry))
    }
}
