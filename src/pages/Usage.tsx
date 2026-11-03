import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { agentChipClass, agentColor as agentColorOf } from '../agents';
import * as echarts from 'echarts/core';
import {
  GridComponent,
  LegendComponent,
  TitleComponent,
  TooltipComponent,
} from 'echarts/components';
import { BarChart, LineChart, PieChart } from 'echarts/charts';
import { CanvasRenderer } from 'echarts/renderers';
import {
  usageAccounts,
  usageCollect,
  usageMatrix,
  usageOverview,
  wbOverview,
  type MatrixCell,
  type UsageAccount,
  type UsageOverview,
} from '../api';
import { PageHeader } from '../common/components/PageHeader';
import { useThemeStore } from '../stores/theme';

// ECharts 按需导入
echarts.use([
  GridComponent,
  LegendComponent,
  TitleComponent,
  TooltipComponent,
  BarChart,
  LineChart,
  PieChart,
  CanvasRenderer,
]);

type Granularity = 'day' | 'week' | 'month' | 'year' | 'all';
type ViewMode = 'calendar' | 'chart';
type AgentFilter = 'all' | 'workbuddy' | 'zcode' | 'claude';
/** 范围筛选：agent 维度 + 账号维度（account='all' 表示该 Agent 全部账号） */
interface Scope {
  agent: AgentFilter;
  account: string;
}
const SCOPE_ALL: Scope = { agent: 'all', account: 'all' };
const matchScope = (agent: string, account: string, s: Scope): boolean =>
  (s.agent === 'all' || agent === s.agent) && (s.account === 'all' || account === s.account);
const scopeEq = (a: Scope, b: Scope): boolean => a.agent === b.agent && a.account === b.account;

const AGENT_LABEL: Record<string, string> = { zcode: 'ZCode', workbuddy: 'WorkBuddy', claude: 'Claude Code' };
/** 双 Agent 身份色：读 Plugin 岛可配置色（自定义优先，回落注册表默认）；汇总 = 主绿 */
const agentColor = (agent: string) =>
  agent === 'all' ? '#3EB489' : agentColorOf(agent);
/** 模型细分图（柱/饼）显示下限：低于该消耗量的分类直接不显示 */
const MODEL_MIN_TOKENS = 100_000;
/** 同 Agent 多模型时的深浅变体：基调 = 可配置身份色，其余按明度生成深浅交错 */
const agentShades = (id: string): string[] => {
  const base = agentColorOf(id);
  const n = parseInt(base.slice(1), 16);
  const mix = (t: number, dark: boolean) => {
    const ch = (v: number) => Math.max(0, Math.min(255, Math.round(v + (dark ? -t * v : (255 - v) * t))));
    const r = ch((n >> 16) & 255), g = ch((n >> 8) & 255), b = ch(n & 255);
    return `#${((r << 16) | (g << 8) | b).toString(16).padStart(6, '0')}`;
  };
  return [base, mix(0.28, false), mix(0.32, true), mix(0.55, false)];
};
const AGENT_PALETTE: Record<string, string[]> = {
  workbuddy: agentShades('workbuddy'),
  zcode: agentShades('zcode'),
  claude: agentShades('claude'),
};

const fmtTok = (n: number): string => {
  if (n >= 1e8) return `${(n / 1e8).toFixed(2)} 亿`;
  if (n >= 1e4) return `${(n / 1e4).toFixed(1)} 万`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(1)} 千`;
  return String(n);
};

const themeColors = () => {
  const cs = getComputedStyle(document.body);
  const v = (name: string, fallback: string) => cs.getPropertyValue(name).trim() || fallback;
  return {
    text2: v('--color-text-2', '#55685E'),
    text3: v('--color-text-3', '#8CA096'),
    border: v('--color-border', '#DFEDE3'),
    primary: v('--color-primary', '#3EB489'),
  };
};

/** 模型取色：按 Agent 身份色系，同 Agent 内按出现次序取深浅变体 */
const modelColorFn = () => {
  const counters: Record<string, number> = {};
  return (agent: string) => {
    const pal = AGENT_PALETTE[agent] ?? agentShades(agent);
    const i = counters[agent] ?? 0;
    counters[agent] = i + 1;
    return pal[i % pal.length];
  };
};

const todayStr = () => {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
};
const todayYm = () => todayStr().slice(0, 7);
/** 该日期所在周的周一 */
const mondayOf = (date: string): string => {
  const d = new Date(`${date}T00:00:00`);
  const lead = (d.getDay() + 6) % 7;
  d.setDate(d.getDate() - lead);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
};
const shiftDate = (date: string, days: number): string => {
  const d = new Date(`${date}T00:00:00`);
  d.setDate(d.getDate() + days);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
};
const shiftMonth = (ym: string, delta: number): string => {
  const [y, m] = ym.split('-').map(Number);
  const d = new Date(y, m - 1 + delta, 1);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`;
};
/** 某月内所有周的周一（周一落在本月才算该月的周；跨月的周归周一所在月） */
const mondaysOfMonth = (ym: string): string[] => {
  const [y, m] = ym.split('-').map(Number);
  const out: string[] = [];
  let d = mondayOf(`${ym}-01`);
  if (Number(d.slice(5, 7)) !== m || Number(d.slice(0, 4)) !== y) d = shiftDate(d, 7);
  while (Number(d.slice(5, 7)) === m && Number(d.slice(0, 4)) === y) {
    out.push(d);
    d = shiftDate(d, 7);
  }
  return out;
};

/** 聚合视图：某 key（日/月/年）下的合计与 agent-account-model 明细 */
interface Bucket {
  tokens: number;
  calls: number;
  details: { agent: string; account: string; model: string; tokens: number }[];
}

function buildBuckets(matrix: MatrixCell[]): {
  byDay: Map<string, Bucket>;
  byMonth: Map<string, Bucket>;
  byYear: Map<string, Bucket>;
} {
  const byDay = new Map<string, Bucket>();
  const byMonth = new Map<string, Bucket>();
  const byYear = new Map<string, Bucket>();
  const touch = (m: Map<string, Bucket>, k: string) => {
    let b = m.get(k);
    if (!b) {
      b = { tokens: 0, calls: 0, details: [] };
      m.set(k, b);
    }
    return b;
  };
  for (const c of matrix) {
    const bDay = touch(byDay, c.date);
    const ym = c.date.slice(0, 7);
    const bMon = touch(byMonth, ym);
    const by = touch(byYear, c.date.slice(0, 4));
    for (const b of [bDay, bMon, by]) {
      b.tokens += c.tokens;
      b.calls += c.calls;
      // 同 agent-account-model 合并（月/年桶跨天重复出现，必须聚合而非逐条追加）
      const ex = b.details.find(
        (d) => d.agent === c.agent && d.account === c.account && d.model === c.model,
      );
      if (ex) ex.tokens += c.tokens;
      else b.details.push({ agent: c.agent, account: c.account, model: c.model, tokens: c.tokens });
    }
  }
  return { byDay, byMonth, byYear };
}

/** hover 明细文本：workbuddy-glm-5.3-flash: 1.2亿 token */
const detailText = (b: Bucket | undefined, scope: Scope): string => {
  if (!b) return '0 token';
  const lines = b.details
    .filter((d) => matchScope(d.agent, d.account, scope))
    .sort((a, b2) => b2.tokens - a.tokens)
    .map((d) => `${d.agent}-${d.model}: ${fmtTok(d.tokens)} token`);
  return lines.length > 0 ? lines.join('\n') : '0 token';
};

/** 五档色阶（蓝灰起步、绿色收顶，档位色随主题切换，见 theme.css --heat-N） */
const heatColor = (v: number, max: number): string => {
  if (!v) return 'var(--color-surface2)';
  const r = v / Math.max(1, max);
  if (r > 0.6) return 'var(--heat-4)';
  if (r > 0.35) return 'var(--heat-3)';
  if (r > 0.15) return 'var(--heat-2)';
  if (r > 0.04) return 'var(--heat-1)';
  return 'var(--heat-0)';
};

/** ECharts hook：callback ref —— 容器挂载/卸载（如视图切换的条件渲染）时自动 init/dispose */
function useEChart(factory: () => echarts.EChartsCoreOption, deps: unknown[]) {
  const chartRef = useRef<echarts.ECharts | null>(null);
  const [el, setEl] = useState<HTMLDivElement | null>(null);
  const { resolved } = useThemeStore();
  useEffect(() => {
    if (!el) return;
    const chart = echarts.init(el);
    chartRef.current = chart;
    const ro = new ResizeObserver(() => chart.resize());
    ro.observe(el);
    return () => {
      ro.disconnect();
      chart.dispose();
      chartRef.current = null;
    };
  }, [el]);
  useEffect(() => {
    if (chartRef.current) chartRef.current.setOption(factory(), true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [el, resolved, ...deps]);
  return useCallback((node: HTMLDivElement | null) => setEl(node), []);
}

/**
 * 模块级数据缓存（组件卸载不丢）：页面切换回来时若缓存仍在有效期内，
 * 直接复用、跳过全量采集（usageCollect 要扫 jsonl，是最重的一步）。
 * 「刷新」按钮 / 5min 轮询才真正重新采集。
 */
const CACHE_TTL = 5 * 60 * 1000;
let dataCache: {
  at: number;
  matrix: MatrixCell[];
  accounts: UsageAccount[];
  slotLabels: Record<string, string>;
} | null = null;

/**
 * Usage 岛 —— 盈亏日历式三级下钻（年历 → 月历 → 日历）+ 日历/曲线切换 +
 * hover 显示 agent-model 消耗明细 + Agent 三开关。当日消耗用 24h 曲线呈现。
 */
export function UsagePage() {
  const [granularity, setGranularity] = useState<Granularity>('day');
  const [viewMode, setViewMode] = useState<ViewMode>('calendar');
  const [dayYm, setDayYm] = useState(todayYm()); // 日历视图当前月
  const [monY, setMonY] = useState(new Date().getFullYear()); // 月历视图当前年
  const [selectedDate, setSelectedDate] = useState(todayStr()); // 点日格选中的日期
  const [weekStart, setWeekStart] = useState<string>(mondayOf(todayStr())); // 周视图当前周（周一）
  const [weekCalYm, setWeekCalYm] = useState<string>(todayYm()); // 周历锚点月（一页中最新的月份，向前显示其与前两月）
  const [scope, setScope] = useState<Scope>(SCOPE_ALL);
  const [accounts, setAccounts] = useState<UsageAccount[]>([]);
  const [slotLabels, setSlotLabels] = useState<Record<string, string>>({});
  const [matrix, setMatrix] = useState<MatrixCell[] | null>(null);
  const [overview, setOverview] = useState<UsageOverview | null>(null);
  const [progress, setProgress] = useState<{ stage: string; done: number; total: number } | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 采集进度事件
  useEffect(() => {
    let un: (() => void) | null = null;
    listen<{ stage: string; done: number; total: number }>('usage-collect-progress', (e) => {
      setProgress(e.payload);
      if (e.payload.done >= e.payload.total) {
        setTimeout(() => setProgress(null), 1200);
      }
    })
      .then((f) => {
        un = f;
      })
      .catch(() => {});
    return () => {
      un?.();
    };
  }, []);

  const refresh = useCallback(async (force = true) => {
    // 缓存有效期内直接复用（切页回来的挂载路径），不触发全量采集
    if (!force && dataCache && Date.now() - dataCache.at < CACHE_TTL) {
      setMatrix(dataCache.matrix);
      setAccounts(dataCache.accounts);
      setSlotLabels(dataCache.slotLabels);
      return;
    }
    setRefreshing(true);
    setError(null);
    try {
      await usageCollect();
      const [mx, ac] = await Promise.all([usageMatrix(), usageAccounts()]);
      setMatrix(mx);
      setAccounts(ac);
      // WB 槽位 label 用于账户 chip 文案（uid → label；失败降级 uid 前缀）
      const labels: Record<string, string> = {};
      try {
        const o = await wbOverview();
        for (const s of o.slots) if (s.uid && !(s.uid in labels)) labels[s.uid] = s.label;
      } catch {
        /* label 拿不到就降级 uid 前缀，不阻塞主流程 */
      }
      setSlotLabels(labels);
      dataCache = { at: Date.now(), matrix: mx, accounts: ac, slotLabels: labels };
    } catch (e) {
      setError(String(e));
    } finally {
      setRefreshing(false);
    }
  }, []);

  // 挂载：优先吃缓存（有效期内零采集），过期才全量
  useEffect(() => {
    void refresh(false);
  }, [refresh]);

  // 页面停留期间 5min 轮询，保持数据新鲜（同时给缓存续期）
  useEffect(() => {
    const t = setInterval(() => void refresh(true), CACHE_TTL);
    return () => clearInterval(t);
  }, [refresh]);

  // 当前上下文的 overview range
  const overviewRange =
    granularity === 'day'
      ? `date:${selectedDate}`
      : granularity === 'week'
        ? `week:${weekStart}`
        : granularity === 'month'
          ? `month:${dayYm}`
          : 'all';
  useEffect(() => {
    void usageOverview(overviewRange)
      .then(setOverview)
      .catch((e) => setError(String(e)));
  }, [overviewRange, matrix]);

  /** 账户 chip 文案：WB 多账号用槽位 label（无则 uid 前 8 位）；其余单账号端直接用 agent 名 */
  const accountLabel = useCallback(
    (agent: string, account: string): string => {
      if (agent !== 'workbuddy') return AGENT_LABEL[agent] ?? agent;
      const lb = slotLabels[account];
      return `WB·${lb && lb.trim() ? lb : account.slice(0, 8) || '未归属'}`;
    },
    [slotLabels],
  );

  /** 当前范围的显示名 */
  const scopeLabel =
    scope.account !== 'all'
      ? accountLabel(scope.agent, scope.account)
      : scope.agent === 'all'
        ? '全部 Agent'
        : AGENT_LABEL[scope.agent] ?? scope.agent;

  // agent 过滤后的聚合
  const filtered = useMemo(() => {
    const rows = (matrix ?? []).filter((c) => matchScope(c.agent, c.account, scope));
    return buildBuckets(rows);
  }, [matrix, scope]);

  // 切月联动：翻月/下钻只写 dayYm，这里把选中日钳进新月份，24h 曲线、标题、日格高亮同步跟随
  useEffect(() => {
    if (selectedDate.slice(0, 7) === dayYm) return; // 已在当月（含点日格手动选日）不动
    if (dayYm === todayYm()) {
      setSelectedDate(todayStr()); // 翘回本月 = 今天，与初始态一致
      return;
    }
    // 历史月：选该月用量最大的日（与日历格同一套 scope 过滤口径），全空月回落 1 号
    let best: string | null = null;
    let bestTok = -1;
    for (const [date, b] of filtered.byDay) {
      if (date.startsWith(dayYm)) {
        const tok = b.details.reduce((s, x) => s + x.tokens, 0);
        if (tok > bestTok) {
          bestTok = tok;
          best = date;
        }
      }
    }
    setSelectedDate(best ?? `${dayYm}-01`);
  }, [dayYm, filtered, selectedDate]);

  const totals = useMemo(() => {
    const list = (overview?.totals ?? []).filter((t) => matchScope(t.agent, t.account, scope));
    const sum = (f: (t: (typeof list)[number]) => number) => list.reduce((a, t) => a + f(t), 0);
    return {
      calls: sum((t) => t.calls),
      input: sum((t) => t.input_tokens),
      cache: sum((t) => t.cache_read_tokens),
      all: sum((t) => t.input_tokens + t.output_tokens + t.cache_read_tokens),
      credit: sum((t) => t.credit),
    };
  }, [overview, scope]);

  /** 出现过用量的 agent 列表（数据驱动：WB/ZCode/Claude 及未来扩展自动进 chips 与曲线 series） */
  const agentIds = useMemo(() => {
    const s = new Set<string>();
    for (const t of overview?.totals ?? []) s.add(t.agent);
    for (const a of accounts) s.add(a.agent);
    const prio = ['workbuddy', 'zcode', 'claude'];
    return [...s].sort(
      (x, y) =>
        (prio.indexOf(x) === -1 ? 99 : prio.indexOf(x)) -
        (prio.indexOf(y) === -1 ? 99 : prio.indexOf(y)),
    );
  }, [overview, accounts]);

  // ── 日历模式渲染数据 ──
  const maxOfDay = Math.max(1, ...[...filtered.byDay.values()].map((b) => b.tokens));
  const maxOfMonth = Math.max(1, ...[...filtered.byMonth.values()].map((b) => b.tokens));
  const maxOfYear = Math.max(1, ...[...filtered.byYear.values()].map((b) => b.tokens));
  const years = useMemo(
    () => [...filtered.byYear.keys()].sort().reverse(),
    [filtered.byYear],
  );

  /** 日历格子（日粒度）：当月网格 */
  const dayCells = useMemo(() => {
    const [y, m] = dayYm.split('-').map(Number);
    const first = new Date(y, m - 1, 1);
    const lead = (first.getDay() + 6) % 7; // 周一开头
    const days = new Date(y, m, 0).getDate();
    const cells: { day: number | null; date?: string }[] = [];
    for (let i = 0; i < lead; i += 1) cells.push({ day: null });
    for (let d = 1; d <= days; d += 1) {
      cells.push({ day: d, date: `${dayYm}-${String(d).padStart(2, '0')}` });
    }
    return cells;
  }, [dayYm]);

  const dayCard = (date: string): Bucket | undefined => filtered.byDay.get(date);

  // ── 曲线模式（echarts）：x = 时间（按天/按月/按年），堆叠柱 + hover 明细 ──
  const chartRef = useEChart(() => {
    const c = themeColors();
    const agents = scope.agent === 'all' ? agentIds : [scope.agent];
    const seriesName = (a: string) =>
      scope.account !== 'all' ? accountLabel(a, scope.account) : (AGENT_LABEL[a] ?? a);
    let cats: string[] = [];
    let seriesData: Record<string, number[]> = {};
    let title = '';
    if (granularity === 'day') {
      cats = dayCells.filter((x) => x.day).map((x) => x.date!);
      title = `按天用量 · ${dayYm}`;
      for (const a of agents) {
        seriesData[a] = cats.map((d) => {
          const b = filtered.byDay.get(d);
          if (!b) return 0;
          return b.details
            .filter((x) => x.agent === a && (scope.account === 'all' || x.account === scope.account))
            .reduce((s, x) => s + x.tokens, 0);
        });
      }
    } else if (granularity === 'week') {
      cats = Array.from({ length: 7 }, (_, i) => shiftDate(weekStart, i));
      title = `按天用量 · ${weekStart.slice(5)} ~ ${shiftDate(weekStart, 6).slice(5)}`;
      for (const a of agents) {
        seriesData[a] = cats.map((d) => {
          const b = filtered.byDay.get(d);
          if (!b) return 0;
          return b.details
            .filter((x) => x.agent === a && (scope.account === 'all' || x.account === scope.account))
            .reduce((s, x) => s + x.tokens, 0);
        });
      }
    } else if (granularity === 'month' || granularity === 'all') {
      // 月：'month' 为当年 12 个月；'all' 为全历史出现的每一个月
      cats =
        granularity === 'month'
          ? Array.from({ length: 12 }, (_, i) => `${monY}-${String(i + 1).padStart(2, '0')}`)
          : [...filtered.byMonth.keys()].sort();
      title = granularity === 'month' ? `按月用量 · ${monY}` : '按月用量 · 全部历史';
      for (const a of agents) {
        seriesData[a] = cats.map((ym) => {
          const b = filtered.byMonth.get(ym);
          if (!b) return 0;
          return b.details
            .filter((x) => x.agent === a && (scope.account === 'all' || x.account === scope.account))
            .reduce((s, x) => s + x.tokens, 0);
        });
      }
    } else if (granularity === 'year') {
      cats = [...filtered.byYear.keys()].sort();
      title = '按年用量';
      for (const a of agents) {
        seriesData[a] = cats.map((y) => {
          const b = filtered.byYear.get(y);
          if (!b) return 0;
          return b.details
            .filter((x) => x.agent === a && (scope.account === 'all' || x.account === scope.account))
            .reduce((s, x) => s + x.tokens, 0);
        });
      }
    }
    return {
      backgroundColor: 'transparent',
      title: { text: title, left: 12, top: 8, textStyle: { color: c.text2, fontSize: 12, fontWeight: 700 } },
      tooltip: {
        trigger: 'axis',
        formatter: (ps: { axisValue: string; seriesName: string; value: number }[]) => {
          const key =
            granularity === 'day'
              ? ps[0]?.axisValue
              : granularity === 'month'
                ? ps[0]?.axisValue
                : ps[0]?.axisValue;
          const b =
            granularity === 'day'
              ? filtered.byDay.get(key)
              : granularity === 'year'
                ? filtered.byYear.get(key)
                : filtered.byMonth.get(key);
          const lines = (ps ?? []).map(
            (p) => `${p.seriesName}: ${fmtTok(p.value)} tok`,
          );
          const detail = detailText(b, scope)
            .split('\n')
            .map((l) => `<div style="opacity:.75">${l}</div>`)
            .join('');
          return `${ps[0]?.axisValue ?? ''}<br/>${lines.join('<br/>')}<br/>${detail}`;
        },
      },
      legend: { top: 8, right: 12, textStyle: { color: c.text2 } },
      grid: { left: 56, right: 20, top: 44, bottom: 28 },
      xAxis: {
        type: 'category',
        data: cats.map((x) =>
          granularity === 'day' ? x.slice(5) : granularity === 'all' ? x.slice(2) : x,
        ),
        axisLabel: { color: c.text3 },
        axisLine: { lineStyle: { color: c.border } },
      },
      yAxis: {
        type: 'value',
        axisLabel: { color: c.text3, formatter: (v: number) => fmtTok(v) },
        splitLine: { lineStyle: { color: c.border, type: 'dashed' } },
      },
      series: agents.map((a) => ({
        name: seriesName(a),
        type: 'bar' as const,
        stack: 'total',
        data: seriesData[a] ?? [],
        itemStyle: { color: agentColor(a), borderRadius: [3, 3, 0, 0] },
        barMaxWidth: 32,
      })),
    } satisfies echarts.EChartsCoreOption;
  }, [granularity, filtered, dayYm, monY, scope, accountLabel, agentIds]);

  const granularityChip = (g: Granularity, label: string) => (
    <span
      key={g}
      className={`chip ${granularity === g ? 'ok' : 'off'}`}
      style={{ cursor: 'pointer', fontSize: 12, padding: '4px 14px' }}
      onClick={() => setGranularity(g)}
    >
      {label}
    </span>
  );

  const navLabel =
    granularity === 'day'
      ? dayYm
      : granularity === 'week'
        ? `${weekStart.slice(5)} ~ ${shiftDate(weekStart, 6).slice(5)}`
        : granularity === 'month'
          ? String(monY)
          : granularity === 'year'
            ? '全部年份'
            : '全部历史';

  const navShift = (delta: number) => {
    if (granularity === 'day') setDayYm(shiftMonth(dayYm, delta));
    else if (granularity === 'week') setWeekStart(shiftDate(weekStart, delta * 7));
    else if (granularity === 'month') setMonY((y) => y + delta);
  };

  return (
    <div>
      <PageHeader
        title="用量分析"
        sub="请求级真实 usage · 历史永久留存 ~/.agenthub（卸载 Agent 后仍可查）· 正文永不入库"
      />

      {/* 首次全量提取进度条 */}
      {progress && progress.done < progress.total && (
        <div className="card" style={{ padding: 12, marginBottom: 12 }}>
          <div className="flex items-center" style={{ gap: 10 }}>
            <span className="spinning" style={{ color: 'var(--color-primary)' }}>↻</span>
            <div style={{ fontSize: 12, color: 'var(--color-text-2)', flex: 'none' }}>
              正在提取本地用量数据（{progress.stage === 'zcode' ? 'ZCode 账本' : 'WorkBuddy 日志'} ·{' '}
              {progress.done}/{progress.total}）
            </div>
            <div style={{ flex: 1 }}>
              <div className="bar">
                <i style={{ width: `${Math.round((progress.done / Math.max(1, progress.total)) * 100)}%` }} />
              </div>
            </div>
          </div>
        </div>
      )}

      {/* 控制行：粒度 + 导航 + 视图切换 + Agent */}
      <div className="mb-3 flex items-center" style={{ gap: 8, flexWrap: 'wrap' }}>
        <b style={{ fontSize: 12, color: 'var(--color-text-2)' }}>粒度</b>
        {granularityChip('day', '日')}
        {granularityChip('week', '周')}
        {granularityChip('month', '月')}
        {granularityChip('year', '年')}
        {granularityChip('all', '全部')}
        {granularity !== 'year' && granularity !== 'all' && (
          <>
            <button className="btn gh" style={{ fontSize: 11, padding: '3px 10px' }} onClick={() => navShift(-1)}>
              ‹
            </button>
            <span
              className="chip off"
              style={{ fontFamily: 'Consolas, monospace', fontSize: 12, padding: '4px 14px' }}
            >
              {navLabel}
            </span>
            <button className="btn gh" style={{ fontSize: 11, padding: '3px 10px' }} onClick={() => navShift(1)}>
              ›
            </button>
          </>
        )}
        <span style={{ flex: 1 }} />
        {refreshing && <span className="chip warn">采集中…</span>}
        <button className="btn gh" onClick={() => void refresh()} disabled={refreshing}>
          <span className={refreshing ? 'spinning' : ''} style={{ display: 'inline-block' }}>↻</span> 刷新
        </button>
        <span
          className={`chip ${viewMode === 'calendar' ? 'ok' : 'off'}`}
          style={{ cursor: 'pointer' }}
          onClick={() => setViewMode('calendar')}
        >
          日历
        </span>
        <span
          className={`chip ${viewMode === 'chart' ? 'ok' : 'off'}`}
          style={{ cursor: 'pointer' }}
          onClick={() => setViewMode('chart')}
        >
          曲线
        </span>
      </div>
      <div className="mb-3 flex items-center" style={{ gap: 8, flexWrap: 'wrap' }}>
        <b style={{ fontSize: 12, color: 'var(--color-text-2)' }}>范围</b>
        {(
          [
            [{ agent: 'all', account: 'all' }, '汇总'],
            ...agentIds.map((a) => [{ agent: a, account: 'all' }, `${AGENT_LABEL[a] ?? a} 汇总`]),
          ] as [Scope, string][]
        ).map(([v, label]) => (
          <span
            key={label}
            className={`chip ${scopeEq(scope, v) ? agentChipClass(v.agent) : 'off'}`}
            style={{ cursor: 'pointer', fontSize: 11.5, padding: '4px 12px' }}
            onClick={() => setScope(v)}
          >
            {label}
          </span>
        ))}
        {accounts.length > 0 && (
          <span style={{ color: 'var(--color-border)', margin: '0 2px' }}>|</span>
        )}
        {accounts.map((a) => {
          const s: Scope = { agent: a.agent as AgentFilter, account: a.account };
          const active = scopeEq(scope, s);
          return (
            <span
              key={`${a.agent}:${a.account}`}
              className={`chip ${active ? agentChipClass(a.agent) : 'off'}`}
              style={{ cursor: 'pointer', fontSize: 11.5, padding: '4px 12px' }}
              title={`${AGENT_LABEL[a.agent] ?? a.agent} · ${a.calls} 次调用`}
              onClick={() => setScope(s)}
            >
              {accountLabel(a.agent, a.account)}
            </span>
          );
        })}
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

      {/* 四张统计卡（当前下钻上下文） */}
      <div className="mb-3 grid grid-cols-4 gap-2.5">
        <div className="stat">
          <div className="k">
            {scopeLabel} 合计
            <span className="chip ok" style={{ marginLeft: 2 }}>请求级真实</span>
          </div>
          <div className="v">{fmtTok(totals.all)}<small> tok</small></div>
          <div className="k">
            {granularity === 'day'
              ? selectedDate
              : granularity === 'month'
                ? dayYm
                : '全部'}
            {totals.credit > 0 && <> · 官方积分 {Math.round(totals.credit).toLocaleString()}</>}
          </div>
        </div>
        <div className="stat">
          <div className="k">调用次数</div>
          <div className="v">{totals.calls}<small> 次</small></div>
          <div className="k">请求级统计</div>
        </div>
        <div className="stat">
          <div className="k">输入 tok</div>
          <div className="v">{fmtTok(totals.input)}<small> tok</small></div>
          <div className="k">≈ ZCode 官方「累计 Token」口径</div>
        </div>
        <div className="stat">
          <div className="k">缓存命中</div>
          <div className="v" style={{ color: 'var(--color-ok)' }}>{fmtTok(totals.cache)}<small> tok</small></div>
          <div className="k">cache_read</div>
        </div>
      </div>

      {/* 主区：日历 或 曲线。day 粒度下日历与当日 24h 曲线左右并排且等高
          （各 min 500px，一行放不下两个时自动换行；单卡片时占满整行） */}
      <div style={{ display: 'flex', flexWrap: 'wrap', gap: 12, marginBottom: 12 }}>
      {viewMode === 'calendar' ? (
        <div className="card" style={{ padding: 14, flex: '1 1 500px', minWidth: 500 }}>
          {/* 年历：年份格 */}
          {granularity === 'year' && (
            <>
              <div style={{ fontSize: 12, fontWeight: 700, color: 'var(--color-text-2)', marginBottom: 8 }}>
                年历 · 点击年份下钻到月历
              </div>
              <div className="grid grid-cols-4 gap-2.5">
                {years.length === 0 && (
                  <div style={{ fontSize: 12, color: 'var(--color-text-3)' }}>暂无数据</div>
                )}
                {years.map((y) => {
                  const b = filtered.byYear.get(y);
                  return (
                    <div
                      key={y}
                      className="card hover-zoom"
                      style={{ padding: 12, cursor: 'pointer' }}
                      title={detailText(b, scope)}
                      onClick={() => {
                        setMonY(Number(y));
                        setGranularity('month');
                      }}
                    >
                      <div className="flex items-center justify-between">
                        <b style={{ fontSize: 14 }}>{y}</b>
                        <span className="chip off">{b?.calls ?? 0} 次</span>
                      </div>
                      <div style={{ fontSize: 16, fontWeight: 800, fontFamily: 'Consolas, monospace', marginTop: 4 }}>
                        {fmtTok(b?.tokens ?? 0)} <small style={{ fontWeight: 400 }}>tok</small>
                      </div>
                      <div className="bar" style={{ marginTop: 6 }}>
                        <i style={{ width: `${((b?.tokens ?? 0) / maxOfYear) * 100}%` }} />
                      </div>
                    </div>
                  );
                })}
              </div>
            </>
          )}

          {/* 全部：逐年月历网格（历史全量） */}
          {granularity === 'all' && (
            <>
              <div style={{ fontSize: 12, fontWeight: 700, color: 'var(--color-text-2)', marginBottom: 8 }}>
                全部历史 · 逐年月历 · 点击月份下钻到日历
              </div>
              {[...years].reverse().map((y) => {
                const maxOfYearAll = Math.max(
                  1,
                  ...Array.from({ length: 12 }, (_, i) =>
                    filtered.byMonth.get(`${y}-${String(i + 1).padStart(2, '0')}`)?.tokens ?? 0,
                  ),
                );
                return (
                  <div key={y} style={{ marginBottom: 14 }}>
                    <div style={{ fontSize: 12, fontWeight: 700, margin: '2px 0 6px', color: 'var(--color-text-2)' }}>
                      {y} 年
                    </div>
                    <div className="grid grid-cols-4 gap-2.5">
                      {Array.from({ length: 12 }, (_, i) => {
                        const ym = `${y}-${String(i + 1).padStart(2, '0')}`;
                        const b = filtered.byMonth.get(ym);
                        return (
                          <div
                            key={ym}
                            className="card hover-zoom"
                            style={{
                              padding: 12,
                              cursor: b ? 'pointer' : 'default',
                              opacity: b ? 1 : 0.45,
                              borderColor: b ? heatColor(b.tokens, maxOfYearAll) : undefined,
                            }}
                            title={detailText(b, scope)}
                            onClick={
                              b
                                ? () => {
                                    setDayYm(ym);
                                    setMonY(Number(y));
                                    setGranularity('day');
                                  }
                                : undefined
                            }
                          >
                            <div className="flex items-center justify-between">
                              <b style={{ fontSize: 13 }}>{ym}</b>
                              <span className="chip off">{b?.calls ?? 0} 次</span>
                            </div>
                            <div style={{ fontSize: 14, fontWeight: 800, fontFamily: 'Consolas, monospace', marginTop: 4 }}>
                              {b ? fmtTok(b.tokens) : '—'}
                            </div>
                            <div className="bar" style={{ marginTop: 6 }}>
                              <i style={{ width: `${((b?.tokens ?? 0) / maxOfYearAll) * 100}%` }} />
                            </div>
                          </div>
                        );
                      })}
                    </div>
                  </div>
                );
              })}
            </>
          )}

          {/* 周历：三个月一页（锚点月 + 前两月），‹ › 每次翻 3 个月；点日期跳到所在周并查看当日 24h 曲线 */}
          {granularity === 'week' && (
            <>
              <div className="flex items-center" style={{ marginBottom: 8 }}>
                <span style={{ fontSize: 12, fontWeight: 700, color: 'var(--color-text-2)', flex: 1 }}>
                  周历 · {weekCalYm.slice(0, 4)}年 · 点击日期查看当日 24h 曲线
                </span>
                <button className="btn gh" style={{ fontSize: 11, padding: '3px 10px' }} onClick={() => setWeekCalYm(shiftMonth(weekCalYm, -3))}>
                  ‹
                </button>
                <span className="chip off" style={{ fontFamily: 'Consolas, monospace', fontSize: 11, padding: '3px 10px' }}>
                  {[0, -1, -2].map((k) => shiftMonth(weekCalYm, k)).map((ym) => ym.slice(5)).join(' / ')}
                </span>
                <button className="btn gh" style={{ fontSize: 11, padding: '3px 10px' }} onClick={() => setWeekCalYm(shiftMonth(weekCalYm, 3))}>
                  ›
                </button>
              </div>
              <div style={{ display: 'grid', gridTemplateColumns: 'repeat(3, 1fr)', gap: 10 }}>
                {[0, -1, -2].map((k) => shiftMonth(weekCalYm, k)).map((ym) => {
                  const [cy, cm] = ym.split('-').map(Number);
                  return (
                    <div key={ym} className="card" style={{ padding: 10 }}>
                      <div style={{ fontSize: 12, fontWeight: 700, color: 'var(--color-text-1)', marginBottom: 8 }}>
                        {cy}年{cm}月
                      </div>
                      {mondaysOfMonth(ym).map((mon) => {
                        const days = Array.from({ length: 7 }, (_, i) => shiftDate(mon, i));
                        const weekTok = days.reduce((s, d) => s + (filtered.byDay.get(d)?.tokens ?? 0), 0);
                        const isCur = mon === weekStart;
                        return (
                          <div key={mon} style={{ marginBottom: 8 }}>
                            <div className="flex items-center" style={{ gap: 6, marginBottom: 3 }}>
                              <span
                                style={{
                                  fontSize: 10, fontFamily: 'Consolas, monospace', flex: 1,
                                  color: isCur ? 'var(--color-primary-strong)' : 'var(--color-text-3)',
                                  fontWeight: isCur ? 700 : 400,
                                }}
                              >
                                {mon.slice(5)} ~ {shiftDate(mon, 6).slice(5)}
                              </span>
                              <span style={{ fontSize: 10, fontFamily: 'Consolas, monospace', color: 'var(--color-text-2)' }}>
                                {weekTok > 0 ? fmtTok(weekTok) : '—'}
                              </span>
                            </div>
                            <div style={{ display: 'grid', gridTemplateColumns: 'repeat(7, 1fr)', gap: 3 }}>
                              {days.map((date) => {
                                const b = filtered.byDay.get(date);
                                const inMonth = date.slice(0, 7) === ym;
                                return (
                                  <div
                                    key={date}
                                    className="hover-zoom"
                                    title={detailText(b, scope)}
                                    onClick={() => {
                                      setWeekStart(mon);
                                      setSelectedDate(date);
                                    }}
                                    style={{
                                      height: 30,
                                      borderRadius: 6,
                                      cursor: 'pointer',
                                      display: 'flex',
                                      alignItems: 'center',
                                      justifyContent: 'center',
                                      fontSize: 10.5,
                                      fontWeight: 700,
                                      background: heatColor(b?.tokens ?? 0, maxOfDay),
                                      border:
                                        date === selectedDate
                                          ? '1.5px solid var(--color-primary-strong)'
                                          : isCur
                                            ? '1px solid color-mix(in srgb, var(--color-primary) 55%, transparent)'
                                            : '1px solid var(--color-border)',
                                      opacity: inMonth ? 1 : 0.4,
                                      color: b && b.tokens / maxOfDay > 0.35 ? '#fff' : inMonth ? 'var(--color-text-2)' : 'var(--color-text-3)',
                                    }}
                                  >
                                    {Number(date.slice(8))}
                                  </div>
                                );
                              })}
                            </div>
                          </div>
                        );
                      })}
                    </div>
                  );
                })}
              </div>
            </>
          )}

          {/* 月历：12 个月格 */}
          {granularity === 'month' && (
            <>
              <div className="flex items-center" style={{ marginBottom: 8 }}>
                <span style={{ fontSize: 12, fontWeight: 700, color: 'var(--color-text-2)', flex: 1 }}>
                  月历 · {monY} · 点击月份下钻到日历
                </span>
                <button className="btn gh" style={{ fontSize: 11, padding: '3px 10px' }} onClick={() => setMonY((y) => y - 1)}>
                  ‹
                </button>
                <span className="chip off" style={{ fontFamily: 'Consolas, monospace', fontSize: 11, padding: '3px 10px' }}>
                  {monY}
                </span>
                <button className="btn gh" style={{ fontSize: 11, padding: '3px 10px' }} onClick={() => setMonY((y) => y + 1)}>
                  ›
                </button>
              </div>
              <div className="grid grid-cols-4 gap-2.5">
                {Array.from({ length: 12 }, (_, i) => {
                  const ym = `${monY}-${String(i + 1).padStart(2, '0')}`;
                  const b = filtered.byMonth.get(ym);
                  return (
                    <div
                      key={ym}
                      className="card hover-zoom"
                      style={{
                        padding: 12,
                        cursor: 'pointer',
                        borderColor: b ? heatColor(b.tokens, maxOfMonth) : undefined,
                      }}
                      title={detailText(b, scope)}
                      onClick={() => {
                        setDayYm(ym);
                        setGranularity('day');
                      }}
                    >
                      <div className="flex items-center justify-between">
                        <b style={{ fontSize: 13 }}>{i + 1} 月</b>
                        <span className="chip off">{b?.calls ?? 0} 次</span>
                      </div>
                      <div style={{ fontSize: 14, fontWeight: 800, fontFamily: 'Consolas, monospace', marginTop: 4 }}>
                        {b ? fmtTok(b.tokens) : '—'}
                      </div>
                      <div className="bar" style={{ marginTop: 6 }}>
                        <i style={{ width: `${((b?.tokens ?? 0) / maxOfMonth) * 100}%` }} />
                      </div>
                    </div>
                  );
                })}
              </div>
            </>
          )}

          {/* 日历：当月逐日格 */}
          {granularity === 'day' && (
            <>
              <div className="flex items-center" style={{ marginBottom: 8 }}>
                <span style={{ fontSize: 12, fontWeight: 700, color: 'var(--color-text-2)', flex: 1 }}>
                  日历 · {dayYm} · 点击日期查看当日 24h 曲线（当前：{selectedDate}）
                </span>
                <button className="btn gh" style={{ fontSize: 11, padding: '3px 10px' }} onClick={() => setDayYm(shiftMonth(dayYm, -1))}>
                  ‹
                </button>
                <span className="chip off" style={{ fontFamily: 'Consolas, monospace', fontSize: 11, padding: '3px 10px' }}>
                  {dayYm}
                </span>
                <button className="btn gh" style={{ fontSize: 11, padding: '3px 10px' }} onClick={() => setDayYm(shiftMonth(dayYm, 1))}>
                  ›
                </button>
              </div>
              <div style={{ display: 'grid', gridTemplateColumns: 'repeat(7, 1fr)', gap: 6 }}>
                {['一', '二', '三', '四', '五', '六', '日'].map((d) => (
                  <div key={d} style={{ fontSize: 10.5, color: 'var(--color-text-3)', textAlign: 'center' }}>
                    周{d}
                  </div>
                ))}
                {dayCells.map((cell, i) => {
                  if (!cell.day) return <div key={`lead-${i}`} />;
                  const date = cell.date!;
                  const b = dayCard(date);
                  const isSel = date === selectedDate;
                  return (
                    <div
                      key={date}
                      className="hover-zoom"
                      title={detailText(b, scope)}
                      style={{
                        minHeight: 64,
                        borderRadius: 10,
                        padding: '6px 8px',
                        cursor: 'pointer',
                        background: heatColor(b?.tokens ?? 0, maxOfDay),
                        border: isSel ? '1.5px solid var(--color-primary-strong)' : '1px solid var(--color-border)',
                        color: b && b.tokens / maxOfDay > 0.35 ? '#fff' : 'var(--color-text-2)',
                      }}
                      onClick={() => {
                        setSelectedDate(date);
                        setViewMode('calendar');
                      }}
                    >
                      <div style={{ fontSize: 11, fontWeight: 700 }}>{cell.day}</div>
                      <div style={{ fontSize: 10.5, fontFamily: 'Consolas, monospace', marginTop: 2 }}>
                        {b ? fmtTok(b.tokens) : '—'}
                      </div>
                    </div>
                  );
                })}
              </div>
            </>
          )}
        </div>
      ) : (
        <div className="card" style={{ padding: 8, flex: '1 1 500px', minWidth: 500 }}>
          <div ref={chartRef} style={{ width: '100%', height: 320 }} />
        </div>
      )}

      {/* 当日 24h 曲线（点日格后展示） */}
      {granularity === 'day' && selectedDate && overview?.curve && overview.curve.length > 0 && (
        <div className="card" style={{ padding: 8, flex: '1 1 500px', minWidth: 500, display: 'flex', flexDirection: 'column' }}>
          <DayCurve overview={overview} date={selectedDate} scope={scope} accountLabel={accountLabel} />
        </div>
      )}
      </div>

      {/* 按模型细分：柱状 + 饼图 双图并排 */}
      <div className="grid grid-cols-2 gap-3">
        <div>
          <div style={{ fontSize: 11, color: 'var(--color-text-3)', marginBottom: 6 }}>
            按模型细分（Top 8 · {scopeLabel} · 低于 10 万 tok 不显示）
          </div>
          <ModelBars overview={overview} scope={scope} />
        </div>
        <div>
          <div style={{ fontSize: 11, color: 'var(--color-text-3)', marginBottom: 6 }}>
            按模型占比（饼图 · {scopeLabel}）
          </div>
          <ModelPie overview={overview} scope={scope} />
        </div>
      </div>

      <div style={{ fontSize: 10.5, color: 'var(--color-text-3)', marginTop: 10 }}>
        · 日历格与曲线点悬浮显示 agent-model 消耗明细 · 点击年格→月历→日历逐级下钻 ·
        点日格切当日 24h 曲线 · 数据永久留存 ~/.agenthub
      </div>
    </div>
  );
}

/** 当日 24h 曲线（overview.curve 驱动，按范围筛选；按账号分组或按 Agent 聚合） */
function DayCurve({
  overview,
  date,
  scope,
  accountLabel,
}: {
  overview: UsageOverview;
  date: string;
  scope: Scope;
  accountLabel: (agent: string, account: string) => string;
}) {
  const byAccount = scope.account !== 'all';
  const buckets = new Map<string, number[]>();
  for (const p of overview.curve) {
    if (!matchScope(p.agent, p.account, scope)) continue;
    const key = byAccount ? `${p.agent}|${p.account}` : p.agent;
    const arr = buckets.get(key) ?? new Array(48).fill(0);
    if (p.bucket >= 0 && p.bucket < 48) arr[p.bucket] += p.tokens;
    buckets.set(key, arr);
  }
  const ref = useEChart(() => {
    const c = themeColors();
    const series = [...buckets.entries()].map(([key, arr]) => {
      const [agent, account] = key.split('|');
      return {
        name: byAccount ? accountLabel(agent, account) : (AGENT_LABEL[agent] ?? agent),
        type: 'line' as const,
        smooth: true,
        showSymbol: false,
        data: arr,
        lineStyle: { width: 2, color: agentColor(agent) },
        itemStyle: { color: agentColor(agent) },
        areaStyle: { opacity: 0.12 },
      };
    });
    return {
      backgroundColor: 'transparent',
      title: { text: `当日调用量分布 · ${date}（每 30min）`, left: 12, top: 8, textStyle: { color: c.text2, fontSize: 12, fontWeight: 700 } },
      tooltip: { trigger: 'axis', valueFormatter: (v: number) => `${fmtTok(v)} tok` },
      legend: { top: 8, right: 12, textStyle: { color: c.text2 } },
      grid: { left: 56, right: 20, top: 44, bottom: 28 },
      xAxis: {
        type: 'category',
        data: Array.from({ length: 48 }, (_, i) => `${String(Math.floor(i / 2)).padStart(2, '0')}:${i % 2 ? '30' : '00'}`),
        axisLabel: { color: c.text3, interval: 5 },
        axisLine: { lineStyle: { color: c.border } },
      },
      yAxis: {
        type: 'value',
        axisLabel: { color: c.text3, formatter: (v: number) => fmtTok(v) },
        splitLine: { lineStyle: { color: c.border, type: 'dashed' } },
      },
      series,
    } satisfies echarts.EChartsCoreOption;
  }, [overview, date, scope, accountLabel]);
  return <div ref={ref} style={{ width: '100%', flex: 1, minHeight: 260 }} />;
}

/** 按模型细分（当前上下文 overview.models） */
function ModelBars({ overview, scope }: { overview: UsageOverview | null; scope: Scope }) {
  const models = useMemo(
    () =>
      (overview?.models ?? []).filter(
        (m) => matchScope(m.agent, m.account, scope) && m.tokens >= MODEL_MIN_TOKENS,
      ),
    [overview, scope],
  );
  const ref = useEChart(() => {
    const c = themeColors();
    const colorOf = modelColorFn();
    const sorted = [...models].sort((a, b) => b.tokens - a.tokens).slice(0, 8);
    return {
      backgroundColor: 'transparent',
      tooltip: { trigger: 'axis', axisPointer: { type: 'shadow' }, valueFormatter: (v: number) => `${fmtTok(v)} tok` },
      grid: { left: 8, right: 56, top: 8, bottom: 8, containLabel: true },
      xAxis: {
        type: 'value',
        axisLabel: { color: c.text3, formatter: (v: number) => fmtTok(v) },
        splitLine: { show: false },
      },
      yAxis: {
        type: 'category',
        inverse: true,
        data: sorted.map((m) => m.model || '(unknown)'),
        axisLabel: { color: c.text2, fontFamily: 'Consolas, monospace', fontSize: 11 },
        axisLine: { show: false },
        axisTick: { show: false },
      },
      series: [
        {
          type: 'bar',
          data: sorted.map((m) => ({
            value: m.tokens,
            itemStyle: { color: colorOf(m.agent), borderRadius: [0, 4, 4, 0] },
          })),
          barMaxWidth: 14,
          label: {
            show: true,
            position: 'right',
            color: c.text3,
            fontFamily: 'Consolas, monospace',
            fontSize: 10.5,
            formatter: (p: { value: number }) => fmtTok(p.value),
          },
        },
      ],
    } satisfies echarts.EChartsCoreOption;
  }, [models]);
  return <div ref={ref} style={{ width: '100%', height: Math.max(160, (models.length || 1) * 34 + 40) }} />;
}

/** 按模型占比（环形饼图，与柱状同数据源；hover 显示 token 数） */
function ModelPie({ overview, scope }: { overview: UsageOverview | null; scope: Scope }) {
  const models = useMemo(
    () =>
      (overview?.models ?? []).filter(
        (m) => matchScope(m.agent, m.account, scope) && m.tokens >= MODEL_MIN_TOKENS,
      ),
    [overview, scope],
  );
  const ref = useEChart(() => {
    const c = themeColors();
    const colorOf = modelColorFn();
    const sorted = [...models].sort((a, b) => b.tokens - a.tokens).slice(0, 8);
    const data = sorted.map((m) => ({
      name: `${AGENT_LABEL[m.agent] ?? m.agent}·${m.model || '(unknown)'}`,
      value: m.tokens,
      itemStyle: { color: colorOf(m.agent) },
    }));
    return {
      backgroundColor: 'transparent',
      tooltip: {
        trigger: 'item',
        formatter: (p: { name: string; value: number; percent: number }) =>
          `${p.name}<br/>${fmtTok(p.value)} tok（${p.percent}%）`,
      },
      legend: { type: 'scroll', bottom: 0, left: 'center', textStyle: { color: c.text3, fontSize: 10 } },
      series: [
        {
          type: 'pie',
          radius: ['38%', '68%'],
          center: ['50%', '44%'],
          data,
          itemStyle: { borderRadius: 4, borderColor: 'transparent', borderWidth: 2 },
          label: { show: false },
          emphasis: {
            label: { show: true, formatter: '{b}\n{d}%', color: c.text2, fontSize: 11 },
          },
        },
      ],
    } satisfies echarts.EChartsCoreOption;
  }, [models]);
  return <div ref={ref} style={{ width: '100%', height: Math.max(160, (models.length || 1) * 34 + 40) }} />;
}
