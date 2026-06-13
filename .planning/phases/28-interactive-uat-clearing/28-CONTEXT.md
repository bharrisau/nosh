# Phase 28: Interactive UAT Clearing - Context

**Gathered:** 2026-06-13 (batched all-phase discussion v1.4)
**Status:** Ready for planning (most specifics intentionally deferred — see re-ask triggers)

<domain>
## Phase Boundary

A human operator confirms, **one item at a time, conversationally**, that every carried-forward validation item and the new M7 remote-access path work in a live environment. Process-driven; no new feature code unless a gap is found (then a tracked gap-closure plan is created). This is the milestone's finishing step.
</domain>

<decisions>
## Implementation Decisions

### Format (user's milestone-level decision)
- **D-01:** Guided, **interactive, step-by-step** walkthrough — present one UAT item, wait for the operator to confirm/report, then advance. **Not** a single dumped UAT document. (`/gsd:verify-work` style, not `/gsd:audit-uat` style.)

### Failure handling — user decision
- **D-02:** When an item fails or is inconclusive: **log it, create a tracked gap-closure plan/todo, and continue** the walkthrough. At the end, decide whether any open gap blocks shipping. (Chosen over halt-on-first-failure — keeps the pass moving, nothing lost.)

### Scope of items (from UAT-01 / UAT-02)
- **D-03:** Carried-forward backlog (UAT-01): Phase 19 Windows alt-screen visual re-test (4 full-screen TUI scenarios); 999.3 client rendering-correctness pack; 999.4 `read -s` / predictive-echo fix on the Windows client; confirm `build-windows` + `cargo audit` CI green.
- **D-04:** New M7 path (UAT-02): Mode A WebTransport connect; blocking TOFU prompt shows SHA-256 hex + requires `yes`; interactive shell; scrollback; predictive echo; simulated network change → transparent reattach.

### Claude's Discretion
- **The "fourth full-screen TUI" in UAT-01 (ROADMAP SC#1):** recommend **tmux** — it stresses alt-screen + scrollback interaction together. Confirm with the operator at phase start (vim/htop/Claude Code are the other three).
- Ordering of items within the walkthrough — recommend backlog first (UAT-01), then the new M7 path (UAT-02), so known-good baseline behaviour is confirmed before exercising new transport.
</decisions>

<specifics>
## Specific Ideas

- This phase is *why the user asked for the milestone the way they did* — they explicitly want to be walked through pending UAT interactively rather than handed a doc. Honour that: short, concrete, one-at-a-time prompts; the operator drives a real client/server.
- Mode A live-test environment is straightforward — a nosh server binding UDP/443 directly (no proxy needed for Mode A); pair with a client on a different machine/network to exercise the network-change reattach.
</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Phase scope & decisions
- `.planning/ROADMAP.md` §"Phase 28" — goal + 4 success criteria
- `.planning/REQUIREMENTS.md` — UAT-01, UAT-02
- `.planning/research/SUMMARY.md` §"Phase 6: Interactive UAT Clearing"

### Carried-forward UAT artifacts (the items to walk through)
- `.planning/phases/19-*/19-HUMAN-UAT.md` (archived under `.planning/milestones/v1.3-phases/` if moved) — Phase 19 Windows alt-screen 4-scenario re-test
- `.planning/phases/999.3-client-terminal-rendering-correctness-pack-platform-agnostic/` — 999.3 VERIFICATION human_needed items
- `.planning/phases/999.4-predictive-echo-repaint-pacing-live-fix-round-2/` — 999.4 `read -s` / predictive-echo Windows items
- `docs/windows-client-test.md` — prior Windows live-test sign-off format to mirror
- `.planning/STATE.md` §"Deferred Items" — the authoritative carried-forward list at v1.3 close

### Prior decisions feeding the M7 walkthrough
- `.planning/phases/24-*/24-CONTEXT.md`, `25-*/25-CONTEXT.md`, `26-*/26-CONTEXT.md` — what "working" means for connect / TOFU / reattach
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `docs/windows-client-test.md` sign-off format (v1.1) — reuse the structure for the Windows re-test record.
- The 999.3 / 999.4 phase dirs were intentionally preserved in `.planning/phases/` (not cleared at milestone switch) precisely because this phase consumes their human_needed items.

### Established Patterns
- Gap-closure-plan-on-failure (D-02) mirrors how v1.3 handled the two opus-verifier-caught blockers — create a tracked increment, fix, regression-test, then close.

### Integration Points
- Depends on Phases 23–27 all shipping; the M7 walkthrough exercises the full stack (transport seam → WebTransport endpoint → inner auth → reattach → hardening).
</code_context>

<deferred>
## Deferred Ideas

None — this phase intentionally adds no new scope; it validates.

## Pending Decisions — Re-Ask Before Planning

The bulk of Phase 28's specifics genuinely depend on Phases 24–27 having shipped. Capture-now-decide-later:

1. **Full live-test script + environment** — *Dependency: Phases 24–27 complete.*
   RE-ASK TRIGGER (autonomous): **after Phase 27 completes**, before Phase 28 planning — ask the operator: (a) which two hosts/networks will run the Mode A client↔server live test, and how the network-change step will be performed (Wi-Fi↔cellular, VPN toggle, etc.); (b) which "fourth full-screen TUI" to use (recommend tmux); (c) operator availability/scheduling for the Windows-client re-tests (UAT-01 needs a physical Windows host, as in v1.1/v1.2).
2. **Whether any carried-forward item is already obsolete** — *Dependency: Phases 23–27 may incidentally fix or moot a 999.3/999.4 item.*
   RE-ASK TRIGGER (autonomous): after Phase 27 completes — re-read 999.3/999.4 VERIFICATION files and confirm with the operator which items still need a live pass vs which are now covered by automated tests landed during v1.4.

> Note for `/gsd:autonomous`: do NOT auto-run Phase 28 unattended — it is operator-driven by design. Pause and surface the trigger-1 questions when Phase 27 completes.
</deferred>

---

*Phase: 28-interactive-uat-clearing*
*Context gathered: 2026-06-13*
