//! provider_zcode —— ZCode v2/config.json 直写（原 core/providers.rs ZCode 段平移）。
//!
//! 实测结构（2026-10-31）：
//! - `~/.zcode/v2/config.json`：`{provider:{<id>:{name,kind,options:{apiKey,baseURL},models:{…}}}}`
//! - `~/.zcode/v2/setting.json`：`modelProviderFamilySelectedKeys` 决定默认（改后需重启 ZCode）
//!
//! 隐私红线：API Key 只存在于 ZCode 目标文件；DB 不落 key。
//! 跨域编排（providers_overview 的 WB 标注 / provider_save 的 WB 写穿 / provider_delete
//! 的 WB 摘除）在 src-tauri 组装桥，本包不依赖 WB 插件。

use serde::Serialize;
use serde_json::{json, Map, Value};

use agenthub_core::fsutil::{self, Res};
use agenthub_core::store::{self, Store};

use crate::adapter as zcode;

/// kind 规范型：anthropic | openai-compatible
pub const KINDS: [&str; 2] = ["anthropic", "openai-compatible"];

pub fn read_json(path: &std::path::Path) -> Res<Value> {
    match fsutil::read_opt(path) {
        Some(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("JSON 解析失败 {}: {e}", path.display())),
        None => Ok(json!({})),
    }
}

/// ZCode 侧实测 provider（从 v2/config.json 读出）
#[derive(Debug, Serialize)]
pub struct ZcodeProviderLive {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub has_api_key: bool,
    pub models: Vec<String>,
    pub builtin: bool,
    /// 已写穿到 WB models.json 的本 provider 模型（组装桥经 WB 插件标注）
    pub wb_models: Vec<String>,
}

pub fn read_zcode_providers() -> Res<Vec<ZcodeProviderLive>> {
    let root = read_json(&zcode::v2_config())?;
    let mut out = Vec::new();
    if let Some(provs) = root.get("provider").and_then(Value::as_object) {
        for (id, v) in provs {
            out.push(ZcodeProviderLive {
                id: id.clone(),
                name: v
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_string(),
                kind: v
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("openai-compatible")
                    .to_string(),
                base_url: v
                    .get("options")
                    .and_then(|o| o.get("baseURL"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                has_api_key: v
                    .get("options")
                    .and_then(|o| o.get("apiKey"))
                    .map(|k| k.as_str().map(|s| !s.is_empty()).unwrap_or(false))
                    .unwrap_or(false),
                models: v
                    .get("models")
                    .and_then(Value::as_object)
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default(),
                builtin: id.starts_with("builtin:"),
                wb_models: Vec::new(), // 组装桥经 WB 插件 synced_ids 标注
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// 生成新 model 条目（勾选拉取结果时的默认值，可在 UI 再编辑）
pub fn default_model_entry() -> Value {
    json!({
        "reasoning": { "enabled": true, "variants": ["enabled", "off"], "defaultVariant": "enabled" },
        "limit": { "context": 1000000, "output": 131072 },
        "modalities": { "input": ["text"], "output": ["text"] }
    })
}

/// 写入/更新一个 provider 到 v2/config.json（备份 + 原子写 + 保留未知键与既有 model 的 zcode 子对象）。
/// api_key 传 None 表示不改 key（掩码保存路径）。
pub struct ProviderWrite {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub api_key: Option<String>,
    /// Some(模型列表) = 用默认值重建 models；None = 保留既有 models
    pub reset_models_with: Option<Vec<String>>,
}

pub fn write_zcode_provider(w: &ProviderWrite) -> Res<()> {
    let path = zcode::v2_config();
    let mut root = read_json(&path)?;
    let backups = store::backups_dir();
    if path.exists() {
        fsutil::backup_file(&path, &backups)?;
    }
    if !root.is_object() {
        root = json!({});
    }
    let obj = root.as_object_mut().unwrap();
    if !obj.contains_key("provider") {
        obj.insert("provider".into(), json!({}));
    }
    let provs = obj.get_mut("provider").unwrap().as_object_mut().unwrap();
    let existing = provs.get(&w.id).cloned().unwrap_or_else(|| json!({}));
    let mut entry = existing.as_object().cloned().unwrap_or_default();
    entry.insert("name".into(), json!(w.name));
    entry.insert("kind".into(), json!(w.kind));
    // options：合并保留未知键（apiKeyRequired 等）
    let mut options = entry
        .get("options")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    options.insert("baseURL".into(), json!(w.base_url));
    if let Some(key) = &w.api_key {
        options.insert("apiKey".into(), json!(key));
    }
    entry.insert("options".into(), Value::Object(options));
    // models：按需重建或保留；重建时保留同名既有条目（含 zcode 子对象）
    if let Some(model_ids) = &w.reset_models_with {
        let old = entry.get("models").and_then(Value::as_object).cloned();
        let mut models = Map::new();
        for mid in model_ids {
            if let Some(prev) = old.as_ref().and_then(|o| o.get(mid)) {
                models.insert(mid.clone(), prev.clone());
            } else {
                models.insert(mid.clone(), default_model_entry());
            }
        }
        entry.insert("models".into(), Value::Object(models));
    }
    provs.insert(w.id.clone(), Value::Object(entry));

    let bytes = serde_json::to_vec_pretty(&root).map_err(|e| e.to_string())?;
    fsutil::atomic_write(&path, &bytes)?;
    store_log(w);
    Ok(())
}

fn store_log(w: &ProviderWrite) {
    if let Ok(conn_store) = Store::open() {
        let _ = conn_store.with(|conn| {
            store::log_op(
                conn,
                "provider.write_zcode",
                &w.id,
                json!({ "name": w.name, "kind": w.kind, "models_reset": w.reset_models_with.is_some(), "key_changed": w.api_key.is_some() }),
            );
            Ok(())
        });
    }
}

pub fn delete_zcode_provider(id: &str) -> Res<()> {
    let path = zcode::v2_config();
    let mut root = read_json(&path)?;
    if let Some(provs) = root.get_mut("provider").and_then(Value::as_object_mut) {
        if provs.remove(id).is_none() {
            return Err(format!("provider 不存在：{id}"));
        }
    } else {
        return Err("config.json 无 provider 节点".into());
    }
    let backups = store::backups_dir();
    if path.exists() {
        fsutil::backup_file(&path, &backups)?;
    }
    let bytes = serde_json::to_vec_pretty(&root).map_err(|e| e.to_string())?;
    fsutil::atomic_write(&path, &bytes)?;
    if let Ok(conn_store) = Store::open() {
        let _ = conn_store.with(|conn| {
            store::log_op(conn, "provider.delete_zcode", id, json!({}));
            Ok(())
        });
    }
    Ok(())
}

/// 设置默认 provider（family → SelectedKeys）。改后需重启 ZCode 生效。
pub fn set_default_provider(family: &str, key: &str) -> Res<()> {
    let path = zcode::v2_setting();
    let mut root = read_json(&path)?;
    let backups = store::backups_dir();
    if path.exists() {
        fsutil::backup_file(&path, &backups)?;
    }
    if !root.is_object() {
        root = json!({});
    }
    let obj = root.as_object_mut().unwrap();
    if !obj.contains_key("modelProviderFamilySelectedKeys") {
        obj.insert("modelProviderFamilySelectedKeys".into(), json!({}));
    }
    obj.get_mut("modelProviderFamilySelectedKeys")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert(family.to_string(), json!(key));
    let bytes = serde_json::to_vec_pretty(&root).map_err(|e| e.to_string())?;
    fsutil::atomic_write(&path, &bytes)?;
    if let Ok(conn_store) = Store::open() {
        let _ = conn_store.with(|conn| {
            store::log_op(
                conn,
                "provider.set_default",
                family,
                json!({ "key": key }),
            );
            Ok(())
        });
    }
    Ok(())
}

/// 读取当前默认 provider（family → selected key）
pub fn read_selected_keys() -> Res<Map<String, Value>> {
    let root = read_json(&zcode::v2_setting())?;
    Ok(root
        .get("modelProviderFamilySelectedKeys")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default())
}

/// 从目标文件解析某 provider 的 key（编辑既有 provider 时免重输；key 不出 Rust 层、不入 DB）
pub fn resolve_api_key(id: &str, explicit: Option<&str>) -> Res<String> {
    if let Some(k) = explicit.filter(|s| !s.is_empty()) {
        return Ok(k.to_string());
    }
    let root = read_json(&zcode::v2_config())?;
    let key = root
        .get("provider")
        .and_then(|p| p.get(id))
        .and_then(|p| p.get("options"))
        .and_then(|o| o.get("apiKey"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if key.is_empty() {
        return Err(format!("provider「{id}」未配置 API Key，请先填写"));
    }
    Ok(key.to_string())
}

// ─── Tauri commands（M4，ZCode 域内） ───

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct FetchModelsInput {
    pub id: String,
    pub kind: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub models_url: Option<String>,
}

#[tauri::command]
pub fn provider_fetch_models(
    state: tauri::State<'_, Store>,
    input: FetchModelsInput,
) -> Result<agenthub_core::modelfetch::FetchResult, String> {
    if !KINDS.contains(&input.kind.as_str()) {
        return Err(format!(
            "未知 kind：{}（支持 anthropic / openai-compatible）",
            input.kind
        ));
    }
    agenthub_core::plugins::require_enabled(&state, "zcode")?;
    let key = resolve_api_key(&input.id, input.api_key.as_deref())?;
    Ok(agenthub_core::modelfetch::fetch_models(
        &input.kind,
        &input.base_url,
        &key,
        input.models_url.as_deref(),
    ))
}

#[tauri::command]
pub fn provider_set_default(
    state: tauri::State<'_, Store>,
    family: String,
    key: String,
) -> Result<(), String> {
    agenthub_core::plugins::require_enabled(&state, "zcode")?;
    set_default_provider(&family, &key)
}
