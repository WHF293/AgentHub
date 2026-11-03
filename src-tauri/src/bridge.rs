//! bridge —— 组装层跨域编排桥。
//!
//! 壳依赖全部插件包，因此「跨插件联动」的编排放这里（插件之间零依赖）：
//! - providers_overview：ZCode 直读 + WB 插件标注写穿状态
//! - provider_save：ZCode 直写 + WB 写穿（openai-compatible 且勾选了模型才落盘）
//! - provider_delete：ZCode 删除 + WB 托管条目摘除
//! Key 只在 Rust 层流转（resolve 回读 → 写穿），不入 DB / 不进日志 / 不回前端。

use agenthub_core::store::Store;
use agenthub_plugin_workbuddy::provider_wb;
use agenthub_plugin_zcode::provider_zcode;
use serde::Serialize;
use tauri::State;

#[derive(Serialize)]
pub struct ProvidersOverview {
    pub providers: Vec<provider_zcode::ZcodeProviderLive>,
    pub selected_keys: serde_json::Map<String, serde_json::Value>,
}

#[tauri::command]
pub fn providers_overview(state: State<'_, Store>) -> Result<ProvidersOverview, String> {
    // ZCode 插件停用：Model 岛主存储在 ZCode v2/config.json，停用即空态（前端提示去 Plugin 岛开启）
    if !agenthub_core::plugins::active_ids(&state)?.iter().any(|id| id == "zcode") {
        return Ok(ProvidersOverview {
            providers: Vec::new(),
            selected_keys: Default::default(),
        });
    }
    let mut providers = provider_zcode::read_zcode_providers()?;
    for p in &mut providers {
        p.wb_models = provider_wb::synced_ids(&p.id);
    }
    let selected_keys = provider_zcode::read_selected_keys()?;
    Ok(ProvidersOverview {
        providers,
        selected_keys,
    })
}

/// 多单词参数一律收进单个 input 结构体（serde 语义自控，规避 IPC 参数大小写约定）
#[derive(Debug, serde::Deserialize)]
pub struct SaveProviderInput {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub models: Option<Vec<String>>,
    /// 分发到 ZCode（勾选写入 v2/config.json；不勾 = 移除，模型岛列表不再显示）
    #[serde(default)]
    pub dispatch_zcode: bool,
    /// 分发到 WorkBuddy（勾选且 openai-compatible 才写穿；不勾 = 摘除托管条目）
    #[serde(default)]
    pub dispatch_wb: bool,
}

/// 保存 provider（分发驱动）：勾 ZCode 写 v2/config.json（不勾移除）；勾 WB 且
/// openai-compatible 写穿 models.json（不勾摘除托管条目）。至少勾一个。
/// Key 只在 Rust 层流转：显式传入优先，否则从 ZCode 目标文件回读（删除前回读，窗口保证）。
#[tauri::command]
pub fn provider_save(state: State<'_, Store>, input: SaveProviderInput) -> Result<String, String> {
    if !provider_zcode::KINDS.contains(&input.kind.as_str()) {
        return Err(format!("未知 kind：{}", input.kind));
    }
    if !input.dispatch_zcode && !input.dispatch_wb {
        return Err("至少勾选一个分发工具（WorkBuddy / ZCode）".into());
    }
    agenthub_core::plugins::require_enabled(&state, "zcode")?;
    let wb_enabled = agenthub_core::plugins::active_ids(&state)?
        .iter()
        .any(|id| id == "workbuddy");
    let api_key_snapshot = input.api_key.clone();
    let mut notes: Vec<String> = Vec::new();

    // 1) ZCode：勾选写入（先写后删，保住 WB 写穿的 key 回读窗口）
    if input.dispatch_zcode {
        provider_zcode::write_zcode_provider(&provider_zcode::ProviderWrite {
            id: input.id.clone(),
            name: input.name,
            kind: input.kind.clone(),
            base_url: input.base_url.clone(),
            api_key: input.api_key,
            reset_models_with: input.models.clone(),
        })?;
        notes.push("已写入 ZCode".into());
    }

    // 2) WB 分发（key：显式传入优先，否则从 config 回读 —— 此时 config 尚未删除）
    if input.dispatch_wb {
        if input.kind != "openai-compatible" {
            notes.push("WB 侧仅支持 OpenAI 兼容接入点，未写穿".into());
        } else if !wb_enabled {
            notes.push("WB 插件未启用，未写穿 models.json".into());
        } else {
            let key = provider_zcode::resolve_api_key(&input.id, api_key_snapshot.as_deref()).ok();
            let models = input.models.clone().unwrap_or_default();
            let rep = provider_wb::sync_wb_provider(&input.id, &input.base_url, key.as_deref(), &models)?;
            notes.push(format!(
                "WB 写 {} 清 {} 跳过 {}（models.json 热加载即生效）",
                rep.written.len(),
                rep.removed.len(),
                rep.skipped.len()
            ));
        }
    } else if wb_enabled {
        let n = provider_wb::remove_wb_provider(&input.id).unwrap_or(0);
        if n > 0 {
            notes.push(format!("已从 WB 摘除 {n} 条托管条目"));
        }
    }

    // 3) 未勾 ZCode：移出 config（模型岛列表不再显示；WB 侧条目独立可用）
    if !input.dispatch_zcode {
        provider_zcode::delete_zcode_provider_if_exists(&input.id)?;
        notes.push("未分发到 ZCode：已从 v2/config.json 移除（模型岛列表不再显示）".into());
    }
    Ok(notes.join(" · "))
}
#[tauri::command]
pub fn provider_delete(state: State<'_, Store>, id: String) -> Result<String, String> {
    agenthub_core::plugins::require_enabled(&state, "zcode")?;
    provider_zcode::delete_zcode_provider(&id)?;
    // WB 侧清理仅在 WB 插件启用时执行
    let wb_enabled = agenthub_core::plugins::active_ids(&state)?
        .iter()
        .any(|x| x == "workbuddy");
    let n = if wb_enabled {
        provider_wb::remove_wb_provider(&id).unwrap_or(0)
    } else {
        0
    };
    Ok(if n > 0 {
        format!("已从 ZCode 删除 · WB 侧同步清理 {n} 条")
    } else {
        "已从 ZCode 删除".into()
    })
}
