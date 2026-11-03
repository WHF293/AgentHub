import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  pluginInstall,
  pluginList,
  pluginToggle,
  pluginUninstall,
  type PluginInfo,
} from '../api';
import { refreshPlugins } from '../stores/plugins';
import { agentMeta, getConfiguredColor, setAgentColor } from '../agents';
import { PageHeader } from '../common/components/PageHeader';
import { ConfirmDialog } from '../common/components/ConfirmDialog';
import { toast } from '../stores/ui';

/** 预设色板：与 UI 主题协调的一组身份色 */
const PRESET_COLORS = [
  '#2E9E74', '#4CC79C', '#4AA3DF', '#5B8DEF', '#8B5CF6',
  '#C05FD9', '#E8698D', '#D96459', '#D99A2B', '#5FB861',
  '#3BB0C9', '#6B7FDB',
];

/** Agent 主题色设置弹窗：预设色板 + 自定义取色器 + 恢复默认，保存后全局立即生效 */
function ColorDialog({
  agentId,
  agentName,
  onClose,
}: {
  agentId: string;
  agentName: string;
  onClose: () => void;
}) {
  const meta = agentMeta(agentId);
  const configured = getConfiguredColor(agentId);
  const [sel, setSel] = useState<string>(configured ?? meta.defaultColor ?? '#8CA096');

  const commit = (color: string | null) => {
    setAgentColor(agentId, color);
    toast(`${agentName} 主题色已更新（全局立即生效）`);
    onClose();
  };

  return (
    <div
      style={{
        position: 'fixed', inset: 0, background: 'rgba(0,0,0,.35)', zIndex: 200,
        display: 'flex', alignItems: 'center', justifyContent: 'center',
      }}
      onClick={onClose}
    >
      <div className="card" style={{ width: 420, cursor: 'default', padding: 20 }} onClick={(e) => e.stopPropagation()}>
        <div className="flex items-center" style={{ gap: 9 }}>
          <span
            style={{ width: 14, height: 14, borderRadius: 999, background: sel, flex: 'none', boxShadow: '0 0 0 3px color-mix(in srgb, currentColor 8%, transparent)' }}
          />
          <b style={{ fontSize: 14 }}>{agentName} · 主题色</b>
        </div>
        <div style={{ fontSize: 11.5, color: 'var(--color-text-3)', marginTop: 5 }}>
          该颜色将全局生效：开关、chip、卡片高亮、用量图表、主按钮等所有身份色位置
        </div>

        <div style={{ fontSize: 11, fontWeight: 700, color: 'var(--color-text-2)', margin: '14px 0 8px' }}>预设色板</div>
        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(6, 1fr)', gap: 8 }}>
          {PRESET_COLORS.map((c) => (
            <button
              key={c}
              type="button"
              title={c}
              onClick={() => setSel(c)}
              style={{
                width: '100%', aspectRatio: '1', borderRadius: 9, border: sel === c ? '2.5px solid var(--color-text-1)' : '2.5px solid transparent',
                background: c, cursor: 'pointer', padding: 0, outline: sel === c ? '2px solid var(--color-surface)' : undefined,
              }}
            />
          ))}
        </div>

        <div className="flex items-center" style={{ gap: 10, marginTop: 14 }}>
          <label
            className="flex items-center"
            style={{ gap: 8, fontSize: 11.5, fontWeight: 700, color: 'var(--color-text-2)', cursor: 'pointer' }}
          >
            自定义
            <input
              type="color"
              value={sel}
              onChange={(e) => setSel(e.target.value)}
              style={{ width: 34, height: 26, border: '1px solid var(--color-border)', borderRadius: 7, cursor: 'pointer', padding: 0, background: 'none' }}
            />
          </label>
          <span style={{ fontSize: 11, color: 'var(--color-text-3)', fontFamily: 'Consolas, monospace' }}>{sel.toUpperCase()}</span>
        </div>

        <div className="flex items-center" style={{ gap: 8, marginTop: 18 }}>
          {configured && (
            <button className="btn gh" onClick={() => commit(null)}>
              ↺ 恢复默认
            </button>
          )}
          <span style={{ flex: 1 }} />
          <button className="btn gh" onClick={onClose}>取消</button>
          <button className="btn p" onClick={() => commit(sel)}>保存</button>
        </div>
      </div>
    </div>
  );
}

/** 单个插件卡片（installed = 安装态；卸载后仅保留「重新安装」入口，数据保留） */
function PluginCard({
  p,
  onToggle,
  onInstall,
  onUninstall,
  busy,
  onColor,
}: {
  p: PluginInfo;
  onToggle: (agent: string, enabled: boolean) => void;
  onInstall: (agent: string) => void;
  onUninstall: () => void;
  busy: string | null;
  onColor: (agent: string) => void;
}) {
  const meta = agentMeta(p.agent);
  const active = p.installed && p.enabled;
  return (
    <div className="item-card sm" style={{ opacity: p.installed ? (active ? 1 : 0.72) : 0.5 }}>
      <div className="card-top">
        {p.installed ? (
          <span
            className={`sw${active ? ' on' : ''}`}
            style={meta.accentVar && active ? { background: meta.accentVar } : undefined}
            title={active ? '已启用（点击停用）' : '已停用（点击启用）'}
            onClick={() => !busy && onToggle(p.agent, !p.enabled)}
          />
        ) : (
          <span className="sw" title="已卸载" />
        )}
        <div className="grow" style={{ minWidth: 0 }}>
          <div className="nm flex items-center" style={{ gap: 7 }}>
            {meta.accentVar && (
              <span
                title="点击设置主题色（全局生效）"
                style={{
                  width: 9, height: 9, borderRadius: 999, background: meta.accentVar, flex: 'none',
                  cursor: 'pointer', boxShadow: '0 0 0 2px color-mix(in srgb, currentColor 10%, transparent)',
                  transition: 'transform .15s',
                }}
                onClick={(e) => {
                  e.stopPropagation();
                  onColor(p.agent);
                }}
                onMouseEnter={(e) => (e.currentTarget.style.transform = 'scale(1.35)')}
                onMouseLeave={(e) => (e.currentTarget.style.transform = 'scale(1)')}
              />
            )}
            {p.display_name}
          </div>
        </div>
        {!p.installed ? (
          <span className="chip warn">已卸载</span>
        ) : (
          <span className={`chip ${active ? 'ok' : 'off'}`}
            title={p.source === 'auto' ? '首次运行时按本机安装探测自动启用' : '由用户手动开启/关闭'}
          >
            {active ? '已启用' : '已停用'}
          </span>
        )}
      </div>
      <div className="ds desc-clamp" style={{ flex: 1, minHeight: 0 }} title={p.desc}>
        {p.desc}
      </div>
      <div className="card-actions" style={{ alignItems: 'center' }}>
        <span
          className={`chip ${p.installed_probe ? 'ok' : 'off'}`}
          title="本机安装探测（目录存在性，仅提示，与安装态无关）"
        >
          {p.installed_probe ? '✓ 已检测到本机安装' : '本机未检测到'}
        </span>
        {p.caps.skills && <span className="chip off">skills</span>}
        {p.caps.mcp && <span className="chip off">mcp</span>}
        <span style={{ flex: 1 }} />
        {p.installed ? (
          <button
            className="btn gh"
            style={{ fontSize: 11, padding: '3px 10px' }}
            disabled={busy !== null}
            title="卸载 = 全岛隐藏 + 停止参与（数据保留，可随时重装）"
            onClick={() => onUninstall()}
          >
            卸载
          </button>
        ) : (
          <button
            className="btn p"
            style={{ fontSize: 11, padding: '3px 10px' }}
            disabled={busy !== null}
            onClick={() => onInstall(p.agent)}
          >
            重新安装
          </button>
        )}
      </div>
    </div>
  );
}

/** Plugin 岛 —— 可插拔 agent 插件管理（VS Code 式：默认全装、可卸载重装、启停持久化） */
export function PluginsPage() {
  const [items, setItems] = useState<PluginInfo[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [colorDlg, setColorDlg] = useState<string | null>(null);
  const [uninstalling, setUninstalling] = useState<PluginInfo | null>(null);
  const [query, setQuery] = useState('');

  const refresh = useCallback(async () => {
    setError(null);
    try {
      setItems((await pluginList()).items);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const apply = async (action: () => Promise<unknown>, done: string) => {
    setError(null);
    setMessage(null);
    try {
      await action();
      refreshPlugins();
      setMessage(done);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const toggle = (agent: string, enabled: boolean) => {
    setBusy(agent);
    const meta = agentMeta(agent);
    void apply(
      () => pluginToggle(agent, enabled),
      `${meta.name} 插件已${enabled ? '启用' : '停用'}（已持久化；各功能岛已同步置灰或恢复）`,
    );
  };

  const install = (agent: string) => {
    setBusy(agent);
    const meta = agentMeta(agent);
    void apply(
      () => pluginInstall(agent),
      `${meta.name} 已重新安装并启用（历史数据一直在，从未删除）`,
    );
  };

  const uninstall = (agent: string) => {
    setBusy(agent);
    const meta = agentMeta(agent);
    void apply(
      () => pluginUninstall(agent),
      `${meta.name} 已卸载（联动停用；用量历史与配置保留，可随时重新安装）`,
    );
  };

  const q = query.trim().toLowerCase();
  const filtered = useMemo(() => {
    const list = items ?? [];
    if (!q) return list;
    return list.filter(
      (p) =>
        p.display_name.toLowerCase().includes(q) ||
        p.agent.toLowerCase().includes(q) ||
        p.desc.toLowerCase().includes(q),
    );
  }, [items, q]);
  const installedList = filtered.filter((p) => p.installed);
  const uninstalledList = filtered.filter((p) => !p.installed);
  const installedTotal = (items ?? []).filter((p) => p.installed).length;
  const enabledCount = (items ?? []).filter((p) => p.installed && p.enabled).length;

  const card = (p: PluginInfo) => (
    <PluginCard
      key={p.agent}
      p={p}
      onToggle={toggle}
      onInstall={install}
      onUninstall={() => setUninstalling(p)}
      busy={busy}
      onColor={setColorDlg}
    />
  );

  return (
    <div>
      <PageHeader
        title={`Plugin（${items?.length ?? '…'}）`}
        sub={`内置插件随应用分发 · 默认全部安装 · 已安装 ${installedTotal ?? '…'} / 已启用 ${enabledCount ?? '…'} · 卸载仅隐藏与停用（数据保留，可随时重装）`}
      />

      <div className="mb-3 flex items-center" style={{ gap: 8 }}>
        <input
          className="flex"
          placeholder="搜索插件（名称 / 描述 / id）…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          style={{
            border: '1px solid var(--color-border)',
            borderRadius: 9,
            padding: '6px 12px',
            background: 'var(--color-surface)',
            color: 'var(--color-text-1)',
            fontSize: 12,
            outline: 'none',
            minWidth: 200,
          }}
        />
        <span className="chip off" title="社区插件市场（后续版本：按 AgentAdapter 接口贡献的插件包编译接入）">
          ⛁ 插件市场（敬请期待）
        </span>
        <span style={{ flex: 1 }} />
        {message && <span className="chip ok">{message}</span>}
        <button
          className="btn gh"
          onClick={() => {
            void refresh();
          }}
        >
          ↻ 重新探测
        </button>
      </div>

      {error && (
        <div
          style={{
            fontSize: 12,
            color: 'var(--color-danger)',
            background: 'color-mix(in srgb, var(--color-danger) 8%, transparent)',
            borderRadius: 10,
            padding: '8px 12px',
            marginBottom: 12,
            whiteSpace: 'pre-wrap',
          }}
        >
          {error}
        </div>
      )}

      {items === null ? (
        <div className="card" style={{ color: 'var(--color-text-3)' }}>加载中…</div>
      ) : items.length === 0 ? (
        <div className="card" style={{ color: 'var(--color-text-3)' }}>无已注册适配器</div>
      ) : (
        <>
          <div className="flex items-center" style={{ gap: 8, margin: '4px 0 8px' }}>
            <b style={{ fontSize: 12, color: 'var(--color-text-2)' }}>已安装</b>
            <span className="chip off">{installedList.length}</span>
          </div>
          {installedList.length === 0 ? (
            <div className="card" style={{ color: 'var(--color-text-3)', marginBottom: 14 }}>
              {q ? '搜索无匹配的已安装插件' : '没有已安装插件'}
            </div>
          ) : (
            <div className="item-grid">
              {installedList.map(card)}
            </div>
          )}

          {uninstalledList.length > 0 && (
            <>
              <div className="flex items-center" style={{ gap: 8, margin: '16px 0 8px' }}>
                <b style={{ fontSize: 12, color: 'var(--color-text-2)' }}>已卸载</b>
                <span className="chip off">{uninstalledList.length}</span>
                <span style={{ fontSize: 11, color: 'var(--color-text-3)' }}>
                  卸载不删数据：用量历史与目标文件配置均保留
                </span>
              </div>
              <div className="item-grid" style={{ opacity: 0.85 }}>
                {uninstalledList.map(card)}
              </div>
            </>
          )}
        </>
      )}

      {colorDlg && (
        <ColorDialog
          agentId={colorDlg}
          agentName={agentMeta(colorDlg).name}
          onClose={() => setColorDlg(null)}
        />
      )}

      {uninstalling && (
        <ConfirmDialog
          title={`卸载插件 · ${uninstalling.display_name}`}
          message={`卸载后 ${uninstalling.display_name} 将从所有功能岛隐藏并停止参与（Skills / MCP / 用量采集 / 模型托管全部停止）。\n\n数据全部保留：Usage 岛历史用量、已写入目标文件的配置均不删除，随时可重新安装恢复。确认卸载？`}
          confirmLabel="卸载（保留数据）"
          danger
          onConfirm={() => {
            const agent = uninstalling.agent;
            setUninstalling(null);
            uninstall(agent);
          }}
          onCancel={() => setUninstalling(null)}
        />
      )}

      <div className="card" style={{ padding: 14, marginTop: 12 }}>
        <b style={{ fontSize: 12.5 }}>工作方式</b>
        <div style={{ fontSize: 11.5, color: 'var(--color-text-2)', lineHeight: 1.7, marginTop: 6 }}>
          · 每个插件 = 一个独立包（packages/workbuddy / packages/zcode / packages/claude），
          经 AgentAdapter 接口接入，内置随应用分发、默认全部安装。
          <br />
          · <b>卸载</b> = 全岛隐藏 + 全部门控关闭（联动停用），用量历史与目标文件配置全部保留，
          「重新安装」即恢复；<b>启停</b> = 参与开关（持久化，重启后以本页配置为准）。
          <br />
          · 首次开启应用自动探测本机安装了哪些 agent 并默认启用装了的；未装的不启用但保持已安装状态。
          <br />
          · 新增插件（社区贡献）：按 AgentAdapter 接口新增 packages/&lt;id&gt; 包 → 组装层注册 → 重编译，
          详见 AGENTS.md「插件接入指南」。
          <br />· 点 agent 名旁的<b>色点</b>可自定义该 agent 的主题色（预设色板 / 自定义取色器），
          全局生效并持久化；新插件注册即有默认身份色。
        </div>
      </div>
    </div>
  );
}
