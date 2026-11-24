import React from 'react';
import ReactDOM from 'react-dom/client';
import 'uno.css';
import './styles/theme.css';
import App from './App';
import { applyAgentColors } from './agents';

// 启动即注入用户自定义的 agent 身份色（Plugin 岛配置，localStorage 持久化）
applyAgentColors();

// 兜底显示窗口：窗口 visible:false 启动（Rust 侧页面 load 完成后 show），
// 万一 load 事件未触发（异常场景）3s 后强制显示，避免窗口永远不可见
setTimeout(() => {
  import('@tauri-apps/api/window')
    .then(({ getCurrentWindow }) => getCurrentWindow().show().catch(() => {}))
    .catch(() => {});
}, 3000);

// 页面禁止缩放：拦截 Ctrl+滚轮 / 触控板捏合 / Ctrl +/-/0 快捷键
// （Tauri 侧另在 tauri.conf.json 配置 zoomHotkeysEnabled:false 双保险）
window.addEventListener(
  'wheel',
  (e) => {
    if (e.ctrlKey) e.preventDefault();
  },
  { passive: false },
);
window.addEventListener('gesturestart', (e) => e.preventDefault());
window.addEventListener('keydown', (e) => {
  if ((e.ctrlKey || e.metaKey) && ['=', '-', '0', '+'].includes(e.key)) e.preventDefault();
});

// 屏蔽 WebView 原生右键菜单（返回/刷新/另存为/打印…）；开发模式保留便于调试
if (!import.meta.env.DEV) {
  window.addEventListener('contextmenu', (e) => e.preventDefault());
}

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
