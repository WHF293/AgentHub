//! account —— M5 WorkBuddy 多账号槽位（Rust 化旧 ~/.workbuddy-launcher 方案）。
//!
//! 流程（方案 §4）：选择槽位 → tasklist 检测 WorkBuddy.exe（未退出则由 UI 引导，
//! 绝不静默强杀）→ 备份当前 auth → 复制槽位文件 → JSON 校验 → 启动 WB → 失败回滚。
//! 安全红线：auth 文件只做文件级复制；仅解析 uid/uin 元数据，token 永不读取/展示/落库。

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::adapters::wb;
use crate::fsutil::{self, Res};
use crate::store::{self, Store};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub const WB_EXE: &str = r"D:\Application\workbuddy\WorkBuddy.exe";

pub fn slots_dir() -> PathBuf {
    store::hub_dir().join("slots")
}

fn slot_file(slot_id: &str) -> PathBuf {
    slots_dir().join(slot_id).join("workbuddy-desktop.info")
}

/// auth 文件里的账号元数据（只取 uid/uin/nickname，不碰 token）
#[derive(Debug, Serialize, Clone, Default)]
pub struct AccountMeta {
    pub uid: String,
    pub uin: String,
    pub nickname: String,
}

/// auth/槽位文件账号元数据解析（只取 uid/uin/nickname，token 永不读取 —— 安全红线）
pub(crate) fn parse_meta(path: &Path) -> AccountMeta {
    let mut meta = AccountMeta::default();
    let Some(bytes) = fsutil::read_opt(path) else {
        return meta;
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return meta;
    };
    let acct = v.get("account").unwrap_or(&v);
    meta.uid = acct
        .get("uid")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    meta.uin = match acct.get("uin") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    meta.nickname = acct
        .get("nickname")
        .or_else(|| acct.get("nickName"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    meta
}

#[derive(Debug, Serialize)]
pub struct WbSlot {
    pub id: String,
    pub label: String,
    pub uid: String,
    pub uin: String,
    pub saved_at: String,
    /// 槽位文件是否完好
    pub file_ok: bool,
}

#[derive(Debug, Serialize)]
pub struct WbOverview {
    pub auth_found: bool,
    pub current: AccountMeta,
    pub process_running: bool,
    pub wb_installed: bool,
    pub slots: Vec<WbSlot>,
}

pub fn process_running() -> bool {
    Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq WorkBuddy.exe", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|out| {
            let s = String::from_utf8_lossy(&out.stdout);
            s.to_lowercase().contains("workbuddy.exe")
        })
        .unwrap_or(false)
}

/// 强杀 WB（UI 侧必须先二次确认；工具不静默调用）
pub fn kill_process() -> Res<()> {
    let out = Command::new("taskkill")
        .args(["/IM", "WorkBuddy.exe", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("taskkill 执行失败: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "taskkill 失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn list_slots(conn: &rusqlite::Connection) -> Res<Vec<WbSlot>> {
    let mut stmt = conn
        .prepare("SELECT id, label, IFNULL(uid,''), IFNULL(uin,''), IFNULL(saved_at,'') FROM wb_slots ORDER BY saved_at DESC")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(WbSlot {
                id: row.get(0)?,
                label: row.get(1)?,
                uid: row.get(2)?,
                uin: row.get(3)?,
                saved_at: row.get(4)?,
                file_ok: false,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        let mut s = r.map_err(|e| e.to_string())?;
        s.file_ok = slot_file(&s.id).is_file();
        out.push(s);
    }
    Ok(out)
}

pub fn overview(store: &Store) -> Res<WbOverview> {
    let auth = wb::auth_info();
    let current = auth.as_ref().map(|p| parse_meta(p)).unwrap_or_default();
    let slots = store.with(list_slots)?;
    Ok(WbOverview {
        auth_found: auth.is_some(),
        current,
        process_running: process_running(),
        wb_installed: Path::new(WB_EXE).is_file(),
        slots,
    })
}

/// 保存当前登录为槽位（替代旧 Save-Login-A.bat）
pub fn save_slot(store: &Store, label: &str) -> Res<WbSlot> {
    let auth = wb::auth_info().ok_or("未找到 workbuddy-desktop.info，请先登录 WB")?;
    let meta = parse_meta(&auth);
    let id = format!("slot-{}", chrono::Local::now().format("%Y%m%d%H%M%S"));
    let dest = slot_file(&id);
    std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::copy(&auth, &dest).map_err(|e| format!("槽位快照失败: {e}"))?;
    let saved_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    store.with(|conn| {
        conn.execute(
            "INSERT INTO wb_slots(id, label, uid, uin, slot_dir, saved_at) VALUES (?1,?2,?3,?4,?5,?6)",
            rusqlite::params![id, label, meta.uid, meta.uin, dest.to_string_lossy(), saved_at],
        )
        .map_err(|e| e.to_string())?;
        store::log_op(
            conn,
            "wb.save_slot",
            label,
            serde_json::json!({ "id": id, "uid": meta.uid }),
        );
        Ok(())
    })?;
    Ok(WbSlot {
        id,
        label: label.to_string(),
        uid: meta.uid,
        uin: meta.uin,
        saved_at,
        file_ok: true,
    })
}

#[derive(Debug, Serialize, Default)]
pub struct SwitchReport {
    pub ok: bool,
    /// 各步骤留痕（UI 流程条）
    pub steps: Vec<String>,
    pub error: Option<String>,
    /// 会话复制结果（勾选「复制会话」时；复制失败不阻塞切号）
    #[serde(default)]
    pub sessions: Vec<crate::sessions::SessionCopyResult>,
}

/// 切换到指定槽位：检测进程 → 备份 → 恢复 → 校验 →（复制会话）→ 启动，失败全程回滚
pub fn switch_account(store: &Store, slot_id: &str, copy_ids: &[String]) -> Res<SwitchReport> {
    let mut report = SwitchReport::default();
    let run = |report: &mut SwitchReport| -> Res<()> {
        // 1) 进程检测：绝不静默强杀
        if process_running() {
            return Err(
                "WorkBuddy.exe 正在运行 —— 请先完全退出 WB（或由界面确认后强制结束）再切换"
                    .into(),
            );
        }
        report.steps.push("进程检测：WB 未运行 ✓".into());

        // 2) 三重备份之当前 auth → backups/auth-<ts>/
        let auth = wb::auth_info().ok_or("未找到当前登录态文件 workbuddy-desktop.info")?;
        let backup_dir = store::backups_dir().join(format!("auth-{}", fsutil::now_stamp()));
        std::fs::create_dir_all(&backup_dir).map_err(|e| e.to_string())?;
        let backup_file = backup_dir.join(auth.file_name().unwrap_or_default());
        std::fs::copy(&auth, &backup_file).map_err(|e| format!("备份当前登录态失败: {e}"))?;
        report
            .steps
            .push(format!("备份当前 auth → {}", backup_file.display()));

        // 3) 恢复槽位（先写临时文件再原子替换）
        let src = slot_file(slot_id);
        if !src.is_file() {
            return Err(format!("槽位文件缺失：{}", src.display()));
        }
        let tmp = auth.with_extension(format!(
            "info.tmp-{}",
            std::process::id()
        ));
        std::fs::copy(&src, &tmp).map_err(|e| format!("读取槽位失败: {e}"))?;
        std::fs::rename(&tmp, &auth).map_err(|e| format!("写入登录态失败: {e}"))?;
        report.steps.push("恢复槽位文件 ✓".into());

        // 4) JSON 校验 + uid 一致性
        let meta = parse_meta(&auth);
        let slot_meta = parse_meta(&src);
        if meta.uid.is_empty() {
            // 回滚
            let _ = std::fs::copy(&backup_file, &auth);
            return Err("恢复后的登录态 JSON 无 account.uid，已回滚".into());
        }
        if !slot_meta.uid.is_empty() && slot_meta.uid != meta.uid {
            // 理论上应相等；不一致说明复制损坏，回滚
            let _ = std::fs::copy(&backup_file, &auth);
            return Err("槽位与恢复结果 uid 不一致（疑似损坏），已回滚".into());
        }
        report
            .steps
            .push(format!("JSON 校验 ✓（uid {}…）", &meta.uid[..meta.uid.len().min(8)]));

        // 4.5) 复制会话到目标账号（可选；WB 关闭窗口内执行，失败不阻塞切号）
        if !copy_ids.is_empty() {
            let source_uid = parse_meta(&backup_file).uid;
            if source_uid == meta.uid {
                report
                    .steps
                    .push("复制会话：目标账号与当前账号相同，跳过".into());
            } else {
                let sess_backup = store::backups_dir().join(format!("session-{}", fsutil::now_stamp()));
                if let Err(e) = crate::sessions::backup_db(&sess_backup) {
                    report.steps.push(format!("复制会话：workbuddy.db 备份失败（{e}），已跳过复制"));
                } else {
                    let results =
                        crate::sessions::copy_sessions(store, copy_ids, &meta.uid, &sess_backup);
                    let ok = results.iter().filter(|r| r.status == "copied").count();
                    let skip = results.iter().filter(|r| r.status == "skipped").count();
                    let fail = results.iter().filter(|r| r.status == "failed").count();
                    report.sessions = results;
                    report.steps.push(format!(
                        "复制会话 ×{} ✓（成功 {ok} · 跳过 {skip} · 失败 {fail}）",
                        copy_ids.len()
                    ));
                }
            }
        }

        // 5) 启动 WB
        if Path::new(WB_EXE).is_file() {
            Command::new(WB_EXE)
                .spawn()
                .map_err(|e| format!("启动 WorkBuddy 失败: {e}"))?;
            report.steps.push("已启动 WorkBuddy ✓".into());
        } else {
            report
                .steps
                .push("未找到 WorkBuddy.exe，跳过自动启动（可手动打开）".into());
        }
        Ok(())
    };

    match run(&mut report) {
        Ok(()) => {
            report.ok = true;
        }
        Err(e) => {
            report.error = Some(e.clone());
            // 失败兜底回滚：若存在最近的 auth 备份则恢复
            if let Some(latest) = latest_auth_backup() {
                if let Some(auth) = wb::auth_info() {
                    let _ = std::fs::copy(&latest, &auth);
                    report.steps.push(format!("已回滚 ← {}", latest.display()));
                }
            }
        }
    }
    let _ = store.with(|conn| {
        store::log_op(
            conn,
            "wb.switch",
            slot_id,
            serde_json::json!({ "ok": report.ok, "error": report.error, "steps": report.steps }),
        );
        Ok(())
    });
    Ok(report)
}

fn latest_auth_backup() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(store::backups_dir())
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .map(|n| n.to_string_lossy().starts_with("auth-"))
                    .unwrap_or(false)
        })
        .collect();
    dirs.sort();
    let dir = dirs.pop()?;
    std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_file())
}

/// 导入旧 ~/.workbuddy-launcher 槽位（accounts.json + slots/<dir>/workbuddy-desktop.info）
pub fn import_legacy(store: &Store) -> Res<usize> {
    let legacy = crate::adapters::agent_home().join(".workbuddy-launcher");
    let accounts_json = legacy.join("accounts.json");
    let Some(bytes) = fsutil::read_opt(&accounts_json) else {
        return Ok(0);
    };
    let Ok(map) = serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&bytes) else {
        return Ok(0);
    };
    let mut count = 0;
    for (label, dir) in map {
        let Some(dir) = dir.as_str().map(String::from) else {
            continue;
        };
        let src = legacy.join("slots").join(&dir).join("workbuddy-desktop.info");
        if !src.is_file() {
            continue;
        }
        let meta = parse_meta(&src);
        // 去重：同 uid 槽位已存在则跳过
        let exists: i64 = store.with(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM wb_slots WHERE IFNULL(uid,'') = ?1 AND ?1 != ''",
                rusqlite::params![meta.uid],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())
        })?;
        if exists > 0 {
            continue;
        }
        let id = format!("legacy-{}", chrono::Local::now().format("%Y%m%d%H%M%S%3f"));
        let dest = slot_file(&id);
        std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::copy(&src, &dest).map_err(|e| e.to_string())?;
        let saved_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        store.with(|conn| {
            conn.execute(
                "INSERT INTO wb_slots(id, label, uid, uin, slot_dir, saved_at) VALUES (?1,?2,?3,?4,?5,?6)",
                rusqlite::params![id, label, meta.uid, meta.uin, dest.to_string_lossy(), saved_at],
            )
            .map_err(|e| e.to_string())?;
            store::log_op(conn, "wb.import_legacy", &label, serde_json::json!({ "id": id }));
            Ok(())
        })?;
        count += 1;
    }
    Ok(count)
}

/// 删除槽位
pub fn delete_slot(store: &Store, slot_id: &str) -> Res<()> {
    store.with(|conn| {
        conn.execute(
            "DELETE FROM wb_slots WHERE id = ?1",
            rusqlite::params![slot_id],
        )
        .map_err(|e| e.to_string())?;
        store::log_op(conn, "wb.delete_slot", slot_id, serde_json::json!({}));
        Ok(())
    })?;
    let _ = std::fs::remove_dir_all(slots_dir().join(slot_id));
    Ok(())
}

// ─── Tauri commands（M5） ───

use tauri::State;

#[tauri::command]
pub fn wb_overview(state: State<'_, Store>) -> Result<WbOverview, String> {
    overview(&state)
}

/// 槽位改名 input（WB / ZCode 共用）
#[derive(Debug, serde::Deserialize)]
pub struct SlotRenameInput {
    pub slot_id: String,
    pub label: String,
}

#[tauri::command]
pub fn wb_rename_slot(state: State<'_, Store>, input: SlotRenameInput) -> Result<(), String> {
    crate::plugins::require_enabled(&state, "workbuddy")?;
    let label = input.label.trim();
    if label.is_empty() {
        return Err("槽位名称不能为空".into());
    }
    state.with(|conn| {
        conn.execute(
            "UPDATE wb_slots SET label = ?2 WHERE id = ?1",
            rusqlite::params![input.slot_id, label],
        )
        .map_err(|e| e.to_string())?;
        store::log_op(conn, "wb.rename_slot", label, serde_json::json!({ "id": input.slot_id }));
        Ok(())
    })
}

#[tauri::command]
pub fn wb_save_slot(state: State<'_, Store>, label: String) -> Result<WbSlot, String> {
    crate::plugins::require_enabled(&state, "workbuddy")?;
    save_slot(&state, &label)
}

/// 切换账号（多单词参数收进 input 结构体，规避 IPC 大小写约定）
#[derive(Debug, serde::Deserialize)]
pub struct SwitchInput {
    pub slot_id: String,
    pub confirm_kill: bool,
    /// 勾选「复制会话」时随切号一起复制的源会话 id（空 = 不复制）
    #[serde(default)]
    pub copy_session_ids: Vec<String>,
}

#[tauri::command]
pub fn wb_switch(state: State<'_, Store>, input: SwitchInput) -> Result<SwitchReport, String> {
    crate::plugins::require_enabled(&state, "workbuddy")?;
    if process_running() {
        if !input.confirm_kill {
            let mut r = SwitchReport::default();
            r.error = Some(
                "WorkBuddy.exe 正在运行 —— 请手动完全退出后重试，或勾选「强制结束」再切换".into(),
            );
            return Ok(r);
        }
        kill_process()?;
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }
    switch_account(&state, &input.slot_id, &input.copy_session_ids)
}

#[tauri::command]
pub fn wb_import_legacy(state: State<'_, Store>) -> Result<usize, String> {
    crate::plugins::require_enabled(&state, "workbuddy")?;
    import_legacy(&state)
}

#[tauri::command]
pub fn wb_delete_slot(state: State<'_, Store>, slot: String) -> Result<(), String> {
    crate::plugins::require_enabled(&state, "workbuddy")?;
    delete_slot(&state, &slot)
}
