import { useCallback, useEffect, useState } from 'react';
import { enable as autostartEnable, disable as autostartDisable, isEnabled as autostartIsEnabled } from '@tauri-apps/plugin-autostart';
import { invoke } from '@tauri-apps/api/core';
import { recentOpLog, updateProxyGet, updateProxySet, unbindStatus, unbindRun, unbindReclaim, type OpLogRow, type UnbindReport } from '../api';
import { getDefaultPage, setDefaultPage, type DefaultPage } from '../stores/nav';
import { PageHeader } from '../common/components/PageHeader';
import { useThemeStore, type ThemeMode } from '../stores/theme';
import { toast } from '../stores/ui';

const CLOSE_TO_TRAY_KEY = 'agenthub.closeToTray';

/** M8 设置岛：主题 / 开机自启 / 关闭到托盘 / 最近操作 */
export function SettingsPage() {
  const { mode, resolved, setMode } = useThemeStore();
  const [autostart, setAutostart] = useState(true);
  const [closeToTray, setCloseToTray] = useState<boolean>(
    () => localStorage.getItem(CLOSE_TO_TRAY_KEY) !== '0',
  );
  const [ops, setOps] = useState<OpLogRow[]>([]);
  const [defaultPage, setDefaultPageState] = useState<DefaultPage>(() => getDefaultPage());
  const [proxyUrl, setProxyUrl] = useState('');
  const [proxySaved, setProxySaved] = useState('');
  const [savingProxy, setSavingProxy] = useState(false);
  // 解绑与卸载：状态探测 / 二次确认 / 执行中 / 报告
  const [unbound, setUnbound] = useState<boolean | null>(null);
  const [confirmUnbind, setConfirmUnbind] = useState(false);
  const [unbinding, setUnbinding] = useState(false);
  const [unbindReport, setUnbindReport] = useState<UnbindReport | null>(null);
  const [reclaiming, setReclaiming] = useState(false);

  useEffect(() => {
    void recentOpLog(10)
      .then(setOps)
      .catch(() => {});
    void unbindStatus()
      .then(setUnbound)
      .catch(() => setUnbound(false));
    void updateProxyGet()
      .then((u) => {
        // 未配置时预填默认本地代理（默认 127.0.0.1:7897，可随意改；清空保存 = 恢复直连）
        setProxyUrl(u || 'http://127.0.0.1:7897');
        setProxySaved(u);
      })
      .catch(() => {});
    // 自启动：以系统实际状态为准（浏览器预览回退 localStorage）
    (async () => {
      try {
        setAutostart(await autostartIsEnabled());
      } catch {
        setAutostart(localStorage.getItem('agenthub.autostart') !== '0');
      }
    })();
  }, []);

  const toggleAutostart = useCallback(async (on: boolean) => {
    setAutostart(on);
    localStorage.setItem('agenthub.autostart', on ? '1' : '0');
    try {
      if (on) await autostartEnable();
      else await autostartDisable();
      toast(on ? '已开启开机自启' : '已关闭开机自启');
    } catch {
      toast(on ? '自启动开启失败（需在应用内操作）' : '自启动关闭失败', 'err');
      setAutostart(!on);
    }
  }, []);

  const toggleCloseToTray = useCallback(async (on: boolean) => {
    setCloseToTray(on);
    localStorage.setItem(CLOSE_TO_TRAY_KEY, on ? '1' : '0');
    try {
      await invoke('set_close_to_tray', { enabled: on });
      toast(on ? '关闭将最小化到托盘' : '关闭将直接退出应用');
    } catch {
      toast('设置失败', 'err');
      setCloseToTray(!on);
    }
  }, []);

  const saveProxy = useCallback(async () => {
    setSavingProxy(true);
    try {
      await updateProxySet(proxyUrl.trim());
      setProxySaved(proxyUrl.trim());
      toast(proxyUrl.trim() ? '更新代理已保存，立即生效' : '已清除更新代理，恢复直连');
    } catch (e) {
      toast(String(e).replace(/^.*?"|".*$/g, '') || '保存失败', 'err');
    } finally {
      setSavingProxy(false);
    }
  }, [proxyUrl]);

  const modes: [ThemeMode, string][] = [
    ['system', '跟随系统'],
    ['light', '浅色'],
    ['dark', '深色'],
  ];

  /** 执行解绑（二次确认后）：恢复各 Agent 独立配置，移除指向 ~/.agenthub 的链接 */
  const doUnbind = useCallback(async () => {
    setUnbinding(true);
    try {
      const r = await unbindRun();
      setUnbindReport(r);
      setUnbound(true);
      setConfirmUnbind(false);
      toast(r.failed > 0 ? '解绑完成，但有失败项（详情见报告）' : '解绑完成，各 Agent 已恢复独立配置');
    } catch (e) {
      toast(String(e).replace(/^.*?"|".*$/g, '') || '解绑失败', 'err');
    } finally {
      setUnbinding(false);
    }
  }, []);

  /** 重新接管：清解绑标志 → skills 迁移 → MCP 同步 → 全局记忆统一（全部幂等） */
  const doReclaim = useCallback(async () => {
    setReclaiming(true);
    try {
      const steps = await unbindReclaim();
      setUnbound(false);
      setUnbindReport({
        steps,
        failed: 0,
        backup_dir: '',
        already_unbound: false,
      });
      toast('重新接管完成，链接已重建');
    } catch (e) {
      toast(String(e).replace(/^.*?"|".*$/g, '') || '重新接管失败', 'err');
    } finally {
      setReclaiming(false);
    }
  }, []);

  return (
    <div>
      <PageHeader title="Setting" sub="所有写操作均有备份，可在此管理偏好" />
      <div className="list">
        <div className="li">
          <div className="ic">◐</div>
          <div className="grow">
            <div className="nm">主题</div>
            <div className="ds">CSS variables 双主题 · 当前{resolved === 'dark' ? '深色' : '浅色'}</div>
          </div>
          {modes.map(([m, label]) => (
            <span
              key={m}
              className={`chip ${mode === m ? 'ok' : 'off'}`}
              style={{ cursor: 'pointer' }}
              onClick={() => setMode(m)}
            >
              {label}
            </span>
          ))}
        </div>
        <div className="li">
          <div className="ic">⌂</div>
          <div className="grow">
            <div className="nm">默认打开页面</div>
            <div className="ds">
              启动 AgentHub 时默认显示的岛（例外：首次安装默认打开顶栏 ❔「使用说明」页，方便快速上手）
            </div>
          </div>
          {(
            [
              ['dashboard', 'Usage 用量'],
              ['accounts', 'Account 账号'],
            ] as [DefaultPage, string][]
          ).map(([v, label]) => (
            <span
              key={v}
              className={`chip ${defaultPage === v ? 'ok' : 'off'}`}
              style={{ cursor: 'pointer' }}
              title={`下次启动默认打开 ${label}`}
              onClick={() => {
                setDefaultPageState(v);
                setDefaultPage(v);
                toast(`已设置：下次启动默认打开 ${label}`);
              }}
            >
              {label}
            </span>
          ))}
        </div>
        <div className="li">
          <div className="ic">▲</div>
          <div className="grow">
            <div className="nm">开机自启</div>
            <div className="ds">登录 Windows 后自动启动 AgentHub（Tauri autostart 插件）</div>
          </div>
          <span
            className={`sw${autostart ? ' on' : ''}`}
            onClick={() => void toggleAutostart(!autostart)}
          />
        </div>
        <div className="li">
          <div className="ic">⬇</div>
          <div className="grow">
            <div className="nm">关闭进入系统托盘</div>
            <div className="ds">点关闭不退出，最小化到托盘（托盘菜单可「打开面板 / 退出」）</div>
          </div>
          <span
            className={`sw${closeToTray ? ' on' : ''}`}
            onClick={() => void toggleCloseToTray(!closeToTray)}
          />
        </div>
        <div className="li">
          <div className="ic">⇅</div>
          <div className="grow">
            <div className="nm">更新代理</div>
            <div className="ds">
              检查更新 / 下载安装包经此代理访问 GitHub（默认 127.0.0.1:7897；代理工具换地址/端口直接改这里，
              支持 http / socks5，无需更新应用）；清空保存 = 恢复直连
            </div>
            <div className="flex" style={{ gap: 6, marginTop: 6 }}>
              <input
                style={{
                  border: '1px solid var(--color-border)',
                  borderRadius: 9,
                  padding: '6px 10px',
                  background: 'var(--color-surface)',
                  color: 'var(--color-text-1)',
                  fontSize: 12,
                  flex: 1,
                  minWidth: 0,
                  outline: 'none',
                }}
                placeholder="http://127.0.0.1:7897"
                value={proxyUrl}
                onChange={(e) => setProxyUrl(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && proxyUrl.trim() !== proxySaved) void saveProxy();
                }}
              />
              <button
                className="btn gh"
                disabled={savingProxy || proxyUrl.trim() === proxySaved}
                onClick={() => void saveProxy()}
              >
                保存
              </button>
            </div>
          </div>
          <span className={`chip ${proxySaved ? 'ok' : 'off'}`}>
            {proxySaved ? '走代理' : '直连'}
          </span>
        </div>
        <div className="li">
          <div className="ic">⛓</div>
          <div className="grow">
            <div className="nm">解除绑定（卸载 AgentHub 前使用）</div>
            <div className="ds">
              把 Skills / 全局记忆还原为各 Agent 独立配置，移除指向 ~/.agenthub 的链接，卸载本应用后
              WorkBuddy / ZCode / Claude Code 不会再出现技能、记忆异常；MCP / 模型配置自包含，保留现状仅停止同步。
              ~/.agenthub 数据（用量历史、备份）全部保留。
            </div>
            {unbindReport && (
              <div
                style={{
                  marginTop: 6,
                  padding: '6px 10px',
                  borderRadius: 9,
                  background: 'var(--color-surface)',
                  border: '1px solid var(--color-border)',
                  fontSize: 11.5,
                  color: 'var(--color-text-2)',
                  lineHeight: 1.7,
                  maxHeight: 180,
                  overflow: 'auto',
                }}
              >
                {unbindReport.steps.map((s, i) => (
                  <div key={i}>{s}</div>
                ))}
                {unbindReport.backup_dir && (
                  <div style={{ color: 'var(--color-text-3)' }}>归档：{unbindReport.backup_dir}</div>
                )}
              </div>
            )}
            {confirmUnbind ? (
              <div className="flex" style={{ gap: 6, marginTop: 6 }}>
                <button className="btn" disabled={unbinding} onClick={() => void doUnbind()}>
                  {unbinding ? '解绑中…（秒级，请勿关闭）' : '确认解绑：还原各 Agent 独立配置'}
                </button>
                <button className="btn gh" disabled={unbinding} onClick={() => setConfirmUnbind(false)}>
                  取消
                </button>
              </div>
            ) : unbound ? (
              <div className="flex" style={{ gap: 6, marginTop: 6 }}>
                <button className="btn gh" disabled={reclaiming} onClick={() => void doReclaim()}>
                  {reclaiming ? '接管中…' : '重新接管'}
                </button>
              </div>
            ) : (
              <div className="flex" style={{ gap: 6, marginTop: 6 }}>
                <button className="btn gh" disabled={unbinding || unbound === null} onClick={() => setConfirmUnbind(true)}>
                  解除绑定
                </button>
              </div>
            )}
          </div>
          <span className={`chip ${unbound ? 'ok' : 'off'}`}>
            {unbound === null ? '…' : unbound ? '已解绑' : '已接管'}
          </span>
        </div>
        <div className="li">
          <div className="ic">↻</div>
          <div className="grow">
            <div className="nm">操作日志 op_log（最近 10 条）</div>
            <div className="ds">
              {ops.length === 0
                ? '暂无写操作记录'
                : ops.map((o) => `${o.created_at ?? ''} ${o.action} ${o.target ?? ''}`).join('  ·  ')}
            </div>
          </div>
          <span className="chip off">{ops.length} 条</span>
        </div>
      </div>
      <div style={{ fontSize: 10.5, color: 'var(--color-text-3)', marginTop: 10 }}>
        · 所有对目标配置文件的写入：先备份 → 原子写 → 校验 → 失败回滚 · 中央仓库 ~/.agenthub/ ·
        数据全部本地，不外发
      </div>
    </div>
  );
}
