#!/usr/bin/env bash
# Merge train: brings one pull request at a time up to date with main and
# runs CI on it, instead of every open pull request rerunning CI each time
# main moves.
#
# main requires a branch to be up to date before it merges, so each merge
# left every other open pull request behind, and bringing them all up to date
# reran the whole CI on each: 119 of the 222 pull request heads CI tested
# from 2026-10-05 to 10-06 were "merge main" commits. The train keeps the
# guarantee (what reaches main was tested exactly as it lands) and spends one
# CI run per merge on it.
#
# The queue is the open pull requests into main with auto-merge enabled,
# oldest enablement first. A pull request boards only when the latest CI run
# on its head passed: one that is red or still running has no chance yet and
# keeps its place. The pull request on board carries the `merge-train` label.
# Each tick, run on every push to main, every finished CI run, every
# auto-merge enablement and on a schedule as a backstop, does at most one of:
#
#   - on board and behind main: update it again (main moved under it);
#   - on board, CI running: wait;
#   - on board, CI green: wait for auto-merge, which GitHub does itself;
#   - on board, CI red, conflicting, or stuck: put it off the train,
#     disable its auto-merge and say why in a comment; re-enabling auto-merge
#     puts it back in the queue;
#   - nobody on board: board the next candidate, merging main into it if it
#     is behind, and start CI on it.
#
# The update is pushed with the workflow's GITHUB_TOKEN. GitHub does not run
# CI for that push: it creates the pull_request run but holds it for a
# maintainer's approval ("Approve workflows to run" on the pull request).
# Nobody needs to approve it: the train dispatches CI on the branch itself
# (ci.yml's workflow_dispatch), whose check runs land on the branch's head
# commit, which is what the required checks are read from. Approving the held
# run only runs CI a second time.
#
# DRY_RUN=1 prints what a tick would change instead of changing it, for a
# run by hand: GITHUB_REPOSITORY=mgth/Omniphony DRY_RUN=1 .github/scripts/merge-train.sh
set -euo pipefail

repo="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY must name the repository}"
base="main"
label="merge-train"
ci_workflow="ci.yml"
# CI takes about 15 minutes plus queueing; past this the pull request on
# board is taken off rather than holding the train forever.
stall_minutes="${MERGE_TRAIN_STALL_MINUTES:-120}"
dry="${DRY_RUN:-0}"

log() { echo "merge-train: $*" >&2; }

act() {
  if [ "$dry" = 1 ]; then
    echo "DRY RUN: $*" >&2
  else
    "$@"
  fi
}

# Open same-repository pull requests into main, as one JSON array.
open_prs() {
  gh pr list -R "$repo" --base "$base" --state open --limit 200 \
    --json number,headRefName,headRefOid,isDraft,isCrossRepository,mergeable,autoMergeRequest,labels
}

pr_field() { # <json> <number> <jq filter on the pull request>
  jq -r --argjson n "$2" ".[] | select(.number == \$n) | $3" <<<"$1"
}

# State of the latest CI run on a commit: success, failure, pending or none.
# Runs that never ran are left out: cancelled ones (superseded by a newer
# push), skipped ones, and the pull_request run GitHub creates for the
# train's own update and holds for a maintainer's approval
# (`action_required`, see the header). On 2026-10-06 that held run, created
# in the same second as the dispatched one, was read as a failure and threw
# the first pull request off the train.
ci_state() {
  gh run list -R "$repo" --workflow "$ci_workflow" --commit "$1" --limit 20 \
    --json status,conclusion,createdAt \
    --jq '[.[] | select(.conclusion as $c
                        | ["cancelled", "skipped", "action_required", "stale"]
                        | index($c) | not)]
          | sort_by(.createdAt) | last
          | if . == null then "none"
            elif .status != "completed" then "pending"
            elif .conclusion == "success" then "success"
            else "failure" end'
}

behind_by() { gh api "repos/$repo/compare/$base...$1" --jq .behind_by; }

ensure_label() {
  act gh label create "$label" -R "$repo" --color FBCA04 \
    --description "On the merge train: being brought up to date with main and tested" \
    --force >/dev/null
}

# Takes a pull request off the train: label off, auto-merge off, and a
# comment saying why and how to get back on.
eject() { # <number> <reason>
  log "#$1 off the train: $2"
  act gh pr edit "$1" -R "$repo" --remove-label "$label" >/dev/null || true
  act gh pr merge "$1" -R "$repo" --disable-auto || true
  act gh pr comment "$1" -R "$repo" --body "Merge train: $2

Auto-merge has been disabled. Re-enable it to put this pull request back in the queue." >/dev/null
}

# Starts CI on a branch, by dispatch (see the header).
dispatch_ci() { # <number> <branch>
  log "#$1: starting CI on $2"
  act gh workflow run "$ci_workflow" -R "$repo" --ref "$2"
}

# Merges main into a pull request and waits for its new head. Returns 2 on a
# conflict, after taking the pull request off the train, and 1 when the head
# has not moved yet (the next tick looks again).
update_branch() { # <number> <head sha>
  log "#$1: merging $base into it"
  if [ "$dry" = 1 ]; then
    act gh api -X PUT "repos/$repo/pulls/$1/update-branch" -f "expected_head_sha=$2"
    return 0
  fi
  if ! gh api -X PUT "repos/$repo/pulls/$1/update-branch" -f "expected_head_sha=$2" >/dev/null; then
    eject "$1" "merging \`$base\` into this branch failed (most likely a conflict). Merge \`$base\` and resolve it by hand."
    return 2
  fi
  # The update is asynchronous: CI must start on the new head, not the old.
  for _ in $(seq 1 24); do
    if [ "$(gh pr view "$1" -R "$repo" --json headRefOid --jq .headRefOid)" != "$2" ]; then
      return 0
    fi
    sleep 5
  done
  log "#$1: the head did not move within two minutes; the next tick retries"
  return 1
}

# Minutes since the label was last put on a pull request.
minutes_on_board() { # <number>
  local at
  at="$(gh api "repos/$repo/issues/$1/events" --paginate \
    --jq "[.[] | select(.event == \"labeled\" and .label.name == \"$label\")] | last | .created_at // empty" \
    | tail -n 1)"
  [ -n "$at" ] || { echo 0; return; }
  echo $(( ($(date -u +%s) - $(date -u -d "$at" +%s)) / 60 ))
}

# Handles the pull request on board. Returns 0 when the train must wait for
# it, 1 when the train is free for the next one.
tend_on_board() { # <prs json> <number>
  local prs="$1" n="$2" head branch state
  head="$(pr_field "$prs" "$n" .headRefOid)"
  branch="$(pr_field "$prs" "$n" .headRefName)"

  if [ "$(pr_field "$prs" "$n" '.autoMergeRequest != null')" != true ]; then
    log "#$n: auto-merge was disabled; taking it off the train"
    act gh pr edit "$n" -R "$repo" --remove-label "$label" >/dev/null
    return 1
  fi
  if [ "$(pr_field "$prs" "$n" .mergeable)" = CONFLICTING ]; then
    eject "$n" "this branch conflicts with \`$base\`."
    return 1
  fi
  if [ "$(behind_by "$head")" -gt 0 ]; then
    # main moved under it (a merge outside the train): update again.
    local rc=0
    update_branch "$n" "$head" || rc=$?
    case "$rc" in
      0) dispatch_ci "$n" "$branch"; return 0 ;;
      1) return 0 ;;
      *) return 1 ;;
    esac
  fi

  state="$(ci_state "$head")"
  case "$state" in
    pending)
      log "#$n: CI running on ${head:0:8}; waiting"
      return 0 ;;
    none)
      # A run dispatched a moment ago may not be listed yet: give it a few
      # minutes before dispatching again (a second dispatch would cancel the
      # first through ci.yml's concurrency group).
      if [ "$(minutes_on_board "$n")" -ge 3 ]; then
        dispatch_ci "$n" "$branch"
      else
        log "#$n: no CI run listed on ${head:0:8} yet; waiting"
      fi
      return 0 ;;
    failure)
      eject "$n" "CI failed on ${head:0:8}, this branch brought up to date with \`$base\`."
      return 1 ;;
    success)
      if [ "$(minutes_on_board "$n")" -ge "$stall_minutes" ]; then
        eject "$n" "CI passed on ${head:0:8} but the pull request has not merged after ${stall_minutes} minutes on the train (a required check or review is missing?)."
        return 1
      fi
      log "#$n: CI passed on ${head:0:8}; waiting for auto-merge"
      return 0 ;;
  esac
}

# Boards the next candidate. Returns 0 when one boarded.
board_next() { # <prs json>
  local prs="$1" n head branch rc
  # Queue order: oldest auto-merge enablement first.
  for n in $(jq -r '[.[] | select(.autoMergeRequest != null and (.isDraft | not) and (.isCrossRepository | not))]
                    | sort_by(.autoMergeRequest.enabledAt) | .[].number' <<<"$prs"); do
    head="$(pr_field "$prs" "$n" .headRefOid)"
    branch="$(pr_field "$prs" "$n" .headRefName)"
    if [ "$(pr_field "$prs" "$n" .mergeable)" = CONFLICTING ]; then
      eject "$n" "this branch conflicts with \`$base\`."
      continue
    fi
    if [ "$(ci_state "$head")" != success ]; then
      continue  # no chance yet: keeps its place
    fi
    log "#$n boards the train"
    ensure_label
    act gh pr edit "$n" -R "$repo" --add-label "$label" >/dev/null
    if [ "$(behind_by "$head")" -gt 0 ]; then
      rc=0
      update_branch "$n" "$head" || rc=$?
      case "$rc" in
        0) dispatch_ci "$n" "$branch" ;;
        2) continue ;;  # conflict: off the train, try the next one
      esac
    else
      log "#$n: already up to date and green; waiting for auto-merge"
    fi
    return 0
  done
  log "no candidate: nothing with auto-merge enabled and a green CI"
  return 1
}

prs="$(open_prs)"
on_board="$(jq -r --arg l "$label" '[.[] | select(any(.labels[]; .name == $l))] | .[].number' <<<"$prs")"

first=""
for n in $on_board; do
  if [ -z "$first" ]; then
    first="$n"
  else
    log "#$n also carries the label; only one rides at a time"
    act gh pr edit "$n" -R "$repo" --remove-label "$label" >/dev/null
  fi
done

if [ -n "$first" ] && tend_on_board "$prs" "$first"; then
  exit 0
fi
# The train is free: refresh (an ejection changed labels) and board the next.
board_next "$(open_prs)" || true
