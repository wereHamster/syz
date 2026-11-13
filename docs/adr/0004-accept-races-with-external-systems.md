# ADR-0004: Accept Races with External Systems

## Context

Syz integrates with external systems that it doesn't control.
Users act on them at any time, and webhooks arrive late or not at all.
There is no transaction or lock that spans syzd and them, so races with them can be narrowed but not eliminated.

For example, a user closes a bump's pull request while a publish of that bump is running.
The publish opens a new pull request, and whichever order syzd handles the webhook and the publish outcome in, the user's intent is undone.
The two actions don't say which the user meant, so no ordering is right.

## Decision

We don't chase these races.

- Order syzd's own work against itself (see ADR-0001).
- Keep operations idempotent and convergent, so that later runs or the user correct a wrong outcome.
- Fix a race only if it is realistic and leaves a bad state that nothing corrects later.

## Consequences

- A user's action is occasionally undone, and the user corrects it by repeating it.
- Syzd stays simple, with no guards against interleavings that can't be told apart from intended behavior.
- A race with an external system is not a defect by itself. Races within syzd, and plain bugs, still are.
