//! zaccount —— ZCode 账号槽位（与 WB 槽位同构：登录态文件级复制，永不读内容）。
//!
//! 登录态 = ~/.zcode/v2/credentials.json（oauth token 全部 `enc:v1:` 应用层加密，
//! AgentHub 不解密、不读取、只整文件复制）。流程：备份 → 复制槽位 → JSON 校验 →
//! 失败回滚。ZCode 为 CLI（每次启动重读凭据），无需进程管理。

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use serde::Serialize;

use crate::adapters::zcode;
use crate::fsutil::{self, Res};
use crate::store::{self, Store};

/// ZCode 桌面主程序（与 WB_EXE 同约定：安装路径写死，卸载/迁移后启动按钮降级提示）
const ZCODE_EXE: &str = r"D:\Application\zcode\ZCode.exe";

pub fn slots_dir() -> PathBuf {
    store::hub_dir().join("zslots")
}

fn slot_file(slot_id: &str) -> PathBuf {
    slots_dir().join(slot_id).join("credentials.json")
}

pub fn cred_file() -> PathBuf {
    zcode::dir().join("v2").join("credentials.json")
}

#[derive(Debug, Serialize)]
pub struct ZcodeSlot {
    pub id: String,
    pub label: String,
    pub saved_at: String,
    /// 槽位文件是否完好
    pub file_ok: bool,
    /// 槽位快照与当前登录态文件逐字节一致（即该账号当前生效）
    pub active: bool,
}

#[derive(Debug, Serialize)]
pub struct ZcodeAccountOverview {
    pub installed: bool,
    pub cred_found: bool,
    /// ZCode.exe 是否正在运行
    pub running: bool,
    pub slots: Vec<ZcodeSlot>,
}

/// ZCode.exe 进程探测（与 WB 同构：tasklist /FI）
/// 注意必须 CREATE_NO_WINDOW：GUI 程序裸拉控制台子进程时，Win11 默认终端
/// （Windows Terminal）会弹出一个可见窗口承载它 —— 即 Account 岛每 10s 轮询
/// 「桌面闪现透明框」的根因（2026-11-01 实锤）。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
fn zcode_running() -> bool {
    Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq ZCode.exe", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .to_lowercase()
                .contains("zcode.exe")
        })
        .unwrap_or(false)
}

/// 当前登录态与槽位快照是否逐字节一致（凭据仅几 KB，直接比较；不解析内容）
fn same_as_live(slot_id: &str) -> bool {
    let live = fsutil::read_opt(&cred_file());
    let snap = fsutil::read_opt(&slot_file(slot_id));
    match (live, snap) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

fn list_slots(conn: &rusqlite::Connection) -> Res<Vec<ZcodeSlot>> {
    let mut stmt = conn
        .prepare("SELECT id, label, IFNULL(saved_at,'') FROM zcode_slots ORDER BY saved_at DESC")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        let (id, label, saved_at) = r.map_err(|e| e.to_string())?;
        let file_ok = slot_file(&id).is_file();
        out.push(ZcodeSlot {
            active: file_ok && cred_file().is_file() && same_as_live(&id),
            file_ok,
            id,
            label,
            saved_at,
        });
    }
    Ok(out)
}

#[tauri::command]
pub async fn zcode_overview(state: State<'_, Store>) -> Result<ZcodeAccountOverview, String> {
    let slots = state.with(list_slots)?;
    Ok(ZcodeAccountOverview {
        installed: zcode::installed(),
        cred_found: cred_file().is_file(),
        running: zcode_running(),
        slots,
    })
}

/// 启动 ZCode 桌面（CLI 每次启动重读凭据，启动即使用当前登录态/已恢复槽位）
#[tauri::command]
pub async fn zcode_launch() -> Result<(), String> {
    // 插件门控需读 DB：借全局 Store 不可行（此命令无 State），用独立连接判定
    gate_zcode_plugin()?;
    if zcode_running() {
        return Err("ZCode 已在运行".into());
    }
    if !PathBuf::from(ZCODE_EXE).is_file() {
        return Err(format!("未找到 ZCode.exe（{ZCODE_EXE}），可手动打开"));
    }
    Command::new(ZCODE_EXE)
        .spawn()
        .map_err(|e| format!("启动 ZCode 失败: {e}"))?;
    Ok(())
}

use tauri::State;

/// ZCode 插件门控（无 State 的命令用：独立短连接读 plugin_state）
fn gate_zcode_plugin() -> Result<(), String> {
    let path = store::db_path();
    if !path.exists() {
        return Ok(()); // 首次运行尚未建库，不拦截
    }
    let conn = rusqlite::Connection::open(&path).map_err(|e| e.to_string())?;
    let enabled: Option<i64> = conn
        .query_row(
            "SELECT enabled FROM plugin_state WHERE agent = 'zcode'",
            [],
            |row| row.get(0),
        )
        .ok();
    match enabled {
        Some(0) => Err("ZCode 插件已停用（到 Plugin 岛开启后可用）".into()),
        _ => Ok(()),
    }
}

#[tauri::command]
pub async fn zcode_save_slot(state: State<'_, Store>, label: String) -> Result<ZcodeSlot, String> {
    crate::plugins::require_enabled(&state, "zcode")?;
    let src = cred_file();
    if !src.is_file() {
        return Err("未找到 ~/.zcode/v2/credentials.json（ZCode 未登录？）".into());
    }
    let slot_id = format!("zslot-{}", fsutil::now_stamp());
    let dst = slot_file(&slot_id);
    std::fs::create_dir_all(dst.parent().expect("slot dir")).map_err(|e| e.to_string())?;
    std::fs::copy(&src, &dst).map_err(|e| format!("复制凭据失败: {e}"))?;
    // JSON 可解析校验（只验结构合法性，不读取字段值）
    let bytes = fsutil::read_opt(&dst).ok_or("槽位文件写入后不可读")?;
    serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|e| format!("槽位 JSON 校验失败: {e}"))?;
    let saved_at = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    let slot = ZcodeSlot {
        active: true,
        file_ok: true,
        id: slot_id,
        label: label.clone(),
        saved_at: saved_at.clone(),
    };
    state.with(|conn| {
        conn.execute(
            "INSERT INTO zcode_slots(id, label, slot_dir, saved_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![slot.id, label, dst.parent().unwrap().to_string_lossy(), saved_at],
        )
        .map_err(|e| e.to_string())?;
        store::log_op(conn, "zcode.save_slot", &label, serde_json::json!({ "id": slot.id }));
        Ok(())
    })?;
    Ok(slot)
}

#[derive(Debug, Serialize)]
pub struct ZcodeSwitchReport {
    pub ok: bool,
    pub steps: Vec<String>,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn zcode_switch(
    state: State<'_, Store>,
    input: ZcodeSwitchInput,
) -> Result<ZcodeSwitchReport, String> {
    let _ = state; // 占位：与 WB 切换不同，无 DB 写操作；保留结构以便扩展
    crate::plugins::require_enabled(&state, "zcode")?;
    let mut steps = Vec::new();
    let slot_id = input.slot_id;
    let src = slot_file(&slot_id);
    if !src.is_file() {
        return Ok(ZcodeSwitchReport {
            ok: false,
            steps,
            error: Some(format!("槽位文件缺失: {}", src.display())),
        });
    }
    let target = cred_file();
    // 1) 备份当前登录态
    let backup = match fsutil::backup_file(&target, &store::hub_dir().join("backups")) {
        Ok(p) => {
            steps.push(format!("已备份当前凭据 → {}", p.display()));
            p
        }
        Err(e) => {
            return Ok(ZcodeSwitchReport {
                ok: false,
                steps,
                error: Some(format!("备份失败: {e}")),
            })
        }
    };
    // 2) 恢复槽位（原子写）
    let bytes = match fsutil::read_opt(&src) {
        Some(b) => b,
        None => {
            return Ok(ZcodeSwitchReport {
                ok: false,
                steps,
                error: Some("槽位文件不可读".into()),
            })
        }
    };
    if let Err(e) = fsutil::atomic_write(&target, &bytes) {
        let _ = std::fs::copy(&backup, &target);
        return Ok(ZcodeSwitchReport {
            ok: false,
            steps,
            error: Some(format!("恢复槽位失败（已回滚）: {e}")),
        });
    }
    steps.push("已写入槽位凭据".into());
    // 3) JSON 校验；失败回滚
    if serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
        let _ = std::fs::copy(&backup, &target);
        return Ok(ZcodeSwitchReport {
            ok: false,
            steps,
            error: Some("槽位 JSON 校验失败（已回滚）".into()),
        });
    }
    steps.push("JSON 校验通过".into());
    steps.push("下次启动 ZCode 即使用新账号（CLI 每次启动重读凭据）".into());
    Ok(ZcodeSwitchReport {
        ok: true,
        steps,
        error: None,
    })
}

/// IPC 单 input 结构体（多单词参数禁散参约定）
#[derive(Debug, serde::Deserialize)]
pub struct ZcodeSwitchInput {
    pub slot_id: String,
}

#[tauri::command]
pub async fn zcode_delete_slot(state: State<'_, Store>, slot: String) -> Result<(), String> {
    crate::plugins::require_enabled(&state, "zcode")?;
    state.with(|conn| {
        conn.execute("DELETE FROM zcode_slots WHERE id = ?1", rusqlite::params![slot])
            .map_err(|e| e.to_string())?;
        store::log_op(conn, "zcode.delete_slot", &slot, serde_json::json!({}));
        Ok(())
    })?;
    let dir = slots_dir().join(&slot);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("删除槽位文件失败: {e}"))?;
    }
    Ok(())
}

/// ZCode 槽位改名（与 WB 改名同构，只更新 DB label）
#[tauri::command]
pub async fn zcode_rename_slot(
    state: State<'_, Store>,
    input: crate::account::SlotRenameInput,
) -> Result<(), String> {
    crate::plugins::require_enabled(&state, "zcode")?;
    let label = input.label.trim();
    if label.is_empty() {
        return Err("槽位名称不能为空".into());
    }
    state.with(|conn| {
        conn.execute(
            "UPDATE zcode_slots SET label = ?2 WHERE id = ?1",
            rusqlite::params![input.slot_id, label],
        )
        .map_err(|e| e.to_string())?;
        store::log_op(conn, "zcode.rename_slot", label, serde_json::json!({ "id": input.slot_id }));
        Ok(())
    })
}
