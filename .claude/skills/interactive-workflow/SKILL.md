---
name: interactive-workflow
description: Use this skill when running a large, mixed set of efforts — investigations + decided fixes + verifications — toward a goal (e.g. a launch push, a pre-release audit, a refactor campaign) where the human wants to stay in the loop on what the investigations surface. The Interactive Workflow is the default for any multi-effort push that is part "find out what's true" and part "land the decided thing." Use it instead of hardcoding a multi-agent DAG up front — those bury the decision points and fan out expensively before the facts are in.
version: 1.0.0
---

# Interactive Workflow

The Interactive Workflow is an orchestrator-first methodology for running a large, mixed set of efforts (investigations + decided fixes + verifications) via individually-dispatched sub-agents. The human stays in the loop on decisions the investigations surface. It is the default for any multi-effort push where the work is part *find-out-what's-true* and part *land-the-decided-thing*.

Reference implementation for this repo: the AGENTS.md section "## The Interactive Workflow" keeps the operational essentials inline and points here for the full methodology.

---

## Core shape

**You (the main session) are the orchestrator, and the ONLY decider.** You hold the whole picture, dispatch sub-agents as *instruments*, ingest what they return, and decide the next move. You do not hand the steering wheel to a workflow script or a sub-agent.

**A living tracker document is your control surface.** When you START a workflow, pick a fitting name for it (a generic `TRACKER.md` / `STATUS.md`, or whatever suits the effort) — don't inherit a stale name out of habit. It is ONE self-contained markdown file that holds EVERYTHING below; never split it across two documents:

- A **status board**: one row per effort with a status cell. Vocab: `todo · probing · needs-decision · ready · landing · verifying · done · blocked`.
- A **detail card** per effort: what it covers, what the sub-agent found, what decision was made or is pending.
- A **Questions-awaiting-human queue** — a SECTION of this same doc (not a separate file): everything the investigations surfaced that only the human can answer.
- A reusable **codebase map**: file paths, key symbols, module boundaries — embed the relevant slice into every sub-agent prompt so agents don't re-derive it.

The tracker is extensible by design: new efforts get a new ID and a new row. You update it after EVERY sub-agent returns — fold its facts into the card, advance the status, re-derive "what's next" from the board.

**Make the tracker cheap to read back — status boxes + the `todo` skill.** Prefix each effort/action line with a machine-parseable status box: `[ ]` not-started · `[/]` in-progress · `[x]` done · `[-]` cancelled/dropped · `[>]` deferred/forwarded · `[?]` question/blocked-on-answer. (The finer prose vocab above — `probing`/`needs-decision`/`landing`/… — can annotate the line as nuance; the box is the coarse marker a tool can filter.) `[?]` doubles as the Questions-awaiting-human marker — a `todo <tracker> --question` pulls exactly the open decisions to re-surface. Then you pull just the open items with `node scripts/todo/index.mjs <tracker> --not-done` (or `--counts` for a tally, `--section <name>` to scope) instead of re-loading the whole doc — see `.claude/skills/todo/SKILL.md`. Run it as a **queue**: new items go near the TOP, finished ones stay checked in place; when the file bloats, sweep long-`[x]` items into an `## Archive` fold at the bottom. Pairs with `md-toc` (heading→line-range TOC) for a large tracker: `md-toc` to find the section, `todo` to pull its open lines, then `Read` the exact range. After a context reset, `todo --not-done` on the tracker is the fastest way to reconstruct "what's left."

**Capture every follow-up the INSTANT it is discussed or decided — never let it live only in chat (the maintainer, 2026-06-12, emphatic).** A working conversation continuously throws off follow-ups: a decision ("we'll keep this as a nub-local patch"), a rename ("call it `Embedder`, not `Identity`"), a deferred correction ("merge those fields on review"), a sequenced "do X once agent Y returns," an open question to route back to the human. Each one goes into the tracker **as it arises**, not in a retroactive sweep at the end — because chat scrolls away and context resets, and a follow-up that lives only in the conversation **will be dropped.** Maintain an explicit, ordered "Next actions / on-return checklist" in the tracker and append to it the moment you say the words. The tell that you are failing at this: the human has to prompt "make sure you remember everything you need to do once the sub-agent finishes" — by then the follow-ups have already been accumulating in chat instead of on the board. Treat the tracker, not your working memory, as the source of truth for *what's left to do*, exactly as it is for *what's been done*.

**Sub-agents are instruments, not deciders.** A probe returns *facts* — divergences, traces, measurements, file paths, exact error messages — not verdicts. No sub-agent autonomously lands a change to a default / security posture / product behavior / API-config-env surface / error-contract; those route back to the human as a question. Mechanical / clearly-a-bug fixes may land (you review the diff). Every sub-agent prompt is **self-contained**: embed the codebase map slice, the exact task, any relevant context — a model switch starts a fresh cache, so nothing carries over from the orchestrator's context.

**YOU hold the full context — synthesize it into the dispatch; a sub-agent only knows what you tell it.** A sub-agent starts with a fresh cache and zero awareness of everything that's happened across the effort — every superseded number, reversed decision, renamed thing, or newer finding. If you hand it a task without folding in the CURRENT state, it will faithfully act on stale or wrong assumptions and produce confidently-wrong output (a homepage line with a benchmark number that three investigations ago became obsolete). Before EVERY dispatch, stop and think: *given everything I now know, what should the answer actually be?* — then encode that synthesized direction in the prompt. This is not optional relaying; it is the orchestrator's central cognitive job. Especially: catch STALE/SUPERSEDED info (a number that a later benchmark overturned, a claim a later probe disproved) and a tempting-but-UNVERIFIED claim (a figure from a non-neutral harness or special config) — resolve which is true *yourself* before the sub-agent bakes the wrong one into a user-facing artifact. (the maintainer, 2026-06-11: "you're the only one with full context… use your brain power to figure out exactly what needs to be done by the sub-agent.")

**Be proactive on obvious bugs — fix, don't surface-and-wait.** When an investigation/review turns up an obvious, clear-cut bug (a correctness failure, a false claim, a broken contract), just dispatch the fix and report it done — do NOT surface it and wait for the human to say "fix it." The line is bug-vs-decision, not big-vs-small: a hairy-but-clear correctness fix gets fixed; a one-line *posture/default* change still routes to the human. Asking permission to fix an obvious bug wastes the human's attention and stalls the work. (the maintainer, 2026-06-11: "be more proactive when you find obvious known bugs like this.")

**Acknowledge strategy/approach-impacting input explicitly — don't silently fold it (the maintainer, 2026-06-13).** When the human says something that shifts the strategy, approach, or a decision (a reframing, a new constraint, an insight that changes what's worth doing, a correction to your model), your response must LEAD with: (1) an explicit acknowledgment that names what they said and confirms you heard it, (2) how it changes the plan, and (3) what you're now doing about it. Do NOT just quietly update the tracker and proceed with tool calls — the human can't see the tracker mid-turn and needs to see, in chat, that the point landed and its consequence. This is distinct from low-stakes acks: it's specifically for input that moves the strategy. The tell you're failing: the human has to ask "did you hear what I said about X?" or "acknowledge when I say things that impact strategy." Pair this with explaining what you're doing as you do it (narrate the load-bearing moves), not just emitting commits. (Burned 2026-06-13: ran heavy action — commits, pushes, dispatches — with terse acks while the maintainer made several strategy-shifting points, until he had to ask to be acknowledged.)

**CRITICAL: only the orchestrator edits the tracker/handoff doc.** Sub-agents must never directly modify it — they return results to you, and you update the doc. Parallel agents writing the same control surface clobber each other's updates, which is how efforts get lost. The tracker is yours; sub-agents speak to you.

---

## Model-tier every dispatch by JUDGMENT REQUIRED

The cost ladder cheapest → priciest applies to every dispatch. Tier by how much the sub-agent must self-steer:

- **Haiku** — fully-scripted mechanical only: run THESE exact commands, harvest THIS output, trace THIS path, where every decision is pre-made by you. Haiku is cheap but CANNOT self-steer. Do not send Haiku to "investigate" something open-ended and expect a reliable verdict. Give it a script, not a question. Caveats: 200K context ceiling; no `effort`/adaptive-thinking param (passing them errors).
- **Sonnet** — probes where the finding is an OBSERVABLE fact (run X and Y, diff the output — the divergence *is* the result), plus the supporting cast: test scaffolding, doc updates, CI-watching, gates/settle, mechanical-but-not-trivial edits. Sonnet CAN self-steer — but its failure mode is **confident-but-wrong on subtle reasoning**. Do NOT hand it a probe whose deliverable is a *judgment* about subtle correctness or security in complex code ("is this a real bug?", "is this exploitable?", "does this edge case break?") — it returns crisp, plausible, WRONG verdicts. Effort `medium` default.
- **Opus** — the fix that lands; diagnosis; architecture / adversarial review; gnarly debugging; **and any probe whose deliverable is a load-bearing VERDICT requiring subtle reasoning** (is-this-a-real-bug / is-this-exploitable in complex code) where a confident-wrong answer is costly. ~1.67× Sonnet premium, but code + correctness judgments are low-volume vs grunt work — the quality bar lives here. Effort `xhigh` for coding/agentic tasks.
- **Fable** — reserve for the very hardest synthesis or judgment calls. Priciest; use sparingly.

Pattern: *cheap tier gathers & packages → Opus does the real engineering → Sonnet handles the supporting cast → you verify.*

**Re-verify cheap-tier load-bearing claims yourself.** A Haiku "this is a security bug" or "these two diverge" is a *lead, not a fact*; confirm it against code or a foreground experiment before acting on it. Cheap-tier verdicts have mislabeled headless-TTY limits as "permissions," produced code-review instead of empirical results, and found real bugs whose verdicts still needed re-confirmation. Trust the data they harvest; validate the conclusions.

---

## Targeted tests, not the whole suite — and the orchestrator says WHICH

A sub-agent told to "make sure nothing broke" will run *everything* — the full `cargo test --workspace`, every bats harness, a from-scratch rebuild — and burn 20+ minutes on what should be a self-contained task. "Thorough" does NOT mean "run literally every test." It means run the RIGHT tests: the modules the change touched + the load-bearing contracts at risk. The orchestrator holds the context for what's at risk, so the orchestrator must SPECIFY the targeted set in the dispatch — never leave "run the tests" open-ended.

- **Scope to what changed.** `cargo test -p <touched-crate> <module>` for the crate/module edited — not `--workspace`. Reserve the full suite for a genuine final gate, or when a targeted run surfaces something that warrants widening.
- **Incremental builds, always.** Never blow away `target/` for a verification; an incremental rebuild after a merge is minutes, a from-scratch one is much worse.
- **Slow harnesses only when their contract is touched.** Real-PM conformance/conversion harnesses (which spin up actual npm/pnpm/bun installs) are minutes each — run them only when the change touches the install/lockfile contract they guard, not reflexively.
- **Benchmarks: avoid concurrent HEAVY COMPUTE, not agents.** What contaminates an install/perf benchmark is concurrent *heavy compute* — a `cargo build`, another install, a docker image pull, another benchmark — NOT the mere existence of other sub-agents. A sub-agent is just an LLM making tool calls; its own footprint is negligible (the maintainer, 2026-06-11: "an additional sub-agent does not materially tax the machine"). So do NOT drain the whole machine for a benchmark — keep dispatching light/read-only agents. Only serialize the benchmark against *build/install-heavy* jobs. And note: running a build *during* a benchmark's measurement usually forces a re-run, which DELAYS the number — so holding a build the couple minutes until the benchmark finishes is faster, not slower. (Burned 2026-06-11: ran a benchmark during a 20-min sync build; numbers were meaningless.) **But do NOT over-rotate the other way and treat a benchmark as precious** — `hyperfine` auto-runs the minimum samples to reach statistical significance, it's quick and not complicated. Don't agonize over run counts, don't specify "≥20 runs," don't drain the whole fleet for a "perfectly quiet machine." Just run it. (the maintainer, 2026-06-12: "just benchmark with hyperfine… it does the minimum number of runs to reach statistical significance… it should not take very  long.")
- **Calibrate per change.** A 5-line flag gate needs its own module's test + a quick behavior check, not the universe. A lockfile-format change needs the conformance harness. Match the test scope to the blast radius — that judgment is the orchestrator's, encoded in the prompt.

## Match the depth to the task's spirit

A dispatch's model AND its scope/structure must match what the task actually demands. Open-ended, effort-heavy work — "audit the correctness of X", "is the compat byte-for-byte", "is this surface rock-solid" — is NOT one cheap sub-agent pass returning a tidy report in three minutes. That is checkbox-completion, and it produces false "done"s. Read the *spirit* of the ask: a correctness audit means a sustained **differential-fixture campaign** (many fixtures, each diffed against the reference tool, judged by a strong model, adversarially re-verified); a compat claim means actually *running the reference corpus*, not spot-checking a handful; "rock solid" means exercising *every* form empirically, not inferring from source. When the task is broad and consequential, **fan out** (one agent per fixture/subsystem/form) and **loop until dry** — don't collapse it into a single pass to save spend. The cheap single-pass is the wrong economy precisely on the work that least tolerates it.

## Calibrate from mistakes

Every wrong or low-quality sub-agent result is a signal that the dispatch was mis-tiered, under-scoped, or under-specified — not a one-off to fix and forget. When it happens, UPDATE this skill (and the repo's tiering guidance): record the failure mode and the calibration so it doesn't recur. A confidently-wrong verdict from a cheap tier means *route that class up a tier* and/or tighten the prompt. A "done" that was actually thin means *the scope didn't match the spirit* — re-open it and fan out. The methodology compounds by absorbing each miss. (Calibrated 2026-06-12: in autonomous mode I under-parallelized — sat with only 1-2 agents running and held landing work behind a "build lane" out of clobbering-fear, and once ran a sub-agent in the FOREGROUND. the maintainer: "parallelize way better… always background… Go ham." The fix is already written into the always-background and parallelize-aggressively sections above — the miss was not APPLYING them. When you notice the fleet is thin and items remain, that is the signal to fan out NOW, all backgrounded, not to do the next thing hands-on in the main thread.)

## Label every dispatch for the human — and NEVER refer to it by a code in chat

When you launch or refer to a sub-agent, lead with a **sentence-case description of what it's doing** — e.g. "Differential-test the resolver against pnpm". Do NOT surface the agent *type* ("general-purpose") or the opaque runtime task ID as the identifier — those mean nothing to the human.

**The internal tracker code (N3, CP-6, AC1, M11…) is bookkeeping for YOUR control surface — it is meaningless to the human and must NEVER be the identifier in a chat message or status update.** The human experiences the work as "the thing that does X," not "N3." Always name the agent/effort by what it is *doing* in plain words ("the investigation into store verification", "the npm-lockfile-reader fix"); keep the code in the tracker/ledger, or at most in parentheses at the very end as a cross-reference. Leaking codes into status reports makes them unreadable — a repeated, called-out mistake (the maintainer, 2026-06-11). See the user memory `refer-to-efforts-by-description`.

## Delegate ALL project work — even one-off tasks — to sub-agents

In interactive-workflow mode the orchestrator does NOT do project work hands-on in the main thread, no matter how small. A one-line CSS fix, a quick doc tweak, a tiny rename — all of it goes to a backgrounded sub-agent. The orchestrator's own actions are limited to **orchestration-intrinsic** ones: reading/assessing (read-only), dispatching, reviewing returns, and editing its OWN control surfaces (the tracker/ledger, this skill, memory) + the git commits that finalize reviewed work.

**NEVER babysit foreground shells or trivial polling in the main thread.** Sitting there running `pgrep`/`docker ps`/quiet-checks in a loop, re-reading output files, fiddling to "set up" for a task — that is NOT progress, it burns the turn, and it leaves the machine empty of sub-agents. If a task needs a precondition (a quiet machine, a built binary, a cleaned dir), DELEGATE the whole thing — precondition-check + work — to ONE sub-agent and move on to dispatching the next. The moment you notice yourself hand-running shells in the main thread instead of fanning out agents, STOP and dispatch. (the maintainer, 2026-06-11: "never sit and babysit random foreground shells.") The reasoning: hands-on work in the main thread blocks the orchestration loop, can be orphaned by a human interjection, and erodes the orchestrator's whole-picture role. If you catch yourself about to Edit a project file to "just quickly fix" something, stop and dispatch it instead. (the maintainer, 2026-06-11 — "these one-off tasks should always be spun up in a sub-agent because we're in interactive workflow mode.")

## Always background sub-agents

In interactive-workflow mode, EVERY sub-agent runs backgrounded (`run_in_background`) — never in the foreground. A foreground sub-agent blocks the orchestrator's turn, so a message the human force-pushes interrupts it mid-flight and **orphans its work**. Backgrounded, the agent keeps running independently and the human can always reach you. Corollary: keep the orchestrator's OWN foreground actions short (quick reads/checks); never run a long or stateful operation (an agent, a build, an edit-then-commit) in the foreground where an interjection would orphan it.

## Resume agents with context; design yield points

Resuming an existing sub-agent keeps its accumulated context so it doesn't re-derive what it already did — but whether it's actually CHEAPER than a fresh agent depends on the prompt cache, which is TTL'd: ~5 minutes, server-side, content-keyed, and **not** cleared when the agent terminates (termination is a local event; the cached prefix stays warm server-side for the TTL). So choose by turnaround:

- **Quick turnaround (resume within ~5 min) → resume.** Cache is warm; strictly cheaper + faster, full context preserved.
- **Long human-gated pause + the context distills cleanly → summarize into a FRESH agent.** Past the TTL the cache is cold, so a resume re-sends the agent's whole raw transcript *uncached* — often costlier than a fresh agent seeded with a tight summary you write (and the summary is usually cleaner signal than raw tool-call history).
- **Long pause but the raw detail IS the value (can't summarize faithfully) → resume anyway** and eat the cold-cache cost.

Design sub-agent tasks with deliberate **yield points**: when an agent reaches a decision the human owns — a design fork, a posture call, an ambiguity it shouldn't guess — have it STOP and hand its findings back rather than guess or block. You surface the decision (via the question channel), get the answer, then resume-or-reseed per the above. (Where no resume mechanism exists, reseeding a fresh agent from the persisted artifact — the tracker, the doc it already wrote — is the fallback, and is often the right call anyway for a long pause.)

## Background sub-agents are HEADLESS — no TTY, non-interactive

Background sub-agents cannot drive a REPL, pipe into an interactive process, answer a permission prompt, or run anything that blocks on a terminal. Permission mode IS inherited (on bypass they have full perms), so a "blocked by permissions" report from a background agent usually means it hit an *interactive/TTY* surface, not a perms wall. Any probe of interactive surfaces (REPL, piped stdin, `--inspect`, interactive prompts) must be run by the orchestrator in the **FOREGROUND** where a TTY exists.

---

## Parallelize aggressively — clobbering is NOT the worry it seems

Read-only probes parallelize without limit. **Landing agents parallelize too — don't over-serialize on fear of clobbering.** The file tools prevent the actual dangerous failure: Edit/Write enforce read-before-write and REJECT a stale edit (a write to a file changed since you last read it fails, forcing a re-read), and writes are atomic — so two agents editing the same tree can NOT silently clobber each other, and a concurrent `cargo build` reads a consistent (old-or-new) file, never a half-written one. (the maintainer, 2026-06-11: "don't worry about clobbering — the file tools require the agent to read back the current value if it's writing to something that's changed.")

The only residual risk is a **rare cross-file build inconsistency** — agent B's build catches the tree between two of agent A's related multi-file edits — and that just produces a spurious compile error B re-runs past. That cost (an occasional re-run) is far smaller than the cost of serializing all landing work, so **default to parallel.** Reserve serialization for the one case where it genuinely matters: when an agent's DELIVERABLE is a *trustworthy build/test result* on a tree another agent is concurrently mutating (e.g. a delicate merge + its verification suite, or a benchmark) — there, give that agent exclusive use of the tree so its green/red is real. Otherwise, fan out.

Git hygiene across parallel agents: do all work on `main`, no branches or worktrees (per this repo's top rule). Cross-staged commits across parallel agents are fine; committed work cannot be clobbered.

**The two REAL constraints on concurrent codebase work (corrected 2026-06-12 — the old "never two agents editing the same tree" was too strong).** Concurrent EDITING is fine — the file tools prevent silent clobbering. What actually bites:

1. **Raw git ops bypass the file-tool safety.** The "Edit/Write reject stale edits" protection only guards the *file tools*. An agent that runs `git reset`/`git checkout`/`git stash` via Bash on a shared tree bypasses it and WILL wipe a sibling's uncommitted edits — this happened (two agents on the same `vendor/aube` submodule; one ran `git reset` twice and erased the other's WIP). So in any parallel-agent prompt, **forbid destructive git ops** (no `reset`/`checkout`/`stash`; stage only own files; commit fast).
2. **A trustworthy build/test result needs an uncontaminated tree.** When an agent's DELIVERABLE is "my `vendor/aube` patch passes tests + preserves upstream behavior," a sibling's concurrent edits in the same tree contaminate that result. The fix is NOT to serialize — it's to **isolate**: give each fork-patch agent its own `git clone --depth 1 file://…/vendor/aube` + its own `CARGO_TARGET_DIR`, have it produce a tested `git format-patch` patch, and **serialize only the assembly** (you apply the isolated patches onto `nub-fork` one by one, bump the pin once, integrated-test once). That is HOW you fan out fork-patch landing work — several Opus agents in parallel clones, the orchestrator assembling serially — not a reason to run them one at a time. Two agents editing strictly disjoint files in the SAME tree (and not committing) are also fine; isolation is the clean default when each needs its own green/red.

## Parallelization budget — pace by cost, because you CANNOT read the quota

**There is NO supported way for an agent to read the current Max-plan quota** (the rolling 5-hour balance or weekly limit). Verified 2026-06-11: no env var, no `claude` subcommand, no state/cache file, no hook payload carries it; the `/usage` slash command shows it in the human's TUI but is UI-only and not reachable programmatically. The API's `anthropic-ratelimit-*` headers are per-request rate-limit state (not subscription quota) and Claude Code agents have no direct API key anyway. So "throttle by remaining quota %" is impossible — pace by these PROXIES instead:

- **Tier-weighted concurrency cap, not a flat agent count.** Opus/Fable agents are HEAVY, Sonnet MEDIUM, Haiku LIGHT. Cap concurrent *heavy* agents (≈3 Opus at once is a sane default) while cheap read-only Sonnet/Haiku probes fan wider. A round of "4 Opus investigations at once" is the thing to break up — stagger them, or run the cheap probes first and let the heavy ones trail.
- **React to the only ground-truth signal: 429 / rate-limit errors.** On the first one, halve concurrency, serialize, back off. That's the sole real quota feedback an agent gets.
- **The human's `/usage` readout is an EXPLICIT input — treat it as authoritative.** They can see the number; you can't. When they say "we're about to hit quota, hold off" or "go wide," that overrides your heuristic immediately.
- **On a "hold off / about to hit quota" signal: STOP launching, but do NOT kill in-flight agents.** Killing a running agent mid-edit orphans its WIP (the cardinal sin). Let already-spawned agents reach their commit; just launch nothing new. Meanwhile, cheaply PERSIST state to the tracker (your own edits are negligible vs a sub-agent fan-out) so a quota wall or context reset loses nothing — the board, the just-returned findings, the held-but-not-launched queue. Resume the held queue when the human says quota reset.
- **Persist-before-the-wall.** When quota is near, the highest-value use of your remaining budget is folding returned results into the tracker and recording what's queued — not squeezing in one more dispatch.

---

## The question channel — accumulate, then batch-ask

Investigations surface decisions only the human can make. Handle them with a queue + batched asks, never a per-question stall:

1. **Accumulate in the tracker's question queue (the scratchpad).** The moment an investigation surfaces a decision, append it to the tracker's "Questions awaiting &lt;human&gt;" queue — with enough context to decide (what was found, the options, your recommendation). This is the persistent record; the human can glance at it anytime.
2. **Never block per-question.** Keep dispatching everything that does NOT depend on a pending answer — all read-only probes, plus any fix that doesn't touch the open decision. Questions stack in the queue while agents keep churning.
3. **Batch-ask at a checkpoint** — a phase boundary, or the moment a question becomes genuinely blocking. Use the official ask-the-human tool (e.g. `AskUserQuestion`), which is persistent-until-answered and takes several questions at once (up to 4). Enter the block *deliberately*, with a full fleet of agents already running so they keep working while the human decides.
4. **On the answer: sweep + re-dispatch.** Ingest every sub-agent that completed during the block, fold it all into the tracker, then fire the next round driven by the answers.

This stacks questions (in the queue, not via blocking), asks in batches (fewer interruptions than one-at-a-time), and keeps the block cheap (agents don't stop). If a question goes unanswered, assume it was *missed, not declined* and re-surface it at the next checkpoint.

**Mechanisms that do NOT work (don't re-explore):** background sub-agent tasks have no interactive stdin — you cannot have the human type an answer into a running background shell (`read` hits EOF immediately). An open-an-editor-and-watch-for-save channel is technically possible but not worth the weirdness. The official ask-tool + the question-queue doc is the standard — verified empirically 2026-06-11.

---

## Reconcile EVERY returning agent — a dropped thread is the cardinal failure

The orchestrator is the only thing keeping the workflow coherent. **A sub-agent that returns and is never ingested is the single worst failure mode** — its findings, its WIP, its surfaced question all vanish, and the board silently lies about the state of the work. This happens when a completion notification arrives while you're deep in a conversational thread (a design discussion, a question from the human) and you let it scroll past without folding it in. Do not.

Discipline:

- **Maintain a dispatch ledger in the tracker** — every agent you launch gets a row: ID, effort, status (`running`/`done`/`folded`), and whether its result has been ingested. A returning agent is not "handled" until its facts are in the card, its status advanced, and any question it raised is in the queue. *Folded*, not just *finished*.
- **Reconcile the ledger at the TOP of every turn**, before anything else — including before answering the human. A returning agent outranks any conversational thread: ingest it first, then continue the discussion. The human's question will still be there; the agent's result won't re-announce itself.
- **Verify agent status with the task-status tool, NOT file mtime.** A blocked-on-child agent (one that spawned a nested agent and is awaiting it) does not write its own output, so its file looks stale even though it is still running — mtime will tell you "done ~20 min ago" when it is actually mid-flight. Use the structured task-output/status check (`block:false`) to get the true `running`/`completed` state. (Calibrated 2026-06-11: an mtime audit wrongly flagged a still-running Docker probe as a dropped thread.)
- **Never Read a local-agent's `.output` file directly** — it is the full JSONL transcript and overflows your context. Use the task-status tool for status, and rely on the completion notification (or the agent's returned final message) for the result.
- **If a completion was missed, recover it:** the agent's final message is the deliverable; retrieve it via the task tool and fold it. Do not re-dispatch a duplicate — that burns quota and races the original.

## Autonomous mode — when the human steps away

The human can flip the workflow into **autonomous mode** (e.g. "I'm leaving for several hours, switch to non-interactive — keep making progress without me"). This is a distinct operating mode with its own rules. **Record the flag durably in the tracker** (a prominent "AUTONOMOUS MODE: ON" banner) so it survives context compaction, and check it before any human-facing question.

What changes when autonomous mode is ON:

- **Never ask the human a question.** Before EVERY would-be `AskUserQuestion`, check the autonomous flag. If on, you do NOT ask — a blocking modal would stall ALL progress until they return (hours). The question tool is effectively disabled.
- **Bias HARD toward action.** The default is to *resolve and proceed*, not to pause. Keep the machine busy: always have the next thing dispatched; run the queue as a continuous series; on running dry, do a completeness pass and generate more work.
- **Resolve decisions yourself or via a strong sub-agent.** For a decision that would normally route to the human: reason it out, or spin up a smart (Opus/Fable) sub-agent to reach a well-justified conclusion — *then make the call.* Prefer any recommendation already on record. **Document every autonomous decision + its rationale** in a tracker log so the human can review/override on return. (The standing "don't autonomously land a default/security/product decision" rule is *relaxed by the human's explicit delegation* — but you still document, and you still don't make truly irreversible high-stakes brand/public-API guesses; see next.)
- **Only a TRUE blocker gets parked — and "reversible draft" is NEVER a true blocker.** If — even after a smart sub-agent's analysis — a decision is genuinely irreversible/high-stakes (a public-API/brand/major-product call a wrong guess would *damage*, a destructive/external/published action) and you're genuinely uncomfortable, add it to a "Pending for human" list and MOVE ON. Parking is the rare exception; resolving is the default. **The trap to avoid (burned 2026-06-11): parking *reversible draft work* because it touches the human's "domain" (their homepage positioning, their launch-post voice, a UX-voice nit).** A localhost draft, a doc rewrite, marketing copy not yet published, a small voice choice — these are all REVERSIBLE and reviewable. Autonomous mode explicitly delegates them: DRAFT the honest version with the data you have, document your reasoning, and let the human adjust on review. Handing them back as open questions when you could hand back a done draft is the standing "don't touch their positioning" rule mis-fired — autonomous mode RELAXES that rule for reversible work. When in doubt: would a wrong call here *damage* something (published, external, destructive), or just need an edit? If just an edit → resolve it, don't park it.
- **All other hard rules still hold:** flag-review every vendored-fork change before it counts as landed; targeted tests not the whole suite; benchmarks on a quiet machine; small frequent commits; the dispatch ledger and reconcile-first discipline.

When the human re-enables interactive mode, flip the flag off and resume surfacing decisions/questions normally — starting by walking them through the "decisions made autonomously" log and any parked blockers.

## Dynamic, not pre-planned

There is no committed step list to march through. Each turn:

1. Ingest the sub-agent returns that came in.
2. Fold facts into the detail cards; advance statuses.
3. Surface new questions to the human.
4. Dispatch the next round, honoring deps + the build-correctness constraint.
5. Update the board.

The human can re-prioritize or stop after any round — the tracker means nothing is lost and any re-invocation can read "what's next" and continue.

---

## Never kill mid-edit

NEVER stop a sub-agent while it may be mid-file-edit. Killing an agent mid-edit orphans uncommitted WIP. Only stop at a safe point: the active agent has committed, or you are between phases. To shed load, prefer letting the running agent reach its commit over killing it. If you genuinely must stop mid-flight, the FIRST thing you do afterward is `git status` and commit any buildable WIP — so nothing is lost.

---

## Every change inside a vendored fork must be flagged or justified as a latent-bug fix

When the work touches a vendored upstream (here: the `vendor/aube` submodule, which upstreams to `jdx/aube` as one eventual mega-PR), an ABSOLUTE rule governs every behavioral change: it must be a **default-preserving toggle** — the default path reproduces upstream behavior byte-for-byte, and the embedder (nub) opts into the new behavior by flipping a flag (the `set_*` / OnceLock embedder-seam pattern, with a `default == upstream` invariant tested per seam). The ONLY exception is a **latent-bug fix upstream would accept unconditionally** — completing something incomplete/wrong that changes no *intended* upstream behavior (and that classification must be justified, not assumed).

Operationalize it on EVERY fork-touching dispatch:

- **The dispatch prompt must state the rule** and require the agent to classify its change: *behavior change → default-preserving flag* vs *latent-bug fix upstream wants unconditionally → justify why*. Never let a fork agent silently land an unflagged behavior/default/posture/output change — instruct it to STOP and report if the change isn't a clean latent-bug fix.
- **The orchestrator reviews EVERY fork diff for flag-compliance before treating it as landed.** A self-testing fork agent auto-commits to the fork branch + bumps the pin, so the change lands before you see it — its ledger entry is NOT "folded" until you've read the diff and confirmed it's either flagged (default==upstream) or a justified latent-bug fix. If it's an unflagged behavior change, reshape it behind a flag before it stays.
- This is a load-bearing, never-forget rule (the maintainer, 2026-06-11): the whole fork must stay upstreamable as one PR, so a single unflagged behavior change is debt against that. See AGENTS.md ("every vendor/aube change must be conceivably upstreamable").

## Get sign-off before launching an OPINIONATED sub-agent

Classify every dispatch before you launch it:

- **Broad investigation** (profile, explore, diagnose, gather — commits to no design, lands nothing consequential): safe to launch autonomously.
- **Opinionated task / fix / impl** (prescribes a specific solution, or lands changes that touch a default, security posture, product behavior, API/config/env surface, architecture): requires the user's guidance FIRST. Flesh the design out together; be explicit about whether the work is investigation-scope or opinionated-implementation-scope before any agent is told to build it.

A sub-agent prompt must NEVER empower a sub-agent to autonomously land a change to a default / security posture / product decision the user owns — recommend-only until the user signs off.

---

## The win over hardcoded workflows

Hardcoded multi-agent workflows bake a DAG before the facts are in — expensive fan-out, buried decision points, no way to steer mid-flight. The Interactive Workflow stays cheap and dynamic: cheap sub-agents, no fan-out tax, and the orchestrator keeps a coherent mental model of the whole effort instead of delegating it to a script. The human answers the questions the investigations raise *as they arise*.
