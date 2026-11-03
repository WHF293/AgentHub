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
}

/// 保存 provider：直写 ZCode v2/config.json + openai-compatible 写穿 WB models.json
/// （key 仅在显式传入时更新）
#[tauri::command]
pub fn provider_save(state: State<'_, Store>, input: SaveProviderInput) -> Result<String, String> {
    if !provider_zcode::KINDS.contains(&input.kind.as_str()) {
        return Err(format!("未知 kind：{}", input.kind));
    }
    // 插件门控：ZCode 是主存储必须启用；WB 写穿仅在 WB 插件启用时执行
    agenthub_core::plugins::require_enabled(&state, "zcode")?;
    let wb_enabled = agenthub_core::plugins::active_ids(&state)?
        .iter()
        .any(|id| id == "workbuddy");
    // WB 侧写穿要拿真 key：显式传入优先，否则从 ZCode 目标文件回读（不出 Rust 层）
    let wb_key = if wb_enabled {
        provider_zcode::resolve_api_key(&input.id, input.api_key.as_deref()).ok()
    } else {
        None
    };
    provider_zcode::write_zcode_provider(&provider_zcode::ProviderWrite {
        id: input.id.clone(),
        name: input.name,
        kind: input.kind.clone(),
        base_url: input.base_url.clone(),
        api_key: input.api_key,
        reset_models_with: input.models.clone(),
    })?;
    let Some(note) = provider_wb::on_provider_saved(
        &state,
        &input.id,
        &input.kind,
        &input.base_url,
        wb_key.as_deref(),
        input.models.as_deref(),
    ) else {
        return Ok("已写入 ZCode · WB 插件未启用，未写穿 models.json".into());
    };
    Ok(format!("已写入 ZCode · {}", note?))
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
