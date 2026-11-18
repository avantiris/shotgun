---
name: feedback
description: Record and triage the user's feedback after testing Shotgun, then fix it. Use when the user reports how a test round went, lists bugs, annoyances or wishes about the app, or says something feels slow or wrong.
---

# Handle a feedback round

The user tests the running app and reports in free text, often several things
at once and in their own words. The goal is that nothing they said gets lost
and that fixes are verified the way they experienced the problem.

## 1. Record first

Create `docs/feedback/<YYYY-MM-DD>-round-<n>.md`. One item per distinct point:

```
### F<n>.<i> <short title>
- Said: "<the user's words, quoted>"
- Kind: bug | slow | workflow | missing | idea
- Area: import | timeline | viewer | audio | keys | export | colour | project | other
- Status: open
```

Quote, do not paraphrase into something stronger or weaker than what was said.

## 2. Clarify only what blocks

Reproduce each bug yourself in the running app before asking anything. Ask
(with the question dialog) only where the intended behaviour is truly the
user's choice. Items that change something agreed in `docs/spec/01-product.md`
or a spec statement not marked `PROPOSED` need the user's confirmation.

## 3. Order

1. Crashes and data loss.
2. Anything "slow": measure it, add the number to the item, compare with the
   budget in `docs/spec/04-performance.md`. If no budget covers it, add one.
3. Workflow friction in things done many times per session (keys, clicks).
4. Missing features, then ideas (ideas may be scheduled into a later milestone
   instead of built now; say which).

## 4. Fix and verify

For each item: fix, add a test that would have caught it where possible,
verify in the running app with the same steps the user took, commit. Update
the item's status to `fixed (<commit>)`, `scheduled (M<n>)` or `won't do
(<reason, agreed with user>)`. Update specs and the key map document when
behaviour changes.

## 5. Report

List every item with its status, in the user's order. Say plainly which could
not be reproduced or are not fixed.
