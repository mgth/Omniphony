#!/usr/bin/env bash
# Listened check: a pull request into `release` that changes what users hear
# needs the `listened` label before it can merge. CI cannot hear (#683).
#
# "What users hear" is the render and output path: the sources of the crates
# a sample goes through between the decoder and the device, the CLI that
# drives them, and the speaker layout presets. The control plane (OSC), the
# overlay, tests and documentation are left out. HOT and COLD below are the
# whole definition; widen HOT rather than leave a path unchecked.
#
# The label means "someone listened to this head". When new commits arrive
# and they touch the path, the label is removed and the check fails until it
# is listened to and labelled again. A promotion pull request (head = main)
# moves each time main does, so a label set early does not cover changes
# merged after it.
#
# Inputs (environment): GITHUB_REPOSITORY, PR, ACTION (the pull_request
# event's action), BASE_SHA, HEAD_SHA, BEFORE (the previous head, for
# `synchronize`), GH_TOKEN. Run from a checkout with the history of both
# sides. DRY_RUN=1 prints the label change instead of making it, for a run by
# hand:
#   PR=123 ACTION=opened BASE_SHA=origin/release HEAD_SHA=origin/main \
#     GITHUB_REPOSITORY=mgth/Omniphony DRY_RUN=1 .github/scripts/listened-check.sh
set -euo pipefail

LABEL=listened
HOT='^(omniphony-renderer/(renderer|audio_output|audio_input|host_audio|orender_engine|orender_ffi|spdif|script_backend)/src/|omniphony-renderer/src/|layouts/)'
COLD='(^|/)(osc|tests?|benches|examples)/|/osc(_[a-z_]+)?\.rs$|/overlay\.rs$|_tests?\.rs$|\.md$'

summary="${GITHUB_STEP_SUMMARY:-/dev/stdout}"

# Files in $1..$2 on the render or output path.
hot_files() {
  git diff --name-only "$1" "$2" -- | grep -E "$HOT" | grep -vE "$COLD" || true
}

has_label() {
  gh pr view "$PR" --repo "$GITHUB_REPOSITORY" --json labels \
    --jq ".labels[].name" | grep -qx "$LABEL"
}

remove_label() {
  if [ "${DRY_RUN:-}" = 1 ]; then
    echo "DRY_RUN: would remove '$LABEL' from #$PR"
    return
  fi
  gh pr edit "$PR" --repo "$GITHUB_REPOSITORY" --remove-label "$LABEL" >/dev/null
  gh pr comment "$PR" --repo "$GITHUB_REPOSITORY" --body \
    "New commits change the render or output path, so \`$LABEL\` was removed: listen to the new head and add the label again." >/dev/null
}

base="$(git merge-base "$BASE_SHA" "$HEAD_SHA")"
files="$(hot_files "$base" "$HEAD_SHA")"

if [ -z "$files" ]; then
  echo "No file on the render or output path: nothing to listen to." | tee -a "$summary"
  exit 0
fi

{
  echo "## Changes on the render or output path"
  echo
  echo "Commits (newest first):"
  echo
  # The paths come from the file list itself: git log takes no regex.
  # shellcheck disable=SC2086
  git log --no-merges --format='- %h %s' "$base..$HEAD_SHA" -- $files | head -n 200
  echo
  echo "Files: $(printf '%s\n' "$files" | wc -l)"
} >>"$summary"

labelled=false
if has_label; then labelled=true; fi

# New commits on the path since the label was set: it no longer holds. A
# force-push leaves no usable range, so the whole pull request counts as new.
if $labelled && [ "${ACTION:-}" = synchronize ]; then
  if [ -n "${BEFORE:-}" ] && git cat-file -e "$BEFORE^{commit}" 2>/dev/null; then
    new="$(hot_files "$BEFORE" "$HEAD_SHA")"
  else
    new="$files"
  fi
  if [ -n "$new" ]; then
    remove_label
    labelled=false
  fi
fi

if $labelled; then
  echo "Labelled \`$LABEL\`: the render or output path was listened to." | tee -a "$summary"
  exit 0
fi

echo "::error::This pull request changes the render or output path. Listen to its head, then add the '$LABEL' label."
printf '%s\n' "$files" | head -n 50
exit 1
