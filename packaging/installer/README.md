# The combined installer

One installer per OS for the player (mpv-omniphony), the engine and Studio,
composed from the files a release already publishes. Design, layout and
decisions: [`docs/installer.md`](../../docs/installer.md). User-facing steps:
[`docs/install/`](../../docs/install/).

| | |
| --- | --- |
| `linux/build.sh` | composes `omniphony-<tag>-linux-x86_64.tar.gz` from a Studio archive and a player AppImage |
| `linux/install.sh`, `linux/uninstall.sh` | what the bundle runs on the user's machine, per user, no root |
| `linux/applications/` | the menu entries |
| `linux/test.sh` | the three scripts end to end on stand-in files, in a temporary HOME (CI, `lint`) |
| `linux/verify-release.sh` | installs a composed bundle of real files in a container (`installer.yml`) |
| `windows/omniphony.nsi` | the NSIS installer: per user by default, per machine and the renderer service from the command line |
| `windows/build.sh` | composes `omniphony-<tag>-windows-x86_64-setup.exe` from a Studio zip and a player zip (makensis, on Linux or in Git Bash) |
| `windows/make-standins.sh`, `windows/test.ps1`, `windows/standin/` | two installers of stand-in files, and their install, upgrade and uninstall on a real Windows (`installer.yml`) |
| `windows/verify-release.ps1` | installs a composed installer of real files silently and checks the player loads the engine (`installer.yml`) |

`.github/workflows/installer.yml` runs it for a published release, by hand,
and on pull requests that touch this directory.
