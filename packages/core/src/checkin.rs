//! checkin —— WB 每日签到领积分（Account 岛「每日领取 100 积分」）。
//!
//! 原理（社区逆向实证，2026-11）：
//! - POST {auth.domain}/v2/billing/meter/checkin-activity-status 查询今日是否已签到
//! - POST {auth.domain}/v2/billing/meter/daily-checkin 执行签到（幂等：重复返回 code 10001）
//!   成功返回 credit（第 1-6 天 +100，第 7 天 +1000），请求头 Bearer <accessToken> + X-User-Id
//! - WB 5.6.2+ 登录态文件里 accessToken/refreshToken 为 AES-256-GCM 加密信封：
//!   atRestSecretKey 经 WorkBuddy.exe 自带原生绑定（ELECTRON_RUN_AS_NODE 模式）取回，
//!   AES key = SHA-256(atRestSecretKey)，信封自带 keyId 可校验，GCM 需带固定 AAD。
//!
//! 隐私红线（本模块特例，经用户确认）：为调签到接口必须在内存中解密 token；
//! token / atRestSecretKey 只存内存变量，永不落库、永不写盘、永不写日志、永不回传前端。
//! 登录态文件本体仍只做文件级复制（切换账号路径不受影响）。

use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::Mutex;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::State;

use crate::adapters::wb;
use crate::fsutil::{self, Res};
use crate::store::{self, Store};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const FALLBACK_DOMAIN: &str = "www.workbuddy.cn";
const STATUS_URL: &str = "/v2/billing/meter/checkin-activity-status";
const CLAIM_URL: &str = "/v2/billing/meter/daily-checkin";

// atRestSecretKey 进程内缓存（密钥与 WB 安装版本绑定，本会话内复用即可）
static AT_REST_KEY: Mutex<Option<String>> = Mutex::new(None);

fn keys_dir() -> std::path::PathBuf {
    store::hub_dir().join("keys")
}

fn auto_checkin_file() -> std::path::PathBuf {
    keys_dir().join("wb-auto-checkin")
}

pub fn auto_checkin_enabled() -> bool {
    fsutil::read_opt(&auto_checkin_file())
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim() != "0")
        .unwrap_or(true) // 缺省开启
}

// ─── token 信封解密（纯内存） ───

/// 用 WorkBuddy 自身的 Electron 原生绑定取 atRestSecretKey（不落盘）
fn at_rest_secret_key() -> Res<String> {
    if let Some(k) = AT_REST_KEY.lock().unwrap().clone() {
        return Ok(k);
    }
    let exe = crate::account::WB_EXE;
    if !std::path::Path::new(exe).is_file() {
        return Err(format!("未找到 WorkBuddy.exe（{exe}），无法解密登录态 token"));
    }
    // 临时脚本：向 WB 自己要它写加密文件用的密钥
    let js = std::env::temp_dir().join(format!("agenthub-getkey-{}.js", std::process::id()));
    fsutil::atomic_write(&js, br#"const m=process._linkedBinding('electron_browser_workbuddy_storage');process.stdout.write(m.loggerGet());"#)
        .map_err(|e| format!("写临时脚本失败: {e}"))?;
    let out = Command::new(exe)
        .arg(js.to_string_lossy().as_ref())
        .env("ELECTRON_RUN_AS_NODE", "1")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("调 WorkBuddy 运行时失败: {e}"));
    let _ = std::fs::remove_file(&js);
    let out = out?;
    if !out.status.success() {
        return Err(format!(
            "WorkBuddy 运行时返回 {}：{}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("解析密钥输出失败: {e}"))?;
    let key = v
        .get("atRestSecretKey")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("密钥输出缺少 atRestSecretKey")?
        .to_string();
    *AT_REST_KEY.lock().unwrap() = Some(key.clone());
    Ok(key)
}

/// 解密 WB 5.6.2+ 加密信封（AES-256-GCM + 固定 AAD）
fn decrypt_envelope(envelope_b64: &str, secret: &str) -> Res<String> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(envelope_b64)
        .map_err(|e| format!("envelope base64 解码失败: {e}"))?;
    let o: serde_json::Value = serde_json::from_slice(&raw).map_err(|e| format!("envelope 解析失败: {e}"))?;
    let suite = o.get("suite").and_then(|x| x.as_u64()).unwrap_or(1) as u32;
    let key_id = o
        .get("keyId")
        .and_then(|x| x.as_str())
        .ok_or("envelope 缺少 keyId")?;
    let nonce = base64::engine::general_purpose::STANDARD
        .decode(o.get("nonce").and_then(|x| x.as_str()).unwrap_or(""))
        .map_err(|e| format!("nonce 解码失败: {e}"))?;
    let mut ct = base64::engine::general_purpose::STANDARD
        .decode(o.get("ciphertext").and_then(|x| x.as_str()).unwrap_or(""))
        .map_err(|e| format!("ciphertext 解码失败: {e}"))?;
    let tag = base64::engine::general_purpose::STANDARD
        .decode(o.get("authTag").and_then(|x| x.as_str()).unwrap_or(""))
        .map_err(|e| format!("authTag 解码失败: {e}"))?;
    ct.extend_from_slice(&tag);

    let key = Sha256::digest(secret.as_bytes());
    // 注意：GCM 自带认证（authTag），无需再做 keyId 比对
    // （实测 5.5.6 客户端 keyId != sha256(key)[:16]，社区文章的校验公式已失效）
    // 固定 AAD：b"WB-AAD\0" + 0x01 + lp("WBEV1") + lp("sym-v1") + be32(suite) + lp(keyId) + 0x02 0x00 0x00
    let lp = |s: &str| -> Vec<u8> {
        let b = s.as_bytes();
        let mut v = (b.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(b);
        v
    };
    let mut aad: Vec<u8> = b"WB-AAD\0".to_vec();
    aad.push(0x01);
    aad.extend_from_slice(&lp("WBEV1"));
    aad.extend_from_slice(&lp("sym-v1"));
    aad.extend_from_slice(&suite.to_be_bytes());
    aad.extend_from_slice(&lp(key_id));
    aad.extend_from_slice(&[0x02, 0x00, 0x00]);

    let cipher = Aes256Gcm::new(&key);
    let pt = cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload { msg: &ct, aad: &aad },
        )
        .map_err(|_| "GCM 解密失败（InvalidTag）".to_string())?;
    String::from_utf8(pt).map_err(|e| format!("token 非 UTF-8: {e}"))
}

/// 登录态 JSON 里取 token（兼容 5.5 明文字符串与 5.6.2+ 加密信封）
fn extract_token(v: &serde_json::Value, secret: &str) -> Res<String> {
    let t = v
        .pointer("/auth/accessToken")
        .ok_or("登录态缺少 auth.accessToken")?;
    if let Some(s) = t.as_str() {
        return Ok(s.to_string());
    }
    let env = t
        .get("envelope")
        .and_then(|x| x.as_str())
        .ok_or("accessToken 信封格式异常")?;
    decrypt_envelope(env, secret)
}

// ─── 凭据汇总（当前登录态 + 全部槽位，按 uid 去重） ───

#[derive(Debug, Clone)]
pub struct WbCreds {
    pub uid: String,
    /// 仅存内存，永不序列化
    pub token: String,
    pub domain: String,
    pub source: String,
}

/// 汇总各账号凭据：当前登录态优先（token 最新），槽位补齐其余账号
fn resolve_creds(store: &Store) -> Res<Vec<WbCreds>> {
    let secret = at_rest_secret_key()?;
    let mut by_uid: HashMap<String, WbCreds> = HashMap::new();
    let mut push = |path: &std::path::Path, source: &str| -> Res<()> {
        let Some(bytes) = fsutil::read_opt(path) else {
            return Ok(());
        };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return Ok(());
        };
        let uid = v
            .pointer("/account/uid")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if uid.is_empty() || by_uid.contains_key(&uid) {
            return Ok(());
        }
        let token = extract_token(&v, &secret)?;
        let domain = v
            .pointer("/auth/domain")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(FALLBACK_DOMAIN)
            .to_string();
        by_uid.insert(
            uid.clone(),
            WbCreds {
                uid,
                token,
                domain,
                source: source.to_string(),
            },
        );
        Ok(())
    };

    // 1) 当前登录态
    if let Some(auth) = wb::auth_info() {
        push(&auth, "当前登录态")?;
    }
    // 2) 全部 WB 槽位快照（槽位文件与登录态同构）
    let slot_files: Vec<(String, String)> = store.with(|conn| {
        let mut stmt = conn
            .prepare("SELECT id, IFNULL(slot_dir,'') FROM wb_slots ORDER BY saved_at DESC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    })?;
    for (id, dir) in slot_files {
        if dir.is_empty() {
            continue;
        }
        push(&std::path::PathBuf::from(&dir), &format!("槽位 {id}"))?;
    }
    if by_uid.is_empty() {
        return Err("没有可用的 WB 登录态（当前登录 + 槽位均未找到）".into());
    }
    Ok(by_uid.into_values().collect())
}

// ─── 签到接口 ───

/// 展开 reqwest 错误原因链：顶层 "error sending request" 不含真实原因，
/// 底层才是 DNS / 连接拒绝 / TLS 等可定位信息
pub fn net_err(e: reqwest::Error) -> String {
    let mut msg = e.to_string();
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        msg.push_str(&format!(" ← {s}"));
        src = s.source();
    }
    msg
}

fn post_checkin(creds: &WbCreds, path: &str) -> Result<serde_json::Value, String> {
    let url = format!("https://{}{}", creds.domain, path);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        // 签到走固定国内域名，直连即达；显式禁用代理，避免 reqwest 默认读取
        // 会话级 http_proxy/https_proxy 环境变量（终端里启动时常见）导致发送失败
        .no_proxy()
        .build()
        .map_err(|e| e.to_string())?;
    let mut last_err = String::new();
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(800));
        }
        let resp = client
            .post(&url)
            .bearer_auth(&creds.token)
            .header("X-User-Id", &creds.uid)
            .header("Content-Type", "application/json")
            .body("{}")
            .send();
        match resp {
            Ok(resp) => {
                let status = resp.status();
                let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
                if !status.is_success() {
                    // 幂等语义：HTTP 4xx + code 10001 = 今日已签到
                    if body.get("code").and_then(|c| c.as_i64()) == Some(10001) {
                        return Ok(body);
                    }
                    // 5xx 瞬时故障：重试一次
                    if status.is_server_error() {
                        last_err = format!("接口返回 {status}");
                        continue;
                    }
                    return Err(format!("接口返回 {status}"));
                }
                return Ok(body);
            }
            Err(e) => last_err = format!("请求失败: {}", net_err(e)),
        }
    }
    Err(last_err)
}

/// 兼容多种响应形态：code 在顶层或 data 里，布尔字段驼峰/下划线两种
fn parse_bool(v: &serde_json::Value, keys: &[&str]) -> Option<bool> {
    for k in keys {
        if let Some(b) = v.get(k).and_then(|x| x.as_bool()) {
            return Some(b);
        }
        if let Some(b) = v.pointer(&format!("/data/{k}")).and_then(|x| x.as_bool()) {
            return Some(b);
        }
    }
    None
}

fn resp_code(v: &serde_json::Value) -> Option<i64> {
    v.get("code")
        .and_then(|c| c.as_i64())
        .or_else(|| v.pointer("/data/code").and_then(|c| c.as_i64()))
}

fn resp_msg(v: &serde_json::Value) -> String {
    v.get("msg")
        .or_else(|| v.get("message"))
        .or_else(|| v.pointer("/data/msg"))
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string()
}

/// 查询某账号今日是否已签到（true/false/None=未知）
pub fn checkin_status_of(creds: &WbCreds) -> Res<Option<bool>> {
    let body = post_checkin(creds, STATUS_URL)?;
    Ok(parse_bool(
        &body,
        &["today_checked_in", "todayCheckedIn", "checked_in", "checkedIn", "checked"],
    ))
}

/// 执行签到；Ok((是否本次新领取, 积分, 消息))
pub fn checkin_claim_of(creds: &WbCreds) -> Res<(bool, f64, String)> {
    let body = post_checkin(creds, CLAIM_URL)?;
    let code = resp_code(&body);
    match code {
        Some(10001) => Ok((false, 0.0, "今日已签到（幂等，未多发）".into())),
        None | Some(0) => {
            // 成功：{"code":0,...,"credit":100} 或 data.credit
            let credit = body
                .get("credit")
                .and_then(|c| c.as_f64())
                .or_else(|| body.pointer("/data/credit").and_then(|c| c.as_f64()))
                .unwrap_or(100.0);
            let mut msg = if resp_msg(&body).is_empty() {
                format!("领取成功 +{}", credit)
            } else {
                resp_msg(&body)
            };
            if code == Some(0) {
                msg = format!("领取成功 +{credit} 积分");
            }
            Ok((true, credit, msg))
        }
        Some(c) => Err(format!("接口错误 code={c}：{}", resp_msg(&body))),
    }
}

/// 本地签到记录 upsert（只记 uid/日期/积分，不含 token）
fn record_checkin(store: &Store, uid: &str, credit: f64) {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let _ = store.with(|conn| {
        conn.execute(
            "INSERT INTO wb_checkins(uid, last_date, last_credit, claimed_at) VALUES (?1,?2,?3,?4)
             ON CONFLICT(uid) DO UPDATE SET last_date=?2, last_credit=?3, claimed_at=?4",
            rusqlite::params![uid, today, credit, now],
        )
        .map_err(|e| e.to_string())?;
        store::log_op(
            conn,
            "wb.checkin",
            uid,
            serde_json::json!({ "credit": credit, "date": today }),
        );
        Ok(())
    });
}

// ─── 对外结果结构（不含任何 token 字段） ───

#[derive(Debug, Serialize)]
pub struct WbCheckinStatus {
    pub uid: String,
    /// 今日是否已领取（None = 查询失败/未知）
    pub today_claimed: Option<bool>,
    /// 本地记录的最近领取日期与积分
    pub last_date: String,
    pub last_credit: f64,
    /// 凭据来源（当前登录态 / 槽位 xxx）
    pub source: String,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WbCheckinResult {
    pub uid: String,
    pub ok: bool,
    /// claimed=本次领取 / already=今日已领 / failed=失败
    pub status: String,
    pub credit: f64,
    pub msg: String,
}

fn local_record(store: &Store, uid: &str) -> Option<(String, f64)> {
    store
        .with(|conn| {
            conn.query_row(
                "SELECT last_date, last_credit FROM wb_checkins WHERE uid = ?1",
                rusqlite::params![uid],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
            )
            .map_err(|e| e.to_string())
        })
        .ok()
}

#[tauri::command]
pub async fn wb_checkin_status() -> Result<Vec<WbCheckinStatus>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let store = Store::open().map_err(|e| format!("打开本地库失败: {e}"))?;
        let creds_list = resolve_creds(&store)?;
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let mut out = Vec::new();
        for c in creds_list {
            if let Some((d, cr)) = local_record(&store, &c.uid) {
                if d == today {
                    out.push(WbCheckinStatus {
                        uid: c.uid,
                        today_claimed: Some(true),
                        last_date: d,
                        last_credit: cr,
                        source: c.source,
                        error: None,
                    });
                    continue;
                }
            }
            match checkin_status_of(&c) {
                Ok(Some(true)) => {
                    // 网络确认已领（可能用户在 WB 客户端里点的）——落本地记录
                    let credit = local_record(&store, &c.uid).map(|x| x.1).unwrap_or(0.0);
                    record_checkin(&store, &c.uid, credit);
                    out.push(WbCheckinStatus {
                        uid: c.uid,
                        today_claimed: Some(true),
                        last_date: today.clone(),
                        last_credit: credit,
                        source: c.source,
                        error: None,
                    });
                }
                Ok(v) => out.push(WbCheckinStatus {
                    uid: c.uid,
                    today_claimed: v,
                    last_date: String::new(),
                    last_credit: 0.0,
                    source: c.source,
                    error: None,
                }),
                Err(e) => out.push(WbCheckinStatus {
                    uid: c.uid,
                    today_claimed: None,
                    last_date: String::new(),
                    last_credit: 0.0,
                    source: c.source,
                    error: Some(e),
                }),
            }
        }
        Ok(out)
    })
    .await
    .map_err(|e| format!("任务调度失败: {e}"))?
}

#[derive(Debug, serde::Deserialize)]
pub struct WbCheckinInput {
    pub uid: String,
}

#[tauri::command]
pub async fn wb_checkin_claim(input: WbCheckinInput) -> Result<WbCheckinResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let store = Store::open().map_err(|e| format!("打开本地库失败: {e}"))?;
        if !crate::plugins::enabled_ids(&store)
            .map(|ids| ids.iter().any(|a| a == "workbuddy"))
            .unwrap_or(false)
        {
            return Err("WorkBuddy 插件已停用".into());
        }
        let creds = resolve_creds(&store)?
            .into_iter()
            .find(|c| c.uid == input.uid)
            .ok_or_else(|| format!("未找到 uid {} 的登录态", input.uid))?;
        let (claimed, credit, msg) = match checkin_claim_of(&creds) {
            Ok(r) => r,
            Err(e) => {
                return Ok(WbCheckinResult {
                    uid: creds.uid,
                    ok: false,
                    status: "failed".into(),
                    credit: 0.0,
                    msg: e,
                });
            }
        };
        if claimed {
            record_checkin(&store, &creds.uid, credit);
            // 已设置过余额的账户：领取直接入账
            let _ = store.with(|conn| {
                conn.execute(
                    "UPDATE wb_credit_balance SET balance = balance + ?2 WHERE uid = ?1",
                    rusqlite::params![creds.uid, credit],
                )
                .map_err(|e| e.to_string())
            });
        }
        Ok(WbCheckinResult {
            uid: creds.uid,
            ok: true,
            status: if claimed { "claimed" } else { "already" }.into(),
            credit,
            msg,
        })
    })
    .await
    .map_err(|e| format!("任务调度失败: {e}"))?
}

// ─── 启动自动领取 + 开关 ───

#[tauri::command]
pub fn wb_checkin_auto_get() -> bool {
    auto_checkin_enabled()
}

#[derive(Debug, serde::Deserialize)]
pub struct WbCheckinAutoInput {
    pub enabled: bool,
}

#[tauri::command]
pub fn wb_checkin_auto_set(state: State<'_, Store>, input: WbCheckinAutoInput) -> Result<(), String> {
    let _ = &state;
    std::fs::create_dir_all(keys_dir()).map_err(|e| e.to_string())?;
    fsutil::atomic_write(&auto_checkin_file(), if input.enabled { b"1" } else { b"0" })
        .map_err(|e| format!("写入开关失败: {e}"))
}

/// 启动自动领取：遍历全部账号逐个签到（接口幂等，重复调用无害）
pub fn auto_checkin_all(store: &Store) -> Res<Vec<WbCheckinResult>> {
    if !auto_checkin_enabled() {
        return Ok(Vec::new());
    }
    if crate::plugins::enabled_ids(store)
        .map(|ids| !ids.iter().any(|a| a == "workbuddy"))
        .unwrap_or(true)
    {
        return Ok(Vec::new());
    }
    let creds_list = resolve_creds(store)?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut out = Vec::new();
    for c in creds_list {
        if local_record(store, &c.uid).map(|(d, _)| d == today).unwrap_or(false) {
            continue;
        }
        let r = match checkin_claim_of(&c) {
            Ok((claimed, credit, msg)) => {
                if claimed {
                    record_checkin(store, &c.uid, credit);
                    let _ = store.with(|conn| {
                        conn.execute(
                            "UPDATE wb_credit_balance SET balance = balance + ?2 WHERE uid = ?1",
                            rusqlite::params![c.uid, credit],
                        )
                        .map_err(|e| e.to_string())
                    });
                }
                WbCheckinResult {
                    uid: c.uid,
                    ok: true,
                    status: if claimed { "claimed" } else { "already" }.into(),
                    credit,
                    msg,
                }
            }
            Err(e) => WbCheckinResult {
                uid: c.uid,
                ok: false,
                status: "failed".into(),
                credit: 0.0,
                msg: e,
            },
        };
        out.push(r);
    }
    Ok(out)
}
