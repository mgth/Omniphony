import { fileURLToPath } from 'url';
import { defineConfig, searchForWorkspaceRoot } from 'vite';

// The catalogues and the head model belong to the native Studio
// (omniphony-studio-egui/); this deprecated host imports them from there. The
// dev server refuses files outside the workspace unless listed.
const nativeStudio = fileURLToPath(new URL('../omniphony-studio-egui/', import.meta.url));

export default defineConfig({
  root: 'src',
  server: {
    fs: {
      allow: [
        searchForWorkspaceRoot(process.cwd()),
        `${nativeStudio}i18n`,
        `${nativeStudio}assets`
      ]
    }
  },
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
