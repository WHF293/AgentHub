//! provider_wb —— WB models.json 写穿（原 core/providers.rs WB 段平移）。
//!
//! 仅 openai-compatible 接入点可写穿（WB 自定义模型只支持 OpenAI 兼容协议）；
//! 合并式：托管字段 url/apiKey 覆盖，其余保留；他人托管/用户自建条目撞名跳过
//! （未添加 ≠ 禁用，保 WB 侧用户数据）。DB wb_model_entries 只记条目 id 归属，
//! Key 只写 models.json 不入库。

use serde_json::{json, Map, Value};


use agenthub_core::fsutil::{self, Res};
use agenthub_core::store::{self, Store};

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
    let path = crate::adapter::models_json();
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
    let path = crate::adapter::models_json();
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

// ─── 组装桥调用面（src-tauri bridge：跨域编排放壳层，插件之间不互依） ───

/// provider 保存后的 WB 写穿编排：WB 插件未启用返回 None；kind/勾选不符返回提示。
/// 返回 Some(提示文本) 供 provider_save 拼接。
pub fn on_provider_saved(
    store: &Store,
    provider_id: &str,
    kind: &str,
    base_url: &str,
    api_key: Option<&str>,
    models: Option<&[String]>,
) -> Option<Res<String>> {
    let wb_enabled = agenthub_core::plugins::active_ids(store)
        .map(|ids| ids.iter().any(|i| i == "workbuddy"))
        .unwrap_or(false);
    if !wb_enabled {
        return None;
    }
    if kind != "openai-compatible" {
        return Some(Ok("WB 侧仅支持 OpenAI 兼容接入点，本次未同步".into()));
    }
    let Some(models) = models else {
        return Some(Ok("模型列表未勾选变更，WB 未改动".into()));
    };
    // WB 侧写穿要拿真 key：显式传入优先（已由调用方从 ZCode 目标文件回读，不出 Rust 层）
    Some(
        sync_wb_provider(provider_id, base_url, api_key, models)
            .map(|rep| format!("WB 写 {} 清 {} 跳过 {}（models.json 热加载，WB 内即生效）",
                rep.written.len(), rep.removed.len(), rep.skipped.len())),
    )
}

/// provider 总览标注：该 provider 已写穿 WB 的现存条目 id（DB 托管 ∩ models.json 现存）
pub fn synced_ids(provider_id: &str) -> Vec<String> {
    let present = wb_current_ids();
    if present.is_empty() {
        return Vec::new();
    }
    Store::open()
        .ok()
        .and_then(|s| s.with(|conn| store::wb_entries_all(conn)).ok())
        .unwrap_or_default()
        .iter()
        .filter(|(pid, eid)| pid == provider_id && present.contains(eid.as_str()))
        .map(|(_, eid)| eid.clone())
        .collect()
}
