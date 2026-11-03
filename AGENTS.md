# AGENTS.md — AI 编码代理项目说明

面向 AI 编码代理（ZCode / WorkBuddy 等）的项目指南。修改代码前请先读完本文。

## 项目概述

**AgentHub**（v0.4.0）— AI Agent 统一管理工具（Tauri 2 Windows 桌面应用）：可插拔适配器架构
（内置 WorkBuddy / ZCode，社区可按 AgentAdapter 接口扩展 claude / codex / trae 等），提供
Skills / MCP / 第三方模型 / 多账号 / Memory / 用量分析一站式管理台，替代已卸载的 cc-switch。
中央仓库一份配置、分发到各已启用插件端。

- 技术栈：**Tauri v2 + React 19 + TypeScript + Zustand + UnoCSS + ECharts 6（按需导入）+
  SQLite（rusqlite bundled）+ reqwest 0.12（blocking + rustls-tls）+ pnpm**
- 中央存储：`~/.agenthub/`（agenthub.db 单一事实源 + skills/ 中央仓库 + backups/ 时间戳备份
  + market-cache/ + updates/ 在线更新下载区）
- UI：Islands 布局 + 侧栏 8 岛（**Usage / Account / Skill / MCP / Model / Memory / Plugin /
  Setting**，单数文案定版）+ 明暗双主题（CSS variables，明/暗/跟随系统三态）
- 外壳：自绘标题栏（decorations:false）+ 托盘（打开面板/退出，默认关到托盘）+ 单实例插件
  （二次启动唤起已有窗口）+ 开机自启（tauri-plugin-autostart）
- 里程碑：M0-M9 全部落地；在线更新已上线；数据侧已对齐双 Agent 官方口径
  （ZCode 累计 19 亿 / WB 请求级 24.5 亿）

## AI 记忆统一存放（硬性）

本项目记忆统一走 `.ai/`：主题层（`开发方案/`）+ 日志层（`memory/YYYY-MM-DD.md` 按日追加）。
`.ai/` 与 `local_docs/` 已 gitignore，仅本地保存。各 agent 会话结束时把做了什么/为什么追加进
`.ai/memory/<当日>.md`（这是 AgentHub 自身桥接机制的第一个样板项目）。

<!-- agent-hub:start -->
本项目所有 AI agent 共享记忆位于 `.ai/`：
- 开始任务前，先读 `.ai/MEMORY.md` 和最近几天的 `.ai/memory/` 日志
- 完成实质工作后，把做了什么/为什么追加到 `.ai/memory/YYYY-MM-DD.md`
- 稳定的项目约定写入 `.ai/PROJECT.md`
<!-- agent-hub:end -->

## 常用命令

```bash
pnpm install          # 安装依赖
pnpm tauri:dev        # Tauri 桌面开发（Rust + 前端热更）
pnpm dev              # 仅前端（浏览器预览，默认按桌面外壳渲染，?desktopTitlebar=0 关闭模拟）
pnpm build            # tsc --noEmit + vite build（提交前必须通过）
pnpm build:exe        # 打包：tauri build —— Windows amd64 exe + NSIS 安装包（安装版定版）
```

无头验收命令（不启 GUI，打印 JSON / 文本报告）：

```bash
agenthub.exe --migrate [--link-mode symlink|junction|copy]   # M1 skills 迁移
agenthub.exe --sync-mcp [--adopt]                            # MCP 收编 + 双端同步
agenthub.exe --fetch-models <provider-id>                    # /v1/models 候选链
agenthub.exe --wb-status / --wb-save-slot <label>            # 账号岛探测/存槽位
agenthub.exe --wb-sessions                                   # 列当前账号会话（sessions.rs）
agenthub.exe --wb-copy-test <session-id> <target-uid>...    # 会话复制演练，支持多目标（配 USERPROFILE 重定向沙盒）
agenthub.exe --usage-collect [range]                         # 用量双源采集 + 聚合
agenthub.exe --claude-setup --base-url <url> --api-key <key> [--model <m>] [--small-model <m>]
                                                             # Claude Code 模型托管一步到位（settings.json env + 跳过引导）
agenthub.exe --import-ccswitch <备份文件路径>                 # cc-switch SQL 备份用量导入（明细+30天前日汇总，ccs: 前缀幂等）
agenthub.exe --market-refresh                                # Skill 市场目录刷新
agenthub.exe --bridge-test <项目路径>                        # Memory 桥接 init/sync/consolidate
agenthub.exe --unify-memory                                  # 全局记忆统一：真身 ~/.agenthub/MEMORY.md + 双端建链
```

## 目录结构

```
src/                    前端
  api.ts                invoke 封装（TS 类型与 Rust serde 一一对应，字段保持 snake_case）
  common/               useDesktopChrome / TitleBar（自绘外壳 + 版本徽标 + 更新入口）/
                        ConfirmDialog（含 InputDialog）/ AgentCard / PageHeader / LogoMark
  pages/                8 个功能岛（Dashboard = Usage 岛：已启用插件健康卡 + 用量分析；
                        Plugins = 可插拔适配器管理岛）
  agents.ts             前端 agent 注册表（id → 名称 / 身份色，与 Rust adapters::registry() 对应）
  stores/               nav / theme（light|dark|system）/ ui（toast）
  styles/theme.css      主题 token（侧栏/列表/卡片=中性灰，按钮与底色=清新绿，用户定版；
                        WB=绿 / ZCode=天青蓝 双 Agent 身份色全局统一）
src-tauri/src/
  store.rs              SQLite 单一事实源 + schema v1（11 表）+ v1.1~v1.9 增量迁移
                        （headers_json / wb_slots TEXT 主键 / wb_session_seen / credit /
                        wb_model_entries / usage_events.account 账号归属 / zcode_slots /
                        mcp_servers.enabled_json 按 agent 启用态 / plugin_state 插件开关）
  adapters/             可插拔核心层：mod.rs = AgentAdapter trait（能力声明 caps + skills 域
                        链接策略/建链 + MCP 域配置形态/条目翻译）+ 静态注册表 registry()/by_id()；
                        wb.rs / zcode.rs / claude.rs 各实现 trait（WB 逐项链 / ZCode 整目录链 +
                        Claude Code 整目录链 + mcp.json disabled / config.json enabled /
                        claude.json mcpServers 在场即启用 翻译）。社区新增 agent：实现 trait →
                        REGISTRY 加一行 → 前端 agents.ts 补一条，引擎零改动
  plugins.rs            插件启用状态（plugin_state 表）：首次运行按安装探测自动启用（source=auto），
                        之后以用户手动开关为准持久化（source=manual）；enabled_adapters() 是
                        skills 迁移 / MCP 同步 / usage 采集的统一门控入口
  fsutil.rs             软链三级策略（symlink→junction→copy）/ 原子写 / 备份 / fnv1a
  migrate.rs            M1：各启用插件端 skills 并集导入中央仓库 → 按各端策略建链
                        （逐项链 / 整目录链由 AgentAdapter 声明，报告按 agent 分组）
  mcp.rs                统一 MCP 引擎（agent 无关）：按注册表遍历启用插件端做格式翻译
                        （合并式写入：托管字段覆盖，timeout/headers 等保留）；
                        mcp_servers.enabled_json 按 agent 记启用态（NULL 回退旧列）
  providers.rs          ZCode v2/config.json 直写 + WB models.json 写穿（v1.5 托管清单）；
                        Key 只存目标文件不入库
  modelfetch.rs         /v1/models 候选链（models_url 覆写 → /v{N} 原样 → 剥离后缀兜底 →
                        GLM 系网关内置推荐兜底；anthropic 原生 x-api-key 优先）
  market.rs             Skill 市场（anthropics/skills + obra/superpowers + cexll/myclaude，通用树扫描）
  account.rs            WB 槽位：tasklist 检测 →（UI 确认后关进程）→ 备份 → 恢复 → 校验 → 启动 → 回滚
  zaccount.rs           ZCode 槽位：~/.zcode/v2/credentials.json 文件级复制（token enc:v1: 加密
                        不读取）→ 备份 → 原子写入 → JSON 校验 → 失败回滚；CLI 每次启动重读凭据，
                        无需进程管理
  quota.rs              ZCode Coding Plan 官方额度（GET open.bigmodel.cn/api/monitor/usage/quota/limit，
                        Key 读 ZCode v2 config 的 builtin:bigmodel-coding-plan options.apiKey 明文，
                        unit=3→5h / unit=6→周）+ WB 积分汇总（剩余 = ~/.agenthub/keys/wb-credit-total
                        − 本月已用 credit，与账单同源）
  memory.rs             全局记忆统一（真身 ~/.agenthub/MEMORY.md，双端路径建文件符号链接，幂等）
                        + .ai/ 桥接（init/sync/consolidate，FNV 块指纹漂移检测）
  usage.rs              用量双源采集 + 聚合（见下「关键约定」第 5/6 条）；WB 事件经
                        sessions.user_id 打账号标（wb_session_accounts + 存量回填），
                        聚合/矩阵/账号清单输出均带 account 维度（Usage 岛范围筛选数据源）
  update.rs             在线更新（NSIS 安装版，链路对齐 whf-stock-board 实证实现；仓库已公开全匿名）：
                        检测 302 重定向主通道 → 公开 API 兜底；reqwest 直连下载安装包到
                        ~/.agenthub/updates/（真实进度）；安装 = cmd 链 `ping -n 4 & "{setup}" /S
                        /D={原目录} & start "{exe}"` 静默装回重启（raw_arg 传参、/D= 最后且不加引号、
                        延迟 500ms 退出）
  commands.rs           Skills 岛命令 + app_status + op_log + set_close_to_tray
  lib.rs                插件注册（单实例最先）/ 托盘 / 启动即后台采集用量 / IPC 命令注册
  main.rs               无头模式入口（--migrate 等七种）
.github/workflows/release.yml   tag v* 或手动触发 → Windows amd64 NSIS 安装包 → GitHub Release
```

## 关键约定（改代码必读）

1. **打包为 NSIS 安装版**：`bundle.targets: ["nsis"]` + `pnpm build:exe`（tauri build，产出
   `bundle/nsis/AgentHub_{version}_x64-setup.exe`）；NSIS 走默认 currentUser 免管理员提权（同
   whf-stock-board）。2026-10-31 起定版：旧「exe-only（targets:[] + --no-bundle）」方案已废弃
   （历史坑：本版本 CLI 不接受 `--bundles none` / `"none"`，exe-only 曾用空数组 + `--no-bundle` 实现）。
2. **Tauri IPC 多单词参数**一律收进单个 `input` 结构体（serde 自控），禁止散参 ——
   本版本 IPC 对 snake/camel 大小写映射不稳（踩过 `missing required key base_url`）。
3. **写目标文件三步**：备份到 `~/.agenthub/backups/<域>-<时间戳>/` → 原子写（tmp+rename）→
   JSON 可解析校验；失败回滚。MCP 是合并式写入（只覆盖托管字段，timeout/headers 等非托管字段保留）。
4. **隐私红线**：WB auth 文件只做文件级复制永不读内容 —— **唯一例外**：checkin.rs 每日签到
   需在内存中解密 accessToken（atRestSecretKey 经 ELECTRON_RUN_AS_NODE 调 WB 自身取回，
   AES-256-GCM + 固定 AAD；token/密钥只存内存，永不落库/写盘/写日志/回传前端）；
   usage 日志只取 usage/元数据字段，request.messages / response.text 正文永不入库；
   API Key 只存 ZCode 目标文件、不落 AgentHub 库。
5. **用量数据源（实测定版，勿回退）**：
   - ZCode：`~/.zcode/cli/db/db.sqlite` 的 `model_usage` 表全表扫描（SUM(input)=18.87 亿 ≈
     官方 18.9 亿；request_id = 主键 id 幂等去重）。rollout jsonl 只剩近期文件，已废弃，
     历史事件已一次性清除
   - WB：`~/.workbuddy/projects/**/*.jsonl` 的 `providerData.rawUsage/usage`（请求级真实 usage；
     rawUsage.credit = 官方积分，与账单同源；prompt_tokens 含缓存命中 → input = prompt − 命中）
   - 字节偏移增量解析 + mtime 变化重扫兜底 + request_id 去重
6. **采集不卡 UI**：usage 命令全部 async + 分批短锁（400 条/批）+ `usage-collect-progress`
   事件推前端进度条 + COLLECTING 原子互斥防重入；应用启动即后台线程独立 WAL 连接全量+增量采集
   （不依赖页面打开）；历史永久留存 `~/.agenthub/agenthub.db`，卸载双 Agent 后仍可查
7. **软链策略（适配器声明）**：ZCode 整目录链 → `~/.agenthub/skills`；WB 逐项链 —— 由
   AgentAdapter::skills_link() 声明，引擎不硬编码；symlink 探测失败降级 junction；
   所有替换先备份实目录。注意 WB 是「活端」会自发重写 skills 目录打掉软链（云同步/CDN 下载），
   ZCode 是「静端」—— WB 侧 diverged 巡检是已知待办
8. **Skill 市场安装语义**：一律先进中央仓库再分发；「未添加 ≠ 禁用」——单端独有 server 不铺到另一端
8.5. **可插拔插件语义（2026-11-01 定版）**：每个适配器 = 一个插件（Plugin 岛管理）。首次运行
   （plugin_state 空表）按安装探测自动启用；之后一切以用户手动开关为准（持久化，后续启动只读配置
   不自动变更）。停用插件的端不参与导入/分发/同步/采集，Usage 历史数据保留。IPC 与 DB 中的
   agent 一律用稳定 id 字符串（"workbuddy"/"zcode"/社区自定义），前端经 src/agents.ts 渲染身份色
9. **在线更新链路（NSIS 安装版，仓库已公开全匿名）**：update_check 检测双通道（302 重定向主通道，
   免匿名 API 限流 → 公开 API 兜底，资产 `AgentHub_v*_x64-setup.exe`）→ update_install reqwest
   直连下载安装包到 `~/.agenthub/updates/`（真实进度事件 `update-download-progress`）→ NSIS
   `/S /D={原目录}` 静默装回 + start 重启；版本徽标在标题栏（gh CLI 通道随私有仓库时代于
   2026-10-31 移除）
10. **新增 Rust 依赖**（如有）：注意用户机器 rustc 1.98 / cargo 邻域源（rsproxy），锁小版本防漂移
11. `pnpm build`（tsc 严格模式）与 `cargo check` 双绿才算编译通过；UI 文案中文为主

## 发布流程

1. 提交遵循 conventional commits（feat: / fix: / refactor: …）——Release 描述由提交主题自动生成。
2. 本地 `pnpm build:exe` 点验通过后，打 tag 推送（版本号以 `src-tauri/tauri.conf.json` 为准，
   与 package.json 保持同步）：`git tag v0.2.0 && git push origin v0.2.0`。
3. GitHub Actions（`.github/workflows/release.yml`）：tag 触发（或 workflow_dispatch 手动触发补传）；
   守卫 tag 必须在 main 分支历史上；windows-latest 构建 amd64 NSIS 安装包 → 生成带编号变更列表 →
   创建 Release 并上传 `AgentHub_v*_x64-setup.exe`（Release 说明含 SmartScreen 无签名提示话术）。

## 踩坑备忘（摘要，详见 .ai/memory/2026-10-31.md）

- rusqlite `last_insert_rowid()` 在 ON CONFLICT UPDATE 路径不可靠 → upsert 后显式 SELECT。
- SELECT 列与 `row.get` 索引一一对应（加列后全体 +1）。
- `bundle.targets` 不接受 "none"；旧 exe-only 方案 = 空数组 + `--no-bundle`（2026-10-31 起改 NSIS 安装版）。
- NSIS 静默更新：`/S /D=<dir>` 中 `/D=` 必须是最后一个参数且不加引号（NSIS 取到行尾）；cmd 链
  整条命令必须 `CommandExt::raw_arg` 原样传（普通 arg 会转义出 `\"`，cmd 不认）；spawn 后延迟
  500ms 再 `app.exit(0)`，别把 cmd 链一并带走（复用 whf-stock-board 实证参数）。
- 打包前必须 `taskkill agenthub.exe`（运行中的 exe 拒绝替换，os error 5）。
- sed 改过的文件必须重新 Read 再 Edit。
- Tauri dev 端口 1420 被残留 vite 占用时 netstat 定位 taskkill。
- `CommandExt::creation_flags` 必须每个文件单独 `use std::os::windows::process::CommandExt`。
- `chrono` 时间戳转换需 `use chrono::TimeZone` trait；子串搜索用 `str::contains` 而非 slice。
- ECharts 条件渲染下图表容器要用 callback ref（[] 依赖时普通 ref 为 null）。
