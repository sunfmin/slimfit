Who picks a change's mutants, and the goal the picker is given. A second agent picks, never the agent that made the change: an author picks what its own tests already hold.

## The goal, as the picker reads it

You are given a change's diff and its mutants, as `slimfit check --mutants` lists them: per suite, `candidates`, each with `name`, `file`, `line`, `function` and `replacement`, and `removed_tests`, the tests the change removes that ran a candidate's line, each named once. You did not write the change, and you are given nothing of its author's reasoning. Pick at most `picks_max` (under `[check]` in `slimfit.toml`, by default {const:PICKS_MAX}) of the candidates you think most likely to survive the change's tests, spread over its files and functions (a promotion's range holds many landings: weigh how they meet, not one alone): those on the decisions the change makes, such as comparisons, boolean logic, guards, and return values that matter. A candidate with `removed` (indexes into its suite's `removed_tests`) is on a line a test the change deletes or merges away ran: it asks whether the tests left still notice it, so weigh it where the removed test asserted something the others may not: a boundary, an error path, a value only it checked. Skip those that cannot build, such as a body replaced by `Default::default()` for a type with no default. Give one line of why for each pick. Or, when no mutant of the change is worth running, say so, with one line of why. Answer the picks by their exact `name`, each with its why; nothing else.

A listing past {const:BUDGET} bytes is grouped per function: `functions` names each function holding candidates ({const:FUNCTION}), and `candidates` spells out up to {const:EXAMPLES} of each, as many as the budget leaves room for (some functions may have none spelled out). Weigh a function by what it decides and by the removed tests that ran it; `{const:EXPAND}` lists one function's candidates in full, and any candidate so listed may be picked, spelled out here or not.

Every candidate listed runs when picked alone. cargo-mutants runs every field deletion on the lines it is given, whatever its name filter says, so a mutant every line of which holds a field deletion it cannot run without is left out of the listing: two field deletions on one line (`Point { x: a, y: b }`), and whatever else is on that line, are not listed, rather than listed as a group. A sample (see [the sample](help:sample)) draws from the same listing.

## How the author asks

1. `slimfit check --mutants` lists the candidates; its `landing.from` (in a plain `slimfit check`'s answer) is where the change starts: `git diff <from>` is its diff.
2. Hand a second agent exactly three things: this goal (`slimfit help picker`), the diff and the candidates. Not your summary, plan or tests: what it should not see is your reasoning. When the listing is grouped, hand it too the `--function` listing of each function it asks to see in full. Name it by an id its caller can give: its session id, with harness and model, e.g. `claude-code:<session>:<model>`. In factorium the worker asks factorium for it (see [setting up in factorium](help:factorium)).
3. Run its picks as given: `slimfit check --mutants <name>... --why <why>...` at background priority (see [mutants scope](help:mutants-scope)).
4. `slimfit ratchet --picker <its id> --author <yours>` writes the record, both ids in it; when it picked none, `slimfit ratchet --no-mutants "<its why>" --picker <its id> --author <yours>`.

A pick that survives fails as `survived_mutants`: the author kills it, with a fast test through the interface or simpler code, before landing; or loosens it with a reason when it changes nothing observable.

## What slimfit holds of it

`ratchet` refuses a record without `--picker` and `--author`, or with the two equal. The gate's plain `check` holds that the change adds a record naming a picker other than its author (see [what the gate holds](help:mutants-scope)). Both ids are the caller's word: slimfit compares them as given, and never re-decides a pick. `slimfit report` tallies the picks' survival by who picked (`picker`, or `author` for records from before a second agent picked) beside the sample's, so a picker that picks only what the tests hold shows as a trend.
