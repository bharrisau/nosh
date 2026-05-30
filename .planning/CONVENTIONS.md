# Conventions

## GSD agent model selection

When launching any GSD subagent (planner, researcher, executor, verifier, plan-checker, roadmapper, synthesizer, code-reviewer, etc.), resolve its model from the project's configured profile **before** spawning, and pass it explicitly as the `model=` argument:

```bash
gsd-sdk query resolve-model gsd-planner
# → {"model": "opus", "profile": "balanced"}
```

- Pass the agent type as a **positional** arg (`resolve-model gsd-planner`). The `--agent` flag silently falls back to a sonnet default with `"unknown_agent": true`.
- The profile maps **per-agent**, not globally. On the `balanced` profile (this project's setting), `gsd-planner` resolves to **opus** while most other roles (executor, verifier, researcher, plan-checker, roadmapper, synthesizer, code-reviewer) resolve to **sonnet**. Do not generalize one role's tier to the others, and do not default everything to either opus or sonnet.
- When wrapping a GSD skill (e.g. `gsd:plan-phase`) inside an `Agent`, let the skill resolve its own subagents' models — do **not** inject a blanket model override into the wrapper prompt (that can downgrade the planner from opus).
- The profile is changed via `/gsd:set-profile` / `/gsd:settings`; never silently override it.

## GSD verification: separate pass, on opus

Verification must run as an **independent pass on opus**, never collapsed inline into the (sonnet) executor that wrote the code — a peer grading a peer rubber-stamps subtle bugs (it happened in three consecutive M3 phases; opus re-verify caught a real bug each time, including one that broke a core guarantee).

- `.planning/config.json` pins this via per-agent override (GSD #3227): `"model_overrides": { "gsd-verifier": "opus", "gsd-integration-checker": "opus" }`. Confirm with `gsd-sdk query resolve-model gsd-verifier` → `opus`. (Use `model_overrides.<agent-id>`, the per-agent knob — NOT `model_profile_overrides`, which is runtime+tier scoped.)
- Do NOT wrap `execute-phase` in a background agent and rely on it to self-verify: background/Workflow agents can't spawn subagents (one-level nesting), so the separate `gsd-verifier` collapses inline. Run a dedicated verifier agent after execute instead.
- Make the verifier **adversarial**: it should write a probe test that reproduces the suspected failure (fails before the fix, passes after) and trust the code over the executor's summary. The recurring failure mode is tests that pass without exercising the failure (shape-only / bypassed-real-path / hardcoded-input).
