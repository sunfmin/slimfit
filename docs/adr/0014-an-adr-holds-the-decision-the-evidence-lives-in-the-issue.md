# 14. An ADR holds the decision; the evidence lives in the issue

An ADR says what is decided now and why, in its final form. Nothing else goes in it:

- **Evidence goes in a comment on the issue that produced it**: measurements, run logs, per-run tables, samples and their verdicts, variants tried and dropped. Post it with `factorium worker comment`, starting with a header that names it ("Evidence of slimfit#N: …").
- **The ADR cites only the number that carries the decision**, with a link to that comment: `(evidence: slimfit#N, comment "Evidence of slimfit#N")`.
- **No history.** No "as first built", no "the earlier run counted …", no "Amended by". When a decision changes, edit the ADR that holds it into its new final form. When a later ADR replaces one, fold what still holds into the later one and delete the old file; git keeps the history. `docs/adr/` holds only what is decided now.
- **Work is not documentation.** Candidates found while measuring (tests to delete, mutants to kill) are tasks or issues, not ADR content.

The same holds for a reason in `.slimfit/baseline.json` (`slimfit ratchet --loosen --reason`, `slimfit keep --reason`): one sentence of why, plus the issue number. The numbers go in a comment on that issue.
