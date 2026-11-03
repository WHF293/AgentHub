//! skills —— ZCode 插件自带技能展示（原 core/commands.rs 对应段平移）。
//!
//! 插件体系不经中央仓库分发，AgentHub 仅列出全貌（只读）：
//! installed_plugins.json（含 installPath/version）× cli/config.json plugins.enabledPlugins。

use std::collections::HashMap;

use serde::Serialize;

use agenthub_core::fsutil;

use crate::adapter as zcode;

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

/// ZCode 插件自带技能（只读展示）
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
