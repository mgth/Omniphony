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
# The label covers one head, not the pull request. When the label is added,
# the run records the head it was added on in a comment ("Listened to <sha>"),
# taken from the event itself, so it is the head the person saw. Every run,
# whatever its event, then checks that head against the current one: if the
# commits since touch the path, the label no longer holds and is removed. A
# promotion pull request (head = main) moves each time main does, so a label
# set early does not cover render changes merged after it.
#
# No run relies on an earlier one having happened: a run that GitHub
# cancelled or dropped is made up by the next. The one loss is a `labeled`
# run dropped before it records its head; the next run then finds the label
# without a matching record and removes it, so the label has to be added
# again. That fails closed.
#
# Inputs (environment): GITHUB_REPOSITORY, PR, ACTION and LABEL_NAME (the
# pull_request event's action and label), BASE_SHA, HEAD_SHA, GH_TOKEN. Run
# from a checkout with the history of both sides. DRY_RUN=1 prints the
# changes to the pull request instead of making them, for a run by hand:
#   PR=123 ACTION=opened BASE_SHA=origin/release HEAD_SHA=origin/main \
#     GITHUB_REPOSITORY=mgth/Omniphony DRY_RUN=1 .github/scripts/listened-check.sh
set -euo pipefail

LABEL=listened
MARKER='<!-- listened-head -->'
BOT='github-actions[bot]'
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

# The head recorded by the last labelling, from this workflow's own comments
# only: anyone can comment, only the workflow writes the record.
recorded_head() {
  gh api "repos/$GITHUB_REPOSITORY/issues/$PR/comments" --paginate \
    --jq ".[] | select(.user.login == \"$BOT\" and (.body | startswith(\"$MARKER\"))) | .body" |
    grep -oE '[0-9a-f]{40}' | tail -n 1 || true
}

say() {
  if [ "${DRY_RUN:-}" = 1 ]; then
    echo "DRY_RUN: would comment on #$PR: $1"
    return
  fi
  gh pr comment "$PR" --repo "$GITHUB_REPOSITORY" --body "$1" >/dev/null
}

remove_label() {
  if [ "${DRY_RUN:-}" = 1 ]; then
    echo "DRY_RUN: would remove '$LABEL' from #$PR"
  else
    gh pr edit "$PR" --repo "$GITHUB_REPOSITORY" --remove-label "$LABEL" >/dev/null
  fi
  say "$1 \`$LABEL\` was removed: listen to the head ($HEAD_SHA) and add the label again."
}

fail_unlistened() {
  echo "::error::This pull request changes the render or output path. Listen to its head, then add the '$LABEL' label."
  printf '%s\n' "$files" | head -n 50
  exit 1
}

base="$(git merge-base "$BASE_SHA" "$HEAD_SHA")"
files="$(hot_files "$base" "$HEAD_SHA")"

# The label was just added: record the head it was added on. HEAD_SHA comes
# from the event, so a push after the click is not covered by it.
if [ "${ACTION:-}" = labeled ] && [ "${LABEL_NAME:-}" = "$LABEL" ]; then
  say "$MARKER
Listened to $HEAD_SHA."
  echo "Labelled \`$LABEL\` on $HEAD_SHA." | tee -a "$summary"
  exit 0
fi

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

if ! has_label; then
  fail_unlistened
fi

listened="$(recorded_head)"
if [ -z "$listened" ]; then
  remove_label "The label has no recorded head (it was added before this check existed, or its run was dropped), so"
  fail_unlistened
fi
if [ "$listened" != "$HEAD_SHA" ]; then
  # A head that is gone (force-push) leaves no range: everything counts as new.
  if git cat-file -e "$listened^{commit}" 2>/dev/null; then
    new="$(hot_files "$listened" "$HEAD_SHA")"
  else
    new="$files"
  fi
  if [ -n "$new" ]; then
    remove_label "Commits since ${listened:0:10} change the render or output path, so"
    fail_unlistened
  fi
fi

echo "Labelled \`$LABEL\`: listened to ${listened:0:10}, and nothing on the path changed since." | tee -a "$summary"
