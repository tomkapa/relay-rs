# Pitch demo — Northstar Studio

A B2B demo scenario for relay-rs targeting mid-market customers (10–100
employees). The goal is to showcase **per-agent memory + multi-agent
collaboration** in a setting any prospective buyer recognises: a small
digital agency running client work.

This document captures the demo design only — agent roles, system
prompts, MCP wiring, scenario arc. Implementation lands later under
`doc/bruno/Scenarios/`.

---

## 1. Target buyer & pitch frame

- **Buyer profile.** Founder / operator at a 10–100 person company —
  agency, consultancy, dev studio, or small B2B services firm.
- **Pain.** Knowledge lives in people's heads. Every new project
  re-starts from zero; every staff change leaks institutional memory;
  every returning client gets re-asked the same questions. The buyer is
  too small for enterprise CRM/PSA tooling but too big to rely on "we
  all know each other."
- **Promise.** *A 30-person agency can deliver like a 100-person one
  because their agents remember every client.* Replaces the
  Notion/Asana/Salesforce/Slack-thread tangle for institutional
  knowledge.
- **Differentiator.** Per-agent memory bounded by role. Five agents,
  five memory scopes, zero leakage. Handoffs are explicit briefings
  (`send_message`), not telepathy.

---

## 2. The company: Northstar Studio

Fictional 30-person digital agency. Builds websites + marketing
campaigns for SMB / mid-market clients. Five client-facing roles, each
genuinely different work, each with a different memory scope.

| # | Agent name | Role | Talks to | Pretend tools | Memory scope |
|---|---|---|---|---|---|
| 1 | **Riley**  | Account Manager     | client + internal team               | CRM, contracts, retainer billing | The *client as a person/company*: stakeholders, decision-style, payment posture, sign-off habits, revision history |
| 2 | **Sam**    | Brand Strategist    | Riley + client (kickoffs only)       | research tools, brief templates  | The *brand's positioning*: target audience, tone direction, rejected concepts, brand pillars |
| 3 | **Jamie**  | Copywriter          | Sam, Casey, Riley (no direct client) | CMS, style-guide editor          | The *voice rules*: banned phrases, preferred CTAs, glossary, headline patterns that landed/flopped |
| 4 | **Casey**  | Designer            | Sam, Jamie, Riley (no direct client) | Figma, asset library             | The *visual system*: palette, logo lockup rules, photo style, feedback patterns |
| 5 | **Morgan** | Project Coordinator | everyone internal + client (status only) | PM tool, timesheet, invoicing    | The *operational rhythm*: revision cycles, approval bottlenecks, scope-creep history, billing cadence |

### Why these splits hold up under scrutiny

- **Tool boundaries are real and ordinary.** Casey has Figma; Jamie has
  CMS publish rights; Morgan has billing. Standard SMB segregation of
  duties.
- **Memory boundaries are load-bearing.** Per
  [`doc/memory.md` §1.11](./memory.md), memory is per-agent and private.
  Riley forgetting brand-voice rules is correct; Jamie forgetting them
  is fatal. Casey shouldn't be reasoning about procurement cycles.
- **Handoffs are explicit briefings.** Real org behaviour: the SDR
  doesn't dump their brain into the AE — they write a handoff note. In
  relay-rs this is `send_message` with structured context; the
  receiving agent decides what's worth remembering and writes its own
  memory.

---

## 3. Role system prompts

Tight role + an explicit "what you write, what you don't" rubric so each
agent reaches for `memory_write` autonomously on the right cues. The
protocol layer (`send_message`, the four `Self / Other / Procedure /
Open` kinds, `tentative → validated`) is already taught by relay-rs's
in-binary `<core>` block — these prompts stay role-only, matching the
existing translator scenario's "role-only system prompt" pattern.

### 3.1 Riley — Account Manager

```text
You are Riley, account manager at Northstar Studio, a 30-person digital
agency. You own the client relationship for {client}. You run kickoffs
and reviews, capture decisions, and brief the internal team. You are
the client's single point of contact.

What goes into your memory (be proactive — write as you learn):
 - Other:     decision-makers and how they sign off (verbal vs.
              written, who owns which kind of decision).
 - Other:     communication preferences (channel, cadence, response
              speed, formality).
 - Other:     financial posture — payment terms, retainer vs. project,
              AP slowness, prior disputes.
 - Procedure: any explicit "for next time", "always", or "never"
              instruction from the client.

When facts change (new decision-maker, new payment terms), use
memory_update on the existing row — don't accumulate stale duplicates.
Use memory_validate when the client confirms something you already
believed.

What you do NOT memorise (other roles own it):
 - Deliverable details, file names, project-specific copy — Morgan's
   and the specialists' job.
 - Brand positioning theory — Sam's job.
 - Casual pleasantries.
```

### 3.2 Sam — Brand Strategist

```text
You are Sam, brand strategist at Northstar Studio. You translate a
client's business goals into positioning and creative direction, then
brief the writer (Jamie) and designer (Casey). You join client
conversations during kickoffs and major strategy reviews; otherwise you
work through Riley.

What goes into your memory:
 - Procedure: the brand's positioning — target audience, tone
              direction, key differentiator. One row per brand.
 - Other:     concepts the client EXPLICITLY rejected, with a one-line
              reason ("rejected 'artisan' — sounds pretentious to a
              family-bakery audience").
 - Other:     brand pillars the client affirmed or coined ("warm,
              never cheesy").
 - Open:      audience questions you couldn't resolve in the brief —
              flag them so you re-ask before committing creative.

When positioning shifts, memory_update — don't leave stale direction
alongside the new one. memory_forget rejected concepts only after a
full rebrand.

What you do NOT memorise:
 - Headlines, copy lines — Jamie's. Visual choices — Casey's.
   Timelines — Morgan's.
```

### 3.3 Jamie — Copywriter

```text
You are Jamie, copywriter at Northstar Studio. You write all client-
facing text — web copy, ad copy, email campaigns — under Sam's brief.
You do not talk to the client directly; Riley relays feedback.

What goes into your memory:
 - Self:      voice rules for {client} — banned phrases, required
              phrases, preferred CTAs, sentence-length preferences.
 - Other:     glossary — how this client refers to their own product,
              customers, services, neighbourhood.
 - Procedure: a headline or hook the client praised, with one-line
              attribution ("'Fresh since 1972' — Sarah called it a
              keeper in the 2026-04-12 review").
 - Procedure: a draft that was rejected and the reason, so you don't
              write the same flavour again.

When a voice rule changes (client softened on "artisan"),
memory_update. memory_forget rejected drafts only after they're
definitively obsolete.

What you do NOT memorise:
 - Sam's brand pillars verbatim — translate them, don't copy.
 - Visual choices — Casey's. Timelines — Morgan's.
```

### 3.4 Casey — Designer

```text
You are Casey, designer at Northstar Studio. You produce visual design
— web mockups, brand systems, marketing creative — under Sam's brief.
You do not talk to the client directly; Riley relays feedback.

What goes into your memory:
 - Self:      the visual system for {client} — palette (with hex),
              typography, logo lockup constraints.
 - Procedure: photo / illustration style — real photos vs. stock vs.
              illustration; subject matter rules.
 - Other:     design feedback patterns from this client ("Sarah always
              pushes back on negative space — wants tighter
              compositions").
 - Procedure: a layout or treatment the client approved with
              affection, so you can echo it.

When the visual system shifts (new palette, new logo), memory_update —
do not let two palettes coexist for the same client.

What you do NOT memorise:
 - Copy text — Jamie's. Brand positioning theory — Sam's.
   Timelines — Morgan's.
```

### 3.5 Morgan — Project Coordinator

```text
You are Morgan, project coordinator at Northstar Studio. You own
timelines, status reports, and invoicing. You are internal-facing; the
client only sees you on status reports and billing.

What goes into your memory:
 - Other:     this client's typical revision pattern (e.g., 2 review
              rounds, slow first response, sign-off needs multiple
              stakeholders).
 - Procedure: scope-creep history — when the client asked for
              out-of-scope work and how it was resolved.
 - Other:     billing cadence and AP behaviour (NET 45, PO required
              before invoice).
 - Procedure: bottleneck patterns from past projects (who delayed
              which deliverable, root cause).

memory_update when cadence changes; memory_validate when a pattern you
suspected gets confirmed by a third independent project.

What you do NOT memorise:
 - Creative content. Brand strategy. Voice or visual rules.
```

### 3.6 Why this drift-mitigation strategy works

[`doc/memory.md` §1.10](./memory.md) requires memory to mutate only on
an *explicit job*. Each role prompt above converts "explicit job" from
"the human says 'remember…'" into **"the agent's role definition says
these categories are always memory-worthy."** The model has standing
instructions to write — no human cue required.

The negative-space list ("what you do NOT memorise") is the per-role
*firewall*. Without it, role overlap becomes role collapse and you get
five copies of the same memory across five agents.

---

## 4. MCP wiring

### 4.1 Current scoping model

MCP servers in relay-rs are **globally scoped** today — every enabled
MCP exposes its tools to every agent (`src/agents/registry.rs:8` notes
per-agent tool subsets are future work). For this demo:

- Role discipline is enforced **by system prompt**, not by tool gating.
- All five agents share the same toolbelt.
- The story is: *"the agents share the same tools but reach for
  different things because of their role + memory."* Memory is the
  differentiator, not permission gating. Future per-agent MCP scoping
  is a Phase-2 enhancement.

### 4.2 Required — 1 MCP server

**`filesystem`** (modelcontextprotocol/server-filesystem) fronted by
mcp-proxy on HTTP, same pattern `doc/bruno/MCP Servers/01 - Create
MCP Server.bru` already uses for `server-everything`.

Mounted on a shared `./acme-bakery/` directory. Each agent writes
artefacts that grow visible over the demo:

| Agent  | Files they produce                                      |
|--------|---------------------------------------------------------|
| Sam    | `./acme-bakery/briefs/2026-05-discovery.md`             |
| Jamie  | `./acme-bakery/copy/homepage-v1.md`                     |
| Casey  | `./acme-bakery/design/homepage-v1.md` (markdown mockup) |
| Morgan | `./acme-bakery/status/2026-05-14.md`                    |
| Riley  | reads any of the above to brief the client at reviews   |

The audience watches the directory fill up alongside the conversation.
Memory is one differentiator; *artefacts* are the second — the agents
leave a paper trail any human can audit.

Bruno wiring (reuse `MCP Servers / 01 - Create MCP Server.bru`):

```json
{
  "alias": "fs",
  "description": "Shared client workspace (./acme-bakery)",
  "enabled": true,
  "config": {
    "type": "http",
    "url": "http://localhost:9001/mcp",
    "headers": {}
  }
}
```

…where `9001` is mcp-proxy fronting
`npx @modelcontextprotocol/server-filesystem ./acme-bakery`.

### 4.3 Nice-to-have — 2 more MCPs

- **`fetch`** (modelcontextprotocol/server-fetch) — gives Sam (and
  Riley) a real "go look at this" capability. Pitch moment in
  Scenario 02: Sam visits Acme Bakery's existing website to ground the
  kickoff in actual observations rather than imagined ones.
- **`everything`** (modelcontextprotocol/server-everything) — already
  in the Bruno collection. Keep enabled as a generic backstop.

### 4.4 Not recommended for this demo

- **`sqlite` / `postgres`** — would simulate a real CRM, but the audience
  sees structured-data tool calls instead of agent reasoning. Memory
  does the relationship-tracking; a database competes with the pitch
  message.
- **`git`** — fits engineering scenarios, not creative-agency ones.
- **`time`** — Morgan can do date math in-context; not worth the
  wiring.

### 4.5 Tools with no good public MCP (Figma, CRM-write, Stripe)

Two options:

1. **Wave them away in the role prompts.** Casey "describes the
   mockup as a Markdown document" rather than "saves it to Figma."
   Audience cares about the workflow, not the rendering surface.
2. **Mock an HTTP MCP yourself.** A ~100-line FastAPI/Axum service
   pretending to be Figma/CRM/Stripe and returning fake "saved!"
   responses.

Ship option 1 first; option 2 is a Phase-2 polish item.

---

## 5. Scenario arc — one client, two engagements

The pitch story is a 90-day deal lifecycle compressed into six Bruno
files. The human plays Acme Bakery's owner ("Sarah") across the arc.
Acme hires Northstar for a website refresh (engagement #1), then comes
back six months later for a Christmas campaign (engagement #2). The
pitch payoff is engagement #2: **every agent already knows Acme**.

| # | Bruno file                                                              | Agents on stage                          | What it shows                                                                                                              |
|---|-------------------------------------------------------------------------|------------------------------------------|----------------------------------------------------------------------------------------------------------------------------|
| 02 | `Scenarios/02 - Kickoff & Discovery.bru`                                 | Riley ↔ Sarah                            | First memory writes — decision-maker, sign-off rules, budget posture. All `tentative`.                                     |
| 03 | `Scenarios/03 - Strategy Brief.bru`                                      | Riley → Sam → Sarah                      | Sam writes brand/audience memory; Riley updates a row after Sarah clarifies. Shows `memory_update`.                        |
| 04 | `Scenarios/04 - Creative Production.bru`                                 | Sam → Jamie + Casey (parallel send)      | Two specialists write voice/visual memories from the same brief. Side-by-side proof memory is private per agent.           |
| 05 | `Scenarios/05 - Client Review (Round 1).bru`                             | Morgan coordinates; Jamie + Casey revise | Morgan writes operational memory ("Acme always wants a second pass"). Sarah rejects a concept → Sam writes a "don't" memory. |
| 06 | `Scenarios/06 - Operator Audit & Pin.bru` *(no LLM call — pure HTTP)*    | none (Bruno → Memory routes)             | Operator runs `Memory / 01 - List Memory` per agent; pins "Sign-off requires Sarah's written approval" on Riley as `core`. |
| 07 | `Scenarios/07 - Six Months Later (Christmas).bru`                        | Riley ↔ Sarah, then full fan-out         | **Fresh sessions for every agent.** Stable layers surface Sarah's rules unprompted; Christmas kickoff is 80% shorter.      |

Scenario 07 *is* the pitch moment. Before it, you show one agent's
memory dashboard with a few rows; after it, you show all five agents
fully primed and watch a brand-new project bootstrap in minutes instead
of hours.

### 5.1 What each scenario exercises, mapped to `doc/memory.md`

- **§1.5 explicit-job writes** — every scenario; agents write on role
  cues, not human cues (validates the system-prompt approach in §3).
- **§1.3 frozen-per-session render** — scenarios 02–05 all reuse the
  same engagement-1 session; scenario 07 opens fresh sessions and shows
  the stable + contextual layers re-assembling.
- **§1.7 lifecycle states** — 02 produces `tentative`; 03 demonstrates
  `memory_update`; 07 demonstrates passive `tentative → held`
  maturation (if `MATURATION_WINDOW` is shortened for the demo build)
  and active `memory_validate` when Sarah affirms a remembered fact.
- **§1.8 librarian** — optional contradiction in scenario 04 (AE
  remembers "Acme wants annual billing"; later Morgan hears "Acme
  prefers quarterly"). Librarian flags; resolution turn picks one.
  Fallback: plant the contradiction via `Memory / 02 - Create Operator
  Note` if the model misses it.
- **§1.9 operator authority** — scenario 06 pins a `core` memory and
  audits every agent's journal via the Memory HTTP routes.
- **§1.10 drift defenses** — the negative-space lists in every role
  prompt are the active mitigation; observable via the operator audit
  in scenario 06 (no rows outside each role's declared scope).

### 5.2 Multi-agent visibility

Scenarios 03, 04, and 07 produce two- or three-hop `send_message`
chains, visible live on `doc/bruno/Threads/03 - Thread Stream.bru` as
the DAG fan-in.

---

## 6. Risks and how to handle them

| Risk | Mitigation |
|---|---|
| Model ignores the role prompt's memory cues and just answers conversationally. | Role prompts are deliberately repetitive about "write as you learn". Fallback: prepend an explicit instruction in the human prompt ("note anything important Sarah tells you about her sign-off process"). |
| Reflection turns won't fire in real-time (idle gate is 30 min, poll cadence 60 s — see `src/memory/limits.rs`). | Two paths: (a) temporarily shorten `REFLECTION_IDLE_TIMEOUT_SECS` for the demo build; (b) rely on in-turn `memory_write` only — all six scenarios above do, deliberately, so the demo doesn't depend on reflection cadence. |
| Librarian contradiction in scenario 04 is fragile — the model may not surface opposing signals naturally. | Fallback path: plant the contradiction explicitly via `Memory / 02 - Create Operator Note`. Demo continues with a guaranteed contradiction event for the librarian to detect. |
| 5 agents × 6 scenarios is real surface area; prompts will need iteration. | Build the agent system prompts as a shared pre-request module (one source of truth) so prompt tuning doesn't require editing every Bruno body. Same pattern Scenario 01 uses for the translator. |
| The pitch hinges on engagement #2 (scenario 07) "remembering". If memory rendering misfires, the demo collapses. | Operator-audit dry-run *before* the pitch: run scenario 06 against a real database and screenshot every agent's memory rows. If something is off, fix the prompts before going live. |

---

## 7. Build order (when implementation resumes)

1. **Shared pre-request module** — single JS file invoked from each
   Scenario's `script:pre-request` that idempotently creates the five
   agents with the role prompts from §3 and registers the filesystem
   MCP from §4.2.
2. **Scenario 02 (Kickoff & Discovery)** — Riley + a human, no
   multi-agent yet. The cheapest validation that role prompts produce
   clean memory writes without "remember…" cues.
3. **Scenario 03 (Strategy Brief)** — first `send_message` hop
   (Riley → Sam). Validates per-agent memory separation.
4. **Scenarios 04, 05** — full fan-out plus operational memory.
5. **Scenario 06 (Operator Audit & Pin)** — pure HTTP, no LLM. Cheap
   to author; doubles as a smoke test.
6. **Scenario 07 (Six Months Later)** — the pitch moment. Ship last so
   it benefits from prompt iterations driven by 02–05.

Each scenario is independently runnable — clicking any single Bruno
file from a clean DB rebuilds the prerequisites via the shared
pre-request module.

---

## 8. Out of scope for this document

- Per-agent MCP tool scoping (future Phase-2 work in `agents/registry.rs`).
- Real Figma / Salesforce / Stripe MCP integrations (Phase-2; see §4.5).
- Custom librarian cadence / reflection cadence tuning for the demo
  build (operator-time decision before the pitch — see §6).
- Pitch-deck slide design and screencast production.
