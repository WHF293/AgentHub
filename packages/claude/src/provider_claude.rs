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
    /// 可选模型清单（拉取列表后由 AgentHub 写入 settings.json 顶层 modelOptions，卡片切换器数据源）
    pub model_options: Vec<String>,
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
    let model_options = root
        .get("modelOptions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(ClaudeProviderLive {
        base_url: get_s("ANTHROPIC_BASE_URL"),
        has_api_key: !get_s("ANTHROPIC_AUTH_TOKEN").is_empty(),
        // 模型值剥掉 [1m] 后缀再回传（写入时自动补回），UI 显示与选中比对用干净名
        model: strip_1m(&get_s("ANTHROPIC_MODEL")),
        small_fast_model: strip_1m(&get_s("ANTHROPIC_SMALL_FAST_MODEL")),
        onboarding_done,
        configured: env.is_some(),
        model_options,
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
    /// 可选模型清单（拉取列表后保存；None = 不变）
    pub model_options: Option<Vec<String>>,
}

/// 给模型 ID 追加 [1m] 后缀：Claude Code 对不认识的 ID 默认按 200k 上下文，
/// 带 [1m] 则按 1M 处理（请求发出前自动剥掉后缀，厂商不可见）；已带后缀则不动。
fn with_1m(m: &str) -> String {
    let t = m.trim();
    if t.is_empty() || t.to_lowercase().ends_with("[1m]") {
        t.to_string()
    } else {
        format!("{t}[1m]")
    }
}

/// 剥掉 [1m] 后缀（回读展示 / 分档计算用干净名）
fn strip_1m(m: &str) -> String {
    let t = m.trim();
    if t.len() >= 4 && t[t.len() - 4..].eq_ignore_ascii_case("[1m]") {
        t[..t.len() - 4].trim_end().to_string()
    } else {
        t.to_string()
    }
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
    // 提前读既有 modelOptions（切模型掩码路径没带清单时，档位映射的数据源；避开 root 借用冲突）
    let existing_options: Vec<String> = root
        .get("modelOptions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
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
                // 模型值统一带 [1m] 后缀（1M 上下文；Claude Code 请求前自动剥掉）
                env.insert(k.into(), json!(with_1m(s.trim())));
            }
            Some(_) => {
                env.remove(k);
            }
            None => {}
        }
    };
    set_opt(env, "ANTHROPIC_MODEL", &w.model);
    set_opt(env, "ANTHROPIC_SMALL_FAST_MODEL", &w.small_fast_model);
    // 内置档位映射：把厂商模型按命名分档铺到 /model 选择器的四个内置别名上
    // （Opus=最重档 / Sonnet=中档 / Haiku=轻快档 / Fable=剩余档），显示名同步替换
    // （*_MODEL_NAME，旧版本忽略无害）。清单缺失或单模型时退化为全部跟随主模型。
    let models_list: Vec<String> = w
        .model_options
        .clone()
        .map(|opts| {
            opts.into_iter()
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>()
        })
        .or_else(|| Some(existing_options.clone()))
        .unwrap_or_default();
    let main_raw = env
        .get("ANTHROPIC_MODEL")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let small_raw = env
        .get("ANTHROPIC_SMALL_FAST_MODEL")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    // env 里存的是带 [1m] 的值，分档用干净名
    let main = strip_1m(&main_raw);
    let small = strip_1m(&small_raw);
    let fast_kw = ["flash", "lite", "mini", "speed", "fast", "turbo", "nano", "instant"];
    let heavy_kw = ["ultra", "max", "pro", "plus", "xl", "heavy"];
    let has_kw = |m: &str, kws: &[&str]| kws.iter().any(|k| m.to_lowercase().contains(k));
    // Haiku：轻快档（清单里第一个快关键字命中，退化为小模型 / 首个 / 主模型）
    let haiku = models_list
        .iter()
        .find(|m| has_kw(m, &fast_kw))
        .cloned()
        .or_else(|| (!small.is_empty()).then(|| small.clone()))
        .or_else(|| models_list.first().cloned())
        .unwrap_or_else(|| main.clone());
    // Opus：最重档（清单里最后一个重关键字命中，退化为末个 / 主模型）
    let opus = models_list
        .iter()
        .rev()
        .find(|m| has_kw(m, &heavy_kw))
        .cloned()
        .or_else(|| models_list.last().cloned())
        .unwrap_or_else(|| main.clone());
    // Sonnet：中档 = 清单里第一个非 Opus/Haiku 的模型；Fable：剩余档
    let sonnet = models_list
        .iter()
        .find(|m| *m != &opus && *m != &haiku)
        .cloned()
        .unwrap_or_else(|| opus.clone());
    let fable = models_list
        .iter()
        .find(|m| *m != &opus && *m != &sonnet && *m != &haiku)
        .cloned()
        .unwrap_or_else(|| sonnet.clone());
    let aliases = [
        ("ANTHROPIC_DEFAULT_OPUS_MODEL", opus),
        ("ANTHROPIC_DEFAULT_SONNET_MODEL", sonnet),
        ("ANTHROPIC_DEFAULT_FABLE_MODEL", fable),
        ("ANTHROPIC_DEFAULT_HAIKU_MODEL", haiku),
    ];
    let nothing_to_map = main.is_empty() && models_list.is_empty();
    for (key, val) in aliases {
        if nothing_to_map || val.is_empty() {
            env.remove(key);
            env.remove(&format!("{key}_NAME"));
        } else {
            // 档位值带 [1m]（1M 上下文），显示名保持干净模型名
            env.insert(key.into(), json!(with_1m(&val)));
            env.insert(format!("{key}_NAME").into(), json!(val));
        }
    }
    if let Some(opts) = &w.model_options {
        let list: Vec<String> = opts.iter().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
        if list.is_empty() {
            obj.remove("modelOptions");
        } else {
            obj.insert("modelOptions".into(), json!(list));
        }
    }

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
pub async fn claude_providers_overview() -> Result<ClaudeProviderLive, String> {
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
pub async fn claude_provider_fetch_models(
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
pub async fn claude_provider_save(
    state: tauri::State<'_, Store>,
    input: ClaudeSaveInput,
) -> Result<String, String> {
    agenthub_core::plugins::require_enabled(&state, "claude")?;
    write_claude_provider(&input)
}
