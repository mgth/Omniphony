import { defineConfig } from 'vite';

export default defineConfig({
  root: 'src',
  build: {
    outDir: '../dist',
    emptyOutDir: true,
    chunkSizeWarningLimit: 600,
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (id.includes('three/examples/jsm')) {
            return 'three-extras';
          }
          if (id.includes('/node_modules/three/')) {
            return 'three-core';
          }
          if (id.includes('/node_modules/@tauri-apps/')) {
            return 'tauri';
          }
        }
      }
    }
  },
  base: './'
});
