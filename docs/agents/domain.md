# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Before exploring, read these

- **`GLOSSARY.md`** at the repo root, or
- **`GLOSSARY-MAP.md`** at the repo root if it exists: it points at one `GLOSSARY.md` per context. Read each one relevant to the topic.
- **`docs/adr/`**: read ADRs that touch the area you're about to work in. In multi-context repos, also check `src/<context>/docs/adr/` for context-scoped decisions.

If any of these files don't exist, **proceed silently**. Don't flag their absence; don't suggest creating them upfront. The `/domain-modeling` skill (reached via `/grill-with-docs` and `/improve-codebase-architecture`) creates them lazily when terms or decisions actually get resolved.

## File structure

Single-context repo (most repos):

```
/
├── GLOSSARY.md
├── docs/adr/
│   ├── 0001-event-sourced-orders.md
│   └── 0002-postgres-for-write-model.md
└── src/
```

Multi-context repo (presence of `GLOSSARY-MAP.md` at the root):

```
/
├── GLOSSARY-MAP.md
├── docs/adr/                          ← system-wide decisions
└── src/
    ├── ordering/
    │   ├── GLOSSARY.md
    │   └── docs/adr/                  ← context-specific decisions
    └── billing/
        ├── GLOSSARY.md
        └── docs/adr/
```

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in `GLOSSARY.md`. Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal: either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/domain-modeling`).

## An ADR holds the decision; the evidence lives in the issue

An ADR says what is decided now and why, in its final form. Nothing else goes in it:

- **Evidence goes in a comment on the issue that produced it**: measurements, run logs, per-run tables, samples and their verdicts, variants tried and dropped. Post it with `factorium worker comment`, starting with a header that names it ("Evidence of slimfit#N: …").
- **The ADR cites only the number that carries the decision**, with a link to that comment: `(evidence: slimfit#N, comment "Evidence of slimfit#N")`.
- **No history.** No "as first built", no "the earlier run counted …", no "Amended by". When a decision changes, edit the ADR that holds it into its new final form. When a later ADR replaces one, fold what still holds into the later one and delete the old file; git keeps the history. `docs/adr/` holds only what is decided now.
- **Work is not documentation.** Candidates found while measuring (tests to delete, mutants to kill) are tasks or issues, not ADR content.

The same holds for a reason in `.slimfit/baseline.json` (`slimfit ratchet --loosen --reason`, `slimfit keep --reason`): one sentence of why, plus the issue number. The numbers go in a comment on that issue.

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> _Contradicts ADR-0007 (event-sourced orders), but worth reopening because…_
