import { useCallback, useEffect, useState } from 'react';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import {
  memoryFileDelete,
  memoryFileRename,
  memoryFilesList,
  memoryGlobalList,
  memoryGlobalUnify,
  memoryRead,
  memoryWrite,
  projectConsolidate,
  projectImport,
  projectInit,
  projectSync,
  projectsOverview,
  revealPath,
  type MemoryFile,
  type MemoryListItem,
  type ProjectInfo,
} from '../api';
import { PageHeader } from '../common/components/PageHeader';
import { LogoMark } from '../common/components/LogoMark';
import { toast } from '../stores/ui';

const fmtSize = (n: number) => (n >= 1024 ? `${(n / 1024).toFixed(1)} KB` : `${n} B`);

/** 复制文本到剪贴板：优先 Clipboard API，失败降级 execCommand（WebView2 兜底） */
async function copyTextToClipboard(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    try {
      const ta = document.createElement('textarea');
      ta.value = text;
      ta.style.position = 'fixed';
      ta.style.opacity = '0';
      document.body.appendChild(ta);
      ta.select();
      const ok = document.execCommand('copy');
      document.body.removeChild(ta);
      return ok;
    } catch {
      return false;
    }
  }
}

const copyMemoryText = async (text: string) => {
  const ok = await copyTextToClipboard(text);
  toast(ok ? '已复制到剪贴板' : '复制失败，请手动选中文本复制');
};

const openLocalFolder = async (path: string) => {
  try {
    await revealPath(path);
  } catch (e) {
    toast(`打开失败：${e}`);
  }
};

type Tab = 'global' | 'project';

/** G1 全局记忆：默认只读浏览，点【编辑】进入编辑态（底部 取消/保存）；「统一」后双端共写 ~/.agenthub/MEMORY.md */
function GlobalMemory() {
  const [files, setFiles] = useState<MemoryFile[] | null>(null);
  const [selected, setSelected] = useState<MemoryFile | null>(null);
  const [content, setContent] = useState('');
  const [savedContent, setSavedContent] = useState('');
  const [editing, setEditing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [unifyBusy, setUnifyBusy] = useState(false);
  const [unifySteps, setUnifySteps] = useState<string[] | null>(null);

  const loadList = useCallback(async () => {
    try {
      setFiles(await memoryGlobalList());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void loadList();
  }, [loadList]);

  const open = useCallback(async (f: MemoryFile) => {
    setSelected(f);
    setEditing(false);
    setError(null);
    try {
      const c = await memoryRead(f.path);
      setContent(c);
      setSavedContent(c);
    } catch (e) {
      setContent('');
      setSavedContent('');
      setError(String(e));
    }
  }, []);

  const save = async () => {
    if (!selected) return;
    setError(null);
    try {
      await memoryWrite(selected.path, content, selected.agent === 'zcode');
      toast('已保存（写入前自动快照至 backups/memory/）');
      setEditing(false);
      setSavedContent(content);
      await loadList();
    } catch (e) {
      setError(String(e));
    }
  };

  const runUnify = async () => {
    setUnifyBusy(true);
    setError(null);
    try {
      const steps = await memoryGlobalUnify();
      setUnifySteps(steps);
      toast('全局记忆已统一：各启用端共写 ~/.agenthub/MEMORY.md（详见步骤日志）');
      await loadList();
    } catch (e) {
      setError(String(e));
    } finally {
      setUnifyBusy(false);
    }
  };

  const dirty = content !== savedContent;

  return (
    <div
      style={{
        display: 'grid',
        gridTemplateColumns: '300px 1fr',
        gap: 12,
        minHeight: 'min(620px, calc(100vh - 230px))',
      }}
    >
      <div style={{ display: 'flex', flexDirection: 'column', minHeight: 0 }}>
        <div className="flex items-center" style={{ gap: 8, marginBottom: 6, flexWrap: 'wrap' }}>
          <span
            title="用户级 MEMORY 文件（按账号 / Agent）"
            style={{ fontSize: 10, color: 'var(--color-text-3)', whiteSpace: 'nowrap' }}
          >
            用户级 MEMORY 文件
          </span>
          <span style={{ flex: 1 }} />
          <button
            className="btn p"
            style={{ fontSize: 11, padding: '3px 10px', whiteSpace: 'nowrap', flex: 'none' }}
            disabled={unifyBusy}
            title="真身 ~/.agenthub/MEMORY.md，双端原路径替换为符号链接（幂等可重跑）"
            onClick={() => void runUnify()}
          >
            {unifyBusy ? '⟳ 统一中…' : '✦ 统一全局记忆'}
          </button>
        </div>
        <div className="list" style={{ flex: 1, minHeight: 0, overflowY: 'auto' }}>
          {files === null && (
            <div className="li" style={{ color: 'var(--color-text-3)', fontSize: 12 }}>加载中…</div>
          )}
          {files?.length === 0 && (
            <div className="li" style={{ color: 'var(--color-text-3)', fontSize: 12 }}>
              未发现记忆文件
            </div>
          )}
          {files?.map((f) => (
            <div
              key={f.id}
              className="li"
              style={{
                cursor: 'pointer',
                background:
                  selected?.id === f.id
                    ? 'color-mix(in srgb, var(--color-primary) 8%, transparent)'
                    : undefined,
              }}
              onClick={() => void open(f)}
            >
              <div className="ic" style={{ width: 24, height: 24, fontSize: 12 }}>❑</div>
              <div className="grow" style={{ minWidth: 0 }}>
                <div
                  className="nm"
                  title={f.label}
                  style={{ fontSize: 11.5, whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' }}
                >
                  {f.label}
                </div>
                <div className="ds">{fmtSize(f.size)} · {f.updated}</div>
              </div>
            </div>
          ))}
        </div>
        {unifySteps && (
          <div style={{ marginTop: 8 }}>
            <div style={{ fontSize: 10, color: 'var(--color-text-3)', marginBottom: 4 }}>统一结果</div>
            {stepList(unifySteps)}
          </div>
        )}
      </div>
      <div className="flex flex-col" style={{ gap: 10, minWidth: 0 }}>
        {selected ? (
          <>
            <div className="flex items-center" style={{ gap: 8 }}>
              {!editing && <span className="chip off">只读 · 点下方「编辑」修改</span>}
              {editing && <span className="chip warn">编辑中{dirty ? ' · 未保存' : ''}</span>}
              <span style={{ flex: 1 }} />
            </div>
            {error && (
              <div
                style={{
                  fontSize: 12,
                  color: 'var(--color-danger)',
                  background: 'color-mix(in srgb, var(--color-danger) 8%, transparent)',
                  borderRadius: 10,
                  padding: '8px 12px',
                }}
              >
                {error}
              </div>
            )}
            <textarea
              value={content}
              readOnly={!editing}
              onChange={(e) => setContent(e.target.value)}
              spellCheck={false}
              style={{
                flex: 1,
                minHeight: 340,
                border: '1px solid var(--color-border)',
                borderRadius: 12,
                padding: 12,
                background: 'var(--color-code-bg)',
                fontFamily: 'Consolas, monospace',
                fontSize: 11.5,
                lineHeight: 1.8,
                color: 'var(--color-text-2)',
                resize: 'vertical',
                outline: 'none',
              }}
            />
            <div className="flex items-center" style={{ gap: 8, justifyContent: 'flex-end' }}>
              {!editing && (
                <>
                  <button
                    className="btn gh"
                    disabled={!content}
                    onClick={() => void copyMemoryText(content)}
                  >
                    ⧉ 复制文本
                  </button>
                  <button
                    className="btn gh"
                    title="在资源管理器中定位该记忆文件"
                    onClick={() => void openLocalFolder(selected.path)}
                  >
                    📁 打开文件夹
                  </button>
                </>
              )}
              {editing ? (
                <>
                  <button
                    className="btn gh"
                    onClick={() => {
                      setContent(savedContent);
                      setEditing(false);
                      setError(null);
                    }}
                  >
                    取消
                  </button>
                  <button className="btn p" onClick={save}>
                    ✓ 保存
                  </button>
                </>
              ) : (
                <button className="btn p" onClick={() => setEditing(true)}>
                  ✎ 编辑
                </button>
              )}
            </div>
  <div style={{ fontSize: 10.5, color: 'var(--color-text-3)' }}>
    编辑前自动快照至 backups/memory/ · 保存即写回（统一链接自动解析至真身 ~/.agenthub/MEMORY.md）
  </div>
          </>
        ) : (
          <div
            className="card flex flex-col items-center justify-center"
            style={{ minHeight: 300, flex: 1, gap: 10, color: 'var(--color-text-3)' }}
          >
            <div style={{ color: 'var(--color-primary)', opacity: 0.5 }}>
              <LogoMark size={44} />
            </div>
            <div style={{ fontSize: 12 }}>左侧选择要查看的记忆文件</div>
          </div>
        )}
      </div>
    </div>
  );
}

function stepList(steps: string[]) {
  return (
    <div style={{ fontSize: 11.5, color: 'var(--color-text-2)', whiteSpace: 'pre-wrap' }}>
      {steps.map((s, i) => `${i + 1}. ${s}`).join('\n')}
    </div>
  );
}

/** G2 项目记忆：用户手动导入制 —— 导入 / 按项目展开收起 / .ai/memory 文件级编辑·复制·删除·重命名 */
function ProjectMemory() {
  const [projects, setProjects] = useState<ProjectInfo[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [steps, setSteps] = useState<{ path: string; list: string[] } | null>(null);
  // 展开态：同一时间展开一个项目
  const [expanded, setExpanded] = useState<string | null>(null);
  const [files, setFiles] = useState<MemoryListItem[] | null>(null);
  const [filesLoading, setFilesLoading] = useState(false);
  // 文件编辑态（同一时间一个文件）
  const [editPath, setEditPath] = useState<string | null>(null);
  const [editContent, setEditContent] = useState('');
  const [editSaved, setEditSaved] = useState('');
  // 文件重命名态
  const [renamePath, setRenamePath] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState('');

  const refresh = useCallback(async () => {
    setError(null);
    try {
      setProjects(await projectsOverview());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const run = async (label: string, path: string, fn: () => Promise<string[]>) => {
    setBusy(path + label);
    setError(null);
    setMessage(null);
    try {
      const list = await fn();
      setSteps({ path, list });
      setMessage(`${label} 完成`);
      await refresh();
    } catch (e) {
      setError(`${label} 失败：${e}`);
    } finally {
      setBusy(null);
    }
  };

  const loadFiles = useCallback(async (path: string) => {
    setFilesLoading(true);
    setError(null);
    try {
      setFiles(await memoryFilesList(path));
    } catch (e) {
      setError(String(e));
      setFiles([]);
    } finally {
      setFilesLoading(false);
    }
  }, []);

  const toggle = (p: ProjectInfo) => {
    if (expanded === p.path) {
      setExpanded(null);
      setFiles(null);
      return;
    }
    setExpanded(p.path);
    setFiles(null);
    setEditPath(null);
    setRenamePath(null);
    void loadFiles(p.path);
  };

  const importProject = async () => {
    const dir = await openFileDialog({
      directory: true,
      multiple: false,
      title: '选择要导入的项目文件夹',
    });
    if (!dir) return;
    await run('导入', String(dir), () => projectImport(String(dir)));
  };

  // ── .ai/memory 文件级操作 ──

  const startEdit = async (f: MemoryListItem) => {
    setRenamePath(null);
    try {
      const c = await memoryRead(f.path);
      setEditPath(f.path);
      setEditContent(c);
      setEditSaved(c);
    } catch (e) {
      setError(String(e));
    }
  };

  const saveEdit = async () => {
    if (!editPath) return;
    try {
      await memoryWrite(editPath, editContent, false);
      toast('已保存（写入前自动快照至 backups/memory/）');
      setEditPath(null);
      if (expanded) void loadFiles(expanded);
    } catch (e) {
      setError(String(e));
    }
  };

  const copyFile = async (f: MemoryListItem) => {
    try {
      const c = await memoryRead(f.path);
      await copyMemoryText(c);
    } catch (e) {
      setError(String(e));
    }
  };

  const deleteFile = async (f: MemoryListItem) => {
    if (
      !window.confirm(
        `确定删除 ${f.name}？（删除前自动备份到 ~/.agenthub/backups/memory/）`,
      )
    ) {
      return;
    }
    try {
      await memoryFileDelete(f.path);
      toast('已删除（备份保留在 backups/memory/，可手动找回）');
      if (expanded) void loadFiles(expanded);
    } catch (e) {
      setError(String(e));
    }
  };

  const startRename = (f: MemoryListItem) => {
    setEditPath(null);
    setRenamePath(f.path);
    setRenameValue(f.name.replace(/\.md$/i, ''));
  };

  const saveRename = async () => {
    if (!renamePath) return;
    try {
      await memoryFileRename(renamePath, renameValue);
      setRenamePath(null);
      setRenameValue('');
      toast('已重命名');
      if (expanded) void loadFiles(expanded);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div>
      <div className="mb-3 flex items-center" style={{ gap: 8 }}>
        <h5 style={{ fontSize: 14, fontWeight: 800, margin: 0 }}>
          项目记忆（{projects?.length ?? '…'} 个已导入）
        </h5>
        <span style={{ flex: 1 }} />
        {message && <span className="chip ok">{message}</span>}
        <button className="btn gh" onClick={refresh}>↻ 刷新</button>
        <button className="btn p" onClick={() => void importProject()} disabled={busy !== null}>
          ✦ 导入新项目
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

      {steps && (
        <div className="card" style={{ padding: 12, marginBottom: 12 }}>
          <b style={{ fontSize: 12 }}>{steps.path}</b>
          <div style={{ marginTop: 6 }}>{stepList(steps.list)}</div>
        </div>
      )}

      <div className="list">
        {projects === null && (
          <div className="li" style={{ color: 'var(--color-text-3)', fontSize: 12 }}>加载中…</div>
        )}
        {projects?.length === 0 && (
          <div className="li" style={{ color: 'var(--color-text-3)', fontSize: 12 }}>
            未导入项目 —— 点右上角「导入新项目」选择本地文件夹（不再自动识别）
          </div>
        )}
        {projects?.map((p) => (
          <div key={p.path}>
            <div className="li" style={{ flexWrap: 'wrap' }}>
              <button
                className="btn gh"
                style={{ fontSize: 10, width: 26, padding: '3px 0', flex: 'none' }}
                title={expanded === p.path ? '收起' : '展开 .ai/memory 文件列表'}
                onClick={() => toggle(p)}
              >
                {expanded === p.path ? '▼' : '▶'}
              </button>
              <div className="ic">❑</div>
              <div className="grow" style={{ minWidth: 0 }}>
                <div className="nm">
                  {p.name}
                  {p.ai_ready ? (
                    <span className="chip ok" style={{ marginLeft: 6 }}>已桥接 .ai</span>
                  ) : (
                    <span className="chip warn" style={{ marginLeft: 6 }}>未桥接</span>
                  )}
                  {p.bridges.some((b) => b.drifted) && (
                    <span className="chip err" style={{ marginLeft: 6 }}>桥接块漂移</span>
                  )}
                </div>
                <div className="ds">
                  {p.path}
                  {p.bridges.length > 0 && (
                    <>
                      {' · '}
                      {p.bridges
                        .map(
                          (b) =>
                            `${b.file} ${b.drifted ? '漂移' : b.injected ? '✓' : '未注入'}`,
                        )
                        .join(' · ')}
                    </>
                  )}
                  {p.consolidate_candidates > 0 && (
                    <> · 蒸馏候选 {p.consolidate_candidates} 条</>
                  )}
                </div>
              </div>
              <button
                className="btn gh"
                style={{ fontSize: 11, flex: 'none' }}
                title="在资源管理器中打开项目文件夹"
                onClick={() => void openLocalFolder(p.path)}
              >
                📁
              </button>
              {p.ai_ready ? (
                <>
                  <button className="btn gh" style={{ fontSize: 11 }} onClick={() => void run('sync', p.path, () => projectSync(p.path))} disabled={busy !== null}>
                    ↻ sync
                  </button>
                  <button
                    className="btn gh"
                    style={{ fontSize: 11 }}
                    onClick={() => void run('蒸馏', p.path, () => projectConsolidate(p.path))}
                    disabled={busy !== null}
                  >
                    ⚗ 蒸馏{p.consolidate_candidates > 0 ? `（${p.consolidate_candidates}）` : ''}
                  </button>
                </>
              ) : (
                <button className="btn p" style={{ fontSize: 11 }} onClick={() => void run('init', p.path, () => projectInit(p.path))} disabled={busy !== null}>
                  ✦ init
                </button>
              )}
            </div>
            {expanded === p.path && (
              <div
                style={{
                  margin: '0 12px 10px 12px',
                  border: '1px solid var(--color-border)',
                  borderRadius: 10,
                  padding: 10,
                  background: 'var(--color-code-bg)',
                }}
              >
                <div className="flex items-center" style={{ gap: 8, marginBottom: 8 }}>
                  <b style={{ fontSize: 11.5 }}>
                    .ai/memory 记忆文件（{filesLoading ? '…' : files?.length ?? 0}）
                  </b>
                  <span style={{ flex: 1 }} />
                  <button className="btn gh" style={{ fontSize: 11 }} onClick={() => void loadFiles(p.path)}>
                    ↻
                  </button>
                </div>
                {filesLoading && (
                  <div style={{ fontSize: 11.5, color: 'var(--color-text-3)' }}>加载中…</div>
                )}
                {!filesLoading && files?.length === 0 && (
                  <div style={{ fontSize: 11.5, color: 'var(--color-text-3)' }}>
                    暂无记忆文件 —— 各 agent 完成实质工作后按 YYYY-MM-DD-memoryTitle.md 规范记入 .ai/memory/
                  </div>
                )}
                {files?.map((f) => (
                  <div key={f.path} style={{ marginBottom: 8 }}>
                    {renamePath === f.path ? (
                      <div className="flex items-center" style={{ gap: 8 }}>
                        <input
                          value={renameValue}
                          autoFocus
                          onChange={(e) => setRenameValue(e.target.value)}
                          onKeyDown={(e) => {
                            if (e.key === 'Enter') void saveRename();
                            if (e.key === 'Escape') setRenamePath(null);
                          }}
                          style={{
                            flex: 1,
                            minWidth: 0,
                            fontSize: 12,
                            padding: '4px 8px',
                            border: '1px solid var(--color-border)',
                            borderRadius: 8,
                            background: 'var(--color-bg)',
                            color: 'var(--color-text-2)',
                            outline: 'none',
                          }}
                          placeholder="新文件名（.md 后缀自动补全）"
                        />
                        <button className="btn p" style={{ fontSize: 11 }} onClick={() => void saveRename()}>
                          ✓ 确定
                        </button>
                        <button className="btn gh" style={{ fontSize: 11 }} onClick={() => setRenamePath(null)}>
                          ✕
                        </button>
                      </div>
                    ) : (
                      <div className="flex items-center" style={{ gap: 8 }}>
                        <div
                          style={{
                            fontSize: 12,
                            minWidth: 0,
                            overflow: 'hidden',
                            textOverflow: 'ellipsis',
                            whiteSpace: 'nowrap',
                            color: 'var(--color-text-2)',
                          }}
                          title={f.path}
                        >
                          {f.name}
                        </div>
                        <div style={{ fontSize: 10.5, color: 'var(--color-text-3)', flex: 'none' }}>
                          {fmtSize(f.size)} · {f.updated}
                        </div>
                        <span style={{ flex: 1 }} />
                        <button className="btn gh" style={{ fontSize: 11 }} onClick={() => void startEdit(f)}>
                          ✎ 编辑
                        </button>
                        <button className="btn gh" style={{ fontSize: 11 }} onClick={() => void copyFile(f)}>
                          ⧉ 复制
                        </button>
                        <button className="btn gh" style={{ fontSize: 11 }} onClick={() => startRename(f)}>
                          ✏ 重命名
                        </button>
                        <button
                          className="btn gh"
                          style={{ fontSize: 11, color: 'var(--color-danger)' }}
                          onClick={() => void deleteFile(f)}
                        >
                          🗑 删除
                        </button>
                      </div>
                    )}
                    {editPath === f.path && (
                      <div style={{ marginTop: 6 }}>
                        <textarea
                          value={editContent}
                          onChange={(e) => setEditContent(e.target.value)}
                          spellCheck={false}
                          style={{
                            width: '100%',
                            minHeight: 200,
                            border: '1px solid var(--color-border)',
                            borderRadius: 10,
                            padding: 10,
                            background: 'var(--color-bg)',
                            fontFamily: 'Consolas, monospace',
                            fontSize: 11.5,
                            lineHeight: 1.8,
                            color: 'var(--color-text-2)',
                            resize: 'vertical',
                            outline: 'none',
                          }}
                        />
                        <div className="flex items-center" style={{ gap: 8, justifyContent: 'flex-end', marginTop: 6 }}>
                          {editContent !== editSaved && <span className="chip warn">未保存</span>}
                          <button
                            className="btn gh"
                            onClick={() => {
                              setEditPath(null);
                              setEditContent('');
                            }}
                          >
                            取消
                          </button>
                          <button className="btn p" onClick={() => void saveEdit()}>
                            ✓ 保存
                          </button>
                        </div>
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}
          </div>
        ))}
      </div>

      <div style={{ marginTop: 12 }}>
        <div style={{ fontSize: 11, color: 'var(--color-text-3)', marginBottom: 6 }}>
          桥接块预览（幂等标记，注入到各指令文件；只替换标记内内容，绝不碰手写部分）
        </div>
        <div
          style={{
            border: '1px solid var(--color-border)',
            borderRadius: 12,
            padding: 12,
            background: 'var(--color-code-bg)',
            fontFamily: 'Consolas, monospace',
            fontSize: 11,
            lineHeight: 1.8,
            color: 'var(--color-text-2)',
          }}
        >
          {'<!-- agent-hub:start -->'}
          <br />
          本项目所有 AI agent 共享记忆位于 <span style={{ color: 'var(--color-primary-strong)' }}>.ai/</span>：
          <br />
          {'- 开始任务前，先读 .ai/MEMORY.md 和 .ai/memory/ 下与任务相关的记忆文件'}
          <br />
          {'- 完成实质工作后，记入 .ai/memory/（按主题归档，同一主题追加进已有文件）'}
          <br />
          {'  - 文件名规范：YYYY-MM-DD-memoryTitle.md，如 2026-11-26-antd组件库踩坑记录.md'}
          <br />
          {'  - 每条记录必须标注记录人与时间：- [YYYY-MM-DD HH:MM @agent名] 记录内容'}
          <br />
          {'- 稳定的项目约定写入 .ai/PROJECT.md'}
          <br />
          {'<!-- agent-hub:end -->'}
        </div>
        <div style={{ fontSize: 10.5, color: 'var(--color-text-3)', marginTop: 8 }}>
          · 项目为用户手动导入（不再自动识别）· 蒸馏读取项目 .workbuddy/memory/ 自动沉淀，
          去重后标注来源与日期追加进 .ai/MEMORY.md · Windows 上不做文件级 symlink 桥接
        </div>
      </div>
    </div>
  );
}

/** M6 Memory 岛 —— 全局记忆 + 项目记忆 .ai 桥接 */
export function MemoryPage() {
  const [tab, setTab] = useState<Tab>('global');
  return (
    <div>
      <PageHeader title="Memory" sub="全局记忆（用户级）+ 项目记忆 .ai/（跨 Agent 共享）" />
      <div className="mb-3 flex items-center" style={{ gap: 8 }}>
        {(
          [
            ['global', '全局记忆'],
            ['project', '项目记忆（.ai）'],
          ] as [Tab, string][]
        ).map(([v, label]) => (
          <span
            key={v}
            className={`chip ${tab === v ? 'ok' : 'off'}`}
            style={{ cursor: 'pointer', fontSize: 12, padding: '5px 14px' }}
            onClick={() => setTab(v)}
          >
            {label}
          </span>
        ))}
      </div>
      {tab === 'global' ? <GlobalMemory /> : <ProjectMemory />}
    </div>
  );
}
