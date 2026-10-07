#!/usr/bin/env bash
# Clippy ratchet for the renderer workspace.
#
# clippy runs over every member and target; its warnings are counted per crate
# and lint and compared with clippy-baseline.txt (next to Cargo.toml):
#
#   - a count above its baseline fails (a lint absent from the baseline has a
#     baseline of 0, so a crate with no entries is gated at zero warnings);
#   - a count below its baseline also fails, until the lowered baseline is
#     committed with the change that earned it, so the numbers only go down;
#   - a deny-level lint fails clippy itself, before any counting.
#
# The baseline names the clippy toolchain it was taken with. Lints come and go
# between releases, so counts only mean something against a fixed version; the
# script installs that toolchain (rustup) and runs it. Moving to a newer one is
# a change of its own: bump the version line, regenerate, commit.
#
# Usage, from anywhere:
#   omniphony-renderer/scripts/clippy-ratchet.sh
#   UPDATE_CLIPPY_BASELINE=1 omniphony-renderer/scripts/clippy-ratchet.sh
#       rewrites the baseline when counts only went down;
#   UPDATE_CLIPPY_BASELINE=allow-increase ...
#       rewrites it even when one grew. A maintainer's escape hatch (a toolchain
#       bump), never a way to get a change through.
#
# Needs jq.
set -euo pipefail

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
baseline="$workspace/clippy-baseline.txt"
update="${UPDATE_CLIPPY_BASELINE:-}"
cd "$workspace"

toolchain="$(sed -n 's/^# toolchain: *//p' "$baseline")"
if [ -z "$toolchain" ]; then
  echo "clippy-ratchet: no '# toolchain: <version>' line in $baseline" >&2
  exit 2
fi

rustup toolchain install --profile minimal --component clippy "$toolchain" >&2

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

# Package id -> name, for the workspace members (the id's spelling varies with
# the directory name, so it is not parsed).
cargo "+$toolchain" metadata --format-version 1 --no-deps --locked \
  | jq -r '.packages[] | [.id, .name] | @tsv' > "$scratch/names.tsv"

status=0
cargo "+$toolchain" clippy --workspace --all-targets --locked --message-format=json \
  > "$scratch/clippy.json" || status=$?
if [ "$status" -ne 0 ]; then
  jq -r 'select(.reason == "compiler-message" and .message.level == "error") | .message.rendered' \
    "$scratch/clippy.json" >&2
  echo "clippy-ratchet: clippy failed (exit $status); fix the errors above first" >&2
  exit 1
fi

# One record per distinct warning. The library and its test target report the
# same warning twice, so records are keyed by their primary span.
jq -r --rawfile names "$scratch/names.tsv" '
  ($names | split("\n") | map(select(length > 0) | split("\t") | {(.[0]): .[1]}) | add) as $name
  | select(.reason == "compiler-message" and .message.level == "warning")
  | (.message.spans | map(select(.is_primary)) | first) as $span
  | select($span != null)
  | [$name[.package_id], (.message.code.code // "warning"),
     "\($span.file_name):\($span.line_start):\($span.column_start)",
     .message.rendered]
  | @json' "$scratch/clippy.json" | sort -u > "$scratch/warnings.jsonl"

jq -r '.[0] + " " + .[1]' "$scratch/warnings.jsonl" | sort | uniq -c \
  | awk '{ print $2, $3, $1 }' > "$scratch/current.txt"
awk '!/^#/ && NF == 3 { print $1, $2, $3 }' "$baseline" | sort > "$scratch/baseline.txt"

# Every (crate, lint) pair in either file, with both counts.
join -a1 -a2 -e 0 -o 0,1.2,2.2 \
  <(awk '{ print $1 "|" $2, $3 }' "$scratch/baseline.txt" | sort) \
  <(awk '{ print $1 "|" $2, $3 }' "$scratch/current.txt" | sort) \
  > "$scratch/compare.txt"

grew=0; shrank=0
while read -r key was now; do
  crate="${key%%|*}"; lint="${key#*|}"
  if [ "$now" -gt "$was" ]; then
    grew=1
    echo "::error::clippy: $crate has $now $lint warning(s), baseline $was"
    jq -r --arg c "$crate" --arg l "$lint" 'select(.[0] == $c and .[1] == $l) | .[3]' \
      "$scratch/warnings.jsonl"
  elif [ "$now" -lt "$was" ]; then
    shrank=1
    echo "clippy: $crate $lint went down from $was to $now"
  fi
done < "$scratch/compare.txt"

total="$(awk '{ n += $3 } END { print n + 0 }' "$scratch/current.txt")"

write_baseline() {
  {
    echo "# Clippy warnings per crate and lint in the renderer workspace, checked by"
    echo "# scripts/clippy-ratchet.sh (CI job \`lint\`). A crate or lint not listed is"
    echo "# allowed none."
    echo "#"
    echo "# Counts only ever go down. After removing warnings, regenerate with"
    echo "#   UPDATE_CLIPPY_BASELINE=1 omniphony-renderer/scripts/clippy-ratchet.sh"
    echo "# and commit this file with the change."
    echo "#"
    echo "# toolchain: $toolchain"
    echo "# total: $total"
    echo "#"
    echo "# crate lint count"
    cat "$scratch/current.txt"
  } > "$baseline"
  echo "clippy-ratchet: wrote $baseline ($total warnings)"
}

if [ -n "$update" ]; then
  if [ "$grew" -eq 1 ] && [ "$update" != "allow-increase" ]; then
    echo "clippy-ratchet: counts grew; the baseline is only lowered (UPDATE_CLIPPY_BASELINE=allow-increase overrides)" >&2
    exit 1
  fi
  write_baseline
  exit 0
fi

if [ "$grew" -eq 1 ]; then
  echo "clippy-ratchet: new warnings (above). Fix them; the baseline is not raised to let a change through." >&2
  exit 1
fi
if [ "$shrank" -eq 1 ]; then
  echo "clippy-ratchet: warnings went down. Lock it in:" >&2
  echo "  UPDATE_CLIPPY_BASELINE=1 omniphony-renderer/scripts/clippy-ratchet.sh" >&2
  echo "and commit clippy-baseline.txt with the change." >&2
  exit 1
fi
echo "clippy-ratchet: $total warnings, as in the baseline (toolchain $toolchain)"
