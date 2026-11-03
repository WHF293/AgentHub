//! fsutil —— 软链三级策略（symlink → junction → copy）、原子写、备份。
//!
//! Windows 权限现实：目录 symlink 需要开发者模式/管理员；junction 免特权且
//! skills 恰好都是目录；copy 兜底。所有写目标文件前先备份，见方案 §7。
// atomic_write / backup_file 等为 M3+ 同步流预留
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// 统一错误：M1 阶段以可读字符串落 op_log，后续里程碑再细化类型
pub type Res<T> = Result<T, String>;

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 当前本地时间戳（YYYYMMDD_HHMMSS），用于备份目录命名
pub fn now_stamp() -> String {
    let now = chrono::Local::now();
    now.format("%Y%m%d_%H%M%S").to_string()
}

/// 原子写：先写同目录 tmp 文件，再 rename 覆盖（Windows 上 std::fs::rename 带 REPLACE_EXISTING）
pub fn atomic_write(path: &Path, data: &[u8]) -> Res<()> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("无父目录：{}", path.display()))?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".into()),
        std::process::id(),
        now_stamp()
    ));
    fs::write(&tmp, data).map_err(|e| format!("写临时文件失败 {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path)
        .map_err(|e| format!("原子替换失败 {}: {e}", path.display()))?;
    Ok(())
}

/// 读文件（不存在返回 None）
pub fn read_opt(path: &Path) -> Option<Vec<u8>> {
    fs::read(path).ok()
}

/// 备份单文件到 backups_dir，返回备份路径。保留原文件。
pub fn backup_file(path: &Path, backups_dir: &Path) -> Res<PathBuf> {
    if !path.exists() {
        return Err(format!("备份目标不存在：{}", path.display()));
    }
    let stamp = now_stamp();
    let dest = backups_dir.join(format!("{stamp}_{}", path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into())));
    fs::create_dir_all(backups_dir).map_err(|e| e.to_string())?;
    fs::copy(path, &dest).map_err(|e| format!("备份失败 {}: {e}", path.display()))?;
    Ok(dest)
}

/// 备份整个目录（rename 优先，跨卷时降级 copy+删除原目录）到 backups_dir，返回备份路径。
/// 用于把「实目录」让位给「软链」时的安全腾挪。
pub fn move_dir_to_backup(dir: &Path, backups_dir: &Path) -> Res<PathBuf> {
    if !dir.is_dir() {
        return Err(format!("备份目标不是目录：{}", dir.display()));
    }
    let stamp = now_stamp();
    let dest = backups_dir.join(format!(
        "{stamp}_{}",
        dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "dir".into())
    ));
    fs::create_dir_all(backups_dir).map_err(|e| e.to_string())?;
    match fs::rename(dir, &dest) {
        Ok(()) => Ok(dest),
        Err(_) => {
            // 跨卷或被占用：copy 后删除原目录
            copy_dir_recursive(dir, &dest)?;
            fs::remove_dir_all(dir).map_err(|e| format!("备份后清理原目录失败 {}: {e}", dir.display()))?;
            Ok(dest)
        }
    }
}

/// 递归复制目录
pub fn copy_dir_recursive(src: &Path, dest: &Path) -> Res<()> {
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| format!("读目录失败 {}: {e}", src.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        let target = dest.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), &target)
                .map_err(|e| format!("复制文件失败 {} → {}: {e}", entry.path().display(), target.display()))?;
        }
        // symlink 等特殊文件跳过（skill 目录内不应出现）
    }
    Ok(())
}

/// 链接策略三级降级
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkMode {
    /// 目录符号链接（需开发者模式/管理员）
    Symlink,
    /// NTFS junction（免特权，仅目录）
    Junction,
    /// 复制 + diff 回写兜底
    Copy,
}

impl LinkMode {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkMode::Symlink => "symlink",
            LinkMode::Junction => "junction",
            LinkMode::Copy => "copy",
        }
    }
}

/// 是否为软链（symlink 或 junction，即 reparse point）
pub fn is_link(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    match fs::symlink_metadata(path) {
        Ok(md) => md.file_type().is_symlink() || (md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0,
        Err(_) => false,
    }
}

/// 读软链目标（symlink 与 junction 都支持），去掉 \\?\ 前缀
pub fn link_target(path: &Path) -> Option<PathBuf> {
    fs::read_link(path).ok().map(|p| {
        let s = p.to_string_lossy();
        let stripped = s.strip_prefix(r"\\?\").unwrap_or(&s);
        PathBuf::from(stripped)
    })
}

/// 探测可用链接策略：在 app 目录建一个临时 symlink，成功则 Symlink，否则 Junction。
/// junction 在 NTFS 上始终可用；若连 mklink /J 都失败（非 NTFS 卷）返回 Copy。
pub fn detect_link_mode(probe_dir: &Path) -> LinkMode {
    let probe = probe_dir.join(format!(".link-probe-{}", std::process::id()));
    let target = probe_dir;
    let _ = fs::remove_file(&probe);
    let _ = fs::remove_dir(&probe);
    #[cfg(target_os = "windows")]
    {
        if std::os::windows::fs::symlink_dir(target, &probe).is_ok() {
            let _ = fs::remove_dir(&probe);
            return LinkMode::Symlink;
        }
        if make_junction(&probe, target).is_ok() {
            let _ = fs::remove_dir(&probe);
            return LinkMode::Junction;
        }
    }
    LinkMode::Copy
}

fn make_junction(link: &Path, target: &Path) -> Res<()> {
    Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("mklink 执行失败: {e}"))
        .and_then(|out| {
            if out.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "mklink /J 失败: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        })
}

/// 在 link_path 创建指向 target_dir 的目录链接（link_path 必须不存在，由调用方先腾挪）
pub fn make_dir_link(link_path: &Path, target_dir: &Path, mode: LinkMode) -> Res<()> {
    if link_path.exists() || is_link(link_path) {
        return Err(format!("链接位已存在：{}", link_path.display()));
    }
    match mode {
        LinkMode::Symlink => std::os::windows::fs::symlink_dir(target_dir, link_path)
            .map_err(|e| format!("symlink 失败（可降级 junction）: {e}")),
        LinkMode::Junction => make_junction(link_path, target_dir),
        LinkMode::Copy => copy_dir_recursive(target_dir, link_path),
    }
}

/// 在 link_path 创建指向 target_file 的文件符号链接（link_path 必须不存在，由调用方先腾挪）。
/// 单文件仅 symlink 可行：junction 只支持目录，copy 会让内容分叉违背统一语义，故失败即报错不降级。
pub fn make_file_link(link_path: &Path, target_file: &Path) -> Res<()> {
    if link_path.exists() || is_link(link_path) {
        return Err(format!("链接位已存在：{}", link_path.display()));
    }
    std::os::windows::fs::symlink_file(target_file, link_path)
        .map_err(|e| format!("文件 symlink 失败（需开发者模式或管理员权限）: {e}"))
}

/// FNV-1a 64：零依赖稳定 hash（内容级幂等 id / 漂移指纹共用）
pub fn fnv1a(data: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// 解析 SKILL.md 的简单 frontmatter（name / description），容错：无 frontmatter 返回空
pub fn parse_skill_md(path: &Path) -> (Option<String>, Option<String>) {    let mut name = None;
    let mut description = None;
    if let Ok(content) = fs::read_to_string(path) {
        let mut lines = content.lines();
        if lines.next().map(str::trim) == Some("---") {
            for line in lines {
                let t = line.trim();
                if t == "---" {
                    break;
                }
                if let Some(v) = t.strip_prefix("name:") {
                    name = Some(v.trim().trim_matches('"').to_string());
                } else if let Some(v) = t.strip_prefix("description:") {
                    description = Some(v.trim().trim_matches('"').to_string());
                }
            }
        }
    }
    (name, description)
}
