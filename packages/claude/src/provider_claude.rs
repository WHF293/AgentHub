//! provider_claude —— Claude Code 模型托管（settings.json env 块直写，单档）
//! （原 core/providers.rs claude 段平移）。
//!
//! env 托管四键：ANTHROPIC_BASE_URL / ANTHROPIC_AUTH_TOKEN / ANTHROPIC_MODEL /
//! ANTHROPIC_SMALL_FAST_MODEL；其余 env 键与未知键合并保留。模型值一律来自用户在
//! 模型岛手动配置，不预填任何官方模型/key。附带确保 ~/.claude.json 的
//! hasCompletedOnboarding=true（跳过首次引导）。

use serde::Serialize;
use serde_json::{json, Map, Value};


use agenthub_core::fsutil::{self, Res};
use agenthub_core::store::{self, Store};

/// Claude 侧实测模型配置（从 settings.json env 读出；key 不回传只报有无）
#[derive(Debug, Serialize)]
pub struct ClaudeProviderLive {
    pub base_url: String,
    pub has_api_key: bool,
    pub model: String,
    pub small_fast_model: String,
    /// ~/.claude.json 是否已跳过首次引导
    pub onboarding_done: bool,
    /// settings.json 是否已有 env 块（区分「未配置」与「已配置」）
    pub configured: bool,
}

pub fn read_claude_provider() -> Res<ClaudeProviderLive> {
    let root = read_json(&crate::adapter::settings_json())?;
    let env = root.get("env").and_then(Value::as_object);
    let get_s = |k: &str| -> String {
        env.and_then(|e| e.get(k))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let onboarding_done = read_json(&crate::adapter::claude_json())?
        .get("hasCompletedOnboarding")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(ClaudeProviderLive {
        base_url: get_s("ANTHROPIC_BASE_URL"),
        has_api_key: !get_s("ANTHROPIC_AUTH_TOKEN").is_empty(),
        model: get_s("ANTHROPIC_MODEL"),
        small_fast_model: get_s("ANTHROPIC_SMALL_FAST_MODEL"),
        onboarding_done,
        configured: env.is_some(),
    })
}

#[derive(Debug, serde::Deserialize)]
pub struct ClaudeSaveInput {
    pub base_url: String,
    /// None = 掩码保存不碰 key；Some("") = 清除
    pub api_key: Option<String>,
    /// None = 不变；Some("") = 清除该键
    pub model: Option<String>,
    pub small_fast_model: Option<String>,
}

fn read_json(path: &std::path::Path) -> Res<Value> {
    match fsutil::read_opt(path) {
        Some(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("JSON 解析失败 {}: {e}", path.display())),
        None => Ok(json!({})),
    }
}

/// 写 settings.json env 块（备份 → 原子写 → 回读校验）+ 确保 ~/.claude.json 跳过引导。
/// key 只写目标文件；日志只记 key_changed 布尔。
pub fn write_claude_provider(w: &ClaudeSaveInput) -> Res<String> {
    let path = crate::adapter::settings_json();
    if w.base_url.trim().is_empty() {
        return Err("base_url 不能为空".into());
    }
    let mut root = read_json(&path)?;
    if !root.is_object() {
        root = json!({});
    }
    let obj = root.as_object_mut().unwrap();
    if !obj.contains_key("env") {
        obj.insert("env".into(), json!({}));
    }
    let env = obj.get_mut("env").unwrap().as_object_mut().unwrap();
    env.insert("ANTHROPIC_BASE_URL".into(), json!(w.base_url.trim()));
    if let Some(key) = &w.api_key {
        if key.is_empty() {
            env.remove("ANTHROPIC_AUTH_TOKEN");
        } else {
            env.insert("ANTHROPIC_AUTH_TOKEN".into(), json!(key));
        }
    }
    let set_opt = |env: &mut Map<String, Value>, k: &str, v: &Option<String>| {
        match v {
            Some(s) if !s.trim().is_empty() => {
                env.insert(k.into(), json!(s.trim()));
            }
            Some(_) => {
                env.remove(k);
            }
            None => {}
        }
    };
    set_opt(env, "ANTHROPIC_MODEL", &w.model);
    set_opt(env, "ANTHROPIC_SMALL_FAST_MODEL", &w.small_fast_model);

    let backups = store::backups_dir();
    if path.exists() {
        fsutil::backup_file(&path, &backups)?;
    }
    let bytes = serde_json::to_vec_pretty(&root).map_err(|e| e.to_string())?;
    fsutil::atomic_write(&path, &bytes)?;
    // JSON 可解析校验（红线三步）：回读失败即报错
    read_json(&path)?;

    // 跳过首次引导（~/.claude.json 在 HOME 根；缺文件则创建最小配置）
    let cj = crate::adapter::claude_json();
    let mut croot = read_json(&cj)?;
    let done = croot
        .get("hasCompletedOnboarding")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !done {
        if !croot.is_object() {
            croot = json!({});
        }
        croot
            .as_object_mut()
            .unwrap()
            .insert("hasCompletedOnboarding".into(), json!(true));
        if cj.exists() {
            fsutil::backup_file(&cj, &backups)?;
        }
        let cb = serde_json::to_vec_pretty(&croot).map_err(|e| e.to_string())?;
        fsutil::atomic_write(&cj, &cb)?;
    }

    if let Ok(conn_store) = Store::open() {
        let _ = conn_store.with(|conn| {
            store::log_op(
                conn,
                "provider.write_claude",
                "claude",
                json!({ "base_url": w.base_url.trim(), "key_changed": w.api_key.is_some(),
                        "model": w.model, "small_fast_model": w.small_fast_model }),
            );
            Ok(())
        });
    }
    Ok("已写入 ~/.claude/settings.json（Claude Code 重启会话后生效）".into())
}

/// 从 settings.json env 回读 key（获取模型列表掩码路径用；key 不出 Rust 层）
pub fn resolve_claude_api_key(explicit: Option<&str>) -> Res<String> {
    if let Some(k) = explicit.filter(|s| !s.is_empty()) {
        return Ok(k.to_string());
    }
    let key = read_json(&crate::adapter::settings_json())?
        .get("env")
        .and_then(|e| e.get("ANTHROPIC_AUTH_TOKEN"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if key.is_empty() {
        return Err("Claude 未配置 API Key，请先在模型岛填写".into());
    }
    Ok(key)
}

#[tauri::command]
pub fn claude_providers_overview() -> Result<ClaudeProviderLive, String> {
    read_claude_provider()
}

/// 多单词参数收进单个 input 结构体（IPC 约定）
#[derive(Debug, serde::Deserialize)]
pub struct ClaudeFetchModelsInput {
    pub base_url: String,
    pub api_key: Option<String>,
    pub models_url: Option<String>,
}

#[tauri::command]
pub fn claude_provider_fetch_models(
    state: tauri::State<'_, Store>,
    input: ClaudeFetchModelsInput,
) -> Result<agenthub_core::modelfetch::FetchResult, String> {
    agenthub_core::plugins::require_enabled(&state, "claude")?;
    let key = resolve_claude_api_key(input.api_key.as_deref())?;
    Ok(agenthub_core::modelfetch::fetch_models(
        "anthropic",
        &input.base_url,
        &key,
        input.models_url.as_deref(),
    ))
}

#[tauri::command]
pub fn claude_provider_save(
    state: tauri::State<'_, Store>,
    input: ClaudeSaveInput,
) -> Result<String, String> {
    agenthub_core::plugins::require_enabled(&state, "claude")?;
    write_claude_provider(&input)
}
