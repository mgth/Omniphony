import { copyFileSync, chmodSync, existsSync, mkdirSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';
import { execFileSync } from 'child_process';

const scriptDir = dirname(fileURLToPath(import.meta.url));
const studioDir = dirname(scriptDir);
const repoRoot = dirname(studioDir);
const rendererDir = join(repoRoot, 'omniphony-renderer');
const binariesDir = join(studioDir, 'src-tauri', 'binaries');
const defaultSafRoot = join(repoRoot, '..', 'SPARTA', 'SDKs', 'Spatial_Audio_Framework');

const rustInfo = execFileSync('rustc', ['-vV'], { encoding: 'utf8' });
const targetTriple = /^host:\s+(.+)$/m.exec(rustInfo)?.[1]?.trim();
if (!targetTriple) {
  throw new Error('Failed to determine Rust host target triple');
}

const ext = targetTriple.includes('windows') ? '.exe' : '';
const sidecarName = `orender-${targetTriple}${ext}`;
const sidecarPath = join(binariesDir, sidecarName);
const sourcePath = join(rendererDir, 'target', 'release', `orender${ext}`);

execFileSync('cargo', ['build', '--release'], {
  cwd: rendererDir,
  stdio: 'inherit',
  env: {
    ...process.env,
    SAF_ROOT: process.env.SAF_ROOT || defaultSafRoot
  }
});

if (!existsSync(sourcePath)) {
  throw new Error(`Renderer binary not found after build: ${sourcePath}`);
}

mkdirSync(binariesDir, { recursive: true });
copyFileSync(sourcePath, sidecarPath);
if (process.platform !== 'win32') {
  chmodSync(sidecarPath, 0o755);
}

console.log(`Prepared sidecar: ${sidecarPath}`);
