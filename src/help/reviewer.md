Who judges a change's growth, and the goal the reviewer is given. A change that adds more than `growth_allowance` source lines net (under `[check]` in `slimfit.toml`, by default {const:GROWTH_ALLOWANCE}) carries a growth record, judged by a second agent, never the agent that made the change: an author finds its own code needed.

## The goal, as the reviewer reads it

You are given a change's diff, nothing else: you did not write it, and you are given nothing of its author's reasoning. Could this change be done with fewer lines, or by deleting or reusing code that exists? Read the diff and the code around it in the repository: look for what the change repeats of code already there, what it adds that an existing function, type or tool already does, code it leaves behind that nothing needs any more, and lines that a simpler shape would not need. Answer one of two, in one line:

- `as-is`, with why the change cannot be smaller; or
- `cut`, naming what to cut: each place, by file and what to reuse, delete or simplify.

Judge size, not style or correctness: a cut must keep what the change does.

## How the author asks

1. Cut what you can yourself first: `slimfit check`'s `landing.growth` is the source lines the change adds net; at or under the allowance no record is due.
2. Hand a second agent exactly two things: this goal (`slimfit help reviewer`) and the diff (`git diff <landing.from>`, `landing.from` from a plain `slimfit check`). Not your summary or plan. Name it by an id its caller can give: its session id, with harness and model, e.g. `claude-code:<session>:<model>`. In factorium, as `slimfit help factorium` says.
3. It answers `as-is`: `slimfit ratchet --serves "<the issue>" --considered "<the code you weighed reusing or deleting>" --as-is "<its why>" --reviewer <its id> --author <yours>`.
4. It answers `cut`: make the cuts, or answer each with why not. If the change then adds no more than the allowance, no record is due. Otherwise `slimfit ratchet --serves ... --considered ... --cut "<what it named>" --answer "<what you did, or why not>" --reviewer <its id> --author <yours>`.
5. Commit `.slimfit/growth/` with the change.

## What the gate holds

- `ratchet` with any of `--serves`, `--considered`, `--as-is`, `--cut`, `--answer` or `--reviewer` writes `.slimfit/growth/<hash>.json`: the tree it was made of, `lines` (what the change added net then), `serves`, `considered`, `reviewer`, `author`, and `as_is`, or `cut` with `answer`. It refuses one with a field missing or not one line, or with the reviewer the author. Each is a file of its own, named by its contents, so two branches' records merge without a conflict.
- Every `slimfit check` fails with `unrecorded_growth` while the change adds more than the allowance net and no growth record that names a reviewer other than its author. Never on the number alone: lines can be gamed (denser code), so the number asks for the step, and the step is what is held. The ids are compared as given; the gate never re-decides a verdict, nor which lines a record covers. `unrecorded_growth` is never loosened.
- **Net source lines** are, in each suite, the source lines (as `prod_lines` counts them: neither blank nor only a `//` comment, outside tests) of the files the change touches, now, less theirs where the change starts (`landing.from`), summed over the suites. Deleting code elsewhere in the same change pays for what it adds.
- `slimfit next` gives a `justify-growth` task, right after `pick-mutants`, while it is due. `slimfit report` counts the growth records by verdict (`growth_records`) beside each point's `prod` and `test` lines: many `as_is` while lines keep climbing is a rule kept in form only.
