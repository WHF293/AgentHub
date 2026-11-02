import { useCallback, useEffect, useState } from 'react';
import {
  mcpAdopt,
  mcpDelete,
  mcpOverview,
  mcpSave,
  mcpTestConnection,
  mcpToggle,
  mcpValidate,
  type McpOverviewItem,
  type McpOverviewPayload,
  type McpServerInput,
} from '../api';
import { agentChipClass, agentMeta } from '../agents';
import { PageHeader } from '../common/components/PageHeader';
import { ConfirmDialog } from '../common/components/ConfirmDialog';

const STATUS_TEXT: Record<string, string> = {
  ok: '✓',
  disabled: '禁用',
  missing: '未分发',
  drifted: '漂移',
  unsupported: '不支持此类型',
};
const STATUS_COLOR: Record<string, string> = {
  ok: 'ok',
  disabled: 'off',
  missing: 'off',
  drifted: 'warn',
  unsupported: 'warn',
};

/** 解析用户粘贴的 MCP JSON：支持 {mcpServers:{名:条目}}、{名:条目} 批量，编辑态接受裸条目 */
function parseMcpJson(
  text: string,
  fixedName?: string,
): { name: string; entry: Record<string, unknown> }[] {
  const v: unknown = JSON.parse(text);
  if (!v || typeof v !== 'object' || Array.isArray(v)) {
    throw new Error('JSON 需为对象');
  }
  const looksLikeEntry = (x: Record<string, unknown>) =>
    ['type', 'command', 'url', 'args', 'env', 'headers', 'disabled', 'enabled'].some((k) => k in x);
  if (fixedName) {
    const obj = v as Record<string, unknown>;
    if (looksLikeEntry(obj)) return [{ name: fixedName, entry: obj }];
    const keys = Object.keys(obj);
    if (
      keys.length === 1 &&
      obj[keys[0]] &&
      typeof obj[keys[0]] === 'object' &&
      !Array.isArray(obj[keys[0]])
    ) {
      return [{ name: fixedName, entry: obj[keys[0]] as Record<string, unknown> }];
    }
    throw new Error('编辑模式下 JSON 需为单个 server 条目，如 { "type": "stdio", "command": "…" }');
  }
  const obj = v as Record<string, unknown>;
  let map: Record<string, unknown>;
  if (obj.mcpServers && typeof obj.mcpServers === 'object' && !Array.isArray(obj.mcpServers)) {
    map = obj.mcpServers as Record<string, unknown>;
  } else {
    const vals = Object.values(obj);
    if (vals.length > 0 && vals.every((x) => x && typeof x === 'object' && !Array.isArray(x))) {
      map = obj;
    } else {
      throw new Error('JSON 需为 { "mcpServers": { 名称: {…} } } 或 { 名称: {…} }');
    }
  }
  return Object.entries(map).map(([name, entry]) => ({ name, entry: entry as Record<string, unknown> }));
}

function entryToInput(
  name: string,
  e: Record<string, unknown>,
  enabled: Record<string, boolean>,
): McpServerInput {
  const args = Array.isArray(e.args)
    ? e.args.map(String)
    : typeof e.args === 'string'
      ? (e.args as string).split(/\s+/).filter(Boolean)
      : [];
  const kv = (x: unknown): Record<string, string> =>
    x && typeof x === 'object' && !Array.isArray(x)
      ? Object.fromEntries(
          Object.entries(x as Record<string, unknown>).map(([k, v]) => [k, String(v)]),
        )
      : {};
  return {
    name,
    type: String(e.type ?? 'stdio'),
    command: typeof e.command === 'string' ? e.command : null,
    args,
    url: typeof e.url === 'string' ? e.url : null,
    env: kv(e.env),
    headers: kv(e.headers),
    enabled,
  };
}

/** D2 新增/编辑弹窗（表单 / JSON 两种输入方式）；mcpAgents = 已启用且具备 MCP 能力的插件端 */
function ServerModal({
  initial,
  mcpAgents,
  onClose,
  onSaved,
}: {
  initial: McpOverviewItem | null;
  mcpAgents: { agent: string; display_name: string }[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const [name, setName] = useState(initial?.name ?? '');
  const [kind, setKind] = useState(initial?.type ?? 'stdio');
  const [command, setCommand] = useState(initial?.command ?? '');
  const [argsText, setArgsText] = useState('');
  const [url, setUrl] = useState(initial?.url ?? '');
  const [envText, setEnvText] = useState('');
  // 各插件端分发开关（agent id → enabled；默认全开，编辑时取该行现状）
  const [enabled, setEnabled] = useState<Record<string, boolean>>(() => {
    const init: Record<string, boolean> = {};
    for (const a of mcpAgents) {
      init[a.agent] = initial ? (initial.agents.find((x) => x.agent === a.agent)?.enabled ?? true) : true;
    }
    return init;
  });
  const [err, setErr] = useState<string | null>(null);
  const [mode, setMode] = useState<'form' | 'json'>('form');
  const [jsonText, setJsonText] = useState<string>(() =>
    initial
      ? JSON.stringify(
          {
            [initial.name]: {
              type: initial.type,
              ...(initial.command ? { command: initial.command } : {}),
              ...(initial.url ? { url: initial.url } : {}),
            },
          },
          null,
          2,
        )
      : JSON.stringify(
          {
            mcpServers: {
              'tavily-search': {
                type: 'stdio',
                command: 'cmd',
                args: ['/c', 'npx -y tavily-mcp'],
                env: { TAVILY_API_KEY: 'tvly-…' },
              },
            },
          },
          null,
          2,
        ),
  );

  const submit = async () => {
    setErr(null);
    if (mode === 'json') {
      let entries: { name: string; entry: Record<string, unknown> }[];
      try {
        entries = parseMcpJson(jsonText, initial?.name);
      } catch (e) {
        setErr(`JSON 解析失败：${e instanceof Error ? e.message : String(e)}`);
        return;
      }
      const fails: string[] = [];
      let okCount = 0;
      for (const { name: n, entry } of entries) {
        try {
          await mcpSave(entryToInput(n, entry, enabled));
          okCount += 1;
        } catch (e) {
          fails.push(`${n}: ${e}`);
        }
      }
      if (fails.length > 0) {
        setErr(`部分保存失败（成功 ${okCount} 个）：\n${fails.join('\n')}`);
        return;
      }
      onSaved();
      return;
    }
    if (!name.trim()) {
      setErr('名称必填');
      return;
    }
    const input: McpServerInput = {
      name: name.trim(),
      type: kind,
      command: command.trim() || null,
      args: argsText
        .split('\n')
        .map((s) => s.trim())
        .filter(Boolean),
      url: url.trim() || null,
      env: Object.fromEntries(
        envText
          .split('\n')
          .map((l) => l.trim())
          .filter(Boolean)
          .map((l) => {
            const i = l.indexOf('=');
            return i > 0 ? [l.slice(0, i), l.slice(i + 1)] : [l, ''];
          }),
      ),
      enabled,
    };
    try {
      await mcpSave(input);
      onSaved();
    } catch (e) {
      setErr(String(e));
    }
  };

  const field: React.CSSProperties = {
    border: '1px solid var(--color-border)',
    borderRadius: 9,
    padding: '7px 10px',
    background: 'var(--color-surface)',
    color: 'var(--color-text-1)',
    fontSize: 12,
    width: '100%',
    outline: 'none',
  };
  const label: React.CSSProperties = {
    fontSize: 11,
    color: 'var(--color-text-3)',
    marginBottom: 4,
  };

  return (
    <div
      style={{
        position: 'fixed',
        inset: 0,
        background: 'rgba(0,0,0,.25)',
        zIndex: 100,
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
      }}
      onClick={onClose}
    >
      <div
        className="card"
        style={{ width: 560, maxHeight: '84vh', overflow: 'auto', cursor: 'default' }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center" style={{ gap: 8 }}>
          <b style={{ fontSize: 14 }}>{initial ? `编辑 · ${initial.name}` : '新增 MCP Server'}</b>
          <span style={{ flex: 1 }} />
          {(['form', 'json'] as const).map((m) => (
            <span
              key={m}
              className={`chip ${mode === m ? 'ok' : 'off'}`}
              style={{ cursor: 'pointer', fontSize: 11 }}
              onClick={() => setMode(m)}
            >
              {m === 'form' ? '表单' : 'JSON'}
            </span>
          ))}
        </div>
        {mode === 'form' && (
        <div className="grid grid-cols-2 gap-2.5" style={{ marginTop: 12 }}>
          <div>
            <div style={label}>名称</div>
            <input style={field} value={name} disabled={!!initial} onChange={(e) => setName(e.target.value)} />
          </div>
          <div>
            <div style={label}>类型</div>
            <select style={field} value={kind} onChange={(e) => setKind(e.target.value)}>
              <option value="stdio">stdio</option>
              <option value="streamableHttp">streamableHttp（仅 WB）</option>
            </select>
          </div>
          {kind === 'stdio' ? (
            <>
              <div style={{ gridColumn: '1/3' }}>
                <div style={label}>命令 command</div>
                <input style={{ ...field, fontFamily: 'Consolas, monospace' }} value={command} onChange={(e) => setCommand(e.target.value)} placeholder="npx / cmd / uvx…" />
              </div>
              <div style={{ gridColumn: '1/3' }}>
                <div style={label}>参数 args（每行一个）</div>
                <textarea style={{ ...field, minHeight: 56, fontFamily: 'Consolas, monospace' }} value={argsText} onChange={(e) => setArgsText(e.target.value)} placeholder={'/c\nnpx -y some-mcp'} />
              </div>
            </>
          ) : (
            <div style={{ gridColumn: '1/3' }}>
              <div style={label}>URL</div>
              <input style={{ ...field, fontFamily: 'Consolas, monospace' }} value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://…/mcp" />
            </div>
          )}
          <div style={{ gridColumn: '1/3' }}>
            <div style={label}>环境变量 env（可选，KEY=VALUE，每行一个）</div>
            <textarea style={{ ...field, minHeight: 40, fontFamily: 'Consolas, monospace' }} value={envText} onChange={(e) => setEnvText(e.target.value)} placeholder="API_KEY=…" />
          </div>
        </div>
        )}
        {mode === 'json' && (
          <div style={{ marginTop: 12 }}>
            <div style={label}>
              粘贴 MCP JSON（支持 mcpServers 包裹 / 名称→条目 映射，可批量粘贴；headers 支持；
              各插件端的启用态由下方开关统一决定）
            </div>
            <textarea
              style={{ ...field, minHeight: 220, fontFamily: 'Consolas, monospace', lineHeight: 1.6 }}
              value={jsonText}
              onChange={(e) => setJsonText(e.target.value)}
              spellCheck={false}
            />
          </div>
        )}
        <div className="flex items-center flex-wrap" style={{ gap: 12, margin: '12px 0', fontSize: 12 }}>
          {mcpAgents.map((a) => {
            const meta = agentMeta(a.agent);
            return (
              <span key={a.agent} className="flex items-center" style={{ gap: 6 }}>
                <span
                  className={`sw${enabled[a.agent] ? ' on' : ''}`}
                  style={meta.accentVar && enabled[a.agent] ? { background: meta.accentVar } : undefined}
                  onClick={() => setEnabled({ ...enabled, [a.agent]: !enabled[a.agent] })}
                />
                分发到 {a.display_name}
              </span>
            );
          })}
        </div>
        {err && (
          <div style={{ fontSize: 12, color: 'var(--color-danger)', marginBottom: 8, whiteSpace: 'pre-wrap' }}>{err}</div>
        )}
        <div className="flex" style={{ gap: 8, justifyContent: 'flex-end' }}>
          <button className="btn gh" onClick={onClose}>取消</button>
          <button className="btn p" onClick={submit}>保存并同步已启用插件</button>
        </div>
      </div>
    </div>
  );
}

/** 单卡片连接测试态：running / ok / fail（result.message 挂 title 提示根因） */
type McpTestState =
  | { status: 'running' }
  | { status: 'ok'; message: string; latency_ms: number }
  | { status: 'fail'; message: string; latency_ms: number };

/** M3 MCP 岛 —— D1 统一列表（按插件端开关/漂移检测） + D2 弹窗 */
export function McpPage() {
  const [payload, setPayload] = useState<McpOverviewPayload | null>(null);
  const [editing, setEditing] = useState<McpOverviewItem | null>(null);
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<McpOverviewItem | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tests, setTests] = useState<Record<string, McpTestState>>({});

  const runTest = async (item: McpOverviewItem) => {
    if (tests[item.name]?.status === 'running') return;
    setTests((t) => ({ ...t, [item.name]: { status: 'running' } }));
    try {
      const r = await mcpTestConnection(item.name);
      setTests((t) => ({
        ...t,
        [item.name]: {
          status: r.ok ? 'ok' : 'fail',
          message: r.message,
          latency_ms: r.latency_ms,
        },
      }));
    } catch (e) {
      setTests((t) => ({
        ...t,
        [item.name]: { status: 'fail', message: String(e), latency_ms: 0 },
      }));
    }
  };

  const refresh = useCallback(async () => {
    setError(null);
    try {
      setPayload(await mcpOverview());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // 已启用且具备 MCP 能力的插件端（列表非空时以首个 item 的 agents 为准，兜底 WB/ZCode）
  const mcpAgents: { agent: string; display_name: string }[] =
    payload?.items[0]?.agents.map((a) => ({ agent: a.agent, display_name: a.display_name })) ??
    [];

  const adopt = async (overwrite: boolean) => {
    setError(null);
    try {
      const n = await mcpAdopt(overwrite);
      setMessage(`已收编 ${n} 个 server 进统一定义`);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const toggle = async (item: McpOverviewItem, agent: string) => {
    const cur = item.agents.find((a) => a.agent === agent)?.enabled ?? true;
    try {
      await mcpToggle(item.name, agent, !cur);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const remove = (item: McpOverviewItem) => {
    setDeleting(item);
  };

  const doRemove = async (item: McpOverviewItem) => {
    try {
      await mcpDelete(item.name);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const validate = async () => {
    try {
      const p = await mcpValidate();
      setPayload(p);
      const drifted =
        p.items.filter((i) => i.agents.some((a) => a.status === 'drifted')).length +
        p.drift.agents.reduce((n, d) => n + d.only.length, 0);
      setMessage(drifted === 0 ? '目标文件与统一定义一致，无漂移' : `发现 ${drifted} 处漂移/未收编`);
    } catch (e) {
      setError(String(e));
    }
  };

  const items = payload?.items ?? [];
  const hasDb = items.length > 0;
  const driftOnlyTotal = (payload?.drift.agents ?? []).reduce((n, d) => n + d.only.length, 0);
  const hasDrift = driftOnlyTotal > 0;
  const agentLabel = (payload?.drift.agents ?? [])
    .filter((d) => d.only.length > 0)
    .map((d) => `${d.only.length} 个（${d.display_name}）`)
    .join(' / ');

  return (
    <div>
      <PageHeader
        title={`MCP Servers（${items.length || '…'}）`}
        sub="统一定义 → 各启用插件端目标文件（由适配器完成格式翻译）· 仅对 Plugin 岛已启用的适配器生效"
      />

      {/* 收编提示（未接管 / 漂移时显示） */}
      {(!hasDb || hasDrift) && (
        <div className="card" style={{ padding: 14, marginBottom: 12, borderColor: 'var(--color-primary)' }}>
          <div className="flex items-center justify-between">
            <div style={{ fontSize: 12 }}>
              <b>{hasDb ? '发现未收编条目' : '首次接管'}</b>
              <span style={{ color: 'var(--color-text-2)' }}>
                {' '}
                · 目标文件中有 {agentLabel || '0 个'} server 不在统一定义中
              </span>
            </div>
            <button className="btn p" onClick={() => adopt(false)}>
              ⊕ 收编进统一定义
            </button>
          </div>
        </div>
      )}

      <div className="mb-3 flex items-center" style={{ gap: 8 }}>
        <span style={{ flex: 1 }} />
        {message && <span className="chip ok">{message}</span>}
        <button className="btn gh" onClick={validate}>↻ 校验目标文件</button>
        <button className="btn p" onClick={() => setCreating(true)}>⊕ 新增 Server</button>
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

      <div className="item-grid">
        {items.length === 0 ? (
          <div className="item-card" style={{ color: 'var(--color-text-3)', fontSize: 12 }}>
            统一定义为空 —— 点上方「收编」把各启用插件端现有 server 纳入管理，或新增。
          </div>
        ) : (
          items.map((item) => {
            const t = tests[item.name];
            const testChip =
              t?.status === 'running'
                ? { cls: 'off', text: '测试中…' }
                : t?.status === 'ok'
                  ? { cls: 'ok', text: `✓ ${(t.latency_ms / 1000).toFixed(1)}s` }
                  : t?.status === 'fail'
                    ? { cls: 'err', text: '✗ 连接失败' }
                    : { cls: 'off', text: '↻ 测试连接' };
            return (
            <div className="item-card sm" key={item.name}>
              <div className="card-top">
                <span
                  className={`sw${item.agents.some((a) => a.enabled) ? ' on' : ''}`}
                  title="点击切换启用（任一插件端启用即亮）"
                  onClick={() => {
                    const first = item.agents.find((a) => a.enabled);
                    void toggle(item, (first ?? item.agents[0]).agent);
                  }}
                />
                <div className="grow" style={{ minWidth: 0 }}>
                  <div className="nm" style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                    {item.name}
                  </div>
                </div>
              </div>
              <div
                className="ds desc-clamp"
                style={{ flex: 1, minHeight: 0, fontFamily: 'Consolas, monospace' }}
                title={`${item.type} · ${item.command ?? item.url ?? '—'}`}
              >
                {item.type} · {item.command ?? item.url ?? '—'}
              </div>
              <div className="card-actions">
                <span
                  className={`chip ${testChip.cls}`}
                  style={{ cursor: t?.status === 'running' ? 'wait' : 'pointer' }}
                  title={
                    t?.status === 'running'
                      ? '正在做 initialize 握手…'
                      : t
                        ? t.message
                        : '启动进程做 MCP initialize 握手，验证命令能否真正连通'
                  }
                  onClick={() => void runTest(item)}
                >
                  {testChip.text}
                </span>
                {item.agents.map((a) => {
                  return (
                    <span
                      key={a.agent}
                      className={`chip ${a.status === 'ok' ? agentChipClass(a.agent) : STATUS_COLOR[a.status]}`}
                      style={{ cursor: a.status === 'unsupported' ? 'default' : 'pointer' }}
                      title={
                        a.status === 'unsupported'
                          ? `${a.display_name} 不支持此 server 类型`
                          : `点击切换 ${a.display_name} 启用态`
                      }
                      onClick={
                        a.status === 'unsupported' ? undefined : () => toggle(item, a.agent)
                      }
                    >
                      {a.display_name} {STATUS_TEXT[a.status]}
                    </span>
                  );
                })}
                <span
                  className="chip off"
                  style={{ cursor: 'pointer' }}
                  onClick={() => setEditing(item)}
                >
                  编辑
                </span>
                <span className="chip err" style={{ cursor: 'pointer' }} onClick={() => remove(item)}>
                  删除
                </span>
              </div>
            </div>
            );
          })
        )}
      </div>

      {(creating || editing) && (
        <ServerModal
          initial={editing}
          mcpAgents={mcpAgents}
          onClose={() => {
            setCreating(false);
            setEditing(null);
          }}
          onSaved={() => {
            setCreating(false);
            setEditing(null);
            void refresh();
          }}
        />
      )}

      {deleting && (
        <ConfirmDialog
          title="删除 MCP Server"
          message={`删除统一定义「${deleting.name}」并从各启用插件端移除？（目标文件有备份）`}
          confirmLabel="删除"
          danger
          onConfirm={() => {
            const d = deleting;
            setDeleting(null);
            if (d) void doRemove(d);
          }}
          onCancel={() => setDeleting(null)}
        />
      )}
    </div>
  );
}
