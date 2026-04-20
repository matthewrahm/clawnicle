# HN Submission Plan

Draft submission materials for posting clawnicle to [news.ycombinator.com](https://news.ycombinator.com). Not yet submitted.

## Prerequisites

1. **Blog post must be published somewhere linkable.** Options:
   - Personal site (fastest): paste `docs/blog/launch.md` into a rendered page. Medium / Substack / a static site hosted on Vercel all work.
   - GitHub: link directly at `github.com/matthewrahm/clawnicle/blob/main/docs/blog/launch.md` as a fallback, but a real domain reads better.
   - Recommended: a short static site at a personal domain. `matthewrahm.dev/clawnicle` or similar.

2. **Repo has to look polished at URL-land-time.** Currently satisfied: green CI badge, GIF above the fold, measured performance table, plain-English hero, "For engineers" section, clean commit history.

3. **Have an HN account with some karma.** First-time accounts get heavily rate-limited. If necessary, comment on a few unrelated threads to warm up the account.

## Title candidates

HN titles have strong patterns. Short, factual, no clickbait, no em dashes, 80 chars max.

Good:

- `Clawnicle: durable execution for LLM agents, in Rust`
- `Show HN: Clawnicle – save and resume for AI agents`
- `Show HN: Temporal's durable-execution model, redesigned for LLM agents`
- `Show HN: Clawnicle, a Rust runtime that journals and replays LLM agents`

The "Show HN" prefix is the right tag for a project launch; it routes to the Show HN subfeed and gets slightly friendlier treatment from moderators.

**Recommended title:** `Show HN: Clawnicle – save and resume for AI agents (Rust)`

The "(Rust)" suffix is idiomatic on HN and filters out readers looking for Python. Keeps the title under 60 chars.

## Link

Point the submission at the blog post, NOT the GitHub repo. Rationale:

- HN rewards posts that explain the *thinking* behind a launch. The blog post is that artifact; the repo is the reference.
- A blog URL encourages readers to scroll the whole argument before hitting "Discuss" or clicking through to GitHub.
- The repo URL shows up in the comments anyway, once people ask where the code is.

## Self-submitted first comment

Post immediately after submission. HN convention is for the author to leave one comment giving context that doesn't belong in the title or post. Something like:

> Hi HN. I'm Matthew. I've been running an AI agent on a Mac Mini for about a year and lost count of how many times I watched it restart from step 0 after a crash, re-paying for LLM calls I had already paid for. Clawnicle is the library I built to stop that.
>
> The thesis is in the post. The short version: Temporal solved this for services a decade ago; Rust + SQLite WAL + an async-fn-as-workflow shape feels right for LLM agents. v0 ships the journal, replay engine, retry / budget / cancel, LLM prompt caching, a scheduler with crash recovery, sub-workflows, and criterion benchmarks (journal append at ~21 µs, cached replay short-circuit at ~3.7 µs).
>
> I'd especially welcome feedback on: the idempotency-key design for tool calls, the determinism contract (opt-in vs enforced), and workloads where the per-session budget model breaks down. Repo is at https://github.com/matthewrahm/clawnicle; Apache-2.0.

Keep it under ~200 words. One concrete ask for feedback. No emojis, no marketing language.

## Timing

HN has known traffic patterns. US weekdays at 9–11 am ET (~13:00 UTC) is the sweet spot for technical projects: the East Coast is reading coffee-adjacent HN, and the West Coast is logging in. Avoid:

- Fridays after noon (traffic drops into the weekend)
- Monday mornings (front page still has weekend carryover)
- Holiday weeks (US Thanksgiving, Christmas-New Year window)

For a post this technical, **Tuesday or Wednesday, 9:00–10:00 ET** is the recommended slot.

## Ancillary posts

Cross-post to:

- **Lobste.rs** — more technical, smaller but higher-signal crowd. Rust tag + `programming`.
- **r/rust** — hit-or-miss but can drive repo stars. Title: "[Media] Clawnicle: durable execution for LLM agents" with the GIF embedded.
- **rust-users discussions** (users.rust-lang.org) — good for getting the crate name known in the ecosystem.
- **Twitter / Bluesky** if you use either. Pin the GIF.

Do NOT cross-post on the same day as HN. Space by at least 24 hours so HN can front-page without the other posts splitting attention.

## What NOT to do

- No self-upvoting from alt accounts. HN actively detects it and will dead-post the submission.
- No "please upvote" asks. Biggest sin on HN.
- No em-dash pair construction anywhere. (Consistent with the project's style rule, and reads AI-generated on HN specifically.)
- Don't link directly at the repo. Link at the post. People will find the repo through the post.

## Post-submission checklist

Within the first hour:

1. Reply to early comments quickly and substantively. First-hour engagement drives ranking.
2. If someone points out a real bug, fix and push within the thread's lifetime. Being visibly responsive to feedback reads well.
3. Add any FAQ-type answers back into the README or blog post so future readers find them.
4. Do NOT argue with aggressive commenters. Disengage. The thread is for readers, not the one loud critic.

Within 24 hours:

1. Note GitHub stars / issues / discussions created.
2. Triage any PRs. A quick merge of a typo fix is a positive signal.
3. If the post front-pages, expect 20–50 GitHub stars per 1 000 HN reads. Update the memory file with final numbers.
