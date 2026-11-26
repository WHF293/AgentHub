//! AgentHub 应用壳 —— tauri 窗口/托盘/单实例/自启 + 插件注册与 IPC 命令装配。
//!
//! 业务模块：agenthub-core（glob re-export 保持 `agenthub_lib::<mod>` 对 main.rs 兼容）
//! + packages/<id> 插件包（适配器与专属域）；跨插件联动编排在 bridge.rs。
//! 新增插件三步：packages/<id> 新 crate → PLUGINS 数组加一项 → generate_handler 加命令。

pub use agenthub_core::*;

mod bridge;

use std::sync::atomic::Ordering;

use agenthub_core::adapters::{registry_init, AgentAdapter};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager,
};

/// 插件注册表：顺序 = UI 展示与收编冲突取字段的优先级（先者优先）
static PLUGINS: [&'static dyn AgentAdapter; 3] = [
    &agenthub_plugin_workbuddy::adapter::WORKBUDDY,
    &agenthub_plugin_zcode::adapter::ZCODE_ADAPTER,
    &agenthub_plugin_claude::adapter::CLAUDE_ADAPTER,
];

/// 组装层首行必须调用：core 各引擎（skills 迁移 / MCP / 用量 / memory）全部依赖注册表
pub fn init_plugins() {
    registry_init(&PLUGINS);
}

/// 主窗口显出并聚焦（托盘菜单 / 双击托盘共用）
fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

pub fn run() {
    init_plugins();
    tauri::Builder::default()
        // 单实例必须最先注册：二次启动时唤起已有窗口而不是开新进程
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        // 原生文件选择对话框（Usage 岛「导入 cc-switch 备份」入口）
        .plugin(tauri_plugin_dialog::init())
        // 首帧显示优化：窗口 visible:false 启动（消除 WebView2 冷启动 1~2s 白屏），
        // 页面 load 完成后显示 —— 用户看到的第一帧就是渲染完成的界面（FCP/LCP 感知）
        .on_page_load(|webview, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished {
                let _ = webview.show();
            }
        })
        .setup(|app| {            // panic 兜底日志：把 Rust panic 落盘，便于区分「卡死」与「崩溃」
            let _ = std::fs::create_dir_all(store::hub_dir().join("logs"));
            std::panic::set_hook(Box::new(|info| {
                let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                let line = format!("[{ts}] PANIC: {info}\n");
                let _ = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(store::hub_dir().join("logs").join("panic.log"))
                    .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
            }));

            // 中央存储：~/.agenthub/agenthub.db（打开即迁移 schema）
            let store = store::Store::open().expect("初始化 ~/.agenthub 失败");
            app.manage(store);

            // 后台常驻采集线程：启动即全量+增量，之后每 60s 一次增量巡检
            // （增量续扫修复后毫秒级，不依赖页面打开；独立 WAL 连接，不与 UI 抢锁。
            //  数据常新后，前端 Usage 页只需查库，不再主动触发采集。
            //  已解绑（app_settings.unbound=1）时跳过采集，线程空转保活，重新接管后自动恢复）
            //  传 None：后台巡检不向 UI 发进度事件（进度条只对手动「刷新」出现，
            //  否则每 60s 弹一次 ZCode 全量账本扫描进度，纯打扰）
            std::thread::spawn(move || loop {
                if let Ok(s2) = store::Store::open() {
                    if !unbind::is_unbound(&s2) {
                        let _ = usage::collect_all(&s2, None);
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(60));
            });

            // 启动自动签到：遍历全部 WB 账号逐个领取每日积分（接口幂等；
            // 「wb-auto-checkin」开关默认开，Plugin 岛停用 WB 时不执行）
            std::thread::spawn(|| {
                if let Ok(s2) = store::Store::open() {
                    let _ = agenthub_plugin_workbuddy::checkin::auto_checkin_all(&s2);
                }
            });

            // DPI 自适应钳制：配置里的 width/height 是逻辑像素，乘以系统缩放系数后
            // 若超出显示器物理尺寸（如 2K 屏 175% 缩放 → 2800×1925 > 2560×1600），
            // Windows 会把窗口钳制到屏幕内，显得比设定小；此时直接最大化启动，
            // 保证小屏高分屏上也能获得满屏可用面积（普通缩放屏幕仍按配置尺寸居中）。
            if let Some(window) = app.get_webview_window("main") {
                let (want_w, want_h) = app
                    .config()
                    .app
                    .windows
                    .iter()
                    .find(|w| w.label == "main")
                    .map(|w| (w.width, w.height))
                    .unwrap_or((1600.0, 1100.0));
                if let Ok(Some(monitor)) = window.current_monitor() {
                    let sf = monitor.scale_factor();
                    let want = tauri::PhysicalSize::new(
                        (want_w * sf).round() as u32,
                        (want_h * sf).round() as u32,
                    );
                    if want.width > monitor.size().width || want.height > monitor.size().height {
                        let _ = window.maximize();
                    }
                }
            }

            // 托盘：打开面板 / 退出；关闭行为 = 关到托盘（见 on_window_event）
            let open = MenuItem::with_id(app, "open", "打开面板", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;

            TrayIconBuilder::with_id("main-tray")
                .icon(
                    app.default_window_icon()
                        .expect("default window icon missing")
                        .clone(),
                )
                .tooltip("AgentHub")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // 组装桥（跨插件编排）
            bridge::providers_overview,
            bridge::provider_save,
            bridge::provider_delete,
            bridge::claude_provider_apply,
            // core：状态 / Skills / 插件 / MCP / 用量 / 市场 / Memory / 设置 / 更新
            agenthub_core::commands::app_status,
            agenthub_core::commands::run_skills_migration,
            agenthub_core::commands::skills_overview,
            agenthub_core::commands::relink_skill,
            agenthub_core::commands::set_skill_enabled,
            agenthub_core::commands::skill_delete,
            agenthub_core::commands::recent_op_log,
            agenthub_core::commands::set_close_to_tray,
            agenthub_core::plugins::plugin_list,
            agenthub_core::plugins::plugin_toggle,
            agenthub_core::plugins::plugin_install,
            agenthub_core::plugins::plugin_uninstall,
            agenthub_core::mcp::mcp_overview,
            agenthub_core::mcp::mcp_adopt,
            agenthub_core::mcp::mcp_save,
            agenthub_core::mcp::mcp_toggle,
            agenthub_core::mcp::mcp_delete,
            agenthub_core::mcp::mcp_validate,
            agenthub_core::mcp::mcp_test_connection,
            agenthub_core::usage::usage_collect,
            agenthub_core::usage::usage_overview,
            agenthub_core::usage::usage_matrix,
            agenthub_core::usage::usage_accounts,
            agenthub_core::market::market_refresh,
            agenthub_core::market::market_install,
            agenthub_core::memory::memory_global_list,
            agenthub_core::memory::memory_read,
            agenthub_core::memory::memory_write,
            agenthub_core::memory::memory_global_unify,
            agenthub_core::memory::projects_overview,
            agenthub_core::memory::project_init,
            agenthub_core::memory::project_sync,
            agenthub_core::memory::project_consolidate,
            // 项目记忆文件管理（v2.5 用户导入制）
            agenthub_core::memory::project_import,
            agenthub_core::memory::memory_files_list,
            agenthub_core::memory::memory_file_delete,
            agenthub_core::memory::memory_file_rename,
            // 通用工具（Memory 岛「打开文件夹」等）
            agenthub_core::commands::reveal_path,
            // 解绑域（Setting 岛「解绑与卸载」卡片）
            agenthub_core::unbind::unbind_status,
            agenthub_core::unbind::unbind_run,
            agenthub_core::unbind::unbind_reclaim,
            agenthub_core::update::update_check,
            agenthub_core::update::update_install,
            agenthub_core::update::update_proxy_get,
            agenthub_core::update::update_proxy_set,
            // WorkBuddy 插件
            agenthub_plugin_workbuddy::account::wb_overview,
            agenthub_plugin_workbuddy::account::wb_save_slot,
            agenthub_plugin_workbuddy::account::wb_switch,
            agenthub_plugin_workbuddy::account::wb_import_legacy,
            agenthub_plugin_workbuddy::account::wb_delete_slot,
            agenthub_plugin_workbuddy::account::wb_rename_slot,
            agenthub_plugin_workbuddy::sessions::wb_session_list,
            agenthub_plugin_workbuddy::sessions::wb_session_copy,
            agenthub_plugin_workbuddy::checkin::wb_checkin_status,
            agenthub_plugin_workbuddy::checkin::wb_checkin_claim,
            agenthub_plugin_workbuddy::checkin::wb_checkin_auto_get,
            agenthub_plugin_workbuddy::checkin::wb_checkin_auto_set,
            agenthub_plugin_workbuddy::quota_wb::wb_credit_summary,
            agenthub_plugin_workbuddy::quota_wb::wb_credit_by_account,
            agenthub_plugin_workbuddy::quota_wb::wb_credit_set_total,
            agenthub_plugin_workbuddy::quota_wb::wb_credit_set_balance,
            // ZCode 插件
            agenthub_plugin_zcode::provider_zcode::provider_fetch_models,
            agenthub_plugin_zcode::provider_zcode::provider_set_default,
            agenthub_plugin_zcode::quota_zcode::zcode_quota,
            agenthub_plugin_zcode::skills::zcode_plugin_skills,
            agenthub_plugin_zcode::zaccount::zcode_overview,
            agenthub_plugin_zcode::zaccount::zcode_save_slot,
            agenthub_plugin_zcode::zaccount::zcode_switch,
            agenthub_plugin_zcode::zaccount::zcode_delete_slot,
            agenthub_plugin_zcode::zaccount::zcode_rename_slot,
            agenthub_plugin_zcode::zaccount::zcode_launch,
            // Claude Code 插件
            agenthub_plugin_claude::provider_claude::claude_providers_overview,
            agenthub_plugin_claude::provider_claude::claude_provider_fetch_models,
            agenthub_plugin_claude::provider_claude::claude_provider_save,
            agenthub_plugin_claude::ccsimport::usage_import_ccswitch,
        ])
        .on_window_event(|window, event| {
            // 关闭行为：默认关到托盘（设置岛可切换为直接退出）
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if CLOSE_TO_TRAY.load(Ordering::Relaxed) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
