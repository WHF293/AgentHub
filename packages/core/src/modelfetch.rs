//! modelfetch —— M4 在线拉取模型列表。
//! 完全复刻 cc-switch `model_fetch.rs` 的 `/v1/models` 候选链策略（方案 §2.5）：
//! models_url 覆写 → {base}/v1/models → /v{N} 版本段特判 → 兼容后缀剥离 →
//! anthropic 原生（x-api-key）优先。错误信息不携带 API Key。

use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize, Clone)]
pub struct FetchTrace {
    pub url: String,
    pub status: Option<u16>,
    pub ms: u128,
    pub ok: bool,
    pub note: String,
}

#[derive(Debug, Serialize)]
pub struct FetchResult {
    pub models: Vec<String>,
    pub traces: Vec<FetchTrace>,
    /// 全部候选失败时是否来自内置推荐列表（部分网关不提供 /v1/models）
    pub suggested: bool,
}

const TIMEOUT: Duration = Duration::from_secs(20);

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .expect("reqwest client")
}

/// 候选 URL 链：(url, 是否 anthropic 原生端点)
pub fn candidate_urls(kind: &str, base_url: &str, models_url: Option<&str>) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    let base = base_url.trim_end_matches('/');
    if let Some(mu) = models_url.map(str::trim).filter(|s| !s.is_empty()) {
        out.push((mu.to_string(), kind == "anthropic"));
        return out; // 精确覆写优先，不再尝试其他
    }
    let native = kind == "anthropic";
    // /v{N} 版本段特判（智谱 /v4、mimo /v1 等）：base 已是 API 根，先原样拼 /models，
    // 再兜底 /v1/models；否则默认先 {base}/v1/models
    let v_ending = base.rfind("/v").and_then(|pos| {
        let tail = &base[pos + 2..];
        (!tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit())).then_some(pos)
    });
    match v_ending {
        Some(pos) => {
            out.push((format!("{base}/models"), native));
            out.push((format!("{base}/v1/models"), native));
            out.push((format!("{}/v1/models", &base[..pos]), false));
        }
        None => out.push((format!("{base}/v1/models"), native)),
    }
    // 已知兼容后缀剥离（阿里百炼 /apps/anthropic → /compatible-mode/v1 特例见下）
    for suffix in ["/api/anthropic", "/apps/anthropic", "/anthropic"] {
        if let Some(stripped) = base.strip_suffix(suffix) {
            out.push((format!("{stripped}/v1/models"), false));
            out.push((format!("{stripped}/models"), false));
        }
    }
    out
}

fn push_trace(traces: &mut Vec<FetchTrace>, url: String, status: Option<u16>, t0: Instant, ok: bool, note: String) {
    traces.push(FetchTrace {
        url,
        status,
        ms: t0.elapsed().as_millis(),
        ok,
        note,
    });
}

fn try_fetch(
    client: &reqwest::blocking::Client,
    url: &str,
    anthropic_native: bool,
    api_key: &str,
    traces: &mut Vec<FetchTrace>,
) -> Option<Vec<String>> {
    let t0 = Instant::now();
    let mut req = client.get(url);
    if anthropic_native {
        req = req
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01");
    } else {
        req = req.header("Authorization", format!("Bearer {api_key}"));
    }
    let resp = match req.send() {
        Ok(r) => r,
        Err(e) => {
            // reqwest 错误只含 URL（key 在 header，不入错误信息）
            push_trace(traces, url.to_string(), None, t0, false, format!("请求失败: {e}"));
            return None;
        }
    };
    let status = resp.status();
    if !status.is_success() {
        push_trace(
            traces,
            url.to_string(),
            Some(status.as_u16()),
            t0,
            false,
            format!("HTTP {}", status.as_u16()),
        );
        return None;
    }
    let body: Value = match resp.json() {
        Ok(v) => v,
        Err(e) => {
            push_trace(traces, url.to_string(), Some(status.as_u16()), t0, false, format!("JSON 解析失败: {e}"));
            return None;
        }
    };
    let ids: Vec<String> = body
        .get("data")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        push_trace(traces, url.to_string(), Some(status.as_u16()), t0, false, "响应无 data[].id".into());
        return None;
    }
    let mut ids = ids;
    ids.sort();
    ids.dedup();
    let n = ids.len();
    push_trace(traces, url.to_string(), Some(status.as_u16()), t0, true, format!("data[].id × {n} · 已去重排序"));
    Some(ids)
}

/// 按候选链拉取；全部失败时对已知家族回退内置推荐列表（UI 可勾选、可再编辑）
pub fn fetch_models(kind: &str, base_url: &str, api_key: &str, models_url: Option<&str>) -> FetchResult {
    let client = client();
    let mut traces = Vec::new();
    for (url, native) in candidate_urls(kind, base_url, models_url) {
        if let Some(ids) = try_fetch(&client, &url, native, api_key, &mut traces) {
            return FetchResult {
                models: ids,
                traces,
                suggested: false,
            };
        }
    }
    // 已知家族兜底：GLM 系网关（zcode-plan 等）不暴露 /v1/models
    let base_lower = base_url.to_lowercase();
    if kind == "anthropic"
        && (base_lower.contains("bigmodel") || base_lower.contains("z.ai") || base_lower.contains("zcode"))
    {
        traces.push(FetchTrace {
            url: base_url.to_string(),
            status: None,
            ms: 0,
            ok: true,
            note: "网关不提供 /v1/models，已返回 GLM 内置推荐列表（可手动编辑）".into(),
        });
        return FetchResult {
            models: vec!["GLM-5.3".into(), "GLM-5.3-Flash".into()],
            traces,
            suggested: true,
        };
    }
    FetchResult {
        models: vec![],
        traces,
        suggested: false,
    }
}
