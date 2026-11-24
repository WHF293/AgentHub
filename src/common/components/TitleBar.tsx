import { useEffect, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { getVersion } from '@tauri-apps/api/app';
import { listen } from '@tauri-apps/api/event';
import { useDesktopChrome } from '../useDesktopChrome';
import { useNavStore } from '../../stores/nav';
import { useThemeStore } from '../../stores/theme';
import { toast } from '../../stores/ui';
import { updateCheck, updateInstall, type UpdateCheckInfo } from '../../api';
import { ConfirmDialog } from './ConfirmDialog';
import { LogoMark } from './LogoMark';

/**
 * 自绘标题栏 —— whf-stock-board AgentWindowTitlebar.vue 的 React 等价实现。
 *
 * decorations:false 后的替代：data-tauri-drag-region 拖动（Tauri 注入脚本处理，
 * 按钮不带该属性不会误触发）+ 双击最大化 + 自绘 最小化/最大化/关闭 三键；
 * 关闭行为 = 关到托盘（Rust 侧 CloseRequested 拦截）。
 * 命中区与参考项目一致（w-11/h-9），关闭键 hover #E81123。
 */
export function TitleBar() {
  const { desktopChrome, canControlWindow } = useDesktopChrome();
  const { resolved, setMode } = useThemeStore();
  const [isMaximized, setIsMaximized] = useState(false);
  const [ver, setVer] = useState('');
  const [updState, setUpdState] = useState<'idle' | 'checking' | 'downloading'>('idle');
  const [updInfo, setUpdInfo] = useState<UpdateCheckInfo | null>(null);
  const [confirmUpdate, setConfirmUpdate] = useState(false);
  const [dlPercent, setDlPercent] = useState(0);

  useEffect(() => {
    void getVersion()
      .then(setVer)
      .catch(() => {});
    let un: (() => void) | null = null;
    listen<{ received: number; total: number }>('update-download-progress', (e) => {
      const pct = e.payload.total > 0 ? Math.round((e.payload.received / e.payload.total) * 100) : 0;
      setDlPercent(pct);
    })
      .then((f) => {
        un = f;
      })
      .catch(() => {});
    return () => {
      un?.();
    };
  }, []);

  const checkUpdate = async () => {
    if (updState !== 'idle') return;
    setUpdState('checking');
    try {
      const info = await updateCheck();
      if (!info.has_update) {
        toast(`已是最新版本 v${info.current}`);
        setUpdState('idle');
      } else {
        setUpdInfo(info);
        setConfirmUpdate(true);
        setUpdState('idle');
      }
    } catch (e) {
      toast(`检查更新失败：${e instanceof Error ? e.message : String(e)}`, 'err');
      setUpdState('idle');
    }
  };

  const applyUpdate = async () => {
    if (!updInfo) return;
    setConfirmUpdate(false);
    setUpdState('downloading');
    setDlPercent(0);
    try {
      // 成功后应用自动退出并由 bat 替换重启新版（gh 通道无逐字节进度，完成即退出）
      await updateInstall({
        tag: updInfo.tag,
        asset_name: updInfo.asset_name,
        asset_url: updInfo.asset_url,
      });
    } catch (e) {
      toast(`更新失败：${e instanceof Error ? e.message : String(e)}`, 'err');
      setUpdState('idle');
    }
  };

  useEffect(() => {
    if (!canControlWindow) return;
    const appWindow = getCurrentWindow();
    let unlisten: (() => void) | null = null;
    let disposed = false;

    const syncMaximized = () => {
      void appWindow
        .isMaximized()
        .then((v) => {
          if (!disposed) setIsMaximized(v);
        })
        .catch(() => {
          /* 查询失败保持原值（非致命，仅影响按钮图标） */
        });
    };

    syncMaximized();
    // 防抖：拖拽调整窗口时 resize 事件逐帧触发，每帧一次 IPC 往返会造成拖拽卡顿
    let debounce: ReturnType<typeof setTimeout> | null = null;
    void appWindow
      .onResized(() => {
        if (debounce) clearTimeout(debounce);
        debounce = setTimeout(syncMaximized, 200);
      })
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      });

    return () => {
      disposed = true;
      if (debounce) clearTimeout(debounce);
      unlisten?.();
    };
  }, [canControlWindow]);

  if (!desktopChrome) return null;

  const minimizeWindow = () => {
    if (!canControlWindow) return;
    void getCurrentWindow().minimize();
  };
  const toggleMaximizeWindow = () => {
    if (!canControlWindow) return;
    void getCurrentWindow().toggleMaximize();
  };
  const closeWindow = () => {
    if (!canControlWindow) return;
    void getCurrentWindow().close(); // Rust 侧拦截为「关闭到托盘」
  };

  return (
    <div data-tauri-drag-region className="titlebar">
      <div className="brand" data-tauri-drag-region>
        <LogoMark size={15} />
        <span className="tt">AgentHub — AI Agent 统一管理</span>
      </div>
      <div className="flex h-full items-stretch">
        <button
          type="button"
          className="ver-chip"
          title={
            updState === 'checking'
              ? '正在检查更新…'
              : updState === 'downloading'
                ? `正在下载更新 ${dlPercent}%`
                : `v${ver || '…'} · 点击检查更新`
          }
          onClick={() => void checkUpdate()}
        >
          {updState === 'checking'
            ? '检查中…'
            : updState === 'downloading'
              ? `更新 ${dlPercent}%`
              : `v${ver || '…'}`}
        </button>
        {/* Architecture：从侧栏移入顶栏，只显示 icon（原侧栏第 9 岛） */}
        <button
          type="button"
          aria-label="Architecture"
          title="Architecture · 项目架构总览"
          className="winbtn"
          style={{ fontSize: 12, lineHeight: 1 }}
          onClick={() => useNavStore.getState().setActive('architecture')}
        >
          ⬡
        </button>
        <button
          type="button"
          aria-label="切换主题"
          title={`主题：${resolved === 'dark' ? '深色' : '浅色'}（点击切换）`}
          className="winbtn"
          onClick={() => setMode(resolved === 'dark' ? 'light' : 'dark')}
        >
          <svg width="13" height="13" viewBox="0 0 512 512" aria-hidden="true">
            <circle cx="256" cy="256" r="170" fill="none" stroke="currentColor" strokeWidth="44" />
            <path d="M256 86 a170 170 0 0 1 0 340 z" fill="currentColor" />
          </svg>
        </button>
        <button type="button" aria-label="最小化" className="winbtn" onClick={minimizeWindow}>
          <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">
            <path d="M1 6h10" stroke="currentColor" strokeWidth="1.2" />
          </svg>
        </button>
        <button
          type="button"
          aria-label={isMaximized ? '还原' : '最大化'}
          className="winbtn"
          onClick={toggleMaximizeWindow}
        >
          {isMaximized ? (
            <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">
              <rect x="3.5" y="1.5" width="7" height="7" rx="1" stroke="currentColor" strokeWidth="1.2" fill="none" />
              <path d="M8.5 10.5h-7v-7" stroke="currentColor" strokeWidth="1.2" fill="none" />
            </svg>
          ) : (
            <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">
              <rect x="1.5" y="1.5" width="9" height="9" rx="1" stroke="currentColor" strokeWidth="1.2" fill="none" />
            </svg>
          )}
        </button>
        <button type="button" aria-label="关闭" className="winbtn close" onClick={closeWindow}>
          <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">
            <path d="M1.5 1.5l9 9M10.5 1.5l-9 9" stroke="currentColor" strokeWidth="1.2" />
          </svg>
        </button>
      </div>

      {confirmUpdate && updInfo && (
        <ConfirmDialog
          title="发现新版本"
          message={`当前版本 v${updInfo.current} → 最新版本 v${updInfo.latest}\n\n点击「立即更新」将自动下载安装包并静默安装（下载完成后应用会退出，安装器静默装回原目录并自动重启新版，约需数秒）。`}
          confirmLabel="立即更新"
          onConfirm={() => void applyUpdate()}
          onCancel={() => setConfirmUpdate(false)}
        />
      )}
    </div>
  );
}
