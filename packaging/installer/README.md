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

`.github/workflows/installer.yml` runs it for a published release, by hand,
and on pull requests that touch this directory.
