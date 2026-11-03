//! Claude Code adapter —— 路径与资产探测。
//! 全局配置：HOME 根 `~/.claude.json`（mcpServers 容器 + hasCompletedOnboarding 等，
//! 注意它在用户主目录下而非 ~/.claude/ 里）
//! 用户设置：~/.claude/settings.json（模型托管的 env 块：ANTHROPIC_BASE_URL /
//!           ANTHROPIC_AUTH_TOKEN / ANTHROPIC_MODEL / ANTHROPIC_SMALL_FAST_MODEL，见 providers.rs）
//! Skills：~/.claude/skills（整目录软链 → ~/.agenthub/skills；claude 是「静端」不自发重写该目录）
//! 会话日志：~/.claude/projects/**/*.jsonl（assistant 行带 message.usage，见 usage.rs）
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::adapters::{
    agent_home, eq_path, link_status_for, AgentAdapter, AgentCaps, McpConfig, McpEntry, McpLive,
    SkillsLink,
};
use crate::fsutil::{self, LinkMode, Res};

pub fn dir() -> PathBuf {
    agent_home().join(".claude")
}
pub fn skills_dir() -> PathBuf {
    dir().join("skills")
}
pub fn projects_dir() -> PathBuf {
    dir().join("projects")
}
/// 用户级设置（模型托管 env 块）
pub fn settings_json() -> PathBuf {
    dir().join("settings.json")
}
/// 用户级全局配置（HOME 根：mcpServers 容器 + onboarding 标记）
pub fn claude_json() -> PathBuf {
    agent_home().join(".claude.json")
}

pub fn installed() -> bool {
    dir().is_dir()
}

// ─── AgentAdapter 实现（可插拔） ───

/// Claude Code 适配器：skills 整目录链 + MCP ~/.claude.json mcpServers（无 disabled 字段，在场即启用）。
pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn id(&self) -> &'static str {
        "claude"
    }
    fn display_name(&self) -> &'static str {
        "Claude Code"
    }
    fn desc(&self) -> &'static str {
        "skills 整目录链 / claude.json MCP / settings.json 模型托管 / projects 会话用量"
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

    /// Claude 整目录链：~/.claude/skills → ~/.agenthub/skills
    fn ensure_skills_link(&self, central: &Path, backups: &Path, mode: LinkMode) -> Res<String> {
        let cdir = skills_dir();
        let central_str = central.to_string_lossy().to_string();
        if fsutil::is_link(&cdir) {
            if fsutil::link_target(&cdir)
                .map(|t| eq_path(&t.to_string_lossy(), &central_str))
                .unwrap_or(false)
            {
                return Ok("already-linked".into());
            }
            std::fs::remove_dir(&cdir).map_err(|e| format!("移除旧整链失败: {e}"))?;
            fsutil::make_dir_link(&cdir, central, mode)?;
            return Ok("relinked".into());
        }
        if cdir.is_dir() {
            // 防御：确认目录内所有实目录 skill 都已入中央仓库再腾挪
            for info in crate::adapters::scan_skills_dir(&cdir) {
                if info.importable && !central.join(&info.name).is_dir() {
                    let dest = central.join(&info.name);
                    fsutil::copy_dir_recursive(Path::new(&info.path), &dest)
                        .map_err(|e| format!("补导「{}」失败: {e}", info.name))?;
                }
            }
            fsutil::move_dir_to_backup(&cdir, backups)?;
            fsutil::make_dir_link(&cdir, central, mode)?;
            return Ok("replaced-with-link".into());
        }
        std::fs::create_dir_all(cdir.parent().unwrap()).map_err(|e| e.to_string())?;
        fsutil::make_dir_link(&cdir, central, mode)?;
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
        let cdir = skills_dir();
        fsutil::is_link(&cdir)
            && fsutil::link_target(&cdir)
                .map(|t| eq_path(&t.to_string_lossy(), &central.to_string_lossy()))
                .unwrap_or(false)
    }

    fn mcp_config(&self) -> McpConfig {
        McpConfig { path: claude_json(), container: &["mcpServers"] }
    }

    /// Claude mcpServers 条目无禁用字段（删条目 = 停用），非对象/空条目视作异常禁用态
    fn mcp_entry_live(&self, entry: &Value) -> McpLive {
        let ok = entry.as_object().map(|o| {
            o.contains_key("command") || o.contains_key("url") || o.contains_key("type")
        });
        if ok.unwrap_or(false) {
            McpLive::Present
        } else {
            McpLive::Disabled
        }
    }

    fn supports_kind(&self, kind: &str) -> bool {
        matches!(kind, "stdio" | "streamableHttp" | "sse")
    }

    /// 生成 Claude mcpServers 子对象。类型翻译：streamableHttp → http（Claude 命名）；
    /// stdio 不写 type 键（Claude 约定缺省即 stdio，写兼容性最稳）
    fn to_mcp_entry(&self, e: &McpEntry, existing: Option<&Value>) -> Option<Value> {
        let claude_type = match e.kind.as_str() {
            "streamableHttp" => Some("http"),
            "sse" => Some("sse"),
            "stdio" => None,
            _ => return None, // 未知类型该端不支持，同步时跳过
        };
        let mut entry = existing
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        match claude_type {
            Some(t) => entry.insert("type".into(), json!(t)),
            None => entry.remove("type"),
        };
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
        if !e.headers.is_empty() && claude_type.is_some() {
            entry.insert("headers".into(), json!(e.headers));
        } else {
            entry.remove("headers");
        }
        Some(Value::Object(entry))
    }
}
