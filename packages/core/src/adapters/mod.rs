//! adapters —— AgentAdapter 抽象（可插拔核心层）。
//!
//! 每个 agent 一个模块（wb.rs / zcode.rs），各自实现 [`AgentAdapter`] trait 并注册进
//! [`registry()`]。新增 agent 适配器（claude / codex / trae…，社区贡献）只需：
//! 1. 新建 `adapters/<id>.rs` 实现 trait（按能力域裁剪，`caps` 里关掉不支持的域）；
//! 2. 在 `lib.rs` 注册模块 + 在下方 `REGISTRY` 数组加一行；
//! 3. 前端 `src/agents.ts` 注册表补一条名称/身份色。
//! 共享引擎（skills 迁移、MCP 同步、插件门控）与 UI 全部按注册表与能力声明驱动，
//! 不需要改任何引擎代码。用户的启用/停用状态存 SQLite `plugin_state`（见 plugins.rs）。

pub mod claude;
pub mod wb;
pub mod zcode;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;
use serde_json::Value;

use crate::fsutil;

/// Windows 下取用户主目录（本项目 Windows 优先，不走 home 环境变量链）
pub fn agent_home() -> PathBuf {
    PathBuf::from(
        std::env::var("USERPROFILE").unwrap_or_else(|_| r"C:\Users\Public".to_string()),
    )
}

// ─── 能力域声明 ───

/// agent 适配器支持的能力域：共享引擎与 UI 按此跳过/置灰
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct AgentCaps {
    pub skills: bool,
    pub mcp: bool,
}

/// skills 分发策略：逐项链（每个 skill 一条链接）或整目录链（skills 目录整体指向中央仓库）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SkillsLink {
    PerSkill,
    WholeDir,
}

// ─── skills 域共享助手 ───

/// 单 skill 目录在单端的状态：linked / diverged（实目录）/ broken（断链）/ missing
pub fn link_status_for(entry_path: &Path, central_skill: &Path) -> String {
    if fsutil::is_link(entry_path) {
        let matches = fsutil::link_target(entry_path)
            .map(|t| eq_path(&t.to_string_lossy(), &central_skill.to_string_lossy()))
            .unwrap_or(false);
        if matches {
            "linked".into()
        } else {
            "broken".into()
        }
    } else if entry_path.is_dir() {
        "diverged".into()
    } else {
        "missing".into()
    }
}

/// 路径相等（大小写与分隔符不敏感，Windows 语义）
pub fn eq_path(a: &str, b: &str) -> bool {
    a.replace('/', "\\").to_lowercase() == b.replace('/', "\\").to_lowercase()
}

// ─── MCP 域中性结构（避免 adapters 反向依赖 mcp.rs） ───

/// 统一定义 → 目标条目的中性载荷
pub struct McpEntry {
    pub kind: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub env: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
    pub enabled: bool,
}

/// 目标文件里条目的实测状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpLive {
    /// 条目存在且处于启用态
    Present,
    /// 条目存在但被 agent 侧禁用（WB disabled=true / ZCode enabled=false）
    Disabled,
}

/// MCP 配置文件形态：路径 + 容器键路径（如 ["mcp","servers"]）
pub struct McpConfig {
    pub path: PathBuf,
    pub container: &'static [&'static str],
}

// ─── AgentAdapter trait ───

/// agent 适配器：路径约定、资产探测、skills 分发、MCP 格式翻译。
/// 实现须无内部可变状态（&self 并发安全），社区适配器照此接入。
pub trait AgentAdapter: Sync + Send {
    /// 稳定 id（存 DB / IPC 传输用），如 "workbuddy"
    fn id(&self) -> &'static str;
    /// 展示名（UI 文案）
    fn display_name(&self) -> &'static str;
    /// 一句话简介（Plugin 岛卡片副标题）
    fn desc(&self) -> &'static str;
    /// 能力域声明
    fn caps(&self) -> AgentCaps;
    /// 安装探测（目录存在等轻量判定）
    fn installed(&self) -> bool;

    // ── skills 域（caps.skills = true 时实现） ──

    /// 该端 skills 目录
    fn skills_dir(&self) -> PathBuf {
        unreachable!("caps.skills = false 的适配器不使用 skills 域")
    }
    /// 分发策略（逐项链 / 整目录链）
    fn skills_link(&self) -> SkillsLink {
        unreachable!("caps.skills = false 的适配器不使用 skills 域")
    }
    /// 分发/修复单个 skill。返回动作描述："relinked"（本次重建）/ "kept"（本就指向中央仓库）。
    /// WholeDir 端等价于整目录链确保动作。
    fn relink_skill(&self, _name: &str, _central: &Path, _backups: &Path, _mode: fsutil::LinkMode) -> Res<String> {
        unreachable!("caps.skills = false 的适配器不使用 skills 域")
    }
    /// 整端链接确保（迁移用）。返回动作："already-linked" / "relinked" / "replaced-with-link" / "created"
    fn ensure_skills_link(&self, _central: &Path, _backups: &Path, _mode: fsutil::LinkMode) -> Res<String> {
        unreachable!("caps.skills = false 的适配器不使用 skills 域")
    }
    /// 单 skill 在该端的状态（WholeDir 且整链生效时恒为 linked）
    fn skill_link_status(&self, name: &str, central: &Path) -> String {
        link_status_for(&self.skills_dir().join(name), central)
    }
    /// 该端是否处于「整目录链」且指向中央仓库（此时单 skill 无需单独处理）
    fn whole_link_active(&self, _central: &Path) -> bool {
        false
    }

    // ── MCP 域（caps.mcp = true 时实现） ──

    /// MCP 配置文件形态
    fn mcp_config(&self) -> McpConfig {
        unreachable!("caps.mcp = false 的适配器不使用 MCP 域")
    }
    /// 目标条目实测状态（WB 读 disabled / ZCode 读 enabled）
    fn mcp_entry_live(&self, _entry: &Value) -> McpLive {
        unreachable!("caps.mcp = false 的适配器不使用 MCP 域")
    }
    /// 收编时该端是否视作启用（条目缺失 = false）
    fn mcp_enabled_of(&self, entry: Option<&Value>) -> bool {
        entry.map(|e| self.mcp_entry_live(e) == McpLive::Present).unwrap_or(false)
    }
    /// 生成该端条目；None = 该端不支持此类型（如仅 WB 支持的协议），同步时跳过
    fn to_mcp_entry(&self, _entry: &McpEntry, _existing: Option<&Value>) -> Option<Value> {
        unreachable!("caps.mcp = false 的适配器不使用 MCP 域")
    }
    /// 该端是否支持此 server 类型（总览「不支持」标注用）
    fn supports_kind(&self, _kind: &str) -> bool {
        true
    }
    /// 收编时类型名归一（ZCode 的 http → 规范型 streamableHttp；默认原样）
    fn canonical_kind(&self, kind: &str) -> String {
        kind.to_string()
    }

    // ── 账号/登录态探测（Account 岛健康卡用；无登录态概念的端返回 false） ──

    fn auth_found(&self) -> bool {
        false
    }
}

// ─── 注册表 ───

pub static WORKBUDDY: wb::WorkBuddyAdapter = wb::WorkBuddyAdapter;
pub static ZCODE: zcode::ZcodeAdapter = zcode::ZcodeAdapter;
pub static CLAUDE: claude::ClaudeAdapter = claude::ClaudeAdapter;

/// 内置适配器注册表（社区新增 agent 在此追加一行 + lib.rs 注册模块）
static REGISTRY: [&'static dyn AgentAdapter; 3] = [&WORKBUDDY, &ZCODE, &CLAUDE];

/// 全部已注册适配器（注册表顺序 = WB 优先，收编冲突时取先者字段）
pub fn registry() -> &'static [&'static dyn AgentAdapter] {
    &REGISTRY
}

/// 按 id 取适配器
pub fn by_id(id: &str) -> Option<&'static dyn AgentAdapter> {
    REGISTRY.iter().copied().find(|a| a.id() == id)
}

use crate::fsutil::Res;

/// skill 目录扫描结果（中央仓库与各 agent 目录共用）
#[derive(Debug, Serialize, Clone)]
pub struct SkillDirInfo {
    pub name: String,
    pub path: String,
    /// 是软链（symlink/junction）还是实目录
    pub is_link: bool,
    /// 软链目标（归一化，去掉 \\?\ 前缀）
    pub link_target: Option<String>,
    pub description: Option<String>,
    /// 目录 mtime（epoch ms）
    pub mtime_ms: i64,
    /// 是否可导入：实目录且含 SKILL.md
    pub importable: bool,
}

/// 扫描一个 skills 目录（仅目录项；不存在返回空）
pub fn scan_skills_dir(dir: &std::path::Path) -> Vec<SkillDirInfo> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let linked = fsutil::is_link(&path);
        let link_target = fsutil::link_target(&path).map(|p| p.to_string_lossy().to_string());
        // 软链指向的目标是否为目录（断链 = 链接存在但目标不可达）
        let target_ok = link_target
            .as_ref()
            .map(|t| std::path::Path::new(t).is_dir())
            .unwrap_or(!linked);
        let skill_md = path.join("SKILL.md");
        let (fm_name, description) = if skill_md.is_file() {
            fsutil::parse_skill_md(&skill_md)
        } else {
            (None, None)
        };
        let mtime_ms = entry
            .metadata()
            .ok()
            .and_then(|md| md.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        out.push(SkillDirInfo {
            name: fm_name.unwrap_or(name),
            path: path.to_string_lossy().to_string(),
            is_link: linked,
            link_target,
            description,
            mtime_ms,
            // importable：实目录（软链不导入，避免把别人的目标拷进来）；断链不可导入
            importable: !linked && target_ok && skill_md.is_file(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}
