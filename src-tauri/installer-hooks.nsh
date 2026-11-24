;; installer-hooks.nsh —— AgentHub NSIS 安装/卸载钩子（Tauri v2 bundle.windows.nsis.installerHooks）
;;
;; 卸载前询问用户是否解除绑定（方案 §3）：
;;   「是」（默认按钮，回车即选）：卸载器同步执行 agenthub.exe --unbind，
;;     恢复 Skills / 全局记忆为各 Agent 独立实体，移除指向 ~/.agenthub 的链接，
;;     卸载后 WorkBuddy / ZCode / Claude Code 不会出现技能、记忆等异常；
;;   「否」：保留链接直接卸载（各 Agent 可能出现配置异常，重新安装 AgentHub 可修复）。
;;
;; 说明：NSIS MessageBox 无复选框控件，且 Tauri v2 卸载 Section 执行期无法插入自定义
;; nsDialogs 页面，故用「是/否 + 默认是」等价"默认勾选、可取消"交互。
;;
;; 二进制名：Tauri v2 mainBinaryName 缺省 = cargo 包名 → agenthub.exe；
;; 兼容 productName 命名的旧安装（AgentHub.exe），两个都探测。

!macro NSIS_HOOK_PREUNINSTALL
  MessageBox MB_YESNO|MB_ICONQUESTION|MB_TOPMOST \
    "是否在卸载前解除 AgentHub 对各 Agent 的接管绑定？$\n$\n\
    「是」（推荐）：恢复 Skills / 全局记忆为各 Agent 独立配置，移除指向 AgentHub 的链接，卸载后 WorkBuddy / ZCode / Claude Code 不会出现技能、记忆等异常。$\n$\n\
    「否」：保留当前链接直接卸载，各 Agent 可能出现技能、记忆丢失等异常。" \
    IDNO skip_agenthub_unbind
  IfFileExists "$INSTDIR\agenthub.exe" 0 try_agenthub_prodname
    ExecWait '"$INSTDIR\agenthub.exe" --unbind'
    DetailPrint "AgentHub 解绑完成"
    Goto agenthub_unbind_done
  try_agenthub_prodname:
    IfFileExists "$INSTDIR\AgentHub.exe" 0 agenthub_unbind_done
      ExecWait '"$INSTDIR\AgentHub.exe" --unbind'
      DetailPrint "AgentHub 解绑完成"
  agenthub_unbind_done:
skip_agenthub_unbind:
!macroend
