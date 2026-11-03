// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 无头模式：agenthub.exe --migrate [--link-mode symlink|junction|copy]
    // 不启 GUI，直接执行 M1 迁移并打印 JSON 报告（用于验收 / 冒烟）
    let args: Vec<String> = std::env::args().collect();
    agenthub_lib::init_plugins();
    // 无头模式：agenthub.exe --plugin-list / --plugin-set <id> <install|uninstall|enable|disable>
    // 插件生命周期管理（给 uTools 插件 / 脚本用）
    if args.iter().any(|a| a == "--plugin-list") {
        let store = agenthub_lib::store::Store::open().expect("初始化 ~/.agenthub 失败");
        println!(
            "{}",
            serde_json::to_string_pretty(&agenthub_lib::plugins::list(&store).expect("list 失败"))
                .expect("serialize")
        );
        return;
    }
    if let Some(pos) = args.iter().position(|a| a == "--plugin-set") {
        let id = args.get(pos + 1).expect("缺插件 id（workbuddy/zcode/claude）").clone();
        let action = args.get(pos + 2).cloned().unwrap_or_default();
        let store = agenthub_lib::store::Store::open().expect("初始化 ~/.agenthub 失败");
        let r = match action.as_str() {
            "install" => agenthub_lib::plugins::set_installed(&store, &id, true),
            "uninstall" => agenthub_lib::plugins::set_installed(&store, &id, false),
            "enable" => agenthub_lib::plugins::set_enabled(&store, &id, true),
            "disable" => agenthub_lib::plugins::set_enabled(&store, &id, false),
            other => Err(format!("未知动作：{other}（install/uninstall/enable/disable）")),
        };
        match r {
            Ok(_) => println!("✓ {id} {action}"),
            Err(e) => {
                eprintln!("失败: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if args.iter().any(|a| a == "--migrate") {
        let mode = args
            .iter()
            .position(|a| a == "--link-mode")
            .and_then(|i| args.get(i + 1))
            .map(String::as_str);
        let store = agenthub_lib::store::Store::open().expect("初始化 ~/.agenthub 失败");
        match agenthub_lib::migrate::migrate_skills(&store, mode) {
            Ok(report) => {
                println!("{}", serde_json::to_string_pretty(&report).expect("serialize"));
            }
            Err(e) => {
                eprintln!("迁移失败: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    // 无头模式：agenthub.exe --sync-mcp [--adopt]  收编（可选）→ 全量同步双端
    if args.iter().any(|a| a == "--sync-mcp") {
        let store = agenthub_lib::store::Store::open().expect("初始化 ~/.agenthub 失败");
        if args.iter().any(|a| a == "--adopt") {
            let n = agenthub_lib::mcp::adopt_from_targets(&store, false).expect("adopt 失败");
            println!("收编 {n} 个 server");
        }
        match agenthub_lib::mcp::sync_all(&store) {
            Ok(reports) => {
                for r in reports {
                    println!("{} servers → {}", r.written, r.display_name);
                }
            }
            Err(e) => {
                eprintln!("MCP 同步失败: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    // 无头模式：agenthub.exe --fetch-models <provider-id>  验证 /v1/models 候选链
    if let Some(pid) = args
        .iter()
        .position(|a| a == "--fetch-models")
        .and_then(|i| args.get(i + 1))
    {
        let provs = agenthub_plugin_zcode::provider_zcode::read_zcode_providers().expect("读 provider 失败");
        let p = provs
            .iter()
            .find(|p| p.id == *pid)
            .unwrap_or_else(|| panic!("provider 不存在：{pid}"));
        let key = agenthub_plugin_zcode::provider_zcode::resolve_api_key(pid, None).expect("key 缺失");
        let r = agenthub_lib::modelfetch::fetch_models(&p.kind, &p.base_url, &key, None);
        println!(
            "{}",
            serde_json::to_string_pretty(&r).expect("serialize")
        );
        return;
    }
    // 无头模式：agenthub.exe --wb-status  账号岛状态探测（含槽位/进程/当前 uid）
    if args.iter().any(|a| a == "--wb-status") {
        let store = agenthub_lib::store::Store::open().expect("初始化 ~/.agenthub 失败");
        let o = agenthub_plugin_workbuddy::account::overview(&store).expect("wb overview 失败");
        println!(
            "{}",
            serde_json::to_string_pretty(&o).expect("serialize")
        );
        return;
    }
    // 无头模式：agenthub.exe --wb-save-slot <label>（仅快照复制，不切换不启动）
    if let Some(label) = args
        .iter()
        .position(|a| a == "--wb-save-slot")
        .and_then(|i| args.get(i + 1))
    {
        let store = agenthub_lib::store::Store::open().expect("初始化 ~/.agenthub 失败");
        let slot = agenthub_plugin_workbuddy::account::save_slot(&store, label).expect("保存槽位失败");
        println!(
            "{}",
            serde_json::to_string_pretty(&slot).expect("serialize")
        );
        return;
    }
    // 无头模式：agenthub.exe --usage-collect [range]  采集双源 + 打印聚合概览
    if args.iter().any(|a| a == "--usage-collect") {
        let store = agenthub_lib::store::Store::open().expect("初始化 ~/.agenthub 失败");
        let (z, w, c) = agenthub_lib::usage::collect_all(&store, None).expect("采集失败");
        println!("新增事件 zcode={z} workbuddy={w} claude={c}");
        let range = args.get(args.len() - 1).map(String::as_str).unwrap_or("day");
        let o = agenthub_lib::usage::overview(&store, range).expect("聚合失败");
        println!(
            "{}",
            serde_json::to_string_pretty(&o).expect("serialize")
        );
        return;
    }
    // 无头模式：agenthub.exe --import-ccswitch <备份文件路径>  cc-switch SQL 备份用量导入
    // 明细逐条 + 30 天前日汇总按 request_count 合成事件，request_id 加 ccs: 前缀幂等可重跑
    if let Some(pos) = args.iter().position(|a| a == "--import-ccswitch") {
        let path = args.get(pos + 1).expect("缺备份文件路径").clone();
        let store = agenthub_lib::store::Store::open().expect("初始化 ~/.agenthub 失败");
        match agenthub_plugin_claude::ccsimport::import_backup(&store, &path) {
            Ok(report) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).expect("serialize")
                );
            }
            Err(e) => {
                eprintln!("导入失败: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    // 无头模式：agenthub.exe --claude-setup --base-url <url> --api-key <key> [--model <m>] [--small-model <m>]
    // Claude Code 模型托管一步到位：settings.json env 四键 + 跳过首次引导（key 只写目标文件）
    if args.iter().any(|a| a == "--claude-setup") {
        let get = |flag: &str| -> Option<String> {
            args.iter()
                .position(|a| a == flag)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        let base_url = get("--base-url").unwrap_or_default();
        let input = agenthub_plugin_claude::provider_claude::ClaudeSaveInput {
            base_url,
            api_key: get("--api-key"),
            model: get("--model"),
            small_fast_model: get("--small-model"),
        };
        match agenthub_plugin_claude::provider_claude::write_claude_provider(&input) {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("claude-setup 失败: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    // 无头模式：agenthub.exe --market-refresh  刷新全部内置市场目录
    if args.iter().any(|a| a == "--market-refresh") {
        let catalog = agenthub_lib::market::refresh_all();
        for w in &catalog.warnings {
            println!("⚠ {w}");
        }
        for s in &catalog.skills {
            println!(
                "{:16} [{:14}] [{}] {}",
                s.name,
                s.source_name,
                if s.installed { "已装" } else { "未装" },
                s.description.chars().take(52).collect::<String>()
            );
        }
        println!("共 {} 个市场技能", catalog.skills.len());
        return;
    }
    // 无头模式：agenthub.exe --bridge-test <项目路径>  M6 桥接全链路：init → sync → consolidate
    if let Some(path) = args
        .iter()
        .position(|a| a == "--bridge-test")
        .and_then(|i| args.get(i + 1))
    {
        let store = agenthub_lib::store::Store::open().expect("初始化 ~/.agenthub 失败");
        let overview = agenthub_lib::memory::overview_projects(&store).expect("项目注册失败");
        println!(
            "已知项目 {} 个；{}",
            overview.len(),
            overview
                .iter()
                .map(|p| format!("{}(.ai={})", p.name, p.ai_ready))
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!("── init ──");
        for s in agenthub_lib::memory::init_project_bridge(&store, path).expect("init 失败") {
            println!("  {s}");
        }
        println!("── sync ──");
        for s in agenthub_lib::memory::sync_project_bridge(&store, path).expect("sync 失败") {
            println!("  {s}");
        }
        println!("── consolidate ──");
        for s in agenthub_lib::memory::consolidate_project_bridge(&store, path).expect("蒸馏失败") {
            println!("  {s}");
        }
        return;
    }
    // 无头模式：agenthub.exe --wb-sessions  列出当前账号会话（验证 sessions.rs 会话列出）
    if args.iter().any(|a| a == "--wb-sessions") {
        let store = agenthub_lib::store::Store::open().expect("初始化 ~/.agenthub 失败");
        let o = agenthub_plugin_workbuddy::account::overview(&store).expect("wb overview 失败");
        let uid = o.current.uid.clone();
        let list = agenthub_plugin_workbuddy::sessions::list_sessions(&store, &uid).expect("会话列出失败");
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "uid": uid,
                "count": list.len(),
                "sessions": list,
            }))
            .expect("serialize")
        );
        return;
    }
    // 无头模式：agenthub.exe --wb-copy-test <session-id> <target-uid> [target-uid2 ...]
    // 会话复制沙盒演练（配合 USERPROFILE 重定向到临时目录，绝不触碰真实 WB 数据），支持多目标
    if let Some(pos) = args.iter().position(|a| a == "--wb-copy-test") {
        let sid = args.get(pos + 1).expect("缺 session-id").clone();
        let tuids: Vec<String> = args[pos + 2..]
            .iter()
            .filter(|a| !a.starts_with("--"))
            .cloned()
            .collect();
        assert!(!tuids.is_empty(), "缺 target-uid");
        let store = agenthub_lib::store::Store::open().expect("初始化 ~/.agenthub 失败");
        let backup = agenthub_lib::store::backups_dir().join("session-test");
        for tuid in &tuids {
            let results = agenthub_plugin_workbuddy::sessions::copy_sessions(&store, &[sid.clone()], tuid, &backup);
            println!("== target {tuid} ==");
            println!("{}", serde_json::to_string_pretty(&results).expect("serialize"));
        }
        return;
    }
    // 无头模式：agenthub.exe --checkin  WB 全账号签到领积分（给 uTools 插件 / 计划任务用）
    // 注意：受 ~/.agenthub/keys/wb-checkin-auto 开关与插件启用门控约束，与 GUI 同一开关
    if args.iter().any(|a| a == "--checkin") {
        let store = agenthub_lib::store::Store::open().expect("初始化 ~/.agenthub 失败");
        let results = agenthub_plugin_workbuddy::checkin::auto_checkin_all(&store).expect("签到失败");
        println!(
            "{}",
            serde_json::to_string_pretty(&results).expect("serialize")
        );
        return;
    }
    // 无头模式：agenthub.exe --wb-switch <slot-id> [--force]  切换账号槽位（JSON SwitchReport）
    // --force = WB 正在运行时先 taskkill 结束（调用方须已向用户二次确认）；不带则 WB 运行中直接报错
    if let Some(pos) = args.iter().position(|a| a == "--wb-switch") {
        let slot_id = args.get(pos + 1).expect("缺 slot-id（见 --wb-status 输出）").clone();
        let force = args.iter().any(|a| a == "--force");
        let store = agenthub_lib::store::Store::open().expect("初始化 ~/.agenthub 失败");
        if force && agenthub_plugin_workbuddy::account::process_running() {
            if let Err(e) = agenthub_plugin_workbuddy::account::kill_process() {
                eprintln!("结束 WB 进程失败: {e}");
                std::process::exit(1);
            }
            // 等 taskkill 落地，最多 5 秒；switch_account 内部还会再检测一次
            for _ in 0..10 {
                if !agenthub_plugin_workbuddy::account::process_running() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
        match agenthub_plugin_workbuddy::account::switch_account(&store, &slot_id, &[]) {
            Ok(report) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).expect("serialize")
                );
                if !report.ok {
                    std::process::exit(1);
                }
            }
            Err(e) => {
                eprintln!("切号失败: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    // 无头模式：agenthub.exe --unify-memory  全局记忆统一：真身 ~/.agenthub/MEMORY.md + 双端建链
    if args.iter().any(|a| a == "--unify-memory") {
        let store = agenthub_lib::store::Store::open().expect("初始化 ~/.agenthub 失败");
        match agenthub_lib::memory::unify_global_memory(&store) {
            Ok(steps) => {
                for s in steps {
                    println!("{s}");
                }
            }
            Err(e) => {
                eprintln!("统一失败: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    agenthub_lib::run()
}
