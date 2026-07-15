---
name: codex-linear
description: Coordinate PDF.rs development across Linear, Git worktrees, GitHub pull requests, multiple devices, and multiple Codex agents. Use for CHO issue implementation, fixes, evidence work, claiming or releasing work, issue worktrees and branches, PR review or handoff, blocker and completion updates, or Linear/Git/PR status audits in this repository.
---

# Codex Linear

Coordinate execution in Linear while keeping Git and pull requests authoritative for code, review, CI, and merge state.

## Use the coordination model

- Treat an executable leaf issue as the unit of work.
- Treat `runner/<name>` as a device identity, not an individual agent identity.
- Treat the dedicated worktree plus issue branch as the agent identity.
- Treat the pull request as the only integration path into `main`.
- Allow one runner to claim multiple different issues. Allow only one active claim on any single issue.
- Keep parent issues, milestones, and exit-gate trackers free of runner labels, worktrees, branches, and PRs. Derive their state from their executable children.

## Resolve the issue and identities

1. Resolve the Linear issue from an explicit `CHO-<number>` in the request, then from the current branch name.
2. Do not infer an issue from similar titles or nearby code. Ask for the issue identifier when neither source is conclusive.
3. Read the full issue, children, relations, parent, project, milestone, cycle, priority, due date, status, labels, comments, acceptance criteria, and existing branch or PR before changing code or Linear.
4. Classify the issue before claiming it. If it has executable child issues or only coordinates a milestone or exit gate, do not claim it; select a leaf issue instead.
5. Read the device runner from `git config user.name`. Require a non-empty value that is unique across devices; do not use the shared GitHub login.
6. Normalize the runner label as `runner/<lowercase-name>`, replacing whitespace with hyphens.
7. Derive a stable worker ID from the dedicated worktree name. Combine the runner, worker ID, issue, and branch when checking ownership. Do not publish an absolute local path to Linear.

## Select the workflow

- Follow the development lifecycle for implementation, fixes, evidence work, review, or PR handoff.
- Follow the handoff lifecycle when an issue moves between devices or agents.
- Use audit mode for reconciliation, planning, status questions, or scheduled checks.
- If Linear tools are unavailable, stop coordinated development and report that the Linear connection is required. Do not work on an unclaimed issue.

## Run the development lifecycle

### 1. Preflight Git

1. Inspect the current branch, worktree list, worktree status, remotes, and relationship to `origin/main`.
2. Fetch `origin` before starting new work.
3. Treat `origin/main` as the shared baseline. Stop when local `main` has unpublished commits or relevant uncommitted work that must be reconciled first.
4. Keep the primary `main` worktree for synchronization and integration only. Never implement an issue directly on `main`.
5. Give every writing agent its own worktree created from current `origin/main` and its own `codex/cho-<number>-<short-slug>` branch.
6. Do not let subagents that share a worktree make concurrent semantic changes for different issues. Use separate Codex tasks and separate worktrees; shared-worktree subagents may perform read-only analysis.
7. Preserve unrelated user changes. Stop before switching branches, moving files, or claiming work when ownership of dirty changes is unclear.
8. Resume an existing worktree only when its issue, runner, worker ID, branch, and active Linear claim all match.

### 2. Validate the claim

1. Reject the claim when an unresolved `blockedBy` relation exists.
2. Read every `runner/*` label and every claim, release, and handoff comment on the issue.
3. If the current runner label does not exist in the team, create that exact team-scoped label before claiming. Do not create a new label for every agent or issue.
4. Treat the most recent `## Work released`, `## Work handed off`, or `## Work completed` comment as the start of a new claim epoch. Within an epoch, the earliest valid `## Work claimed` comment owns the issue until the next release, handoff, or completion event.
5. Stop on multiple runner labels, a claim owned by another runner, or a same-runner claim with a different worker ID or branch.
6. Treat a runner label without a matching active claim comment, or a claim comment without its runner label, as drift. Report it instead of guessing ownership.
7. Preserve every existing non-runner label when changing the runner label set. Linear label updates replace the full set, so always read and resend the complete intended set.

### 3. Claim and confirm

1. Prepare the clean issue worktree and branch before writing implementation code.
2. Add the runner label while preserving all other labels, set the leaf issue to `In Progress`, and add one claim comment:

```md
## Work claimed

- Runner: `<git user.name>`
- Runner label: `runner/<normalized-name>`
- Worker: `<worktree-id>`
- Issue: `CHO-<number>`
- Branch: `codex/cho-<number>-<short-slug>`
- Base: `origin/main@<commit>`
```

3. Immediately re-read the issue, labels, and comments after writing the claim.
4. If concurrent claims exist in the current epoch, accept the earliest server-created valid claim. If it is not the current claim, stop before editing code and report the clean local worktree and branch; do not remove or overwrite the winning claim.
5. Begin implementation only after the re-read confirms the complete claim tuple.

### 4. Implement the issue

1. Treat Outcome, Scope, Non-goals, Acceptance, repository anchors, and dependency relations as the implementation contract.
2. Keep one executable issue per worktree, branch, and pull request. Reuse a runner label across different issues on the same device.
3. Add or update behavior tests, boundary tests, `PROVENANCE.md`, feature/spec mappings, and capability evidence whenever required.
4. Keep PDFium at `../pdfium` behind the development/CI baseline boundary; never add it to the product dependency or runtime path.
5. Do not post routine progress comments. Update Linear only for a claim, material blocker, release, handoff, review, or completion.

### 5. Record a blocker or release work

1. Add a concise blocker comment containing evidence, affected acceptance criteria, and the exact unblock condition.
2. Add or verify the blocking relation when another Linear issue is responsible.
3. Keep the claim when the agent continues to own the blocked work.
4. When releasing the work, add a `## Work released` comment containing runner, worker, branch, pushed head if any, reason, and next action.
5. Move released work to `Todo` and remove only the active runner label while preserving every other label. Do not delete the branch or worktree automatically.
6. Never infer that an old claim is abandoned from elapsed time alone. Require explicit user authorization before releasing or taking over stale work.

### 6. Open the pull request

1. Run issue-scoped tests and required repository gates before review.
2. Push the issue branch and open or update exactly one pull request containing the `CHO-<number>` identifier.
3. Never push issue work directly to `main`.
4. Move the issue to the correctly spelled `In Review` status only after the remote branch and PR exist.
5. Add one review comment containing the PR, branch and head commit, verification commands and results, traceability changes, and remaining risks.

### 7. Complete the issue

1. Move a leaf issue to `Done` only after its PR is merged, required CI passes, every acceptance criterion is satisfied, and required provenance or traceability updates are present.
2. Add one `## Work completed` comment with the PR, merge commit, verification evidence, and follow-up issue identifiers.
3. Remove only the active runner label while preserving all other labels. Keep execution history in claim, review, completion, and PR records.
4. Do not complete a parent issue, milestone, or release gate until its own acceptance conditions and required children are complete.

## Hand work to another device or agent

1. Require explicit user direction for the handoff.
2. Reuse the existing issue branch and PR. Never create a second PR merely because the runner, device, or worktree changed.
3. Require the current worker to commit and push transferable work. Report uncommitted changes that cannot be handed through Git.
4. Add a `## Work handed off` comment with the old and new runner, old and new worker IDs when known, branch, PR, pushed head, remaining work, and verification state.
5. Replace only the old runner label with the new runner label while preserving all non-runner labels. Create the new team-scoped runner label first if needed.
6. Let the new worker fetch the existing remote branch into a dedicated worktree, add a fresh claim comment, and perform the post-claim re-read before editing.

## Use audit mode

1. Read Linear, local worktrees, branches, remote branches, PRs, and `origin/main` before proposing corrections.
2. Report, at minimum:
   - executable active issues without exactly one consistent runner and active claim;
   - multiple runner labels or concurrent claims on one issue;
   - same-runner claims whose worker ID or branch does not match;
   - parent or exit-gate issues incorrectly carrying runner, branch, or PR state;
   - blocked issues incorrectly marked `In Progress`;
   - `In Review` issues without a remote branch or PR;
   - merged PRs whose issues are not `Done`;
   - local worktrees or branches without matching active claims;
   - active branches based on stale `origin/main`;
   - unpublished local `main` commits;
   - local semantic changes whose apparent issue is still `Backlog` or `Todo`.
3. Keep audit mode read-only unless the user explicitly asks to apply corrections.
4. When corrections are authorized, update only unambiguous state and summarize every Linear, Git, or PR mutation.

## Preserve operational safety

- Read immediately before every Linear write and use exact issue identifiers.
- Never overwrite the full label set with a partial list.
- Never steal, expire, or replace a claim automatically.
- Never modify another worker's branch or worktree without explicit direction.
- Do not push, merge, close, cancel, delete, or reassign work unless the current request authorizes that action.
- Report Linear, Git, worktree, and PR drift rather than hiding it with automatic state changes.
