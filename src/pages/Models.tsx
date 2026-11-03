import { useCallback, useEffect, useState } from 'react';
import {
  claudeProviderFetchModels,
  claudeProviderSave,
  claudeProvidersOverview,
  providerDelete,
  providerFetchModels,
  providerSave,
  providerSetDefault,
  providersOverview,
  type ClaudeProviderLive,
  type FetchResult,
  type ProvidersOverview,
  type ZcodeProviderLive,
} from '../api';
import { PageHeader } from '../common/components/PageHeader';
import { ConfirmDialog, InputDialog } from '../common/components/ConfirmDialog';
import { isAgentEnabled, useEnabledAgents } from '../stores/plugins';

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
const label: React.CSSProperties = { fontSize: 11, color: 'var(--color-text-3)', marginBottom: 4 };

/** 卡片右上角操作键 icon（lucide 风格，stroke currentColor 随主题） */
const IconPencil = () => (
  <svg width={13} height={13} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M17 3a2.83 2.83 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z" />
  </svg>
);
const IconTrash = () => (
  <svg width={13} height={13} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2m3 0v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" />
  </svg>
);

/** 供应商预设（仅预填表单，参考 whf-stock-board ModelManageModal 的预设 chips） */
const PRESETS: { key: string; label: string; kind: string; base: string }[] = [
  { key: 'custom', label: '自定义', kind: '', base: '' },
  { key: 'mimo', label: '小米 MiMo', kind: 'openai-compatible', base: 'https://api.xiaomimimo.com/v1' },
  { key: 'deepseek', label: 'DeepSeek', kind: 'openai-compatible', base: 'https://api.deepseek.com/v1' },
  { key: 'moonshot', label: 'Moonshot', kind: 'openai-compatible', base: 'https://api.moonshot.cn/v1' },
  { key: 'bigmodel', label: 'BigModel', kind: 'anthropic', base: 'https://open.bigmodel.cn/api/anthropic' },
  { key: 'zai', label: 'Z.ai', kind: 'anthropic', base: 'https://api.z.ai/api/anthropic' },
];

/**
 * E2 · Provider 编辑（参考 whf-stock-board ModelManageModal 表单态骨架：
 * 供应商预设 chips → 名称/类型 → 接口地址 → API Key（眼睛 + 获取模型列表）→ 高级配置折叠 → 模型勾选）
 */
function ProviderModal({
  initial,
  onClose,
  onSaved,
}: {
  initial: ZcodeProviderLive | null;
  onClose: () => void;
  onSaved: (message: string) => void;
}) {
  const [id] = useState(initial?.id ?? crypto.randomUUID());
  const [name, setName] = useState(initial?.name ?? '');
  const [kind, setKind] = useState(initial?.kind ?? 'openai-compatible');
  const [baseUrl, setBaseUrl] = useState(initial?.base_url ?? '');
  const [apiKey, setApiKey] = useState('');
  const [modelsUrl, setModelsUrl] = useState('');
  const [fetched, setFetched] = useState<FetchResult | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set(initial?.models ?? []));
  const [fetching, setFetching] = useState(false);
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [showKey, setShowKey] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [presetKey, setPresetKey] = useState('custom');

  const applyPreset = (key: string) => {
    setPresetKey(key);
    const preset = PRESETS.find((p) => p.key === key);
    if (preset?.kind) setKind(preset.kind);
    if (preset?.base) setBaseUrl(preset.base);
    if (!name.trim() && preset && preset.key !== 'custom') setName(preset.label);
  };

  const doFetch = async () => {
    setErr(null);
    setFetching(true);
    try {
      const r = await providerFetchModels({
        id,
        kind,
        base_url: baseUrl,
        api_key: apiKey || undefined,
        models_url: modelsUrl.trim() || undefined,
      });
      setFetched(r);
      setPicked(new Set(initial?.models ?? []));
    } catch (e) {
      setErr(String(e));
    } finally {
      setFetching(false);
    }
  };

  const submit = async () => {
    setErr(null);
    if (!name.trim() || !baseUrl.trim()) {
      setErr('名称与接口地址必填');
      return;
    }
    setSaving(true);
    try {
      const msg = await providerSave({
        id,
        name: name.trim(),
        kind,
        base_url: baseUrl.trim(),
        api_key: apiKey || undefined,
        models: [...picked],
      });
      onSaved(msg);
    } catch (e) {
      setErr(String(e));
    } finally {
      setSaving(false);
    }
  };

  const toggle = (m: string) => {
    setPicked((prev) => {
      const next = new Set(prev);
      if (next.has(m)) next.delete(m);
      else next.add(m);
      return next;
    });
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
        style={{ width: 640, maxHeight: '86vh', overflow: 'auto', cursor: 'default' }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between">
          <b style={{ fontSize: 14 }}>{initial ? `编辑 Provider · ${initial.name}` : '新增 Provider'}</b>
          <span style={{ color: 'var(--color-text-3)', cursor: 'pointer' }} onClick={onClose}>✕</span>
        </div>

        {/* 供应商预设 chips（仅预填，不影响已填内容） */}
        <div style={{ marginTop: 12 }}>
          <div style={label}>供应商</div>
          <div className="flex" style={{ gap: 6, flexWrap: 'wrap' }}>
            {PRESETS.map((preset) => (
              <span
                key={preset.key}
                className={`chip ${presetKey === preset.key ? 'ok' : 'off'}`}
                style={{ cursor: 'pointer' }}
                onClick={() => applyPreset(preset.key)}
              >
                {preset.label}
              </span>
            ))}
          </div>
        </div>

        <div className="grid grid-cols-2 gap-2.5" style={{ marginTop: 10 }}>
          <div>
            <div style={label}>名称</div>
            <input style={field} value={name} onChange={(e) => setName(e.target.value)} />
          </div>
          <div>
            <div style={label}>类型 kind</div>
            <select style={field} value={kind} onChange={(e) => setKind(e.target.value)}>
              <option value="openai-compatible">openai-compatible</option>
              <option value="anthropic">anthropic</option>
            </select>
          </div>
          <div style={{ gridColumn: '1/3' }}>
            <div style={label}>接口地址（baseURL）</div>
            <input style={{ ...field, fontFamily: 'Consolas, monospace' }} value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder="https://…/v1 或 https://…/api/anthropic" />
          </div>
          <div style={{ gridColumn: '1/3' }}>
            <div style={label}>
              API Key
              {initial?.has_api_key && (
                <span style={{ marginLeft: 6 }}>
                  （已配置 ••••••••，留空 = 不修改）
                </span>
              )}
            </div>
            <div className="flex" style={{ gap: 8 }}>
              <div style={{ position: 'relative', flex: 1 }}>
                <input
                  type={showKey ? 'text' : 'password'}
                  style={{ ...field, paddingRight: 30 }}
                  value={apiKey}
                  onChange={(e) => setApiKey(e.target.value)}
                  placeholder={initial?.has_api_key ? '留空保持不变' : 'sk-…'}
                />
                <span
                  title={showKey ? '隐藏密钥' : '显示密钥'}
                  style={{
                    position: 'absolute',
                    right: 9,
                    top: '50%',
                    transform: 'translateY(-50%)',
                    cursor: 'pointer',
                    color: 'var(--color-text-3)',
                    fontSize: 12,
                  }}
                  onClick={() => setShowKey(!showKey)}
                >
                  {showKey ? '🙈' : '👁'}
                </span>
              </div>
              <button className="btn gh" style={{ whiteSpace: 'nowrap' }} onClick={doFetch} disabled={fetching || !baseUrl.trim()}>
                {fetching ? '拉取中…' : '⌕ 获取模型列表'}
              </button>
            </div>
          </div>
        </div>

        {/* 高级配置折叠（stock-board 风格：chevron + 边框卡片） */}
        <div style={{ border: '1px solid var(--color-border)', borderRadius: 10, marginTop: 12 }}>
          <div
            style={{
              display: 'flex',
              alignItems: 'center',
              gap: 6,
              padding: '8px 12px',
              cursor: 'pointer',
              fontSize: 12,
              color: 'var(--color-text-2)',
              userSelect: 'none',
            }}
            onClick={() => setAdvancedOpen(!advancedOpen)}
          >
            <span style={{ color: 'var(--color-text-3)', fontSize: 10 }}>{advancedOpen ? '▼' : '▶'}</span>
            高级配置
          </div>
          {advancedOpen && (
            <div style={{ borderTop: '1px solid var(--color-border)', padding: '10px 12px' }}>
              <div style={label}>
                模型列表接口（models_url，可选）
                <span style={{ marginLeft: 6 }}>网关的 /v1/models 地址异常时覆写，留空走候选链自动探测</span>
              </div>
              <input
                style={{ ...field, fontFamily: 'Consolas, monospace' }}
                value={modelsUrl}
                onChange={(e) => setModelsUrl(e.target.value)}
                placeholder="https://…/v1/models"
              />
            </div>
          )}
        </div>

        <div className="flex items-center" style={{ gap: 8, margin: '12px 0 8px' }}>
          <b style={{ fontSize: 12 }}>模型列表</b>
          {fetched && !fetched.suggested && (
            <span className="chip ok">已拉取 {fetched.models.length} 个</span>
          )}
          {fetched?.suggested && (
            <span className="chip warn" title={fetched.traces.at(-1)?.note}>
              内置推荐（网关无 /v1/models，可编辑）
            </span>
          )}
          {picked.size > 0 && <span className="chip off">已勾选 {picked.size}</span>}
          <span style={{ flex: 1 }} />
        </div>

        {(fetched || initial?.models?.length) && (
          <div className="list" style={{ marginBottom: 10, maxHeight: 220, overflow: 'auto', border: '1px solid var(--color-border)', borderRadius: 10 }}>
            {(fetched?.models ?? initial?.models ?? []).map((m) => (
              <div className="li" key={m} style={{ padding: '6px 12px' }}>
                <span className={`sw${picked.has(m) ? ' on' : ' small'}`} onClick={() => toggle(m)} />
                <div className="grow">
                  <div className="nm" style={{ fontSize: 11.5, fontFamily: 'Consolas, monospace' }}>{m}</div>
                </div>
              </div>
            ))}
          </div>
        )}
        {fetched && (
          <div
            style={{
              fontSize: 10.5,
              color: 'var(--color-text-3)',
              background: 'var(--color-code-bg)',
              borderRadius: 10,
              padding: '6px 10px',
              marginBottom: 10,
            }}
          >
            {fetched.traces.map((t, i) => (
              <div key={i}>
                {t.ok ? '✓' : '✕'} {t.url} {t.status ? `→ ${t.status}` : ''}（{t.ms}ms）· {t.note}
              </div>
            ))}
          </div>
        )}
        {!initial?.models?.length && !fetched && (
          <div style={{ fontSize: 11, color: 'var(--color-text-3)', marginBottom: 10 }}>
            模型名一律从列表勾选，不手写 —— 填好接口地址与 Key 后点「获取模型列表」。
          </div>
        )}
        {!fetched && !!initial?.models?.length && (
          <div style={{ fontSize: 11, color: 'var(--color-text-3)', marginBottom: 10 }}>
            以上为已保存的勾选；点「获取模型列表」拉取网关最新可用列表后重新勾选。
          </div>
        )}
        {kind === 'anthropic' && (
          <div style={{ fontSize: 11, color: 'var(--color-warn, #d9a441)', marginBottom: 10 }}>
            anthropic 型仅写 ZCode（WB 自定义模型只支持 OpenAI 兼容 chat/completions，不写穿 WB）。
          </div>
        )}

        {err && (
          <div style={{ fontSize: 12, color: 'var(--color-danger)', marginBottom: 8, whiteSpace: 'pre-wrap' }}>{err}</div>
        )}
        <div className="flex" style={{ gap: 8, justifyContent: 'flex-end' }}>
          <button className="btn gh" onClick={onClose}>取消</button>
          <button className="btn p" onClick={submit} disabled={saving}>
            {saving ? '保存中…' : '保存并写入双端'}
          </button>
        </div>
      </div>
    </div>
  );
}

/** Claude Code 模型托管编辑（settings.json env 单档；模型值手动配置，不预填任何官方模型） */
function ClaudeModal({
  initial,
  onClose,
  onSaved,
}: {
  initial: ClaudeProviderLive;
  onClose: () => void;
  onSaved: (message: string) => void;
}) {
  const [baseUrl, setBaseUrl] = useState(initial.base_url);
  const [apiKey, setApiKey] = useState('');
  const [model, setModel] = useState(initial.model);
  const [smallModel, setSmallModel] = useState(initial.small_fast_model);
  const [fetched, setFetched] = useState<FetchResult | null>(null);
  const [fetching, setFetching] = useState(false);
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [showKey, setShowKey] = useState(false);
  const [presetKey, setPresetKey] = useState('custom');
  const claudePresets = PRESETS.filter((p) => p.key === 'custom' || p.kind === 'anthropic');

  const applyPreset = (key: string) => {
    setPresetKey(key);
    const preset = claudePresets.find((p) => p.key === key);
    if (preset?.base) setBaseUrl(preset.base);
  };

  const doFetch = async () => {
    setErr(null);
    setFetching(true);
    try {
      const r = await claudeProviderFetchModels({
        base_url: baseUrl,
        api_key: apiKey || undefined,
      });
      setFetched(r);
    } catch (e) {
      setErr(String(e));
    } finally {
      setFetching(false);
    }
  };

  const submit = async () => {
    setErr(null);
    if (!baseUrl.trim()) {
      setErr('接口地址必填');
      return;
    }
    setSaving(true);
    try {
      // model/小模型始终随当前值写入（空串 = 清除该键）；key 留空 = 掩码保存不修改
      const msg = await claudeProviderSave({
        base_url: baseUrl.trim(),
        api_key: apiKey || undefined,
        model,
        small_fast_model: smallModel,
      });
      onSaved(msg);
    } catch (e) {
      setErr(String(e));
    } finally {
      setSaving(false);
    }
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
        style={{ width: 620, maxHeight: '86vh', overflow: 'auto', cursor: 'default' }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between">
          <b style={{ fontSize: 14 }}>Claude Code 模型托管（~/.claude/settings.json env）</b>
          <span style={{ color: 'var(--color-text-3)', cursor: 'pointer' }} onClick={onClose}>✕</span>
        </div>

        <div style={{ marginTop: 12 }}>
          <div style={label}>网关预设</div>
          <div className="flex" style={{ gap: 6, flexWrap: 'wrap' }}>
            {claudePresets.map((preset) => (
              <span
                key={preset.key}
                className={`chip ${presetKey === preset.key ? 'ok' : 'off'}`}
                style={{ cursor: 'pointer' }}
                onClick={() => applyPreset(preset.key)}
              >
                {preset.label}
              </span>
            ))}
          </div>
        </div>

        <div style={{ marginTop: 10 }}>
          <div style={label}>接口地址（ANTHROPIC_BASE_URL）</div>
          <input style={{ ...field, fontFamily: 'Consolas, monospace' }} value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder="https://…/api/anthropic" />
        </div>
        <div style={{ marginTop: 10 }}>
          <div style={label}>
            API Key（ANTHROPIC_AUTH_TOKEN）
            {initial.has_api_key && <span style={{ marginLeft: 6 }}>（已配置 ••••••••，留空 = 不修改）</span>}
          </div>
          <div className="flex" style={{ gap: 8 }}>
            <div style={{ position: 'relative', flex: 1 }}>
              <input
                type={showKey ? 'text' : 'password'}
                style={{ ...field, paddingRight: 30 }}
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder={initial.has_api_key ? '留空保持不变' : 'sk-…'}
              />
              <span
                title={showKey ? '隐藏密钥' : '显示密钥'}
                style={{
                  position: 'absolute',
                  right: 9,
                  top: '50%',
                  transform: 'translateY(-50%)',
                  cursor: 'pointer',
                  color: 'var(--color-text-3)',
                  fontSize: 12,
                }}
                onClick={() => setShowKey(!showKey)}
              >
                {showKey ? '🙈' : '👁'}
              </span>
            </div>
            <button className="btn gh" style={{ whiteSpace: 'nowrap' }} onClick={doFetch} disabled={fetching || !baseUrl.trim()}>
              {fetching ? '拉取中…' : '⌕ 获取模型列表'}
            </button>
          </div>
        </div>
        <div className="grid grid-cols-2 gap-2.5" style={{ marginTop: 10 }}>
          <div>
            <div style={label}>主模型（ANTHROPIC_MODEL，留空 = 清除走网关默认）</div>
            <input style={{ ...field, fontFamily: 'Consolas, monospace' }} value={model} onChange={(e) => setModel(e.target.value)} placeholder="如 glm-5.3 / claude-sonnet-5" />
          </div>
          <div>
            <div style={label}>小模型（ANTHROPIC_SMALL_FAST_MODEL，可选）</div>
            <input style={{ ...field, fontFamily: 'Consolas, monospace' }} value={smallModel} onChange={(e) => setSmallModel(e.target.value)} placeholder="后台任务用，如 glm-5.3-flash" />
          </div>
        </div>

        {fetched && fetched.models.length > 0 && (
          <>
            <div className="flex items-center" style={{ gap: 8, margin: '12px 0 6px' }}>
              <b style={{ fontSize: 12 }}>网关模型列表</b>
              <span className={`chip ${fetched.suggested ? 'warn' : 'ok'}`}>
                {fetched.suggested ? '内置推荐（网关无 /v1/models）' : `已拉取 ${fetched.models.length} 个，单击设为主模型`}
              </span>
              <span style={{ flex: 1 }} />
            </div>
            <div className="list" style={{ maxHeight: 180, overflow: 'auto', border: '1px solid var(--color-border)', borderRadius: 10 }}>
              {fetched.models.map((m) => (
                <div className="li" key={m} style={{ padding: '6px 12px', cursor: 'pointer' }} onClick={() => setModel(m)}>
                  <span className={`sw${model === m ? ' on' : ' small'}`} />
                  <div className="nm" style={{ fontSize: 11.5, fontFamily: 'Consolas, monospace' }}>{m}</div>
                </div>
              ))}
            </div>
          </>
        )}

        {err && (
          <div style={{ fontSize: 12, color: 'var(--color-danger)', marginTop: 8, whiteSpace: 'pre-wrap' }}>{err}</div>
        )}
        <div className="flex" style={{ gap: 8, justifyContent: 'flex-end', marginTop: 12 }}>
          <button className="btn gh" onClick={onClose}>取消</button>
          <button className="btn p" onClick={submit} disabled={saving}>
            {saving ? '保存中…' : '保存并写入 settings.json'}
          </button>
        </div>
      </div>
    </div>
  );
}

/** Claude Code 域（模型岛分区块）：env 托管单档卡片 + 编辑弹窗；模型值一律手动配置 */
function ClaudeDomain() {
  const [live, setLive] = useState<ClaudeProviderLive | null>(null);
  const [editing, setEditing] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setError(null);
    try {
      setLive(await claudeProvidersOverview());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const enabledIds = useEnabledAgents();
  const claudeOff = !isAgentEnabled(enabledIds, 'claude');
  const row = (k: string, v: string, mono = false) => (
    <div className="flex" style={{ gap: 8, fontSize: 11.5, padding: '3px 0' }}>
      <span style={{ color: 'var(--color-text-3)', flex: 'none', width: 150 }}>{k}</span>
      <span style={{ fontFamily: mono ? 'Consolas, monospace' : undefined, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
        {v || <span style={{ color: 'var(--color-text-3)' }}>未配置</span>}
      </span>
    </div>
  );

  return (
    <div style={{ marginTop: 18 }}>
      <div className="flex items-center" style={{ gap: 8, marginBottom: 8 }}>
        <b style={{ fontSize: 13 }}>Claude Code · settings.json env 托管</b>
        {live?.onboarding_done ? (
          <span className="chip ok">已跳过首次引导</span>
        ) : (
          <span className="chip warn" title="保存配置时会自动写入 ~/.claude.json hasCompletedOnboarding=true">未跳过首次引导</span>
        )}
        <span style={{ flex: 1 }} />
        {message && <span className="chip ok">{message}</span>}
        <button className="btn gh" onClick={() => setEditing(true)}>编辑配置</button>
      </div>

      {claudeOff && (
        <div className="card" style={{ padding: 14, marginBottom: 10, borderColor: 'var(--color-warn, #d9a441)', fontSize: 12 }}>
          <b>Claude Code 插件已停用</b>
          <span style={{ color: 'var(--color-text-2)' }}>{' '}—— 到 Plugin 岛开启后可读写 ~/.claude/settings.json。</span>
        </div>
      )}

      {error && (
        <div style={{ fontSize: 12, color: 'var(--color-danger)', marginBottom: 10, whiteSpace: 'pre-wrap' }}>{error}</div>
      )}

      <div className="item-grid" style={claudeOff ? { opacity: 0.45, pointerEvents: 'none' } : undefined}>
        <div className="item-card sm">
          <div className="card-top">
            <div className="grow" style={{ minWidth: 0 }}>
              <div className="nm">ANTHROPIC 环境变量</div>
              <div className="ds">~/.claude/settings.json · 单档直写 · Key 只存目标文件</div>
              <div style={{ marginTop: 6 }}>
                {row('ANTHROPIC_BASE_URL', live?.base_url ?? '…', true)}
                {row('ANTHROPIC_AUTH_TOKEN', live?.has_api_key ? '••••••••（已配置）' : '', true)}
                {row('ANTHROPIC_MODEL', live?.model ?? '', true)}
                {row('ANTHROPIC_SMALL_FAST_MODEL', live?.small_fast_model ?? '', true)}
              </div>
            </div>
          </div>
          <div className="card-actions">
            <span className={`chip ${live?.configured ? 'ok' : 'off'}`}>{live?.configured ? '已配置' : '未配置'}</span>
            <span className="chip off">模型手动配置</span>
          </div>
        </div>
      </div>

      <div style={{ fontSize: 11, color: 'var(--color-text-3)', marginTop: 8 }}>
        · 保存时同步写入 ~/.claude.json&#123;"hasCompletedOnboarding": true&#125; 跳过首次引导 · Claude Code 重启会话后生效 · 模型名以网关实际提供为准（拉取列表后单击选用或手动输入）
      </div>

      {editing && live && (
        <ClaudeModal
          initial={live}
          onClose={() => setEditing(false)}
          onSaved={(msg) => {
            setEditing(false);
            setMessage(msg);
            void refresh();
          }}
        />
      )}
    </div>
  );
}

/** M4 模型岛 —— E1 Provider 列表（ZCode 直写 + WB models.json 写穿） */
export function ModelsPage() {
  const [data, setData] = useState<ProvidersOverview | null>(null);
  const [editing, setEditing] = useState<ZcodeProviderLive | null>(null);
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<ZcodeProviderLive | null>(null);
  const [defaultTarget, setDefaultTarget] = useState<ZcodeProviderLive | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setError(null);
    try {
      setData(await providersOverview());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const remove = (p: ZcodeProviderLive) => {
    setDeleting(p);
  };

  const doRemove = async (p: ZcodeProviderLive) => {
    try {
      setMessage(await providerDelete(p.id));
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const setDefault = (p: ZcodeProviderLive) => {
    setDefaultTarget(p);
  };

  const doSetDefault = async (family: string, key: string) => {
    try {
      await providerSetDefault(family, key);
      setMessage(`已设为 ${family} 家族默认（需重启 ZCode 生效）`);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const all = data?.providers ?? [];
  const providers = all.filter((p) => !p.builtin); // 内置档位（ZCode 官方注册表 builtin:*）不上屏，只显示手动添加的
  const hiddenCount = all.length - providers.length;

  // 插件门控：ZCode 停用 = Model 岛主存储不可用，全页置灰只读；WB 停用隐藏写穿状态 chip
  const enabledIds = useEnabledAgents();
  const zcOff = !isAgentEnabled(enabledIds, 'zcode');
  const wbOff = !isAgentEnabled(enabledIds, 'workbuddy');

  return (
    <div>
      <PageHeader
        title={`模型 Provider（${providers.length || '…'}）`}
        sub="一次配置 → 直写 ZCode v2/config.json + WB models.json · API Key 只存目标文件不入库"
      />

      {zcOff && (
        <div
          className="card"
          style={{
            padding: 14,
            marginBottom: 12,
            borderColor: 'var(--color-warn, #d9a441)',
            fontSize: 12,
          }}
        >
          <b>ZCode 插件已停用</b>
          <span style={{ color: 'var(--color-text-2)' }}>
            {' '}—— Model 岛的主存储是 ZCode v2/config.json，停用期间列表不可读写。到 Plugin 岛开启后恢复。
          </span>
        </div>
      )}

      <div className="mb-3 flex items-center" style={{ gap: 8, ...(zcOff ? { opacity: 0.45, pointerEvents: 'none' } : {}) }}>
        <span style={{ flex: 1 }} />
        {message && <span className="chip ok">{message}</span>}
        <button className="btn p" onClick={() => setCreating(true)}>⊕ 新增 Provider</button>
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

      <div className="item-grid" style={zcOff ? { opacity: 0.45, pointerEvents: 'none' } : undefined}>
        {providers.map((p) => {
          const selectedEntry = Object.entries(data?.selected_keys ?? {}).find(([, v]) =>
            v.endsWith(p.id),
          );
          return (
            <div className="item-card sm" key={p.id}>
              <div className="card-top">
                <div className="grow" style={{ minWidth: 0 }}>
                  <div className="nm" style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                    {p.name}
                    {p.builtin && <span className="chip off" style={{ marginLeft: 6 }}>builtin</span>}
                    {selectedEntry && (
                      <span className="chip ok" style={{ marginLeft: 6 }}>
                        默认 · {selectedEntry[0]}
                      </span>
                    )}
                  </div>
                  <div
                    className="ds"
                    style={{ fontFamily: 'Consolas, monospace', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
                  >
                    {p.base_url || '—'}
                  </div>
                  {/* 已开启的模型一览：单行截断，悬停看全量 */}
                  <div
                    title={p.models.length ? p.models.join(' · ') : '未勾选模型'}
                    style={{
                      fontSize: 10.5,
                      fontFamily: 'Consolas, monospace',
                      color: 'var(--color-text-3)',
                      marginTop: 4,
                      overflow: 'hidden',
                      textOverflow: 'ellipsis',
                      whiteSpace: 'nowrap',
                    }}
                  >
                    {p.models.length ? p.models.join(' · ') : '（未勾选模型）'}
                  </div>
                </div>
                {/* 右上角 icon 操作键：编辑 / 删除（builtin 无删除） */}
                <div style={{ display: 'flex', gap: 2, flex: 'none', alignSelf: 'flex-start' }}>
                  <button className="icon-btn" title="编辑" onClick={() => setEditing(p)}>
                    <IconPencil />
                  </button>
                  {!p.builtin && (
                    <button className="icon-btn danger" title="删除" onClick={() => remove(p)}>
                      <IconTrash />
                    </button>
                  )}
                </div>
              </div>
              <div className="card-actions">
                <span className={`chip ${p.kind === 'anthropic' ? 'ok' : 'off'}`}>{p.kind}</span>
                {!wbOff &&
                  (p.kind === 'openai-compatible' ? (
                    <span
                      className={`chip ${p.models.length > 0 && p.wb_models.length === p.models.length ? 'ok' : 'warn'}`}
                      title={`WB models.json 已同步 ${p.wb_models.length}/${p.models.length}（热加载，WB 模型选择器「自定义」分区可见）`}
                    >
                      WB {p.wb_models.length}/{p.models.length}
                    </span>
                  ) : (
                    <span className="chip off" title="WB 自定义模型仅支持 OpenAI 兼容接入点，anthropic 型不写穿">WB·—</span>
                  ))}
                <span className="chip off">Key{p.has_api_key ? '已配置' : '未配置'}</span>
                <span className="chip warn" style={{ cursor: 'pointer' }} onClick={() => setDefault(p)}>设默认</span>
              </div>
            </div>
          );
        })}
        {providers.length === 0 && (
          <div className="item-card" style={{ color: 'var(--color-text-3)', fontSize: 12 }}>
            {hiddenCount > 0
              ? '没有手动添加的 Provider（ZCode 内置档位已隐藏）—— 点右上角「新增 Provider」添加'
              : '未读取到 provider（~/.zcode/v2/config.json 为空或不存在）'}
          </div>
        )}
      </div>

      <div style={{ fontSize: 11, color: 'var(--color-text-3)', marginTop: 10 }}>
        · 拉取策略：/v1/models 候选链（/v&#123;N&#125; 版本段先拼原样 → /v1/models → 兼容后缀剥离；anthropic 原生 x-api-key 优先）
        · 修改「默认」后需重启 ZCode 生效 · WB 侧：openai-compatible 保存时写穿 models.json（1s 热加载即生效），anthropic 型仅 ZCode
        {hiddenCount > 0 && ` · 已隐藏 ${hiddenCount} 个 ZCode 内置档位（builtin:*），此处仅显示手动添加的 Provider`}
      </div>

      <ClaudeDomain />

      {(creating || editing) && (
        <ProviderModal
          initial={editing}
          onClose={() => {
            setCreating(false);
            setEditing(null);
          }}
          onSaved={(msg) => {
            setCreating(false);
            setEditing(null);
            setMessage(msg);
            void refresh();
          }}
        />
      )}

      {deleting && (
        <ConfirmDialog
          title="删除 Provider"
          message={`删除 provider「${deleting.name}」？（ZCode config.json 与 WB models.json 均先备份）`}
          confirmLabel="删除"
          danger
          onConfirm={() => {
            const p = deleting;
            setDeleting(null);
            if (p) void doRemove(p);
          }}
          onCancel={() => setDeleting(null)}
        />
      )}

      {defaultTarget && (
        <InputDialog
          title="设置默认模型"
          message={`为「${defaultTarget.name}」设置 ZCode 默认模型（selected key 形如 coding-plan:builtin:bigmodel-coding-plan）。`}
          fields={[
            { key: 'family', label: '家族名 family（如 bigmodel / zai）', defaultValue: 'bigmodel' },
            { key: 'key', label: 'SelectedKey', defaultValue: `coding-plan:${defaultTarget.id}`, mono: true },
          ]}
          confirmLabel="设为默认"
          onConfirm={(v) => {
            const family = v.family?.trim();
            const key = v.key?.trim();
            setDefaultTarget(null);
            if (family && key) void doSetDefault(family, key);
          }}
          onCancel={() => setDefaultTarget(null)}
        />
      )}
    </div>
  );
}
