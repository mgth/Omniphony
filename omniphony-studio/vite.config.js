const { defineConfig } = require('vite');

module.exports = defineConfig({
  root: 'src',
  build: { outDir: '../dist', emptyOutDir: true },
  base: './'
});
