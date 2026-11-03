# AgentHub 助手 — uTools 插件

WorkBuddy 两个高频操作的轻量入口：**签到领积分** + **账号槽位切换**。
插件本体零业务逻辑，全部通过 AgentHub CLI（`agenthub.exe`）完成，插件只负责触发和展示。

```
utools-plugin/
├── plugin.json    uTools 插件清单（两个 list 模式功能，无 GUI 页面）
├── preload.js     桥接层：spawn CLI → 解析 JSON → uTools 列表渲染
├── logo.png       占位 logo（1x1，打包前可自行替换）
└── bin/           可选：放一份 agenthub.exe 随插件携带
```

## 前置条件

1. **AgentHub 需重新打包安装**：本插件依赖 2026-11-03 新增的两个无头入口
   （`--checkin` / `--wb-switch`，见 `src-tauri/src/main.rs`），旧版 exe 没有。
2. 签到受 GUI 里的**自动领取开关**约束（`~/.agenthub/keys/wb-checkin-auto`），
   且要求 AgentHub 插件表中 workbuddy 处于启用态——与 GUI 完全同一开关，不会绕过。

## 安装插件

1. 安装 uTools「开发者工具」插件
2. 新建/选择项目指向本目录的 `plugin.json` → 开启运行
3. uTools 主输入框输入 `切号` / `签到` 即可直达
4. 验证通过后可在开发者工具里「打包」成 `.upx` 长期使用

exe 查找顺序：插件 `bin/agenthub.exe` → `%LOCALAPPDATA%\AgentHub\agenthub.exe`（NSIS 默认安装位）。
用已安装版即可，不必往 bin/ 里塞。

## 自动领积分：走 Windows 计划任务（不需要 uTools）

「自动」这半边 uTools 插件做不到（插件隐藏即销毁，无常驻能力），
正确姿势是计划任务每天跑一次 CLI：

```bat
schtasks /Create /TN "AgentHub WB签到" /TR "\"%LOCALAPPDATA%\AgentHub\agenthub.exe\" --checkin" /SC DAILY /ST 09:10 /F
```

说明：
- release exe 是 GUI 子系统（`windows_subsystem = "windows"`），计划任务触发**不弹黑窗**，
  stdout 也不可见——结果记录在 agenthub.db（`record_checkin`），AgentHub GUI 里看签到记录。
- 需电脑开机在 09:10 前后在线（可按需改 `/ST`；漏跑当天手动在 uTools 里补签即可，接口幂等）。

## CLI 直测（不装插件也能用）

```bash
"%LOCALAPPDATA%\AgentHub\agenthub.exe" --wb-status          # 槽位/进程/当前账号（JSON）
"%LOCALAPPDATA%\AgentHub\agenthub.exe" --checkin            # 全账号签到（JSON 数组）
"%LOCALAPPDATA%\AgentHub\agenthub.exe" --wb-switch <slot-id>          # 切号（WB 须未运行）
"%LOCALAPPDATA%\AgentHub\agenthub.exe" --wb-switch <slot-id> --force  # 先 taskkill WB 再切（调用方须已二次确认）
```

## 已知验证点（未实测项，诚实标注）

- release 版 exe 为 GUI 子系统，Node `spawn` 捕获其 stdout 理论可行（管道句柄会传给子进程），
  但**插件内实际拉起 CLI 未在 uTools 环境实测**——首次跑若拿不到输出，优先怀疑这一条。
- `--checkin` 会经 WB 自身解密 accessToken（内存内，与 GUI 同一信任边界），首次跑建议白天试一次。
