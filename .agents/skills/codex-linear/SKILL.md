---
name: codex-linear
description: Coordinate PDF.rs development through Linear and Git across multiple devices. Use for any CHO issue implementation, bug fix, evidence task, review handoff, branch or PR workflow, blocker update, completion update, or Linear/Git status audit in this repository.
---

# Codex Linear

Keep Linear execution state, Git branches, pull requests, tests, and repository evidence synchronized without allowing two devices to work on the same issue.

## Resolve the work identity

1. Resolve the Linear issue from an explicit `CHO-<number>` in the request, then from the current branch name.
2. Do not infer an issue from similar titles or nearby code. Ask for the issue identifier when neither source is conclusive.
3. Read the full issue, relations, parent, milestone, status, labels, and acceptance criteria before changing code or Linear.
4. Read the runner from `git config user.name`. Require a non-empty value that is unique across devices.
5. Normalize the runner label as `runner/<lowercase-name>`, replacing whitespace with hyphens. Do not use the shared GitHub login as the runner.

## Select the workflow

- For implementation, fixes, evidence work, reviews, or PR handoffs, follow the development lifecycle below.
- For reconciliation, planning, status questions, or scheduled checks, use audit mode.
- If Linear tools are unavailable, stop coordinated work and report that the Linear connection is required. Do not silently continue on an unclaimed issue.

## Run the development lifecycle

### 1. Preflight Git

1. Inspect the current branch, worktree status, remotes, and `origin/main` relationship.
2. Fetch `origin` before starting new work.
3. Treat `origin/main` as the shared multi-device baseline. If local `main` contains unpublished commits, stop and report that the baseline must be published or reconciled first.
4. Never start implementation directly on `main`.
5. Use `codex/cho-<number>-<short-slug>` for the issue branch.
6. Preserve unrelated user changes. If the worktree is dirty and the ownership of those changes is unclear, stop before switching branches or claiming the issue.

### 2. Validate and claim the issue

1. Reject the claim when an unresolved `blockedBy` relation exists.
2. Find every existing `runner/*` label.
3. If another runner label exists, stop and report the current runner. Never steal or replace the claim without explicit user direction.
4. Preserve every existing label when adding the current runner. Linear label updates replace the full label set, so always read and resend the complete set.
5. Set the issue to `In Progress` only after Git preflight succeeds.
6. Add one claim comment only when the claim or branch is new:

```md
## Work claimed

- Runner: `<git user.name>`
- Branch: `codex/cho-<number>-<short-slug>`
- Base: `origin/main@<commit>`
```

### 3. Implement the issue

1. Treat Outcome, Scope, Non-goals, Acceptance, repository anchors, and dependency relations as the implementation contract.
2. Keep one issue per branch and one runner per issue.
3. Add or update behavior tests, boundary tests, `PROVENANCE.md`, feature/spec mappings, and capability evidence whenever the issue requires them.
4. Keep PDFium at `../pdfium` behind the development/CI baseline boundary; never add it to the product dependency or runtime path.
5. Do not post routine progress comments. Update Linear only for a new claim, a material blocker, a review handoff, or completion.

### 4. Record a blocker

1. Add a concise comment containing the blocker, evidence, affected acceptance criteria, and exact unblock condition.
2. Add or verify the blocking relation when another Linear issue is responsible.
3. If work is released because of the blocker, move the issue to `Todo` and remove only the current `runner/*` label while preserving all other labels.
4. Do not invent a `Blocked` status or use a label in place of a known issue relation.

### 5. Hand off for review

1. Run issue-scoped tests and the required repository gates before handoff.
2. Push the issue branch and open or update a pull request containing the issue identifier.
3. Move the issue to the correctly spelled `In Review` status only after a remote branch or PR exists.
4. Add one review comment containing the PR, verification commands and results, traceability changes, and remaining risks.
5. Ignore the misspelled `In Reivew` status.

### 6. Complete the issue

1. Move an issue to `Done` only after the PR is merged, required CI passes, every acceptance criterion is satisfied, and required provenance/traceability updates are present.
2. Add one final comment with the PR, merge commit, verification evidence, and any follow-up issue identifiers.
3. Retain the runner label on completed issues as execution history.
4. Do not close a parent issue, milestone, or release gate until all of its own acceptance conditions and required children are complete.

## Use audit mode

1. Read Linear, Git, branch, and PR state before proposing changes.
2. Report, at minimum:
   - active issues with no runner or remote branch;
   - issues with multiple runners;
   - blocked issues incorrectly marked `In Progress`;
   - merged PRs whose issues are not `Done`;
   - `In Review` issues with no remote branch or PR;
   - active branches based on stale `origin/main`;
   - local commits or changes whose apparent work does not match the Linear status.
3. Keep audit mode read-only unless the user explicitly asks to apply corrections.
4. When corrections are authorized, update only unambiguous states and summarize every Linear mutation.

## Preserve operational safety

- Read before every Linear write and use exact issue identifiers.
- Never overwrite the full label set with a partial list.
- Never modify another runner's issue, branch, or worktree without explicit direction.
- Do not push, merge, close, cancel, or reassign work unless the current request authorizes that action.
- Report repository/Linear drift rather than hiding it with automatic state changes.
