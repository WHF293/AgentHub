//! providers —— M4 第三方模型：ZCode v2/config.json 直写 + WB models.json 写穿 + 统一 provider 表。
//!
//! 实测结构（2026-10-31）：
//! - `~/.zcode/v2/config.json`：`{provider:{<id>:{name,kind,options:{apiKey,baseURL},models:{<mid>:{reasoning,limit,modalities,zcode?}}}}}`
//! - `~/.zcode/v2/setting.json`：`modelProviderFamilySelectedKeys` 决定默认（改后需重启 ZCode）
//! - `~/.workbuddy/models.json`：自定义模型（裸数组或 {models:[…]}；条目必须带 id，
//!   认 name/url/apiKey/maxInputTokens/maxOutputTokens/supportsToolCall/supportsImages/
//!   supportsReasoning/reasoning；url 规范化为 …/chat/completions，仅 OpenAI 兼容协议；
//!   WB 侧 1s debounce 热加载，改完即生效无需重启）
//!
//! 隐私红线：API Key 只存在于 ZCode/WB 目标文件；DB 不落 key（wb_model_entries 只记条目 id 归属）。

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::adapters::{wb, zcode};
use crate::fsutil::{self, Res};
use crate::store::{self, Store};

/// kind 规范型：anthropic | openai-compatible
pub const KINDS: [&str; 2] = ["anthropic", "openai-compatible"];

fn read_json(path: &std::path::Path) -> Res<Value> {
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
    /// 已写穿到 WB models.json 的本 provider 模型（DB 托管清单 ∩ 文件现存 id）
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
                wb_models: Vec::new(),
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    // WB 写穿状态：DB 托管清单 ∩ models.json 现存 id（文件为准，防 DB/文件漂移误报）
    let wb_present = wb_current_ids();
    if !wb_present.is_empty() {
        let owned = Store::open()
            .ok()
            .and_then(|s| s.with(|conn| store::wb_entries_all(conn)).ok())
            .unwrap_or_default();
        for p in out.iter_mut() {
            p.wb_models = owned
                .iter()
                .filter(|(pid, eid)| pid == &p.id && wb_present.contains(eid.as_str()))
                .map(|(_, eid)| eid.clone())
                .collect();
        }
    }
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

// ─── WB models.json 写穿（仅 openai-compatible；合并式：托管字段 url/apiKey 覆盖，其余保留） ───

/// WB 写穿结果（前端拼提示用，条目 id 列表）
#[derive(Debug)]
pub struct WbSyncReport {
    pub written: Vec<String>,
    pub removed: Vec<String>,
    /// 与他人托管/用户自建条目 id 撞名而跳过的
    pub skipped: Vec<String>,
}

/// baseURL → WB chat/completions 端点（WB 侧会再规范化一次，双保险）
fn wb_chat_url(base_url: &str) -> String {
    const SUFFIX: &str = "/chat/completions";
    let mut u = base_url.trim().trim_end_matches('/').to_string();
    while u.ends_with(SUFFIX) {
        u.truncate(u.len() - SUFFIX.len());
        u = u.trim_end_matches('/').to_string();
    }
    format!("{u}{SUFFIX}")
}

/// 读 WB models.json：(根值, 是否 {models:[…]} 对象形态)。文件缺失 → 空 {models:[]}
fn read_wb_root() -> Res<(Value, bool)> {
    let path = wb::models_json();
    match fsutil::read_opt(&path) {
        Some(bytes) => {
            let v: Value = serde_json::from_slice(&bytes)
                .map_err(|e| format!("WB models.json 解析失败: {e}"))?;
            if v.is_array() {
                Ok((v, false))
            } else if v.is_object() {
                Ok((v, true))
            } else {
                Err("WB models.json 既非数组也非对象".into())
            }
        }
        None => Ok((json!({ "models": [] }), true)),
    }
}

/// 根值 → models 数组可变引用（保持文件原形态写回）
fn wb_models_mut<'a>(root: &'a mut Value, object_form: bool) -> Res<&'a mut Vec<Value>> {
    if object_form {
        root.as_object_mut()
            .ok_or("WB models.json 对象形态异常")?
            .entry("models".to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("WB models.json 的 models 字段非数组".into())
    } else {
        root.as_array_mut().ok_or("WB models.json 非数组".into())
    }
}

/// WB models.json 当前全部条目 id
fn wb_current_ids() -> std::collections::HashSet<String> {
    match read_wb_root() {
        Ok((mut root, object_form)) => match wb_models_mut(&mut root, object_form) {
            Ok(entries) => entries
                .iter()
                .filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_string))
                .collect(),
            Err(_) => Default::default(),
        },
        Err(_) => Default::default(),
    }
}

/// 从 DB 取托管映射（provider_id → entry_id）；DB 不可用视作空（仅影响冲突判定与清理）
fn wb_owned_map() -> Vec<(String, String)> {
    Store::open()
        .ok()
        .and_then(|s| s.with(|conn| store::wb_entries_all(conn)).ok())
        .unwrap_or_default()
}

/// 写回 models.json：JSON 可解析校验 → 备份 → 原子写
fn write_wb_root(root: &Value) -> Res<()> {
    let path = wb::models_json();
    let bytes = serde_json::to_vec_pretty(root).map_err(|e| e.to_string())?;
    serde_json::from_slice::<Value>(&bytes).map_err(|e| format!("写出 JSON 校验失败: {e}"))?;
    if path.exists() {
        fsutil::backup_file(&path, &store::backups_dir())?;
    }
    fsutil::atomic_write(&path, &bytes)?;
    Ok(())
}

/// 把 provider 的勾选模型写穿进 WB models.json；无变化不落盘、DB 清单照常对齐
pub fn sync_wb_provider(
    provider_id: &str,
    base_url: &str,
    api_key: Option<&str>,
    models: &[String],
) -> Res<WbSyncReport> {
    if base_url.trim().is_empty() {
        return Err("baseURL 为空，无法写穿 WB".into());
    }
    let (mut root, object_form) = read_wb_root()?;
    let owned = wb_owned_map();
    let owner_of: std::collections::HashMap<&str, &str> =
        owned.iter().map(|(p, e)| (e.as_str(), p.as_str())).collect();

    let url = wb_chat_url(base_url);
    let mut written = Vec::new();
    let mut skipped = Vec::new();
    let mut managed_now: Vec<String> = Vec::new();

    // 条目增改 + 托管差集清理都在 entries 借用内完成，块尾把摘除清单传出
    let removed: Vec<String> = {
        let entries = wb_models_mut(&mut root, object_form)?;
        let mut by_id: std::collections::HashMap<String, usize> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.get("id").and_then(Value::as_str).unwrap_or("").to_string(), i))
            .filter(|(id, _)| !id.is_empty())
            .collect();

        for m in models {
            let mid = m.trim();
            if mid.is_empty() {
                continue;
            }
            // 他人托管/用户自建条目撞名 → 跳过不覆盖（未添加 ≠ 禁用，保 WB 侧用户数据）
            if owner_of.get(mid).is_some_and(|p| *p != provider_id) {
                skipped.push(mid.to_string());
                continue;
            }
            match by_id.get(mid).copied() {
                Some(i) => {
                    let obj = entries[i].as_object_mut().ok_or("WB 模型条目非对象")?;
                    obj.insert("id".into(), json!(mid));
                    obj.insert("url".into(), json!(url));
                    if let Some(k) = api_key.filter(|s| !s.is_empty()) {
                        obj.insert("apiKey".into(), json!(k));
                    }
                }
                None => {
                    let mut e = Map::new();
                    e.insert("id".into(), json!(mid));
                    e.insert("name".into(), json!(mid));
                    e.insert("url".into(), json!(url));
                    if let Some(k) = api_key.filter(|s| !s.is_empty()) {
                        e.insert("apiKey".into(), json!(k));
                    }
                    // 默认能力字段照抄 WB 本地模型写入默认（实测 addToModelsJson）
                    e.insert("maxInputTokens".into(), json!(131072));
                    e.insert("maxOutputTokens".into(), json!(8192));
                    e.insert("supportsToolCall".into(), json!(true));
                    e.insert("supportsImages".into(), json!(false));
                    e.insert("supportsReasoning".into(), json!(true));
                    e.insert(
                        "reasoning".into(),
                        json!({
                            "defaultEffort": "medium",
                            "supportedEfforts": ["low", "medium", "high"],
                            "canDisableThinking": true
                        }),
                    );
                    entries.push(Value::Object(e));
                    by_id.insert(mid.to_string(), entries.len() - 1);
                }
            }
            written.push(mid.to_string());
            managed_now.push(mid.to_string());
        }

        // 托管差集清理：本 provider 曾写入、本次未勾选的条目从 WB 摘除
        let stale: Vec<String> = owned
            .iter()
            .filter(|(p, e)| p == &provider_id && !managed_now.contains(e))
            .map(|(_, e)| e.clone())
            .collect();
        if !stale.is_empty() {
            entries.retain(|e| {
                let id = e.get("id").and_then(Value::as_str).unwrap_or("");
                !stale.iter().any(|s| s == id)
            });
        }
        stale
    };

    // 无增删不落盘（避免无谓触碰用户正在编辑的文件）；DB 清单照常对齐本次语义
    if !written.is_empty() || !removed.is_empty() {
        write_wb_root(&root)?;
    }
    if let Ok(conn_store) = Store::open() {
        let _ = conn_store.with(|conn| {
            store::wb_entries_replace(conn, provider_id, &managed_now)?;
            store::log_op(
                conn,
                "provider.wb_sync",
                provider_id,
                json!({
                    "written": written.len(),
                    "removed": removed.len(),
                    "skipped": skipped.len()
                }),
            );
            Ok(())
        });
    }
    Ok(WbSyncReport { written, removed, skipped })
}

/// provider 删除时摘除其在 WB 的全部托管条目，返回摘除数
pub fn remove_wb_provider(provider_id: &str) -> Res<usize> {
    let owned = Store::open()
        .ok()
        .and_then(|s| s.with(|conn| store::wb_entries_for(conn, provider_id)).ok())
        .unwrap_or_default();
    if owned.is_empty() {
        return Ok(0);
    }
    let (mut root, object_form) = read_wb_root()?;
    let removed = {
        let entries = wb_models_mut(&mut root, object_form)?;
        let before = entries.len();
        entries.retain(|e| {
            let id = e.get("id").and_then(Value::as_str).unwrap_or("");
            !owned.iter().any(|s| s == id)
        });
        before - entries.len()
    };
    if removed > 0 {
        write_wb_root(&root)?;
        if let Ok(conn_store) = Store::open() {
            let _ = conn_store.with(|conn| {
                store::log_op(
                    conn,
                    "provider.wb_remove",
                    provider_id,
                    json!({ "removed": removed }),
                );
                Ok(())
            });
        }
    }
    let _ = Store::open()
        .ok()
        .and_then(|s| s.with(|conn| store::wb_entries_remove_provider(conn, provider_id)).ok());
    Ok(removed)
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

// ─── Tauri commands（M4） ───

use serde::Deserialize;
use serde_json::Value as V;

#[derive(Serialize)]
pub struct ProvidersOverview {
    pub providers: Vec<ZcodeProviderLive>,
    pub selected_keys: Map<String, V>,
}

#[tauri::command]
pub fn providers_overview(state: tauri::State<'_, crate::store::Store>) -> Result<ProvidersOverview, String> {
    // ZCode 插件停用：Model 岛主存储在 ZCode v2/config.json，停用即空态（前端提示去 Plugin 岛开启）
    if !crate::plugins::enabled_ids(&state)?.iter().any(|id| id == "zcode") {
        return Ok(ProvidersOverview {
            providers: Vec::new(),
            selected_keys: Map::new(),
        });
    }
    Ok(ProvidersOverview {
        providers: read_zcode_providers()?,
        selected_keys: read_selected_keys()?,
    })
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
        .and_then(V::as_str)
        .unwrap_or("");
    if key.is_empty() {
        return Err(format!("provider「{id}」未配置 API Key，请先填写"));
    }
    Ok(key.to_string())
}

/// 多单词参数一律收进单个 input 结构体（serde 语义自控，规避 IPC 参数大小写约定）
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
    state: tauri::State<'_, crate::store::Store>,
    input: FetchModelsInput,
) -> Result<crate::modelfetch::FetchResult, String> {
    if !KINDS.contains(&input.kind.as_str()) {
        return Err(format!(
            "未知 kind：{}（支持 anthropic / openai-compatible）",
            input.kind
        ));
    }
    crate::plugins::require_enabled(&state, "zcode")?;
    let key = resolve_api_key(&input.id, input.api_key.as_deref())?;
    Ok(crate::modelfetch::fetch_models(
        &input.kind,
        &input.base_url,
        &key,
        input.models_url.as_deref(),
    ))
}

#[derive(Debug, Deserialize)]
pub struct SaveProviderInput {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub models: Option<Vec<String>>,
}

/// 保存 provider：直写 ZCode v2/config.json + openai-compatible 写穿 WB models.json（key 仅在显式传入时更新）
#[tauri::command]
pub fn provider_save(
    state: tauri::State<'_, crate::store::Store>,
    input: SaveProviderInput,
) -> Result<String, String> {
    if !KINDS.contains(&input.kind.as_str()) {
        return Err(format!("未知 kind：{}", input.kind));
    }
    // 插件门控：ZCode 是主存储必须启用；WB 写穿仅在 WB 插件启用时执行
    crate::plugins::require_enabled(&state, "zcode")?;
    let wb_enabled = crate::plugins::enabled_ids(&state)?.iter().any(|id| id == "workbuddy");
    let id = input.id.clone();
    let base_url = input.base_url.clone();
    // WB 侧写穿要拿真 key：显式传入优先，否则从 ZCode 目标文件回读（不出 Rust 层）
    let wb_key = if wb_enabled {
        resolve_api_key(&id, input.api_key.as_deref()).ok()
    } else {
        None
    };
    write_zcode_provider(&ProviderWrite {
        id,
        name: input.name,
        kind: input.kind.clone(),
        base_url,
        api_key: input.api_key,
        reset_models_with: input.models.clone(),
    })?;
    if !wb_enabled {
        return Ok("已写入 ZCode · WB 插件未启用，未写穿 models.json".into());
    }
    if input.kind != "openai-compatible" {
        return Ok("已写入 ZCode · WB 侧仅支持 OpenAI 兼容接入点，本次未同步".into());
    }
    let Some(models) = input.models else {
        return Ok("已写入 ZCode（模型列表未勾选变更，WB 未改动）".into());
    };
    let rep = sync_wb_provider(&input.id, &input.base_url, wb_key.as_deref(), &models)?;
    Ok(format!(
        "已写入 ZCode · WB 写 {} 清 {} 跳过 {}（models.json 热加载，WB 内即生效）",
        rep.written.len(),
        rep.removed.len(),
        rep.skipped.len()
    ))
}

#[tauri::command]
pub fn provider_delete(
    state: tauri::State<'_, crate::store::Store>,
    id: String,
) -> Result<String, String> {
    crate::plugins::require_enabled(&state, "zcode")?;
    delete_zcode_provider(&id)?;
    // WB 侧清理仅在 WB 插件启用时执行
    let wb_enabled = crate::plugins::enabled_ids(&state)?.iter().any(|x| x == "workbuddy");
    let n = if wb_enabled {
        remove_wb_provider(&id).unwrap_or(0)
    } else {
        0
    };
    Ok(if n > 0 {
        format!("已从 ZCode 删除 · WB 侧同步清理 {n} 条")
    } else {
        "已从 ZCode 删除".into()
    })
}

#[tauri::command]
pub fn provider_set_default(
    state: tauri::State<'_, crate::store::Store>,
    family: String,
    key: String,
) -> Result<(), String> {
    crate::plugins::require_enabled(&state, "zcode")?;
    set_default_provider(&family, &key)
}

// ─── Claude Code 模型托管（settings.json env 块直写，单档） ───
// env 托管四键：ANTHROPIC_BASE_URL / ANTHROPIC_AUTH_TOKEN / ANTHROPIC_MODEL / ANTHROPIC_SMALL_FAST_MODEL；
// 其余 env 键与未知键合并保留。模型值一律来自用户在模型岛手动配置，不预填任何官方模型/key。
// 附带确保 ~/.claude.json 的 hasCompletedOnboarding=true（跳过首次引导，cc-switch 同款体验）。

use crate::adapters::claude;

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
    let root = read_json(&claude::settings_json())?;
    let env = root.get("env").and_then(Value::as_object);
    let get_s = |k: &str| -> String {
        env.and_then(|e| e.get(k))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let onboarding_done = read_json(&claude::claude_json())?
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

#[derive(Debug, Deserialize)]
pub struct ClaudeSaveInput {
    pub base_url: String,
    /// None = 掩码保存不碰 key；Some("") = 清除
    pub api_key: Option<String>,
    /// None = 不变；Some("") = 清除该键
    pub model: Option<String>,
    pub small_fast_model: Option<String>,
}

/// 写 settings.json env 块（备份 → 原子写 → 回读校验）+ 确保 ~/.claude.json 跳过引导。
/// key 只写目标文件；日志只记 key_changed 布尔。
pub fn write_claude_provider(w: &ClaudeSaveInput) -> Res<String> {
    let path = claude::settings_json();
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
    let cj = claude::claude_json();
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
    let key = read_json(&claude::settings_json())?
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
#[derive(Debug, Deserialize)]
pub struct ClaudeFetchModelsInput {
    pub base_url: String,
    pub api_key: Option<String>,
    pub models_url: Option<String>,
}

#[tauri::command]
pub fn claude_provider_fetch_models(
    state: tauri::State<'_, crate::store::Store>,
    input: ClaudeFetchModelsInput,
) -> Result<crate::modelfetch::FetchResult, String> {
    crate::plugins::require_enabled(&state, "claude")?;
    let key = resolve_claude_api_key(input.api_key.as_deref())?;
    Ok(crate::modelfetch::fetch_models(
        "anthropic",
        &input.base_url,
        &key,
        input.models_url.as_deref(),
    ))
}

#[tauri::command]
pub fn claude_provider_save(
    state: tauri::State<'_, crate::store::Store>,
    input: ClaudeSaveInput,
) -> Result<String, String> {
    crate::plugins::require_enabled(&state, "claude")?;
    write_claude_provider(&input)
}
