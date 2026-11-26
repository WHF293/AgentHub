import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { NAV_ITEMS, useNavStore } from './stores/nav';
import { useToastStore } from './stores/ui';
import { TitleBar } from './common/components/TitleBar';
import { LogoMark } from './common/components/LogoMark';
import { DashboardPage } from './pages/Dashboard';
import { SkillsPage } from './pages/Skills';
import { McpPage } from './pages/Mcp';
import { ModelsPage } from './pages/Models';
import { AccountsPage } from './pages/Accounts';
import { MemoryPage } from './pages/Memory';
import { PluginsPage } from './pages/Plugins';
import { SettingsPage } from './pages/Settings';
import { ArchitecturePage } from './pages/Architecture';
import { HelpPage } from './pages/Help';

function ToastHost() {
  const toasts = useToastStore((s) => s.toasts);
  return (
    <div className="toast-host">
      {toasts.map((t) => (
        <div key={t.id} className={`toast-item${t.kind === 'err' ? ' err' : ''}`}>
          {t.text}
        </div>
      ))}
    </div>
  );
}

function CurrentPage() {
  const active = useNavStore((s) => s.active);
  switch (active) {
    case 'dashboard':
      return <DashboardPage />;
    case 'skills':
      return <SkillsPage />;
    case 'mcp':
      return <McpPage />;
    case 'models':
      return <ModelsPage />;
    case 'accounts':
      return <AccountsPage />;
    case 'memory':
      return <MemoryPage />;
    case 'plugins':
      return <PluginsPage />;
    case 'settings':
      return <SettingsPage />;
    case 'architecture':
      return <ArchitecturePage />;
    case 'help':
      return <HelpPage />;
  }
}

export default function App() {
  const { active, setActive, lang } = useNavStore();
  // 侧栏收起态（持久化）：收起只显示 icon，hover 浮出文案
  const [collapsed, setCollapsed] = useState(
    () => localStorage.getItem('agenthub.sidebarCollapsed') === '1',
  );
  const toggleSidebar = () =>
    setCollapsed((c) => {
      localStorage.setItem('agenthub.sidebarCollapsed', c ? '0' : '1');
      return !c;
    });

  // 启动时恢复「关闭进托盘」偏好（Rust 侧默认开启）
  useEffect(() => {
    if (localStorage.getItem('agenthub.closeToTray') === '0') {
      void invoke('set_close_to_tray', { enabled: false }).catch(() => {});
    }
  }, []);

  return (
    <div className="relative flex h-full flex-col">
      <TitleBar />
      <ToastHost />

      <div className="relative z-1 flex min-h-0 flex-1">
        <aside className={`sidebar${collapsed ? ' collapsed' : ''}`}>
          {/* 品牌区点击进入使用说明页（首装默认打开；随时可从这回到快速上手） */}
          <div
            className="logo"
            style={{ color: 'var(--color-primary)', cursor: 'pointer' }}
            title="AgentHub · 使用说明"
            onClick={() => setActive('help')}
          >
            <LogoMark size={20} />
            <span className="logo-text" style={{ color: 'var(--color-text-1)' }}>
              AgentHub
            </span>
          </div>
          <nav>
            {NAV_ITEMS.map((item) => {
              const label = lang === 'zh' ? item.labelZh : item.label;
              return (
                <button
                  key={item.id}
                  type="button"
                  className={`nv${active === item.id ? ' on' : ''}`}
                  data-label={label}
                  title={label}
                  onClick={() => setActive(item.id)}
                >
                  <span className="ico">{item.icon}</span>
                  <span className="nv-label">{label}</span>
                </button>
              );
            })}
          </nav>
          <div className="flex-1" />
          <button
            type="button"
            className="side-toggle"
            title={collapsed ? '展开侧栏' : '收起侧栏'}
            onClick={toggleSidebar}
          >
            {collapsed ? '»' : '«'}
          </button>
        </aside>

        <main className="min-w-0 flex-1 overflow-y-auto px-5 py-4">
          <CurrentPage />
        </main>
      </div>
    </div>
  );
}
