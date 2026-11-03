/**
 * 前端 agent 注册表 —— 与 Rust adapters::registry() 对应。
 * 新增 agent 适配器（社区贡献）时在此补一条：id 稳定、身份色给一个 defaultColor
 * （同时把同名 CSS 变量 --color-agent-<id> 加进 theme.css 作为明暗双主题默认值），
 * 用户可在 Plugin 岛点色点自定义；未知 agent 自动回落中性样式。
 */
export interface AgentMeta {
  id: string;
  name: string;
  /** 身份色 CSS 变量引用（theme.css 定义；undefined = 中性色） */
  accentVar?: string;
  /** 身份色变量名（用于运行时覆盖注入；与 accentVar 对应） */
  colorVar?: string;
  /** 默认身份色（浅色主题值；图表等 JS 上下文兜底用） */
  defaultColor?: string;
  /** chip 的身份色 class（zc = 蓝，wb 走默认 ok 绿；undefined = 默认） */
  chipClass?: string;
}

export const AGENTS: Record<string, AgentMeta> = {
  workbuddy: {
    id: 'workbuddy',
    name: 'WorkBuddy',
    accentVar: 'var(--color-agent-wb)',
    colorVar: '--color-agent-wb',
    defaultColor: '#2E9E74',
    chipClass: 'wb',
  },
  zcode: {
    id: 'zcode',
    name: 'ZCode',
    accentVar: 'var(--color-agent-zcode)',
    colorVar: '--color-agent-zcode',
    defaultColor: '#4AA3DF',
    chipClass: 'zc',
  },
  // Claude Code 插件（adapters/claude.rs）：正式适配器，参与 Skills/MCP/用量采集；
  // 同 id 承接 cc-switch 备份导入（--import-ccswitch）的历史用量，天然合并展示
  claude: {
    id: 'claude',
    name: 'Claude Code',
    accentVar: 'var(--color-agent-claude)',
    colorVar: '--color-agent-claude',
    defaultColor: '#D97757',
    chipClass: 'claude',
  },
};

/** 按 id 取元信息；未注册的 agent（未来社区适配器）回落中性展示 */
export function agentMeta(id: string): AgentMeta {
  return AGENTS[id] ?? { id, name: id };
}

/** chip 正常态 class：注册表有身份色用之，否则默认 ok 绿 */
export function agentChipClass(id: string, fallback = 'ok'): string {
  const meta = AGENTS[id];
  return meta?.chipClass ?? fallback;
}

/* ── Agent 主题色：用户可配置（Plugin 岛色点弹窗），存 localStorage、启动注入 :root ── */

const COLOR_KEY = 'agenthub.agentColors';

type ColorMap = Record<string, string>;

function readColors(): ColorMap {
  try {
    return JSON.parse(localStorage.getItem(COLOR_KEY) ?? '{}') as ColorMap;
  } catch {
    return {};
  }
}

/** 用户自定义色（未配置返回 null = 跟随主题默认） */
export function getConfiguredColor(id: string): string | null {
  return readColors()[id] ?? null;
}

/** JS 上下文取身份色（图表/色板用）：自定义色优先，否则注册表默认色 */
export function agentColor(id: string): string {
  return getConfiguredColor(id) ?? AGENTS[id]?.defaultColor ?? '#8CA096';
}

/** 设置 agent 主题色并立即全局生效；color = null 恢复主题默认 */
export function setAgentColor(id: string, color: string | null): void {
  const map = readColors();
  if (color) map[id] = color;
  else delete map[id];
  localStorage.setItem(COLOR_KEY, JSON.stringify(map));
  applyAgentColors();
}

/** 启动时把用户自定义身份色注入 DOM（未配置的变量保持 CSS 明暗双主题默认值）。
 *  必须注入到 document.body：暗色主题在 body.dark 上重新声明了这些变量，
 *  body 比 html 离内容更近，注入 html 会被 body.dark 的默认值盖掉。 */
export function applyAgentColors(): void {
  const root = document.body ?? document.documentElement;
  const map = readColors();
  for (const [id, color] of Object.entries(map)) {
    const v = AGENTS[id]?.colorVar;
    if (v && /^#[0-9a-fA-F]{3,8}$/.test(color)) {
      root.style.setProperty(v, color);
    }
  }
}
