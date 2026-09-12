import { copyFileSync, chmodSync, existsSync, mkdirSync, readFileSync } from 'fs';
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

// --locked, like CI: a release artefact must be built from the versions the
// lockfile names, not from whatever the registry offers on the day.
execFileSync('cargo', ['build', '--release', '--locked'], {
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

// ── engine library (liborender) ────────────────────────────────────────────
// Bundled as a resource and deployed at startup (src-tauri/src/engine_deploy.rs)
// to the per-user path the forked mpv's runtime loader searches — so shipping
// Studio ships the engine every consumer uses, built from this same commit as
// the CLI sidecar above. The Linux artifact is named after its soname
// (liborender.so.<ABI major>, read from the generated header) to match what
// the loader probes.
execFileSync('cargo', ['build', '--release', '--locked', '-p', 'orender_ffi'], {
  cwd: rendererDir,
  stdio: 'inherit'
});

const engineDir = join(binariesDir, 'engine');
mkdirSync(engineDir, { recursive: true });

let libSrc, libName;
if (process.platform === 'win32') {
  libSrc = join(rendererDir, 'target', 'release', 'orender.dll');
  libName = 'orender.dll';
} else if (process.platform === 'darwin') {
  libSrc = join(rendererDir, 'target', 'release', 'liborender.dylib');
  libName = 'liborender.dylib';
} else {
  const header = readFileSync(
    join(rendererDir, 'orender_ffi', 'include', 'orender.h'), 'utf8');
  const major = /^#define ORENDER_ABI_MAJOR (\d+)$/m.exec(header)?.[1];
  if (!major) {
    throw new Error('ORENDER_ABI_MAJOR not found in orender.h');
  }
  libSrc = join(rendererDir, 'target', 'release', 'liborender.so');
  libName = `liborender.so.${major}`;
}
if (!existsSync(libSrc)) {
  throw new Error(`Engine library not found after build: ${libSrc}`);
}
copyFileSync(libSrc, join(engineDir, libName));
if (process.platform !== 'win32') {
  chmodSync(join(engineDir, libName), 0o755);
}

console.log(`Prepared engine library: ${join(engineDir, libName)}`);
