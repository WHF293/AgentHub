//! mcp —— 统一 MCP 引擎：单一事实源（SQLite）→ 按 agent 注册表翻译写入。
//!
//! 每个启用插件的 agent 适配器（adapters::registry()，caps.mcp = true）提供：
//! 配置文件形态（路径 + 容器）、条目启用态判定、条目生成（含类型翻译）。
//! 引擎本身不含任何具体 agent 的格式知识 —— 新增 agent 适配器零改动接入。
//!
//! 纪律：写前备份 → 保留目标文件未知键 → 原子写 → serde_json 序列化保证合法。

use std::collections::{BTreeMap, HashMap};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tauri::State;

use crate::adapters::{self, McpEntry, McpLive};
use crate::fsutil::{self, Res};
use crate::plugins;
use crate::store::{self, Store};

// ─── 统一模型 ───

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct McpServer {
    pub name: String,
    /// stdio | streamableHttp（规范型）
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// 按 agent 的启用态（agent id → enabled；缺失键按 true 处理，与旧行为一致）
    #[serde(default)]
    pub enabled: HashMap<String, bool>,
}

impl McpServer {
    fn enabled_for(&self, agent: &str) -> bool {
        self.enabled.get(agent).copied().unwrap_or(true)
    }
}

// ─── DB 存取 ───

fn row_to_server(row: &rusqlite::Row) -> rusqlite::Result<McpServer> {
    let enabled_json: Option<String> = row.get(7)?;
    let enabled = match enabled_json {
        Some(s) => serde_json::from_str::<HashMap<String, bool>>(&s).unwrap_or_default(),
        // 旧数据未迁移：回退 enabled_wb / enabled_zcode 旧列（v1.8 起 legacy，仅读不写）
        None => HashMap::from([
            ("workbuddy".to_string(), row.get::<_, i64>(8)? != 0),
            ("zcode".to_string(), row.get::<_, i64>(9)? != 0),
        ]),
    };
    Ok(McpServer {
        name: row.get(0)?,
        kind: row
            .get::<_, Option<String>>(1)?
            .unwrap_or_else(|| "stdio".into()),
        command: row.get(2)?,
        args: row
            .get::<_, Option<String>>(3)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        url: row.get(4)?,
        env: row
            .get::<_, Option<String>>(5)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        headers: row
            .get::<_, Option<String>>(6)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        enabled,
    })
}

pub fn list_servers(conn: &Connection) -> Res<Vec<McpServer>> {
    let mut stmt = conn
        .prepare(
            "SELECT name, type, command, args_json, url, env_json, headers_json,
                    enabled_json, enabled_wb, enabled_zcode
             FROM mcp_servers ORDER BY name",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], row_to_server)
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

pub fn upsert_server(conn: &Connection, s: &McpServer) -> Res<()> {
    conn.execute(
        "INSERT INTO mcp_servers(name, type, command, args_json, url, env_json, headers_json, enabled_json, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8, datetime('now','localtime'))
         ON CONFLICT(name) DO UPDATE SET
           type=excluded.type, command=excluded.command, args_json=excluded.args_json,
           url=excluded.url, env_json=excluded.env_json, headers_json=excluded.headers_json,
           enabled_json=excluded.enabled_json, updated_at=excluded.updated_at",
        rusqlite::params![
            s.name,
            s.kind,
            s.command,
            serde_json::to_string(&s.args).map_err(|e| e.to_string())?,
            s.url,
            serde_json::to_string(&s.env).map_err(|e| e.to_string())?,
            serde_json::to_string(&s.headers).map_err(|e| e.to_string())?,
            serde_json::to_string(&s.enabled).map_err(|e| e.to_string())?,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn delete_server(conn: &Connection, name: &str) -> Res<()> {
    conn.execute("DELETE FROM mcp_servers WHERE name = ?1", rusqlite::params![name])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 单 agent 启用态切换：读行 → 改 map → 写回 enabled_json
fn toggle_enabled(conn: &Connection, name: &str, agent: &str, enabled: bool) -> Res<()> {
    let Some(mut s) = list_servers(conn)?
        .into_iter()
        .find(|s| s.name == name)
    else {
        return Err(format!("统一定义中无此 server：{name}"));
    };
    s.enabled.insert(agent.to_string(), enabled);
    conn.execute(
        "UPDATE mcp_servers SET enabled_json = ?2, updated_at = datetime('now','localtime') WHERE name = ?1",
        rusqlite::params![name, serde_json::to_string(&s.enabled).map_err(|e| e.to_string())?],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ─── 目标文件读写（保留未知键） ───

fn read_json(path: &std::path::Path) -> Res<Value> {
    match fsutil::read_opt(path) {
        Some(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("JSON 解析失败 {}: {e}", path.display())),
        None => Ok(json!({})),
    }
}

fn servers_map_of(value: &Value, container: &[&str]) -> Map<String, Value> {
    let mut cur = value;
    for key in container {
        cur = cur.get(key).unwrap_or(&Value::Null);
    }
    cur.as_object().cloned().unwrap_or_default()
}

// ─── 总览（DB 定义 × 各启用端实测） ───

/// 单 agent 维度的 server 运行态
#[derive(Debug, Serialize)]
pub struct McpAgentState {
    pub agent: String,
    pub display_name: String,
    /// 统一定义中该端是否启用
    pub enabled: bool,
    /// 与目标文件实测：ok / disabled / missing / drifted / unsupported
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct McpOverviewItem {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub command: Option<String>,
    pub url: Option<String>,
    /// 各启用插件端的实测（数组顺序 = 注册表顺序）
    pub agents: Vec<McpAgentState>,
}

/// 单 agent 维度的漂移：只存在于该端目标文件、未纳入统一定义的名字
#[derive(Debug, Serialize)]
pub struct McpAgentDrift {
    pub agent: String,
    pub display_name: String,
    pub only: Vec<String>,
}

#[derive(Debug, Serialize, Default)]
pub struct McpDrift {
    pub agents: Vec<McpAgentDrift>,
}

/// 已启用且具备 MCP 能力的适配器（注册表顺序）
fn enabled_mcp_adapters(store: &Store) -> Res<Vec<&'static dyn adapters::AgentAdapter>> {
    Ok(plugins::active_adapters(store)?
        .into_iter()
        .filter(|a| a.caps().mcp)
        .collect())
}

/// 总览：DB 定义 × 各启用插件端实测
pub fn overview(store: &Store) -> Res<(Vec<McpOverviewItem>, McpDrift)> {
    let adapters = enabled_mcp_adapters(store)?;
    let servers = store.with(list_servers)?;
    // 各端目标文件现状
    let mut live_maps: Vec<(&'static dyn adapters::AgentAdapter, Map<String, Value>)> = Vec::new();
    for a in adapters.iter().copied() {
        let cfg = a.mcp_config();
        live_maps.push((a, servers_map_of(&read_json(&cfg.path)?, cfg.container)));
    }

    let mut items = Vec::new();
    for s in &servers {
        let mut agent_states = Vec::new();
        for (a, live) in &live_maps {
            let enabled = s.enabled_for(a.id());
            let unsupported = !a.supports_kind(&s.kind);
            let status = if unsupported {
                "unsupported".to_string()
            } else {
                match live.get(&s.name).map(|e| a.mcp_entry_live(e)) {
                    None => "missing",
                    Some(McpLive::Present) if enabled => "ok",
                    Some(McpLive::Disabled) if !enabled => "ok",
                    _ => "drifted",
                }
                .to_string()
            };
            agent_states.push(McpAgentState {
                agent: a.id().to_string(),
                display_name: a.display_name().to_string(),
                enabled,
                status,
            });
        }
        items.push(McpOverviewItem {
            name: s.name.clone(),
            kind: s.kind.clone(),
            command: s.command.clone(),
            url: s.url.clone(),
            agents: agent_states,
        });
    }
    let mut drift = McpDrift::default();
    for (a, live) in &live_maps {
        let only: Vec<String> = live
            .keys()
            .filter(|n| !servers.iter().any(|s| &s.name == *n))
            .cloned()
            .collect();
        drift.agents.push(McpAgentDrift {
            agent: a.id().to_string(),
            display_name: a.display_name().to_string(),
            only,
        });
    }
    Ok((items, drift))
}

/// 收编：把各端目标文件里已有的 server 并入统一定义（首次接管 / 漂移回填）。
/// 字段冲突时按注册表顺序取先者（WorkBuddy 优先，格式最全）。
pub fn adopt_from_targets(store: &Store, overwrite: bool) -> Res<usize> {
    let adapters = enabled_mcp_adapters(store)?;
    let mut live_maps: Vec<(&'static dyn adapters::AgentAdapter, Map<String, Value>)> = Vec::new();
    for a in adapters.iter().copied() {
        let cfg = a.mcp_config();
        live_maps.push((a, servers_map_of(&read_json(&cfg.path)?, cfg.container)));
    }

    let mut names: Vec<String> = Vec::new();
    for (_, m) in &live_maps {
        names.extend(m.keys().cloned());
    }
    names.sort();
    names.dedup();

    let mut count = 0;
    store.with(|conn| {
        for name in &names {
            let existing = conn
                .query_row(
                    "SELECT COUNT(*) FROM mcp_servers WHERE name = ?1",
                    rusqlite::params![name],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|e| e.to_string())?;
            if existing > 0 && !overwrite {
                continue;
            }
            // 各字段按注册表顺序取第一个有值者
            let mut kind: Option<String> = None;
            let mut command: Option<String> = None;
            let mut args: Vec<String> = Vec::new();
            let mut url: Option<String> = None;
            let mut env: BTreeMap<String, String> = BTreeMap::new();
            let mut headers: BTreeMap<String, String> = BTreeMap::new();
            let mut enabled: HashMap<String, bool> = HashMap::new();
            for (a, m) in &live_maps {
                let Some(entry) = m.get(name) else {
                    // 该端没有此条目 → 视作未分发
                    enabled.insert(a.id().to_string(), false);
                    continue;
                };
                enabled.insert(a.id().to_string(), a.mcp_enabled_of(Some(entry)));
                if kind.is_none() {
                    kind = Some(
                        entry
                            .get("type")
                            .and_then(Value::as_str)
                            .map(|k| a.canonical_kind(k))
                            .unwrap_or_else(|| "stdio".into()),
                    );
                }
                if command.is_none() {
                    command = entry.get("command").and_then(Value::as_str).map(String::from);
                }
                if args.is_empty() {
                    args = entry
                        .get("args")
                        .cloned()
                        .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok())
                        .unwrap_or_default();
                }
                if url.is_none() {
                    url = entry.get("url").and_then(Value::as_str).map(String::from);
                }
                if env.is_empty() {
                    env = entry
                        .get("env")
                        .cloned()
                        .and_then(|v| serde_json::from_value(v).ok())
                        .unwrap_or_default();
                }
                if headers.is_empty() {
                    headers = entry
                        .get("headers")
                        .cloned()
                        .and_then(|v| serde_json::from_value(v).ok())
                        .unwrap_or_default();
                }
            }
            upsert_server(
                conn,
                &McpServer {
                    name: name.clone(),
                    kind: kind.unwrap_or_else(|| "stdio".into()),
                    command,
                    args,
                    url,
                    env,
                    headers,
                    enabled,
                },
            )?;
            count += 1;
        }
        store::log_op(
            conn,
            "mcp.adopt",
            "mcp_servers",
            json!({ "adopted": count, "overwrite": overwrite }),
        );
        Ok(())
    })?;
    Ok(count)
}

fn write_json_preserving(
    path: &std::path::Path,
    backups: &std::path::Path,
    container: &[&str],
    servers: &Map<String, Value>,
) -> Res<()> {
    let mut root = read_json(path)?;
    if path.exists() {
        fsutil::backup_file(path, backups)?;
    }
    // 定位/创建容器对象
    let mut cur = &mut root;
    for key in container {
        if !cur.is_object() {
            *cur = json!({});
        }
        if cur.get(key).is_none() {
            cur.as_object_mut().unwrap().insert(key.to_string(), json!({}));
        }
        cur = cur.get_mut(key).unwrap();
    }
    *cur = Value::Object(servers.clone());
    let bytes = serde_json::to_vec_pretty(&root).map_err(|e| e.to_string())?;
    fsutil::atomic_write(path, &bytes)
}

/// 单 agent 同步结果
#[derive(Debug, Serialize)]
pub struct McpAgentSyncReport {
    pub agent: String,
    pub display_name: String,
    pub written: usize,
}

/// 全量同步：DB → 各启用插件端目标文件。
/// 写入语义：某端「启用」或「该端原本已有此条目」才写（disabled/enabled 反映状态）；
/// 单端独有的 server 不以禁用态铺到另一端（保持「未添加」）；不支持该类型的端跳过。
pub fn sync_all(store: &Store) -> Res<Vec<McpAgentSyncReport>> {
    let adapters = enabled_mcp_adapters(store)?;
    let servers = store.with(list_servers)?;
    let backups = store::backups_dir();
    let mut reports = Vec::new();

    for a in &adapters {
        let cfg = a.mcp_config();
        let existing = servers_map_of(&read_json(&cfg.path)?, cfg.container);
        let mut out_map = Map::new();
        for s in &servers {
            let enabled = s.enabled_for(a.id());
            if !enabled && !existing.contains_key(&s.name) {
                continue;
            }
            let entry = McpEntry {
                kind: s.kind.clone(),
                command: s.command.clone(),
                args: s.args.clone(),
                url: s.url.clone(),
                env: s.env.clone(),
                headers: s.headers.clone(),
                enabled,
            };
            if let Some(v) = a.to_mcp_entry(&entry, existing.get(&s.name)) {
                out_map.insert(s.name.clone(), v);
            }
        }
        write_json_preserving(&cfg.path, &backups, cfg.container, &out_map)?;
        reports.push(McpAgentSyncReport {
            agent: a.id().to_string(),
            display_name: a.display_name().to_string(),
            written: out_map.len(),
        });
    }

    store.with(|conn| {
        store::log_op(
            conn,
            "mcp.sync_all",
            "targets",
            json!({
                "servers": servers.len(),
                "per_agent": reports.iter().map(|r| json!({"agent": r.agent, "written": r.written})).collect::<Vec<_>>(),
            }),
        );
        Ok(())
    })?;
    Ok(reports)
}

/// 删除 server 并同步
pub fn remove_and_sync(store: &Store, name: &str) -> Res<()> {
    store.with(|conn| {
        delete_server(conn, name)?;
        store::log_op(conn, "mcp.delete", name, json!({}));
        Ok(())
    })?;
    sync_all(store)?;
    Ok(())
}

// ─── Tauri commands ───

#[derive(Serialize)]
pub struct McpOverviewPayload {
    pub items: Vec<McpOverviewItem>,
    pub drift: McpDrift,
}

#[tauri::command]
pub fn mcp_overview(state: State<'_, Store>) -> Result<McpOverviewPayload, String> {
    let (items, drift) = overview(&state)?;
    Ok(McpOverviewPayload { items, drift })
}

/// 收编目标文件现有 server 进统一定义（首次接管 / 漂移回填）
#[tauri::command]
pub fn mcp_adopt(state: State<'_, Store>, overwrite: bool) -> Result<usize, String> {
    adopt_from_targets(&state, overwrite)
}

#[tauri::command]
pub fn mcp_save(state: State<'_, Store>, server: McpServer) -> Result<(), String> {
    state.with(|conn| {
        upsert_server(conn, &server)?;
        store::log_op(
            conn,
            "mcp.save",
            &server.name,
            json!({ "kind": server.kind, "enabled": server.enabled }),
        );
        Ok(())
    })?;
    sync_all(&state)?;
    Ok(())
}

#[tauri::command]
pub fn mcp_toggle(
    state: State<'_, Store>,
    name: String,
    agent: String,
    enabled: bool,
) -> Result<(), String> {
    if adapters::by_id(&agent).is_none() {
        return Err(format!("未知 agent：{agent}"));
    }
    plugins::require_enabled(&state, &agent)?;
    state.with(|conn| {
        toggle_enabled(conn, &name, &agent, enabled)?;
        store::log_op(conn, "mcp.toggle", &name, json!({ "agent": agent, "enabled": enabled }));
        Ok(())
    })?;
    sync_all(&state)?;
    Ok(())
}

#[tauri::command]
pub fn mcp_delete(state: State<'_, Store>, name: String) -> Result<(), String> {
    remove_and_sync(&state, &name)
}

/// 校验目标文件：重算各端 diff（含漂移）
#[tauri::command]
pub fn mcp_validate(state: State<'_, Store>) -> Result<McpOverviewPayload, String> {
    let (items, drift) = overview(&state)?;
    state.with(|conn| {
        store::log_op(
            conn,
            "mcp.validate",
            "targets",
            json!({
                "drifted": items
                    .iter()
                    .map(|i| i.agents.iter().filter(|a| a.status == "drifted").count())
                    .sum::<usize>(),
                "only": drift.agents.iter().map(|d| d.only.len()).sum::<usize>(),
            }),
        );
        Ok(())
    })?;
    Ok(McpOverviewPayload { items, drift })
}

// ─── 连接测试（真实握手） ───

#[derive(Debug, Serialize, Clone)]
pub struct McpTestResult {
    pub ok: bool,
    pub message: String,
    pub latency_ms: u64,
}

/// npx 首次拉包可能较慢，给足超时
const MCP_TEST_TIMEOUT_SECS: u64 = 60;

fn initialize_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "agenthub", "version": env!("CARGO_PKG_VERSION") }
        }
    })
}

/// stdio：起进程 → stdin 写 initialize → 逐行读 stdout 等 id=1 响应（带总超时）
fn test_stdio(s: &McpServer) -> McpTestResult {
    use std::io::{BufRead, Read, Write};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let tail_of = |buf: &str| {
        let chars: Vec<char> = buf.chars().collect();
        let start = chars.len().saturating_sub(200);
        let tail: String = chars[start..].iter().collect();
        if start > 0 { format!("…{tail}") } else { tail }
    };

    let start = Instant::now();
    let fail = |msg: String| McpTestResult {
        ok: false,
        latency_ms: start.elapsed().as_millis() as u64,
        message: msg,
    };
    let Some(cmd) = s.command.as_deref() else {
        return fail("stdio 类型缺少 command".into());
    };

    let mut child = match Command::new(cmd)
        .args(&s.args)
        .envs(s.env.iter())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return fail(format!("进程启动失败（{cmd}）：{e}")),
    };

    // stderr 后台收集（失败时给出根因摘要，最多留 4KB）
    let stderr_buf = Arc::new(Mutex::new(String::new()));
    if let Some(stderr) = child.stderr.take() {
        let sink = Arc::clone(&stderr_buf);
        std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = std::io::Read::take(stderr, 4096).read_to_end(&mut v);
            *sink.lock().unwrap_or_else(|e| e.into_inner()) =
                String::from_utf8_lossy(&v).into_owned();
        });
    }

    // stdout 后台逐行读 → channel（read 本身阻塞，超时由主循环 recv_timeout 控制）
    let (tx, rx) = mpsc::channel::<String>();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }

    // 写 initialize（换行分隔 JSON，与主流 SDK stdio 帧一致）
    // stdin 保留到函数结束（提前关闭会导致部分 server 立即退出）
    let mut stdin = child.stdin.take();
    if let Some(si) = stdin.as_mut() {
        let req = format!("{}\n", initialize_request());
        if let Err(e) = si.write_all(req.as_bytes()).and_then(|_| si.flush()) {
            let _ = child.kill();
            let _ = child.wait();
            return fail(format!("initialize 写入失败：{e}"));
        }
    }

    let deadline = start + Duration::from_secs(MCP_TEST_TIMEOUT_SECS);
    let stderr_tail = || {
        let buf = stderr_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if buf.is_empty() { String::new() } else { format!(" · stderr：{}", tail_of(&buf)) }
    };
    let mut result: Option<McpTestResult> = None;
    loop {
        let now = Instant::now();
        if now >= deadline {
            result = Some(fail(format!(
                "超时（{MCP_TEST_TIMEOUT_SECS}s 内无有效 initialize 响应）{}",
                stderr_tail()
            )));
            break;
        }
        match rx.recv_timeout(deadline - now) {
            Ok(line) => {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if v.get("id").and_then(Value::as_i64) != Some(1) {
                    continue;
                }
                if let Some(err) = v.get("error") {
                    result = Some(fail(format!("server 返回错误：{err}")));
                    break;
                }
                if let Some(res) = v.get("result") {
                    let name = res["serverInfo"]["name"].as_str().unwrap_or("-");
                    let ver = res["serverInfo"]["version"].as_str().unwrap_or("-");
                    result = Some(McpTestResult {
                        ok: true,
                        latency_ms: start.elapsed().as_millis() as u64,
                        message: format!("握手成功 · serverInfo {name} {ver}"),
                    });
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                result = Some(fail(format!(
                    "超时（{MCP_TEST_TIMEOUT_SECS}s 内无有效 initialize 响应）{}",
                    stderr_tail()
                )));
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let tail = stderr_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
                result = Some(fail(if tail.is_empty() {
                    "进程提前退出且无任何输出（检查 command/args 是否指向存在的可执行文件）".into()
                } else {
                    format!("进程提前退出 · stderr：{}", tail_of(&tail))
                }));
                break;
            }
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    result.unwrap_or_else(|| fail("未知错误".into()))
}

/// streamableHttp：POST initialize，2xx 即通
fn test_http(s: &McpServer) -> McpTestResult {
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let fail = |msg: String| McpTestResult {
        ok: false,
        latency_ms: start.elapsed().as_millis() as u64,
        message: msg,
    };
    let Some(url) = s.url.as_deref() else {
        return fail("streamableHttp 类型缺少 url".into());
    };
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => return fail(format!("HTTP client 构建失败：{e}")),
    };
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body(initialize_request().to_string());
    for (k, v) in &s.headers {
        req = req.header(k, v);
    }
    match req.send() {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                McpTestResult {
                    ok: true,
                    latency_ms: start.elapsed().as_millis() as u64,
                    message: format!("HTTP {status} · 握手成功"),
                }
            } else {
                fail(format!("HTTP {status}"))
            }
        }
        Err(e) => fail(format!("请求失败：{e}")),
    }
}

/// 单 server 连接测试：真实走一遍 MCP initialize 握手（不写库、不动目标文件）
#[tauri::command]
pub async fn mcp_test_connection(
    state: State<'_, Store>,
    name: String,
) -> Result<McpTestResult, String> {
    let server = {
        let servers = state.with(list_servers)?;
        servers
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("统一定义中无此 server：{name}"))?
    };
    let r = tauri::async_runtime::spawn_blocking(move || match server.kind.as_str() {
        "streamableHttp" => test_http(&server),
        _ => test_stdio(&server),
    })
    .await
    .map_err(|e| format!("测试任务失败：{e}"))?;
    Ok(r)
}
