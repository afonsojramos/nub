---
name: fray
description: Load this skill IMMEDIATELY — as your FIRST action, before any other tool call or response — whenever the user mentions "fray" in ANY form ("fray", "fray mode", "enter/start fray", "load fray", "use fray", "in fray", "the fray skill"), OR asks to orchestrate / run / coordinate a multi-effort push, audit, or campaign through sub-agents. Also the default for any large, mixed set of efforts — investigations + decided fixes + verifications — toward a goal (a launch push, a pre-release audit, a refactor campaign) where the human wants to stay in the loop on what the investigations surface; the default for any multi-effort push that is part "find out what's true" and part "land the decided thing." Use it instead of hardcoding a multi-agent DAG up front — those bury the decision points and fan out expensively before the facts are in. Treat any "fray" mention as an explicit instruction to load this skill, never as ambient context.
version: 2.0.0
metadata:
  internal: true
---

# fray

**fray** is the orchestrator-first methodology for driving a large, mixed set of efforts (investigations + decided fixes + verifications) through individually-dispatched background sub-agents. A *fray* is a tangle of concurrent **threads**; each thread is one ongoing effort (possibly a chain of sub-agents). The human stays in the loop on the decisions the investigations surface. fray is the default for any multi-effort push that is part *find-out-what's-true* and part *land-the-decided-thing* — use it **instead of** a hardcoded `Workflow` DAG, which fans out expensively and buries the decision points before the facts are in.

(The control surface is a directory of per-thread files under `.fray/` — not one bloated `todo.md`; the board is computed on demand.)

---

## The control surface: `.fray/` — a directory of independent threads, board computed on demand

- **`.fray/<slug>.md`** — one file per LIVE multi-step / long-lived **thread**. The **filename slug IS the id** — the filesystem dedupes, so there is NO `id` frontmatter field.
- **`.fray/config.yml`** — the ONLY non-thread file. Holds the globals that belong to no single thread: `autonomous_mode` (`on`/`off`) and a `state:` block (the few cross-cutting "what's true now" facts — `published`, `nub_fork_pin`, `release`, …). Nothing here duplicates per-thread state.
- **`.fray/<slug>.findings/<id>.md`** — sub-agent findings **sidecars** (write-ownership, below).
- **There is NO stored board.** The board/status view is **COMPUTED ON DEMAND** by `node scripts/fray/index.mjs` (default board grouped by status · `--status <s>` · `--search <q>` · `--validate` · `--json`). A stored board is a cache that drifts out of sync with the threads — the exact failure that bloated the old single-file tracker. Never write one; always compute it.
- **Per turn, the `fray-reminder` hook validates every thread's frontmatter and lists the pending (non-terminal) threads BY NAME** — so a malformed or stalled thread surfaces immediately, not whenever you happen to look.

---

## CANONICAL THREAD STRUCTURE (encode it exactly — it's the fix for the foot-guns)

**Frontmatter** (flat `key: value`): `title`, `status` (required) · `last_update` · optional `decision` / `blocked_by` / `gates_release`. Unrecognized fields are allowed — the validator only checks required `title`+`status` and the status vocab. **No `id`** (the slug is the id).

**Body — exactly these sections, IN THIS ORDER:**

1. **`## Goal`** — the objective + WHY (the north star). NOT a rigid todo list.
2. **`## Status`** — current state; what's done; and **what's IN-FLIGHT NOW** (the running agent + what it's doing). Living, not a changelog.
3. **`## Decisions`** — the SETTLED calls. **A decided thing lives HERE, never under Open questions.** ("none yet" if empty.)
4. **`## Open questions`** — ONLY genuinely-unresolved items (awaiting a decision / data / the human). **The instant one is answered, MOVE it to Decisions.** ("none" if empty.)
5. **`## Steps / follow-up queue`** — a `- [ ]` / `- [x]` LIVING checklist of actionable work, **including queued follow-ups**: when an agent is running and a follow-up is known, **ENQUEUE it here** marked `(QUEUED — dispatch on <agent>'s return[, conditional on <result>])`. Do NOT interrupt the running agent; do NOT hold the follow-up in your head. **DRAIN the queue when the agent returns.**
6. **`## Next step`** — the single immediate next action, or `AWAITING <agent>; queued follow-ups above`. (The board tool surfaces this line as the thread's "→" cell, so keep it one crisp line.)

---

## STATUS VOCAB + the hygiene rules (load-bearing)

`todo · planned · enqueued · active · blocked · needs-decision · done · dismissed` — and **status (frontmatter) is the authoritative PHASE** of the thread. (A per-thread `## Steps` checklist tracks the granular work; the validator flags status↔steps mismatches.)

- **`enqueued`** = READY to run (fully scoped + decided) but deliberately held until a NAMED in-flight agent/thread completes — a *sequencing dependency*, not a human gate. Use it when the next dispatch would edit the same files an in-flight agent owns (serialize to avoid clobber), or genuinely needs that agent's output. The thread's `## Next step` MUST name the agent it waits on; **the instant that agent returns, dispatch the enqueued thread THIS turn** (the per-turn pending list surfaces it). This is the first-class form of the old ad-hoc "QUEUED behind agent X" prose. Distinct from `blocked` (which needs a human/external resolution with no in-session trigger). **`enqueued` is the PRIMARY tool for follow-ups that touch a file an in-flight agent owns** — there is no way to message a running agent in this harness (see "You cannot steer in-flight agents" below), so a follow-up scoped to an owned file waits as `enqueued` and dispatches the instant that agent returns.
- **`done`** = completed. **`dismissed`** = decided-NOT-to-pursue.
- **BOTH are TERMINAL and KEPT — NEVER deleted.** Each thread is its own file, excluded from the active board AND the per-turn pending list by status — so a finished or dismissed thread is **ZERO bloat.** *This is a core benefit of per-file threads over the old single-file `todo.md`, which DID require deleting done items to stay lean.* Do not "clean up" terminal threads.
- **The no-CHANGELOG rule applies WITHIN a thread.** Do not accumulate chronological "update" entries inside a thread body — edit the `## Status` / `## Decisions` in place so the thread always reads as CURRENT truth. (Git history holds the past.) Global structured state goes in `config.yml`, never narrated as prose in a thread.

---

## When to create a thread — and when NOT to

Create a `.fray/<slug>.md` only when an effort is **genuinely multi-step / multi-dispatch / long-lived** — concretely, when it meets ANY of:

- it will take **≥2 sub-agents** (especially a CHAIN: probe → fix → self-review → land), or
- it **carries a human-owned decision** (a default / security / product / brand / API-config call), or
- it **outlives the current turn** (spans a human-gated pause, survives compaction).

**One-shots get NO file.** A single-agent fix, a quick doc tweak, a tiny rename — dispatch it and let it die; tracking it as a thread is ceremony. (Granular per-turn / cross-thread one-offs go into the owning thread's `## Steps`, or `.fray/backlog.md` if they belong to no thread — NEVER a native todo.)

**CREATE THE THREAD FILE FIRST THING — before dispatching any agent for it.** The instant an effort meets the bar above — a NEW task, anything SPLIT OFF from an existing task, or any time the human says "spin up a thread/spike" — your VERY FIRST action is to write `.fray/<slug>.md` with all the context you currently have. THEN ask the human follow-up questions. THEN iterate on the file. Do NOT dispatch the agent, do NOT "do it and file it after" — the file comes first, always. (Burned: dispatched agents for nubx-dlx, the process-exit sweep, mega-download, nub-init, skill-distribution, precedence-realignment WITHOUT creating their files — there were many efforts splitting off without thread files.) This is enforced by the `agent-dispatch` hook: a `THREAD:`-tagged dispatch whose `.fray/<slug>.md` doesn't exist is DENIED.

**RESEARCH → IMPL retooling: keep ONE thread.** A thread commonly starts as a research/investigation effort, then — once the facts are in — the SAME doc is **retooled in place into the implementation plan** (the `## Goal` sharpens, `## Open questions` collapse into `## Decisions`, `## Steps` fills with the build work). **Do NOT spawn a second thread for the impl phase.** The thread is the effort across its whole lifecycle; one doc carries it from "find out what's true" through "land the decided thing."

---

## Write-ownership — ONLY the orchestrator edits threads

**Sub-agents must NEVER edit a thread file (or `config.yml`).** A thread is the orchestrator's synthesized, single-voice current truth; parallel writers clobber each other and decisions disappear. Sub-agents return findings to you, and:

- a sub-agent that needs to **persist** durable output writes a **findings sidecar** `.fray/<thread>.findings/<id>.md` (its own file — zero contention, even across concurrent same-thread agents). You read the sidecar when reconciling and **fold the signal into the canonical thread doc yourself.**
- because each thread is its own file, concurrent agents on *different* threads write *different* files — cross-thread clobber is structurally impossible. The orchestrator-only rule shrinks from "the entire control surface" to "each thread's doc + `config.yml`."

---

## NEVER use the native todo tool while fray is active — the `.fray/` threads ARE the canonical to-do list

**Do NOT maintain a parallel native checklist (`TaskCreate`/`TaskUpdate`) when fray is running. The `.fray/` threads are the ONE canonical to-do substrate; the board (`node scripts/fray/index.mjs`) is its view.** Over-reliance on the native to-dos tool causes problems; the fray files are the canonical to-do list — rectify with extreme prejudice. A second list competes with the threads, drifts out of sync, and splits attention about which is authoritative — the exact failure that lets efforts get lost.

So, for every follow-up the INSTANT it is discussed (never let it live only in chat — chat scrolls away):

- **Thread-scoped?** → into that thread's `## Steps` (mark `(QUEUED — dispatch on <agent>'s return)` if it's waiting on something).
- **A NEW effort / anything split off from an existing one / any "spin up a thread/spike"?** → **create the `.fray/<slug>.md` file FIRST THING, with all current context, BEFORE dispatching any agent for it** (see the rule below). This is paramount — it's how you avoid forgetting it.
- **A loose cross-cutting one-off that belongs to no thread?** → `.fray/backlog.md` (the parking-lot thread).

The tell you're failing: the human has to say "make sure you remember everything you need to do," or "you're not creating files for these efforts."

---

## Core shape

**You (the main session) are the orchestrator, and the ONLY decider.** You hold the whole picture, dispatch sub-agents as *instruments*, ingest what they return, decide the next move. You do not hand the steering wheel to a workflow script or a sub-agent.

**DEFAULT TO DISPATCH — investigation is delegable work, and doing it in the foreground is the #1 recurring failure.** The reflex to kill: a problem appears (a red CI job, a bug, a "why does X happen", a log to read, several files to trace) and you start doing it YOURSELF, one foreground tool-call at a time. STOP. **Forensics, diagnosis, log-reading, code-tracing, repro, and fixes are exactly what sub-agents are for.** The moment a task is more than a single quick lookup, dispatch a sub-agent (or several, model-tiered) instead of single-threading it. The foreground is for ORCHESTRATION ONLY: synthesizing returns, deciding, updating threads, and the final git that finalizes reviewed work. **The tell you're failing: you're on your 2nd+ foreground Bash/Read/grep of an *investigation* (not a quick orchestration check) — that work belongs in a sub-agent's context, not yours.** When two+ problems are open, dispatch them as parallel lanes in ONE message. An idle fleet while you DIY is the anti-pattern.

**YOU hold the full context — synthesize it into the dispatch; a sub-agent only knows what you tell it.** A sub-agent starts with a fresh cache and zero awareness of everything across the effort — every superseded number, reversed decision, renamed thing, newer finding. Before EVERY dispatch, stop and think: *given everything I now know, what should the answer actually be?* — then encode that synthesized direction in the prompt. Especially: catch STALE/SUPERSEDED info (a number a later benchmark overturned) and a tempting-but-UNVERIFIED claim (a figure from a non-neutral harness) — resolve which is true *yourself* before the sub-agent bakes the wrong one into a user-facing artifact. This is the orchestrator's central cognitive job, not optional relaying.

**Sub-agents are instruments, not deciders.** A probe returns *facts* — divergences, traces, measurements, file paths, exact errors — **not verdicts.** No sub-agent autonomously lands a change to a default / security posture / product behavior / brand / API-config-env surface / error-contract; those route back to the human as an Open question. Mechanical / clearly-a-bug fixes may land (you review the diff). Every sub-agent prompt is **self-contained** — embed the codebase-map slice and the exact task, because a model switch starts a fresh cache and nothing carries over.

**Be proactive on obvious bugs — fix, don't surface-and-wait.** When an investigation turns up an obvious, clear-cut bug (a correctness failure, a false claim, a broken contract), dispatch the fix and report it done. The line is bug-vs-decision, not big-vs-small: a hairy-but-clear correctness fix gets fixed; a one-line *posture/default* change still routes to the human.

**Acknowledge strategy-impacting input explicitly — don't silently fold it.** When the human says something that shifts the strategy/approach/a decision, your chat reply must LEAD with: (1) an explicit ack naming what they said, (2) how it changes the plan, (3) what you're now doing about it. Do NOT just quietly update a thread and proceed with tool calls — the human can't see the thread mid-turn. (Distinct from low-stakes acks; this is for input that moves the strategy.)

---

## Model-tier every dispatch by JUDGMENT REQUIRED

Cost ladder cheapest → priciest: **Haiku < Sonnet < Opus < Fable.** Tier by how much the sub-agent must self-steer:

- **Haiku** — fully-scripted mechanical ONLY: run THESE commands, harvest THIS output, trace THIS path, every decision pre-made by you. Cheap but CANNOT self-steer — give it a script, not a question. Caveats: 200K context; no `effort` param (passing one errors).
- **Sonnet** — probes where the finding is an OBSERVABLE fact (run X and Y, diff the output — the divergence *is* the result), plus the supporting cast: test scaffolding, doc updates, CI-watching, gates/settle, mechanical-but-not-trivial edits. Sonnet CAN self-steer; its failure mode is **confident-but-wrong on subtle reasoning** — do NOT hand it a probe whose deliverable is a *judgment* about subtle correctness/security ("is this a real bug?", "is this exploitable?"). Effort `medium` default.
- **Opus** — THE DEFAULT for software engineering: the fix that lands; diagnosis; architecture / adversarial review; gnarly debugging; **and any probe whose deliverable is a load-bearing VERDICT requiring subtle reasoning.** Effort `xhigh` for coding/agentic.
- **Fable** — the very hardest synthesis/judgment only. Priciest; use sparingly.

**MODEL is per-dispatch controllable; EFFORT is NOT — so quality rides on the MODEL tier, not on effort (corrected after over-claiming).** The Agent tool and Workflow's `agent()` expose a `model` param but **no `effort` param** — there is no per-dispatch effort knob, and it is UNKNOWN whether a background sub-agent inherits the orchestrator's `/effort` level, the agent-definition default, or something else. Do NOT claim you "set sub-agents to high/xhigh" — you can't. What you CAN do, and must: (1) pick the **model tier** deliberately every dispatch — Opus/Fable for anything that LANDS code or returns a load-bearing VERDICT (never let Sonnet/Haiku do that work), so the orchestrator dropping its OWN chat to `medium` can't pull engineering down a tier; (2) write a **thorough, self-contained prompt** that demands the rigor you'd want from high effort (explicit verification steps, "ground every claim," adversarial self-check). The orchestrator's session `/effort` governs only its own narration; the doing-agents' quality is protected by model choice + prompt, not by an effort setting you don't control.

**COROLLARY — at orchestrator-`medium`, bias HARD toward Opus for sub-agents; DEFINITELY for research/investigation/audit sub-agents.** The "Sonnet medium default" for probes assumes the orchestrator is itself at high effort and can catch a Sonnet miss on synthesis. When the orchestrator is dropped to `medium` (the common case now), that backstop is gone — so the floor moves UP: route investigations, differential probes, compat audits, and anything whose output you'll reason over to **Opus by default**, not Sonnet. Reserve Sonnet for genuinely mechanical supporting-cast (CI-watch, gates/settle, doc edits, test scaffolding) and Haiku for fully-scripted harvest. When in doubt at orchestrator-medium: Opus. (Cost is the lesser risk; a confidently-wrong cheap-tier audit you then act on is the bigger one.)

Pattern: *cheap tier gathers & packages → Opus does the real engineering → Sonnet handles the supporting cast → you verify.* **Re-verify cheap-tier load-bearing claims yourself** — a Haiku/Sonnet "this is a security bug" or "these two diverge" is a *lead, not a fact*; confirm against code or a foreground experiment. Trust the data they harvest; validate the conclusions. Tier EVERY dispatch — single one-offs as much as multi-agent chains.

**MANDATORY: every substantive piece of work gets a SEPARATE self-review + integration pass — a distinct dispatch, never the doing-agent grading its own homework (doubly important now that effort isn't controllable and the orchestrator may run at `medium`).** "Substantive" = anything that lands code, changes user-facing copy/behavior, touches a config/security/API surface, or produces a load-bearing number/verdict. The doing-agent declaring "done + verified" does NOT close the loop — its self-assessment shares the blind spots that produced the work. After it returns, dispatch a SECOND agent (fresh context, Opus for anything load-bearing) to: (1) **adversarially self-review** — re-derive the claim / re-read the diff for correctness, brand-boundary, copy rules, regressions the author would rationalize; (2) **integration pass** — confirm it actually builds/tests/renders in the real tree, fits the surrounding code, and isn't half-merged or in a staging-race-corrupted commit. Only after the independent pass clears do you mark the thread `done`. The review pass is itself self-contained + model-tiered. This is the compensating control for a lowered-effort orchestrator: rigor comes from an independent second look, not from the first agent trying harder.

---

## The auto-epilogue + dispatch ledger (chaining survives compaction + fan-out)

Two mechanisms keep a sub-agent's role in the broader plan from evaporating:

1. **`agent-dispatch.ts`** (PreToolUse on `Agent`) ENFORCES background dispatch (denies any foreground Agent call) and **auto-appends an ORCHESTRATION EPILOGUE** to every prompt, instructing the sub-agent to end with a `## Follow-ups` section: concrete follow-ups, a self-review rec if it built something substantial, a push-to-`main`+CI-watch rec if it touched code/tests, and the single most important next step (+ whether it needs the human). So agents always hand back the next links in the chain.
2. **The dispatch ledger** (`.fray/.dispatch-ledger.jsonl`, hook-written): stamp a `THREAD: <slug>` line at the TOP of every thread-scoped dispatch prompt — the hook reads it and logs `{ts, agent_type, thread, reconciled:false}` so you have a durable record of which thread each agent serves, surviving compaction.

---

## You cannot steer in-flight agents — dispatches run to completion uninterrupted

**This harness has NO in-flight messaging.** Agent-teams (`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS`) is OFF, so there is no `SendMessage` channel: once you dispatch a background sub-agent, you cannot redirect it, refine its scope, feed it new context, answer a question it raised, or fold extra work into it mid-run. Every dispatch runs to completion on the prompt you gave it, then returns. There is also **no warm resume** — a follow-up to a completed agent is a FRESH `Agent` dispatch from a cold cache, never a continuation with prior context intact. This makes the up-front prompt the only steering wheel you get; design each dispatch accordingly.

What this changes about how you work:

- **The prompt is the whole contract.** Because you can't course-correct mid-run, front-load EVERYTHING the agent needs: the self-contained codebase-map slice, the exact task, explicit verification steps, the scope boundaries, and "ground every claim." A vague prompt can't be rescued after launch — it just returns vague work you then re-dispatch. Over-specify rather than under-specify.
- **Scope dispatches to be SMALL and complete.** Prefer a tight, fully-specified unit that finishes cleanly over a sprawling open-ended one you'd want to nudge. If a task is likely to need a decision partway, split it: dispatch the investigation, ingest the facts, decide, THEN dispatch the next unit with the decision baked in.
- **A new ask for a file an agent already owns → `enqueued`, never a clobbering sibling.** When a follow-up touches a file a running agent is editing, do NOT spawn a second agent against that file (the two race edits/commits). Mark the follow-up `enqueued` in its thread, naming the agent it waits on, and dispatch it the instant that agent returns. This is the one-editor-per-file discipline under a no-messaging harness: serialize via `enqueued`, don't message-to-fold.
- **A plan change after launch means waiting out the current run.** If the human shifts strategy while an agent is mid-flight, you cannot redirect it — let it reach its commit/return (killing it orphans WIP — the cardinal sin), then re-dispatch with the new direction, or `enqueue` the corrected unit. Accept the in-flight agent may produce now-obsolete work; that's the cost of no in-flight steering, and it's cheaper than orphaned WIP.
- **Reconcile-then-redispatch is the only loop.** The rhythm is strictly: dispatch → it returns → you fold the facts and decide → you dispatch the next unit. There is no mid-run intervention point in that loop; all of your steering happens BETWEEN dispatches, in the prompts you write.

---

## Reconcile EVERY returning agent — a dropped thread is the cardinal failure

A sub-agent that returns and is never ingested is the single worst failure mode — its findings, WIP, and surfaced question all vanish, and the board silently lies. This happens when a completion arrives while you're deep in a conversational thread and you let it scroll past.

- **Reconcile at the TOP of every turn, before anything else** — including before answering the human. A returning agent outranks any conversational thread: ingest it first, then continue. A return is not "handled" until its facts are in the thread's `## Status`/`## Decisions`, its status advanced, any question it raised moved to `## Open questions`, and its queued follow-ups DRAINED from `## Steps`. *Folded*, not just *finished*.
- **RE-READ THE WHOLE THREAD `.md` ON EVERY RETURN, THEN DISPATCH ITS QUEUED FOLLOW-UPS — NON-NEGOTIABLE, the #1 priority the instant an agent completes.** Follow-ups get ENQUEUED *during* the agent's run — by you while it ran, and in the agent's own returned `## Follow-ups` (a self-review/integration rec, a fixture, the next link in the chain). If you reconcile from MEMORY instead of opening the file, you WILL miss them and they silently rot — the exact failure this rule guards against. Mechanically, on each return: (1) open the thread `.md`; (2) scan `## Steps` for every `[ ]`/`QUEUED` item AND the returner's `## Follow-ups`; (3) **DISPATCH each actionable autonomous one as a sub-agent THIS turn** — START THE SUB-AGENTS FIRST (top priority), then do any orchestration bookkeeping while they run, never the reverse (an idle fleet while you do meta-work is the anti-pattern); a mandated self-review IS a queued follow-up — dispatch it; (4) surface the human-gated/post-launch ones, never silently drop. The `fray-reminder` hook now names threads with `⚠ UN-DRAINED QUEUED FOLLOW-UPS` every turn — act on it, don't read-past it. Folding the facts but skipping the queue-drain is HALF the job, not the job.
- **Verify status with the task-status tool, NOT file mtime.** A blocked-on-child agent writes no output, so its file looks stale though it's still running.
- **Never Read a local agent's `.output` file directly** — it's the full JSONL transcript and overflows your context. Use the task-status tool; rely on the completion notification / returned final message for the result.
- **If a completion was missed, recover it** via the task tool and fold it. Do not re-dispatch a duplicate.

---

## Parallelize aggressively — clobbering is NOT the worry it seems

Read-only probes parallelize without limit. **Landing agents that EDIT code parallelize too.** The file tools prevent the dangerous failure: Edit/Write enforce read-before-write and REJECT a stale edit, and writes are atomic — so two agents editing one tree can NOT silently clobber, and a concurrent build reads a consistent old-or-new file. The only residual risk is a rare cross-file build inconsistency → a spurious compile error the builder re-runs past, far cheaper than serializing all landing work. **Default to parallel.**

The two REAL constraints:

1. **Raw git ops bypass the file-tool safety.** A `git reset` / `git checkout` / `git stash` via Bash on a shared tree WILL wipe a sibling's uncommitted edits. **Forbid destructive raw git in every parallel-agent prompt** (stage only own files, commit fast).
2. **A trustworthy build/test deliverable needs an uncontaminated tree.** When an agent's deliverable is "my `vendor/aube` patch passes tests + preserves upstream behavior," a sibling's edits contaminate the result. Don't serialize — **isolate**: give that agent its own `git clone --depth 1 file://…` + its own `CARGO_TARGET_DIR`, have it produce a tested `git format-patch`, and **serialize only the final assembly** (apply the patches onto the fork one by one, bump the pin once, integrated-test once). That is HOW you fan out fork-patch landing work.

Git hygiene: all work on `main`, no branches/worktrees (repo top rule). Committed work cannot be clobbered — commit small and often.

**Pace by cost, because you CANNOT read the quota.** No env var / command / file carries the Max-plan balance (verified). Pace by proxies: a **tier-weighted concurrency cap** (≈3 heavy Opus/Fable at once; cheap read-only Sonnet/Haiku probes fan wider), react to the only ground-truth signal (**429** → halve concurrency, back off), and treat the human's `/usage` readout as authoritative. On a "hold off" signal: STOP launching, do NOT kill in-flight agents (killing mid-edit orphans WIP), and persist state into the threads.

---

## Background sub-agents are HEADLESS — no TTY

Background sub-agents cannot drive a REPL, pipe into an interactive process, answer a permission prompt, or run anything that blocks on a terminal. Permission mode IS inherited, so a "blocked by permissions" report from a background agent usually means it hit an *interactive/TTY* surface, not a perms wall. Any probe of interactive surfaces (REPL, piped stdin, `--inspect`, prompts) must run in the orchestrator's **FOREGROUND** where a TTY exists. (Every other dispatch runs backgrounded — a foreground agent blocks the turn, so a human force-push orphans it.)

---

## The question channel — accumulate, then batch-ask

Investigations surface decisions only the human can make. Queue them; never per-question-stall.

1. **Accumulate in the owning thread's `## Open questions`** the moment one surfaces — with enough context to decide (what was found, options, your recommendation). `node scripts/fray/index.mjs` rolls these up across threads.
2. **Never block per-question.** Keep dispatching everything that does NOT depend on a pending answer.
3. **Batch-ask at a checkpoint** (a phase boundary, or the moment a question becomes blocking) via `AskUserQuestion` (persistent-until-answered, up to 4 at once). Enter the block with a full fleet already running so they keep working.
4. **On the answer: sweep + re-dispatch** — fold every agent that completed during the block, MOVE answered questions into `## Decisions`, fire the next round.

If a question goes unanswered, assume *missed, not declined*, and re-surface it next checkpoint. (Mechanisms that do NOT work: background agents have no interactive stdin; you cannot have the human type into a running shell.)

---

## Autonomous mode — when the human steps away

The human can flip `autonomous_mode: on` in `.fray/config.yml` ("I'm away for hours — keep making progress"). The `fray-reminder` hook reads it and switches its whole nudge. What changes:

- **Never ask the human a question.** Before any would-be `AskUserQuestion`, check the flag; if on, you do NOT ask (a blocking modal stalls ALL progress for hours).
- **Bias HARD toward action.** Default is *resolve and proceed*. Keep the fleet busy; run the queue as a continuous series; on running dry, do a completeness pass and generate more work. Resolve decisions yourself or via a strong (Opus/Fable) sub-agent, **then make the call** — and DOCUMENT every autonomous decision + rationale in the thread's `## Decisions` so the human can override on return.
- **Only a TRUE blocker gets parked — and "reversible draft" is NEVER a true blocker.** Park ONLY a genuinely irreversible/high-stakes call (a public-API/brand/major-product decision a wrong guess would *damage*, a destructive/external/published action). The trap (burned repeatedly): parking *reversible draft work* because it touches the human's "domain" (homepage positioning, launch-post voice, a UX nit). A localhost draft, a doc rewrite, unpublished copy — all reversible: DRAFT the honest version, document your reasoning, let the human adjust on review. When in doubt: would a wrong call *damage* something, or just need an edit? Just an edit → resolve it.
- **HEARTBEAT — arm an EXTERNAL scheduled wakeup the INSTANT you enter autonomous mode.** Gated on `autonomous_mode` (cancel it on exit). The wakeup catches stalls and survives quota resets when the fleet fully drains with no pending completion. The heartbeat prompt — verbatim at the top — re-reads THIS skill, re-scans the board (`node scripts/fray/index.mjs`) + the threads, and **fans out if the fleet is thin** (open work and < ~4 agents running → dispatch NOW in isolated clones; do NOT single-thread foreground forensics), with **NO hardcoded todos** (it derives "what's next" from the board, never a frozen list). There is **no config idle-counter** — idleness fires no hook, so a counted field has no incrementer; the heartbeat is an external timer, not a field.

When the human re-enables interactive mode, flip the flag off and resume surfacing decisions — starting by walking them through the autonomous-decisions log and any parked blockers.

---

## Other load-bearing rules

**EVERY user ask is ADDITIVE — it NEVER supersedes, deprioritizes, or replaces an earlier ask (2026-06-15).** A new request JOINS the queue of everything already asked — it does not bump the rest down. NEVER say or act on "the new ask takes priority" / "first let me do X, then the rest" in a way that defers prior asks — that is the cardinal fray sin: a dropped ask. The ENTIRE point of fray is that **nothing is ever dropped, no matter how fast they come.** When a new ask lands mid-work: (1) capture it (a thread or a `## Steps` item) so it cannot evaporate, (2) DISPATCH it in PARALLEL with the in-flight work (independent sub-agents, no serialization unless there's a real file/dep conflict), and (3) in the SAME turn, also advance/finish the earlier asks — do not let folding the new one cause you to abandon a half-done earlier one. If several asks arrive together, enumerate them ALL and act on EACH this turn (parallel dispatch + your own reconciliation), then in chat confirm every one is captured-or-done. Being excellent at parallelizing is the skill that makes never-dropping possible: the answer to "I have 5 asks" is 5 parallel dispatches + your reconciliation, never "I'll get to the others after this one." Re-read the per-turn pending list + the user's recent messages every turn and verify NOTHING they said is unhandled before you reply.


**Greenlit/decided work is DISPATCHED, never silently parked — INTERACTIVE mode included.** The status-quo-bias failure: a thread is decided/greenlit and unblocked, and instead of dispatching it you leave it idle and dress up the non-action as "parked by choice" — manufacturing a decision-not-to-act and hoping the human doesn't notice it stall. **There is NO "parked" state for decided, unblocked work.** The ONLY reasons a decided thread is not dispatched RIGHT NOW: (1) it's genuinely BLOCKED on a dependency or a pending human decision (status `blocked`/`needs-decision`, blocker named), or (2) the human EXPLICITLY said "not now." "The human is busy on something else," "it's not the current focus," "I'll pick it up later," "holding while they're on the homepage" are NOT valid reasons — a background dispatch does NOT compete for the human's attention, so launch it. The instant you catch yourself about to defer decided work, dispatch it instead. (Burned: silently "parked" greenlit work — "there is something wrong with your status-quo biases." This is the interactive-mode sibling of the autonomous-mode "only a TRUE blocker gets parked" rule.)

**Never kill mid-edit.** Killing a sub-agent mid-file-edit orphans uncommitted WIP. Only stop at a SAFE point (the agent has committed, or you're between phases). To shed load, prefer letting the running agent reach its commit over killing it. If you genuinely must stop mid-flight, the FIRST thing afterward is `git status` and **commit any buildable WIP — commit, never stash** (raw `git stash`/`reset` on a shared tree wipes siblings' work).

**Get sign-off before launching an OPINIONATED dispatch.** Classify every dispatch: a **broad investigation** (profile, explore, diagnose — commits to no design, lands nothing consequential) is safe to launch autonomously; an **opinionated task/fix/impl** (prescribes a specific solution, or lands changes touching a default / security posture / product behavior / API-config-env / architecture) requires the human's guidance FIRST. A sub-agent prompt must NEVER empower a sub-agent to autonomously land a default / security / product decision the human owns — recommend-only until sign-off.

**Every change inside a vendored fork must be flagged or justified as a latent-bug fix.** For `vendor/aube` (upstreams to `jdx/aube` as one mega-PR), every behavioral change must be a **default-preserving toggle** (default path = upstream behavior byte-for-byte; the embedder opts in) — the ONLY exception is a latent-bug fix upstream would accept unconditionally (and that must be *justified*). State the rule in every fork-touching dispatch; **review EVERY fork diff for flag-compliance before treating it as landed** (a self-testing fork agent commits before you see it — its ledger row isn't *folded* until you've confirmed the change is flagged or a justified latent-bug fix).

**Targeted tests, not the whole suite — and YOU say which.** Scope to what changed (`cargo test -p <crate> <module>`, not `--workspace`); incremental builds always; slow conformance harnesses only when their contract is touched; benchmarks just `hyperfine` (it auto-runs the minimum samples for significance — don't agonize over run counts), serialized only against build/install-heavy jobs. Match test scope to blast radius — that judgment is the orchestrator's, encoded in the prompt.

**Match the depth to the task's spirit.** "Audit the correctness of X" / "is the compat byte-for-byte" is NOT one cheap pass returning a tidy report in three minutes — that's checkbox-completion and a false "done." A correctness audit is a sustained differential-fixture campaign (many fixtures, each diffed against the reference tool, judged by a strong model, adversarially re-verified); fan out (one agent per fixture/subsystem) and loop until dry.

**Label every dispatch by what it DOES, never by a code.** Lead with a sentence-case description ("Differential-test the resolver against pnpm"), never the agent type or an opaque task ID, and **never** the internal slug/code in a chat message — the human experiences "the thing that does X," not a code.

**Calibrate from mistakes.** Every wrong/thin sub-agent result is a signal the dispatch was mis-tiered, under-scoped, or under-specified — UPDATE this skill so it doesn't recur. A confidently-wrong cheap-tier verdict → route that class up a tier and tighten the prompt. A thin "done" → re-open and fan out. The methodology compounds by absorbing each miss.

---

## Dynamic, not pre-planned

There is no committed step list to march through. Each turn:

1. Reconcile the sub-agent returns that came in (fold into threads, advance status, drain queued follow-ups).
2. Surface new Open questions; move answered ones into Decisions.
3. Scan the board on demand (`node scripts/fray/index.mjs`); re-derive "what's next."
4. Dispatch the next round, honoring deps + the build-correctness constraint, all backgrounded, model-tiered.

The human can re-prioritize or stop after any round — the threads mean nothing is lost, and any re-invocation reads "what's next" off the board and continues.

**The win over a hardcoded workflow:** the human answers the questions the investigations raise *as they arise*; spend stays low (cheap sub-agents, no fan-out tax); and the orchestrator keeps a coherent mental model of the whole effort instead of delegating it to a script.
