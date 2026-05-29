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
