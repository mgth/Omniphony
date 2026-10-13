#!/usr/bin/env bash
# Install a composed bundle (build.sh) of real release files in a container,
# as a plain user, and check what the install page checks: the engine in the
# per-user folder, the player loading it from there, orender and Studio
# resolving their libraries, the service unit, and an uninstall that keeps the
# config. Run by installer.yml on each image.
#
#   verify-release.sh BUNDLE.tar.gz [IMAGE]     (IMAGE defaults to ubuntu:24.04)
set -euo pipefail

bundle="$(realpath "${1:?usage: verify-release.sh BUNDLE.tar.gz [IMAGE]}")"
image="${2:-ubuntu:24.04}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
chmod 755 "$work"
cp "$bundle" "$work/bundle.tar.gz"

# One second of 5.1 tone: the player has no libavdevice to synthesise one.
python3 - "$work/tone51.wav" <<'PY'
import math, struct, sys, wave
with wave.open(sys.argv[1], "wb") as w:
    w.setnchannels(6); w.setsampwidth(2); w.setframerate(48000)
    w.writeframes(b"".join(
        struct.pack("<h", int(8000 * math.sin(2 * math.pi * 440 * i / 48000))) * 6
        for i in range(48000)))
PY

# What a desktop always has and the prebuilts leave to the host: glibc, the
# display and audio client libraries, PipeWire. Plus systemd for the unit check.
case "$image" in
    ubuntu*|debian*)
        setup='export DEBIAN_FRONTEND=noninteractive
               apt-get update -qq >/dev/null
               apt-get install -y -qq --no-install-recommends systemd \
                 libx11-6 libxext6 libxss1 libxpresent1 libxrandr2 libxfixes3 libxcb1 \
                 libegl1 libgl1 libgbm1 libdrm2 libvulkan1 libva2 libva-drm2 libva-x11-2 \
                 libva-wayland2 libpipewire-0.3-0 \
                 libasound2t64 libpulse0 libwayland-client0 libwayland-cursor0 \
                 libwayland-egl1 libxkbcommon0 libfontconfig1 libfreetype6 \
                 libharfbuzz0b libexpat1 zlib1g >/dev/null' ;;
    fedora*)
        setup='dnf install -y -q systemd util-linux tar gzip \
                 libX11 libXext libXScrnSaver libXpresent libXrandr libXfixes libxcb \
                 libglvnd-egl libglvnd-glx mesa-libEGL mesa-libgbm libdrm vulkan-loader \
                 libva pipewire-libs alsa-lib pulseaudio-libs libwayland-client \
                 libwayland-cursor libwayland-egl libxkbcommon fontconfig freetype \
                 harfbuzz expat zlib-ng-compat libstdc++ libgcc >/dev/null' ;;
    archlinux*)
        setup='pacman -Syu --noconfirm --needed systemd util-linux tar gzip \
                 libx11 libxext libxss libxpresent libxrandr libxfixes libxcb \
                 libglvnd mesa libdrm vulkan-icd-loader libva libpipewire alsa-lib \
                 libpulse wayland libxkbcommon fontconfig freetype2 harfbuzz expat \
                 zlib >/dev/null' ;;
    *) echo "verify-release.sh: no package list for $image" >&2; exit 2 ;;
esac

cat > "$work/as-user.sh" <<'USER'
set -euo pipefail
fail() { echo "FAIL: $*" >&2; exit 1; }
cd "$HOME"
tar -xzf /work/bundle.tar.gz
dir="$(echo omniphony-*-linux-x86_64)"
"$dir/install.sh" --service

lib="$HOME/.local/share/omniphony/lib"
ls "$lib"/liborender.so.* >/dev/null || fail "no engine in $lib"

echo "== orender and Studio resolve their libraries"
"$HOME/.local/bin/orender" --version
for exe in orender omniphony-studio-egui; do
    missing="$(ldd "$HOME/.local/lib/omniphony/studio/$exe" | awk '/not found/ {print $1}')"
    [ -z "$missing" ] || fail "$exe: unresolved $missing"
done

echo "== the player loads the engine from the per-user folder"
export APPIMAGE_EXTRACT_AND_RUN=1
mpv="$HOME/.local/bin/mpv-omniphony"
"$mpv" --no-config --really-quiet /work/tone51.wav --o=probe.mka --oac=eac3 --of=matroska
log="$("$mpv" --no-config --ad=orender,lavc --ao=null --vo=null probe.mka 2>&1 || true)"
line="$(grep -E "orender: loaded liborender .*$lib/liborender\.so\.[0-9]+ \(studio install\)" <<<"$log")" \
    || { echo "$log" | tail -30; fail "the player did not load the installed engine"; }
echo "$line"

echo "== the service unit"
unit="$HOME/.config/systemd/user/omniphony-renderer.service"
[ -f "$unit" ] || fail "no unit written by --service"
# No user manager in a container: a runtime directory is all the check needs.
verify="$(XDG_RUNTIME_DIR="$(mktemp -d)" systemd-analyze --user verify "$unit" 2>&1)" \
    || { echo "$verify"; fail "systemd-analyze rejects the unit"; }
echo "$verify"
! grep -q "omniphony-renderer.service:" <<<"$verify" || fail "systemd-analyze warns about the unit"

echo "== uninstall keeps the config"
mkdir -p "$HOME/.config/omniphony"
echo "render: {}" > "$HOME/.config/omniphony/config.yaml"
"$HOME/.local/lib/omniphony/uninstall.sh"
[ -f "$HOME/.config/omniphony/config.yaml" ] || fail "uninstall removed config.yaml"
[ ! -e "$HOME/.local/lib/omniphony" ] || fail "uninstall left the program folder"
! ls "$lib"/liborender.so.* >/dev/null 2>&1 || fail "uninstall left the engine"
echo ">> installer OK"
USER

docker run --rm -v "$work:/work:ro" "$image" bash -euo pipefail -c "
    $setup
    useradd -m tester
    su tester -c 'bash /work/as-user.sh'
"
echo ">> installer OK on $image"
