import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import UnoCSS from 'unocss/vite';

// Tauri 前端构建配置：固定端口、关闭清屏以保留 Rust 报错输出
export default defineConfig({
  plugins: [UnoCSS(), react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  build: {
    target: 'chrome110',
    minify: 'esbuild',
    sourcemap: false,
    rollupOptions: {
      output: {
        // echarts 拆独立 chunk（Usage 页懒加载时才拉取），首屏 bundle 减半、冷启动更快
        manualChunks: {
          echarts: ['echarts'],
        },
      },
    },
  },
});
