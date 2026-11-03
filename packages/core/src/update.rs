//! update —— 在线更新（NSIS 安装版，链路对齐 whf-stock-board 实证实现；仓库已公开，全匿名通道）：
//! 检测 GitHub Release 最新版：302 重定向主通道（免匿名 API 限流）→ 公开 API 兜底；
//! 下载安装包到 ~/.agenthub/updates/（reqwest 直连流式，真实进度经 update-download-progress 推送）；
//! 安装 = NSIS 静默参数 /S /D=<原目录> + start 重启（cmd 链 raw_arg 原样传整条命令）。

use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use serde::Serialize;
use tauri::Emitter;

use crate::fsutil::{self, Res};
use crate::store;

const REPO: &str = "WHF293/AgentManage";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const USER_AGENT: &str = "AgentHub";

#[derive(Debug, Serialize, Clone)]
pub struct UpdateCheckInfo {
    pub current: String,
    pub latest: String,
    pub has_update: bool,
    /// Release tag（gh 下载通道用）
    pub tag: String,
    pub asset_name: String,
    pub asset_url: String,
    pub release_url: String,
}

struct ReleaseAsset {
    tag: String,
    name: String,
    url: String,
}

fn update_proxy_file() -> PathBuf {
    store::hub_dir().join("update_proxy.txt")
}

/// 读更新代理地址（空文件 / 空白 → None = 直连）。参照 whf-stock-board 的
/// 「更新代理」设定：打包版 reqwest 跟随系统代理不稳定（Clash 后开不感知），
/// 检测与下载必须同走显式代理，否则检测通了下载拉不下来。
pub fn read_update_proxy() -> Option<String> {
    fsutil::read_opt(&update_proxy_file())
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn http_client() -> Res<reqwest::blocking::Client> {
    let mut b = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(20));
    if let Some(proxy) = read_update_proxy() {
        let p = reqwest::Proxy::all(&proxy)
            .map_err(|e| format!("代理地址无效（{proxy}）: {e}"))?;
        b = b.proxy(p);
    }
    b.build().map_err(|e| format!("HTTP 客户端构建失败: {e}"))
}

/// 资产名匹配：AgentHub_v*_x64-setup.exe（NSIS 安装包）
fn pick_asset_name(name: &str) -> bool {
    name.starts_with("AgentHub_v") && name.ends_with("_x64-setup.exe")
}

/// 通道 1：releases/latest 网页 302 重定向解析版本号（公开仓库可用，免匿名 API 限流）
fn fetch_via_redirect(http: &reqwest::blocking::Client) -> Res<ReleaseAsset> {
    let resp = http
        .get(&format!("https://github.com/{REPO}/releases/latest"))
        .send()
        .map_err(|e| format!("Releases 页面请求失败: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Releases 页面返回异常: {e}（私有仓库匿名不可见）"))?;
    let final_url = resp.url().to_string();
    let marker = "/tag/";
    let pos = final_url
        .find(marker)
        .ok_or_else(|| format!("重定向 URL 未含 tag: {final_url}"))?;
    let tag = final_url[pos + marker.len()..]
        .trim_end_matches('/')
        .to_string();
    let latest = tag.trim_start_matches('v').to_string();
    if latest.is_empty() || !latest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Err(format!("重定向 tag 异常: {tag}"));
    }
    // 产物名与 release.yml rename 规则一致：AgentHub_v{version}_x64-setup.exe
    let asset_name = format!("AgentHub_v{latest}_x64-setup.exe");
    Ok(ReleaseAsset {
        tag,
        url: format!("https://github.com/{REPO}/releases/download/v{latest}/{asset_name}"),
        name: asset_name,
    })
}

/// 通道 2：公开 Releases API
fn fetch_via_api(http: &reqwest::blocking::Client) -> Res<ReleaseAsset> {
    let body = http
        .get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|e| format!("GitHub API 请求失败: {e}"))?
        .error_for_status()
        .map_err(|e| format!("GitHub API 返回异常: {e}"))?
        .text()
        .map_err(|e| format!("GitHub API 读取失败: {e}"))?;
    parse_release_json(&body)
}

fn parse_release_json(body: &str) -> Res<ReleaseAsset> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("Release JSON 解析失败: {e}"))?;
    let tag = v
        .get("tag_name")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let assets = v.get("assets").and_then(|x| x.as_array());
    let Some(assets) = assets else {
        return Err("Release 无 assets".into());
    };
    for a in assets {
        let name = a.get("name").and_then(|x| x.as_str()).unwrap_or("");
        if pick_asset_name(name) {
            let url = a
                .get("browser_download_url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if !url.is_empty() {
                return Ok(ReleaseAsset { tag, name: name.to_string(), url });
            }
        }
    }
    Err("最新 Release 未包含 AgentHub_v*_x64-setup.exe 资产".into())
}

/// 版本比较：a < b → true（逐段数值比较，忽略 v 前缀）
fn version_lt(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<i64> {
        s.trim_start_matches('v')
            .split('.')
            .map(|p| p.parse::<i64>().unwrap_or(0))
            .collect()
    };
    let (va, vb) = (parse(a), parse(b));
    for i in 0..3 {
        let x = va.get(i).copied().unwrap_or(0);
        let y = vb.get(i).copied().unwrap_or(0);
        if x != y {
            return x < y;
        }
    }
    false
}

fn check_blocking(current: &str) -> Res<UpdateCheckInfo> {
    let http = http_client()?;
    // 302 主通道（免匿名 API 限流）→ 公开 API 兜底
    let asset = match fetch_via_redirect(&http) {
        Ok(a) => a,
        Err(r_err) => fetch_via_api(&http)
            .map_err(|api_err| format!("检测失败（302: {r_err}；api: {api_err}）"))?,
    };
    let latest = asset.tag.trim_start_matches('v').to_string();
    let has_update = version_lt(current, &latest);
    Ok(UpdateCheckInfo {
        current: current.to_string(),
        tag: asset.tag,
        latest,
        has_update,
        asset_name: asset.name,
        asset_url: asset.url,
        release_url: format!("https://github.com/{REPO}/releases/latest"),
    })
}

fn updates_dir() -> PathBuf {
    store::hub_dir().join("updates")
}

fn download_blocking(app: &tauri::AppHandle, url: &str, dest: &PathBuf) -> Res<()> {
    let http = http_client()?;
    let mut resp = http
        .get(url)
        .send()
        .map_err(|e| format!("下载请求失败: {e}"))?
        .error_for_status()
        .map_err(|e| format!("下载返回异常: {e}"))?;
    let total = resp.content_length().unwrap_or(0);
    let mut file = std::fs::File::create(dest).map_err(|e| format!("写盘失败: {e}"))?;
    let mut buf = [0u8; 65536];
    let mut received: u64 = 0;
    let mut last_emit = std::time::Instant::now();
    loop {
        let n = resp.read(&mut buf).map_err(|e| format!("下载中断: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("写盘失败: {e}"))?;
        received += n as u64;
        if last_emit.elapsed().as_millis() > 200 {
            last_emit = std::time::Instant::now();
            let _ = app.emit(
                "update-download-progress",
                serde_json::json!({ "received": received, "total": total }),
            );
        }
    }
    file.flush().ok();
    let _ = app.emit(
        "update-download-progress",
        serde_json::json!({ "received": received, "total": received }),
    );
    if received == 0 {
        return Err("下载内容为空".into());
    }
    Ok(())
}

/// 仓库已公开：browser_download_url 匿名可下，reqwest 直连即带真实逐字节进度
fn install_blocking(app: &tauri::AppHandle, asset_name: &str, url: &str) -> Res<()> {
    let dir = updates_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let setup = dir.join(asset_name);
    let _ = std::fs::remove_file(&setup);

    download_blocking(app, url, &setup)?;

    // NSIS 静默安装（复用 whf-stock-board 实证链路）：
    // ping -n 4 ≈ 等 3s 让当前应用退出；/S 静默；/D= 装回当前安装目录。
    // NSIS 规范：/D= 必须是最后一个参数且不加引号，含空格也原样取到行尾。
    // 必须用 raw_arg 原样传整条命令 —— 普通 arg 会转义出 \"，cmd 不认该转义。
    let current = std::env::current_exe().map_err(|e| format!("取当前 exe 路径失败: {e}"))?;
    let install_dir = current
        .parent()
        .ok_or_else(|| "取当前安装目录失败".to_string())?;
    let script = format!(
        "ping 127.0.0.1 -n 4 >nul & \"{}\" /S /D={} & start \"\" \"{}\"",
        setup.to_string_lossy().replace('/', "\\"),
        install_dir.to_string_lossy().replace('/', "\\"),
        current.to_string_lossy().replace('/', "\\"),
    );
    Command::new("cmd")
        .arg("/C")
        .raw_arg(&script)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("启动安装程序失败: {e}"))?;
    // 延迟 500ms 退出，避免把 cmd 链一并带走（安装器自行等 3s 再静默安装并重启新版）
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        handle.exit(0);
    });
    Ok(())
}

// ─── Tauri commands ───

/// 检查更新（302 重定向主通道 → 公开 API 兜底）
#[tauri::command]
pub async fn update_check(app: tauri::AppHandle) -> Result<UpdateCheckInfo, String> {
    let current = app.package_info().version.to_string();
    tauri::async_runtime::spawn_blocking(move || check_blocking(&current))
        .await
        .map_err(|e| format!("检测任务失败: {e}"))?
}

/// 更新安装参数（多单词参数收进 input 结构体，规避 IPC 大小写约定）
#[derive(Debug, serde::Deserialize)]
pub struct InstallInput {
    /// 保留字段：曾用于 gh 下载通道，现仅为前端契约兼容
    pub tag: String,
    pub asset_name: String,
    pub asset_url: String,
}

/// 下载 NSIS 安装包并静默安装回原目录后重启（进度经 update-download-progress 事件推送）
#[tauri::command]
pub async fn update_install(app: tauri::AppHandle, input: InstallInput) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        install_blocking(&app, &input.asset_name, &input.asset_url)
    })
    .await
    .map_err(|e| format!("更新任务失败: {e}"))?
}

/// 读更新代理地址（设置岛「更新代理」；空 = 直连）
#[tauri::command]
pub fn update_proxy_get() -> String {
    read_update_proxy().unwrap_or_default()
}

/// 设置更新代理地址（空串 = 清除恢复直连）；写入即生效（每次请求新建 client 时读取）
#[tauri::command]
pub fn update_proxy_set(url: String) -> Result<(), String> {
    let url = url.trim().to_string();
    // 非空时先试建代理对象，避免存进一个用不了的地址
    if !url.is_empty() {
        reqwest::Proxy::all(&url).map_err(|e| format!("代理地址无效: {e}"))?;
    }
    fsutil::atomic_write(&update_proxy_file(), url.as_bytes())
}
