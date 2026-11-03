import { invoke } from '@tauri-apps/api/core';

// ─── 与 Rust serde 结构一一对应（字段名保持 snake_case） ───

export interface AgentCaps {
  skills: boolean;
  mcp: boolean;
}

/** 单 agent 探测状态（app_status.agents 元素；含停用插件） */
export interface AgentStatus {
  agent: string;
  display_name: string;
  desc: string;
  /** 插件启用态（Plugin 岛配置） */
  plugin_enabled: boolean;
  installed: boolean;
  caps: AgentCaps;
  skills_dir: string;
  skills_dir_is_link: boolean;
  skills_dir_target: string | null;
  skills_count: number;
  /** 登录态文件探测（无此概念的端恒 false） */
  auth_found: boolean;
}

export interface AppStatus {
  hub_initialized: boolean;
  central_skills_count: number;
  link_mode: 'symlink' | 'junction' | 'copy';
  /** 全部已注册适配器（前端按 plugin_enabled 过滤展示） */
  agents: AgentStatus[];
}

export interface ImportedSkill {
  name: string;
  from: string;
  took: string;
}

export interface AgentMigrateReport {
  agent: string;
  display_name: string;
  /** 整端动作（逐项链 n/m 或整链动作描述） */
  action: string;
  relinked: string[];
  kept: string[];
  failed: string[];
}

export interface MigrateReport {
  central_dir: string;
  link_mode: string;
  imported: ImportedSkill[];
  skipped_existing: number;
  import_warnings: string[];
  /** 各启用插件端的结果（注册表顺序） */
  agents: AgentMigrateReport[];
  warnings: string[];
}

export type LinkStatus = 'linked' | 'diverged' | 'broken' | 'missing';

export interface SkillAgentState {
  agent: string;
  display_name: string;
  status: LinkStatus;
  /** 该端处于整目录链模式（单 skill 状态恒为 linked） */
  whole_link: boolean;
}

export interface SkillOverviewItem {
  name: string;
  description: string | null;
  source: string;
  source_ref: string | null;
  enabled: boolean;
  /** 各启用插件端的状态（注册表顺序） */
  agents: SkillAgentState[];
  /** WB 端「允许模型自动调用」实测（null = WB 存储未收录；WB 专属特性） */
  wb_auto: boolean | null;
}

export interface OpLogRow {
  id: number;
  action: string;
  target: string | null;
  detail_json: string | null;
  created_at: string | null;
}

// ─── invoke 封装 ───

export const getAppStatus = () => invoke<AppStatus>('app_status');

export const runSkillsMigration = (mode?: string) =>
  invoke<MigrateReport>('run_skills_migration', { mode });

export const skillsOverview = () => invoke<SkillOverviewItem[]>('skills_overview');

export const relinkSkill = (name: string, agent: string) =>
  invoke<string>('relink_skill', { name, agent });

/** 启停：DB 状态 + 写穿 WB「允许模型自动调用」，返回同步结果消息 */
export const setSkillEnabled = (name: string, enabled: boolean) =>
  invoke<string>('set_skill_enabled', { name, enabled });

export const skillDelete = (name: string) => invoke<void>('skill_delete', { name });

export interface PluginSkillItem {
  /** 运行时技能名（plugin:目录名，如 github:commit） */
  name: string;
  plugin: string;
  plugin_version: string;
  description: string | null;
  plugin_enabled: boolean;
}

export const zcodePluginSkills = () => invoke<PluginSkillItem[]>('zcode_plugin_skills');

export const recentOpLog = (limit = 20) => invoke<OpLogRow[]>('recent_op_log', { limit });

// ─── Plugin 岛（可插拔适配器） ───

export interface PluginInfo {
  agent: string;
  display_name: string;
  desc: string;
  /** 用户配置的启用/停用 */
  enabled: boolean;
  /** 本机安装探测（仅提示） */
  installed: boolean;
  caps: AgentCaps;
  /** 状态来源：auto = 首次运行按安装探测；manual = 用户手动设置 */
  source: string;
}

export const pluginList = () =>
  invoke<{ items: PluginInfo[] }>('plugin_list');

export const pluginToggle = (agent: string, enabled: boolean) =>
  invoke<void>('plugin_toggle', { agent, enabled });

// ─── M3 MCP ───

export interface McpAgentState {
  agent: string;
  display_name: string;
  enabled: boolean;
  status: 'ok' | 'disabled' | 'missing' | 'drifted' | 'unsupported';
}

export interface McpOverviewItem {
  name: string;
  type: string;
  command: string | null;
  url: string | null;
  /** 各启用插件端的实测（注册表顺序） */
  agents: McpAgentState[];
}

export interface McpAgentDrift {
  agent: string;
  display_name: string;
  only: string[];
}

export interface McpDrift {
  agents: McpAgentDrift[];
}

export interface McpOverviewPayload {
  items: McpOverviewItem[];
  drift: McpDrift;
}

export interface McpServerInput {
  name: string;
  type: string;
  command: string | null;
  args: string[];
  url: string | null;
  env: Record<string, string>;
  headers?: Record<string, string>;
  /** 按 agent 的启用态（agent id → enabled） */
  enabled: Record<string, boolean>;
}

/** 单 server 连接测试结果（真实 initialize 握手） */
export interface McpTestResult {
  ok: boolean;
  message: string;
  latency_ms: number;
}

export const mcpOverview = () => invoke<McpOverviewPayload>('mcp_overview');
export const mcpTestConnection = (name: string) =>
  invoke<McpTestResult>('mcp_test_connection', { name });
export const mcpAdopt = (overwrite: boolean) =>
  invoke<number>('mcp_adopt', { overwrite });
export const mcpSave = (server: McpServerInput) =>
  invoke<void>('mcp_save', { server });
export const mcpToggle = (name: string, agent: string, enabled: boolean) =>
  invoke<void>('mcp_toggle', { name, agent, enabled });
export const mcpDelete = (name: string) => invoke<void>('mcp_delete', { name });
export const mcpValidate = () => invoke<McpOverviewPayload>('mcp_validate');

// ─── M4 模型 ───

export interface ZcodeProviderLive {
  id: string;
  name: string;
  kind: 'anthropic' | 'openai-compatible' | string;
  base_url: string;
  has_api_key: boolean;
  models: string[];
  builtin: boolean;
  /** 已写穿到 WB models.json 的本 provider 模型（DB 托管清单 ∩ 文件现存 id） */
  wb_models: string[];
}

export interface ProvidersOverview {
  providers: ZcodeProviderLive[];
  selected_keys: Record<string, string>;
}

export interface FetchTrace {
  url: string;
  status: number | null;
  ms: number;
  ok: boolean;
  note: string;
}

export interface FetchResult {
  models: string[];
  traces: FetchTrace[];
  /** 全部候选失败时来自内置推荐列表（部分网关不提供 /v1/models） */
  suggested: boolean;
}

export const providersOverview = () => invoke<ProvidersOverview>('providers_overview');

export const providerFetchModels = (input: {
  id: string;
  kind: string;
  base_url: string;
  api_key?: string;
  models_url?: string;
}) => invoke<FetchResult>('provider_fetch_models', { input });

export const providerSave = (input: {
  id: string;
  name: string;
  kind: string;
  base_url: string;
  api_key?: string;
  models?: string[];
}) => invoke<string>('provider_save', { input });

export const providerDelete = (id: string) => invoke<string>('provider_delete', { id });

export const providerSetDefault = (family: string, key: string) =>
  invoke<void>('provider_set_default', { family, key });

// ─── Claude Code 模型托管（settings.json env 块，单档直写） ───

export interface ClaudeProviderLive {
  base_url: string;
  has_api_key: boolean;
  model: string;
  small_fast_model: string;
  /** ~/.claude.json 是否已跳过首次引导 */
  onboarding_done: boolean;
  /** settings.json 是否已有 env 块 */
  configured: boolean;
}

export const claudeProvidersOverview = () =>
  invoke<ClaudeProviderLive>('claude_providers_overview');

export const claudeProviderFetchModels = (input: {
  base_url: string;
  api_key?: string;
  models_url?: string;
}) => invoke<FetchResult>('claude_provider_fetch_models', { input });

export const claudeProviderSave = (input: {
  base_url: string;
  api_key?: string;
  model?: string;
  small_fast_model?: string;
}) => invoke<string>('claude_provider_save', { input });

// ─── M5 WB 账号 ───

export interface AccountMeta {
  uid: string;
  uin: string;
  nickname: string;
}

export interface WbSlot {
  id: string;
  label: string;
  uid: string;
  uin: string;
  saved_at: string;
  file_ok: boolean;
}

export interface WbOverview {
  auth_found: boolean;
  current: AccountMeta;
  process_running: boolean;
  wb_installed: boolean;
  slots: WbSlot[];
}

export interface SwitchReport {
  ok: boolean;
  steps: string[];
  error: string | null;
  /** 勾选「复制会话」时的逐会话结果（复制失败不阻塞切号） */
  sessions?: SessionCopyResult[];
}

/** WB 会话（当前账号，切号复制选择用） */
export interface WbSessionInfo {
  id: string;
  title: string;
  custom_title: string | null;
  cwd: string;
  project: string;
  status: string;
  model: string | null;
  updated_at: number;
  jsonl_ok: boolean;
  claw: boolean;
  linked: boolean;
}

export interface SessionCopyResult {
  source_id: string;
  new_id: string;
  title: string;
  /** copied / skipped / failed */
  status: string;
  detail: string;
}

export const wbOverview = () => invoke<WbOverview>('wb_overview');
export const wbSaveSlot = (label: string) => invoke<WbSlot>('wb_save_slot', { label });
export const wbSwitch = (slotId: string, confirmKill: boolean, copySessionIds: string[] = []) =>
  invoke<SwitchReport>('wb_switch', {
    input: { slot_id: slotId, confirm_kill: confirmKill, copy_session_ids: copySessionIds },
  });
export const wbSessionList = (uid: string) => invoke<WbSessionInfo[]>('wb_session_list', { uid });

/** 独立复制：一次把会话复制给多个 WB 账号 */
export interface TargetCopyGroup {
  target_uid: string;
  results: SessionCopyResult[];
}
export interface SessionCopyReport {
  ok: boolean;
  /** WB 运行中且未勾选强杀 → 前端显示勾选项后重试 */
  need_kill: boolean;
  killed: boolean;
  restarted: boolean;
  targets: TargetCopyGroup[];
  error: string | null;
}
export const wbSessionCopy = (sessionIds: string[], targetUids: string[], confirmKill: boolean) =>
  invoke<SessionCopyReport>('wb_session_copy', {
    input: { session_ids: sessionIds, target_uids: targetUids, confirm_kill: confirmKill },
  });
export const wbImportLegacy = () => invoke<number>('wb_import_legacy');
export const wbDeleteSlot = (slotId: string) => invoke<void>('wb_delete_slot', { slot: slotId });
export const wbRenameSlot = (slotId: string, label: string) =>
  invoke<void>('wb_rename_slot', { input: { slot_id: slotId, label } });

// ─── ZCode 账号槽位 + 额度 ───

export interface ZcodeSlot {
  id: string;
  label: string;
  saved_at: string;
  file_ok: boolean;
  /** 槽位快照与当前登录态一致（该账号当前生效） */
  active: boolean;
}

export interface ZcodeAccountOverview {
  installed: boolean;
  cred_found: boolean;
  /** ZCode.exe 是否正在运行 */
  running: boolean;
  slots: ZcodeSlot[];
}

export interface ZcodeSwitchReport {
  ok: boolean;
  steps: string[];
  error: string | null;
}

export const zcodeOverview = () => invoke<ZcodeAccountOverview>('zcode_overview');
export const zcodeSaveSlot = (label: string) => invoke<ZcodeSlot>('zcode_save_slot', { label });
export const zcodeSwitch = (slotId: string) =>
  invoke<ZcodeSwitchReport>('zcode_switch', { input: { slot_id: slotId } });
export const zcodeDeleteSlot = (slotId: string) =>
  invoke<void>('zcode_delete_slot', { slot: slotId });
export const zcodeRenameSlot = (slotId: string, label: string) =>
  invoke<void>('zcode_rename_slot', { input: { slot_id: slotId, label } });
export const zcodeLaunch = () => invoke<void>('zcode_launch');

export interface QuotaWindow {
  used_pct: number;
  used: number;
  total: number;
  remaining: number;
  reset_at: string;
}

export interface ZcodeQuota {
  plan_level: string;
  h5: QuotaWindow;
  week: QuotaWindow;
}

/** 官方接口实时额度（读 ZCode 配置中的 Coding Plan Key，不展示不落库） */
export const zcodeQuota = () => invoke<ZcodeQuota>('zcode_quota');

export interface WbCreditSummary {
  month_used: number;
  monthly_total: number;
  remaining: number;
}

export const wbCreditSummary = () => invoke<WbCreditSummary>('wb_credit_summary');
export const wbCreditSetTotal = (total: number) =>
  invoke<void>('wb_credit_set_total', { input: { total } });

/** 按账户（uid）的 WB 积分用量（槽位卡片展示用） */
export interface WbAccountCredit {
  uid: string;
  today_used: number;
  month_used: number;
  remaining: number;
  /** 用户为本账户单独设置的当前积分快照（未设置 = null） */
  balance: number | null;
  /** 余额设置时间 */
  balance_set_at: string;
  /** 设置后的已用（剩余 = balance − used_since_set） */
  used_since_set: number;
}
export const wbCreditByAccount = () => invoke<WbAccountCredit[]>('wb_credit_by_account');

/** 每账户当前积分设置（0 或负数 = 清除） */
export const wbCreditSetBalance = (uid: string, balance: number) =>
  invoke<void>('wb_credit_set_balance', { input: { uid, balance } });

// ─── WB 每日签到领积分 ───

export interface WbCheckinStatus {
  uid: string;
  /** 今日是否已领取（null = 查询失败/未知） */
  today_claimed: boolean | null;
  /** 本地记录的最近领取日期与积分 */
  last_date: string;
  last_credit: number;
  /** 凭据来源（当前登录态 / 槽位 xxx） */
  source: string;
  error: string | null;
}

export interface WbCheckinResult {
  uid: string;
  ok: boolean;
  /** claimed=本次领取 / already=今日已领 / failed=失败 */
  status: 'claimed' | 'already' | 'failed' | string;
  credit: number;
  msg: string;
}

export const wbCheckinStatus = () => invoke<WbCheckinStatus[]>('wb_checkin_status');
export const wbCheckinClaim = (uid: string) =>
  invoke<WbCheckinResult>('wb_checkin_claim', { input: { uid } });
export const wbCheckinAutoGet = () => invoke<boolean>('wb_checkin_auto_get');
export const wbCheckinAutoSet = (enabled: boolean) =>
  invoke<void>('wb_checkin_auto_set', { input: { enabled } });

// ─── M9 用量 ───

export interface AgentTotal {
  agent: string;
  /** 账号 uid（'' = ZCode 单账号/未归属） */
  account: string;
  calls: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  /** 官方逐请求积分（WB rawUsage.credit 与账单同源；ZCode 恒为 0） */
  credit: number;
}

export interface ModelBreakdown {
  agent: string;
  account: string;
  model: string;
  calls: number;
  tokens: number;
}

export interface CurvePoint {
  bucket: number;
  agent: string;
  account: string;
  tokens: number;
}

export interface HeatCell {
  date: string;
  agent: string;
  account: string;
  tokens: number;
  calls: number;
}

export interface UsageOverview {
  range: string;
  totals: AgentTotal[];
  models: ModelBreakdown[];
  curve: CurvePoint[];
  heatmap: HeatCell[];
  /** 日历视图窗口（YYYY-MM-DD 或 YYYY-MM） */
  cal_start: string;
  cal_end: string;
  today: string;
}

export interface MatrixCell {
  date: string;
  agent: string;
  /** 账号 uid（'' = ZCode 单账号/未归属） */
  account: string;
  model: string;
  tokens: number;
  calls: number;
}

export interface UsageAccount {
  agent: string;
  /** 账号 uid（'' = ZCode 单账号/未归属） */
  account: string;
  calls: number;
  tokens: number;
}

export const usageCollect = () =>
  invoke<[number, number, number]>('usage_collect');

export const usageOverview = (range: string) =>
  invoke<UsageOverview>('usage_overview', { range });

export const usageMatrix = () => invoke<MatrixCell[]>('usage_matrix');

export const usageAccounts = () => invoke<UsageAccount[]>('usage_accounts');

// ─── 在线更新 ───

export interface UpdateCheckInfo {
  current: string;
  latest: string;
  has_update: boolean;
  tag: string;
  asset_name: string;
  asset_url: string;
  release_url: string;
}

export const updateCheck = () => invoke<UpdateCheckInfo>('update_check');

export const updateInstall = (info: { tag: string; asset_name: string; asset_url: string }) =>
  invoke<void>('update_install', { input: info });

/** 更新代理（设置岛）：检测与下载共用，空 = 直连 */
export const updateProxyGet = () => invoke<string>('update_proxy_get');
export const updateProxySet = (url: string) => invoke<void>('update_proxy_set', { url });

// ─── M7 Skill 市场 ───

export interface MarketSkill {
  name: string;
  description: string;
  source: string;
  source_name: string;
  installed: boolean;
}

export interface MarketCatalog {
  skills: MarketSkill[];
  warnings: string[];
}

export const marketRefresh = () => invoke<MarketCatalog>('market_refresh');

export const marketInstall = (name: string) =>
  invoke<MarketCatalog>('market_install', { name });

// ─── M6 Memory ───

export interface MemoryFile {
  id: string;
  label: string;
  path: string;
  agent: string;
  readonly_default: boolean;
  size: number;
  updated: string;
}

export interface BridgeStatus {
  file: string;
  injected: boolean;
  drifted: boolean;
}

export interface ProjectInfo {
  path: string;
  name: string;
  ai_ready: boolean;
  bridges: BridgeStatus[];
  consolidate_candidates: number;
}

export const memoryGlobalList = () => invoke<MemoryFile[]>('memory_global_list');
export const memoryRead = (path: string) => invoke<string>('memory_read', { path });
export const memoryWrite = (path: string, content: string, allowZcodeAgents: boolean) =>
  invoke<void>('memory_write', { input: { path, content, allow_zcode_agents: allowZcodeAgents } });
export const memoryGlobalUnify = () => invoke<string[]>('memory_global_unify');
export const projectsOverview = () => invoke<ProjectInfo[]>('projects_overview');
export const projectInit = (path: string) => invoke<string[]>('project_init', { path });
export const projectSync = (path: string) => invoke<string[]>('project_sync', { path });
export const projectConsolidate = (path: string) =>
  invoke<string[]>('project_consolidate', { path });
