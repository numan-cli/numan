# `/oc` GitHub PR commands

These instructions apply when a user message is posted on a GitHub PR via the opencode
GitHub Action and begins with `/oc` (or `/opencode`). The message usually carries a
`<pull_request>` context block (title, body, changed files, comments, reviews) — read it
carefully before answering.

## Model routing

The workflow probes and selects the agent model per command (see `.github/workflows/opencode.yml`):

| Command      | Primary model                          | Fallback chain                                                                                |
| ------------ | -------------------------------------- | --------------------------------------------------------------------------------------------- |
| `/oc review` | `cloudflare-workers-ai/@cf/zai-org/glm-5.2` (CF-first) | `opencode/gpt-5.6-luna` (`variant: max`) → `opencode/big-pickle` → `opencode/nemotron-3-ultra-free` → `opencode/nemotron-3.5-lightning-free` → `opencode/deepseek-v4-flash` → `alibaba-token-plan/qwen3.8-max` → `alibaba/qwen3.8-max` → `opencode/nemotron-3-ultra` |
| `/oc fix`    | `cloudflare-workers-ai/@cf/deepseek-ai/deepseek-v4-flash-0731` (CF-first) | `opencode/big-pickle` → `opencode/nemotron-3-ultra-free` → `opencode/nemotron-3.5-lightning-free` → `opencode/deepseek-v4-flash` → `alibaba-token-plan/qwen3.8-max` → `alibaba/qwen3.8-max` → `opencode/nemotron-3-ultra` |

Each model is probed with a minimal request before the run; a disabled or unavailable model
falls through to the next in the chain. The `opencode/*` models are probed through the
opencode.ai `/zen` gateway; the two `alibaba/*` models are probed through their direct
compatible-mode endpoints (`token-plan.ap-southeast-1.maas.aliyuncs.com` and
`dashscope.aliyuncs.com`) behind the `ALIBABA_TOKEN_PLAN_API_KEY` / `DASHSCOPE_API_KEY`
secrets. `/oc review` runs are short and judgment-heavy, so the cost-efficient
`gpt-5.6-luna` runs with `max` reasoning effort to maximize finding quality while keeping
per-run cost in the tens of cents; `/oc fix` runs are long agentic edit loops, where the
free big-pickle keeps cost at $0. Note that `big-pickle` advertises no reasoning-effort
variants, so `variant: max` is only applied when `gpt-5.6-luna` is actually selected — the
probe clears it on any fallback. Review runs send code snippets to an OpenAI-hosted model —
acceptable for public repos; keep in mind OpenAI may retain requests for evaluation
purposes.

## Using context7

The `context7` MCP server (remote, wired via `.opencode/opencode.json`) is available to
look up **current, version-accurate library/framework documentation** — handy because the
agent's training data can go stale. Use its `resolve-library-id` and `query-docs` tools.

**When to use it** (both commands):

- **Authoring or changing API calls** — before writing code that calls a library, framework,
  or SDK, look up the exact current API so you don't invent a wrong signature or import
  (e.g. React 19 hooks, Express routes, Vite config).
- **Resolving an API-relevant review finding** — when `/oc fix` addresses a comment about a
  library usage, confirm the expected current behavior/API from docs rather than guessing.
- **Copy-pasted code that may be outdated** — verify before trusting it.

**When NOT to use it:**

- For pure project-internal logic, the diff, or code you're already certain about — don't
  spend tool calls (and context) re-confirming things you know.
- For general web/non-library lookups — context7 is a documentation index, not a search
  engine; prefer the model's own knowledge for non-API questions.

**How to use it:**

1. `resolve-library-id` (pass the library name from the message, e.g. "Express", "React").
2. `query-docs` with the returned library ID and a focused, single-concept question.
3. Use `use context7` in tool choice; keep queries narrow so results stay small and relevant.

## `/oc review`

A review also runs **automatically when a pull request is first opened** (the workflow's
`pull_request: [opened]` trigger), in addition to on-demand. It does NOT re-run on later
commits to the same PR.

When a user message is exactly `/oc review` or begins with `/oc review`, treat it as a
request to review the current pull request. Extra text after the shortcut, e.g.
`/oc review focus on security`, scopes the review to those concerns. The same posting
behavior below applies whether the review is triggered by `/oc review` or by PR creation.

### Posting behavior

**One comment per actionable finding.** Do NOT write one big review. Instead:

1. Identify the actionable findings. An actionable finding is one where you can point at a
   concrete problem in the code and, when feasible, propose a specific change.
2. Post each actionable finding as its **own resolvable review thread** via the `gh` CLI
   (preinstalled in GitHub Actions; the `GITHUB_TOKEN` env var is available, no login
   needed). Fall back down this ladder until the finding is posted:

   **Hard rule:** one `gh` comment per actionable finding, at the highest resolution
   available. Never put more than one finding in a single comment, and never restate a
   finding's body in the final summary — the summary is only an index of links.

    > **CRITICAL — the comment body must be the finding CONTENT, never a file path.**
    > Do NOT post the literal string `@…/finding.md` (or any `@path` token) as the body.
    > The `@file` shorthand only works when the `gh` CLI itself expands it; opencode's
    > review posting path does not, so an `@path` value leaks the path into the comment.
     > Always ground the comment in the actual finding text.

     The `gh` calls you make run under the GitHub Actions token, so the review threads you
     create appear as `github-actions[bot]`. Your final reply (step 3) is posted separately
     as `opencode-agent[bot]`. That split is the intended design: individual threads as
     `github-actions[bot]`, summary as `opencode-agent[bot]`.

     a. **Inline line comment** (preferred) — pins the finding to a line in the PR diff and
       creates a resolvable thread. Use the PR head SHA (`Head: { Sha: ... }` in the
       `<pull_request>` context) as `commit_id`, plus the file and line the finding is
       about. Use `gh` CLI with the `@` form ONLY when you are directly invoking `gh` in a
       shell (the `@` must immediately follow `=`, with no surrounding quotes/spaces, so gh
       reads the file):

       ```bash
       gh api repos/{owner}/{repo}/pulls/{pr_number}/comments \
         -f body=@finding.md \
         -f path="src/example.ts" \
         -F line=42 \
         -f commit_id="$HEAD_SHA"
       ```

       If you are posting through opencode's built-in review tooling instead, READ the
       `finding.md` file and pass its full contents as the `body` value — never the path.
      **Never** pass `@path` as the body through opencode's built-in tooling.

       For a finding spanning a line range, add `-F start_line=<first line>` (and, for a
       deletion, `-f start_side=LEFT`).

   b. **File-level comment** — if the exact line is unknown, or the line-comment call
      returns a 422, post a **file-level** review comment (`subject_type=file`). This still
      creates a resolvable thread and is the recommended fallback whenever you know the file
      but not the precise line (same `body` rule applies):

       ```bash
       gh api repos/{owner}/{repo}/pulls/{pr_number}/comments \
         -f body=@finding.md \
         -f path="src/example.ts" \
         -f subject_type=file
       ```

   c. **Issue comment** (last resort, non-resolvable) — only if the file is not part of
      the PR diff at all. Post **one issue comment per finding**:

       ```bash
       gh api repos/{owner}/{repo}/issues/{pr_number}/comments -f body=@finding.md
       ```

    Derive `owner`/`repo` from `baseRepository.nameWithOwner` in the `<pull_request>`
    context (split on `/`), `pr_number` from `Number:`, and `HEAD_SHA` from
    `Head: { Sha: ... }`. Writing the finding body to a temp file (`finding.md`) is a useful
    drafting aid, but the posted `body` must be that file's **contents**, not its name. Post
    threads one at a time — this endpoint is secondary-rate-limited if you post too
    fast — and keep a list of the posted comment IDs/URLs and of which findings fell back to
      an issue comment. If a `gh` call fails at every level for a finding, move on to
      the next finding's comment; for any finding you truly cannot post, reference it (not
      its body) in the summary's "Out of diff" section.
 3. **Your final reply text** (what the action posts as the single `opencode-agent[bot]`
   summary comment) must be a **short summary index only — it must NOT contain finding
   bodies**. It is: overall assessment; one line per threaded finding with its file:line,
   severity, and a link to that finding's comment (both endpoint responses include the
   `html_url`); and an **"Out of diff"** section listing only the *links* to any fallback
   issue comments (from step 2c) plus any finding with no diff location (e.g. missing
   tests, missing docs, cross-file concerns), each with severity and the file(s)/line(s) it
   covers. All finding detail lives in the per-finding comments posted in step 2.
 4. Only trivial, non-actionable nits may be grouped — at most one small extra comment — and
   never mixed with actionable findings. Every actionable finding is its own thread.

### Committing behavior — suggestions only

You are reviewing, not editing:

- **Do NOT modify any files and do NOT leave the working tree dirty.** The action auto-commits
  and pushes any uncommitted changes to the PR branch — that is not wanted here.
- Include a **committable suggestion** in each finding comment when it is feasible to write
  one for that specific finding. Wrap the exact replacement in a GitHub `suggestion`
  fenced block so GitHub renders a one-click **Commit suggestion** button right in the
  comment:

  ````
  ```suggestion
  <exact replacement lines — must match the current file content>
  ```
  ````

  Use one contiguous block per finding, matching the existing lines it replaces; GitHub
  applies it to the file on commit. If a finding does not have a cut-and-dried fix — no
  contiguous single-file replacement — say so and describe the change needed instead of
  inventing code.

### Finding comment format

Each finding comment should contain:

1. **Severity** — `high` / `medium` / `low` (or `critical`).
2. **Location** — `file:line` (or a line range).
3. **Problem** — why it is wrong, grounded in the actual code.
4. **Suggested fix** — a GitHub `suggestion` fenced block (see "Committing behavior")
   when the fix is a contiguous replacement, otherwise a description of the change needed.

### Review scope

Look for: correctness bugs, security issues (injection, secret handling, authorization),
performance, maintainability, and test coverage gaps. Ground every finding in the actual diff
and files. Do not invent issues; verify against the code. If there are no actionable findings,
just say so in the summary comment and do not post finding comments.

## `/oc fix`

When a user message is exactly `/oc fix` or begins with `/oc fix`, fix the review feedback on
the current pull request.

### Behavior

0. **If the request mentions CI, tests, checks, build, lint, "failing", "red", or a workflow,
   check the ACTUAL GitHub Actions run — do not guess from the diff or from a local test run.**
   The `<pull_request>` context contains review comments only; it does NOT contain CI results, so
   "no review comments" is NOT "no failures". `gh` is preinstalled and `GITHUB_TOKEN` is set, so
   query the run directly:

   - Derive `owner`/`repo` from `baseRepository.nameWithOwner` (split on `/`), `HEAD_SHA` from
     `Head: { Sha: ... }`, and the PR branch from `Head: { ref }` / `headRefName`.
   - List every check on the head commit and surface the ones that are not green:

     ```bash
     gh api repos/{owner}/{repo}/commits/{HEAD_SHA}/check-runs \
       --jq '.check_runs[] | select(.status!="completed" or .conclusion!="success") |
             "\(.name) status=\(.status) conclusion=\(.conclusion) app=\(.app.slug)"'
     ```

   - Find the failing workflow run(s) and read the failed-step logs:

     ```bash
     gh run list --repo {owner}/{repo} --branch {branch} --limit 5
     gh run view {run_id} --repo {owner}/{repo} --log-failed
     ```

     If `--log-failed` is empty, use `gh run view {run_id} --repo {owner}/{repo} --log` (or
     `gh api repos/{owner}/{repo}/actions/runs/{run_id}/jobs` → failed job → its steps) to locate
     the error.
   - **Do NOT report "there are no failing tests" / "nothing to do" unless the commands above show
     every check green.** Local `pnpm test` can pass while CI still fails (lint, tsc typecheck,
     build, integration, component suites all run in CI and may not run locally). Treat the CI log
     as the source of truth: it gives the exact `file:line` and error message. Reproduce with the
     project script if helpful (`pnpm lint`, `pnpm test`, etc.), fix the real failure, then re-check
     with `gh run view --log-failed` that the check is now green.
   - When the request is purely about CI (not review comments), you may skip the review-thread
     enumeration in step 1 and go straight to fixing the CI failures — but still enumerate ALL
     failing checks, not just one.

1. **Collect ALL review feedback** — do not rely on the `<pull_request>` context alone; it may
   be partial, out of order, or missing threads you'd otherwise need to resolve. You MUST
   actively enumerate and read every source of feedback on the PR before fixing anything:
   - **List every thread** via the GraphQL query in step 3 below (with its `isResolved` state) so
     you know the complete, canonical set of threads before deciding what to act on.
   - **Skip threads already resolved.** A thread whose `isResolved` is true has already been
     dealt with — do not re-read or re-judge its contents; doing so just blows up context. Record
     it in the summary as already handled and move on. (Only revisit a resolved thread if its
     resolution appears wrong, e.g. resolved without an actual fix.)
   - **Read every inline review comment** on the open (unresolved) threads (`<pull_request_reviews>`
     → comments), not just the first one in each thread.
   - **Read every timeline / issue comment** (`<pull_request_comments>`); a real fix request can
     live there even though it is not a resolvable thread.
   - **Read every review body / overall PR review summary** (`<pull_request_reviews>`), including
     comments on the diff that were never grouped into a thread.
   - **Read the pull request body itself** for context.
   Every comment that contains feedback — open inline threads, timeline, review-body — must be
   judged. Do not skip an open comment only because it is not inline-resolvable; it still counts
   as addressed. Resolved threads are the one exception and may be skipped to save context.
2. **For each comment, judge whether it is valid and actionable** against the current code:
   - **Valid and fixable** → implement the fix by editing files in the working tree. The
     GitHub Action auto-commits and pushes any uncommitted changes to the PR branch; you do
     not need to `git commit`/`git push` yourself (though committing yourself is also fine —
     the action detects it and pushes).
   - **Not valid, not fixable, or already handled** → do not change code for it, but it still
     counts as addressed (addressed *as not valid*): reply on the thread with the reason and
     resolve it (step 3).
   - **Not an inline-resolvable thread but still contains real feedback to address** (e.g. a
      timeline comment or a general review-body request) → address it with a commit too when
      the feedback is valid, and record it in the summary.
3. **Resolve addressed review threads.** A review thread (inline review comment chain) is
   resolvable; timeline comments are not. "Addressed" includes threads you **explicitly
   skip**: a comment judged not valid, already handled, or intentionally not applicable is
   still addressed (as not valid) and gets resolved too. For every thread you resolve, reply
   on the thread with the reason first (a fix summary, or the justification for skipping)
   when possible — the thread then keeps its rationale and the author sees it in place. Use
   `gh`:

   ```bash
   # 1. List threads, their resolved state, and the first comment's databaseId
   gh api graphql -f query='query($owner:String!,$repo:String!,$number:Int!){repository(owner:$owner,name:$repo){pullRequest(number:$number){reviewThreads(first:100){nodes{id isResolved comments(first:10){nodes{databaseId}}}}}}}' -F owner=... -F repo=... -F number=...

   # 2. Reply to the thread with the reason before resolving
   gh api graphql -f query='mutation($id:ID!,$body:String!){addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$id,body:$body}){comment{id}}}' -F id=THREAD_ID -f body=REASON
   #    REST equivalent (reply to the first comment in the thread):
   #    gh api repos/{owner}/{repo}/pulls/{pr_number}/comments/{comment_id}/replies -f body=REASON

   # 3. Resolve the thread
   gh api graphql -f query='mutation($id:ID!){resolveReviewThread(input:{threadId:$id}){thread{isResolved}}}' -F id=THREAD_ID
   ```

    Write the reason to a temp file (`thread.md`) and pass `-f body=@thread.md` when it is
    long, so multiline Markdown survives intact. **The `@` form is only valid when you invoke
    the `gh` CLI directly in a shell** (the `@` must immediately follow `=` with no
    surrounding quotes/spaces). If you post through opencode's built-in review tooling, READ
    the file and pass its contents as the `body` — never the literal `@path` string, which
    would leak the path into the reply. Only leave open a thread you genuinely could
    not address — no fix and no justification — and say why in the summary.
4. **Your final reply text IS the single summary comment** (the action posts it). Do NOT post
   extra per-finding comments. The summary must cover **everything**:
   - **Fixed** — for each addressed item: the change made (file:line) and whether its thread
     was resolved.
   - **Not fixed (resolved as not valid)** — for each comment you skipped: a brief reason
     (invalid, already handled, duplicate, out of scope, not fixable) and a note that its
     thread was replied to and resolved.
   - **Addressed non-thread feedback** — any feedback that wasn't an inline thread but still
     warranted a code change: list the change made.
   - A short overall assessment of remaining risk.

### Fixing behavior notes

- Ground every judgment in the actual diff and files. Verify a comment is still valid against
  the current code before acting on it.
- Keep fixes minimal and targeted to the feedback. Do not refactor unrelated code.
- Resolve every thread you addressed — fixed or explicitly skipped (skipping with a reason
  is addressing *as not valid*) — and reply on each thread with the reason when possible.
  Do not resolve a thread you genuinely could not address. Do not modify files for invalid
  or duplicate feedback.
