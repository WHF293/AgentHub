import { create } from 'zustand';

export type PageId =
  | 'dashboard'
  | 'accounts'
  | 'skills'
  | 'mcp'
  | 'models'
  | 'memory'
  | 'plugins'
  | 'settings'
  | 'architecture'
  | 'help';

export type DefaultPage = 'dashboard' | 'accounts';

export interface NavItem {
  id: PageId;
  label: string;
  /** 中文标签（设置页「侧边栏语言」切到中文时显示） */
  labelZh: string;
  icon: string;
}

/** 侧栏 8 岛（总览已合并用量页；Architecture / 使用说明走顶栏 icon，不占侧栏） */
export const NAV_ITEMS: NavItem[] = [
  { id: 'dashboard', label: 'Usage', labelZh: '用量管理', icon: '▦' },
  { id: 'accounts', label: 'Account', labelZh: '账户管理', icon: '⇄' },
  { id: 'skills', label: 'Skill', labelZh: '技能管理', icon: '▤' },
  { id: 'mcp', label: 'MCP', labelZh: '连接器管理', icon: '⌗' },
  { id: 'models', label: 'Model', labelZh: '模型管理', icon: '✦' },
  { id: 'memory', label: 'Memory', labelZh: '记忆管理', icon: '❑' },
  { id: 'plugins', label: 'Plugin', labelZh: '插件管理', icon: '⧉' },
  { id: 'settings', label: 'Setting', labelZh: '设置', icon: '⚙' },
];

export type UiLang = 'en' | 'zh';
const LANG_KEY = 'agenthub.uiLang';

export function getUiLang(): UiLang {
  return localStorage.getItem(LANG_KEY) === 'zh' ? 'zh' : 'en';
}

const FIRST_RUN_KEY = 'agenthub.firstRunDone';
const DEFAULT_PAGE_KEY = 'agenthub.defaultPage';

/** 读取用户设置的默认打开页（usage / account），未设置 = usage */
export function getDefaultPage(): DefaultPage {
  return localStorage.getItem(DEFAULT_PAGE_KEY) === 'accounts' ? 'accounts' : 'dashboard';
}

/** 设置页「默认打开页面」配置项（下次启动生效） */
export function setDefaultPage(p: DefaultPage): void {
  localStorage.setItem(DEFAULT_PAGE_KEY, p);
}

/**
 * 启动默认页：
 * - 首次安装（本地无使用记录）→ 使用说明页，方便新用户快速上手
 * - 之后按设置页「默认打开页面」打开（默认 Usage）
 */
export function initialPage(): PageId {
  if (localStorage.getItem(FIRST_RUN_KEY) !== '1') {
    localStorage.setItem(FIRST_RUN_KEY, '1');
    return 'help';
  }
  return getDefaultPage();
}

interface NavState {
  active: PageId;
  setActive: (page: PageId) => void;
  /** 侧边栏语言（en=英文默认 / zh=中文），localStorage 持久化，设置页切换即时生效 */
  lang: UiLang;
  setLang: (lang: UiLang) => void;
}

export const useNavStore = create<NavState>((set) => ({
  active: initialPage(),
  setActive: (active) => set({ active }),
  lang: getUiLang(),
  setLang: (lang) => {
    localStorage.setItem(LANG_KEY, lang);
    set({ lang });
  },
}));
