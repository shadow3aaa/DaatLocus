# Runtime Reasoning Roadmap

## Goal

Move runtime reasoning from:

- hand-written baseline prompts
- hand-written bias candidates
- failure-triggered candidate tweaks

to a more DSPy-like flow:

- train/dev split
- teleprompter-style candidate proposal
- bootstrap demos
- compile report and repeatable optimization

## Current State

- Bench-only programs already support:
  - failure-driven proposal
  - auto bootstrap demos
  - compiled tuning selection
- Runtime suites already support:
  - compiled prompt cache
  - failure-driven proposer hooks
  - bootstrap demo hooks
- Runtime compile currently still tends to select `baseline`, because the runtime eval sets are small and too clean.

## Todo

- [ ] Split runtime datasets into `train` and `dev`.
  - `train` should be used for proposal/bootstrap.
  - `dev` should be used only for candidate selection.

- [ ] Add a runtime teleprompter layer.
  - Generate instruction candidates every compile, not only when baseline fails.
  - Keep the first version narrow and deterministic.

- [ ] Add runtime bootstrap demo generation.
  - Start from `resolve_telegram_chat`.
  - Then extend to all `action_phase.*` suites.

- [ ] Upgrade runtime optimizer search.
  - Compare baseline, hand-written bias candidates, auto instruction candidates, auto demo candidates, and combo candidates.
  - Keep tie-breakers on score first, then lower retry count.

- [ ] Add compile reports for runtime suites.
  - Record selected candidate.
  - Record chosen extra instructions.
  - Record chosen demos.
  - Record train/dev scores.

- [ ] Expand runtime boundary cases.
  - Prefer sharper edge cases over longer demos.
  - Focus on semantic mistakes, not only parse failures.

- [ ] Re-validate on VM after each step.
  - `prompt-reset`
  - `optimize reasoning`
  - inspect `reasoning_compiled`
  - inspect `reasoning_traces`

## Execution Order

1. Split runtime datasets into `train` and `dev`.
2. Add the runtime teleprompter.
3. Add runtime bootstrap demos.
4. Upgrade runtime optimizer search.
5. Add compile reports.
6. Expand runtime edge-case coverage only where the above still fails.

## Immediate Next Step

- [ ] Implement `train/dev` split for runtime datasets, starting with `resolve_telegram_chat`.
