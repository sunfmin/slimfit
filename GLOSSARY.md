# Glossary

**Run** (of a suite): where a suite's commands run and what they leave. Each command runs with `sh -c` in the suite's root and writes its log in the suite's scratch directory (`$SLIMFIT_OUT`); the suite's reports are found, cleared and read there, and a missing or unreadable one is a fault with its fix. The test run, a change's mutants and the sample all run a suite's commands through one (`measure::Run`). _Avoid_: workdir, invocation.

**Picks** (of a change): the mutants of a change a second agent chose, each by the name its suite's tool lists it under and with a one-line why, as many as the picker sizes to the diff (ADR 0008, 0015). `measure::Mutants::of` is the one place `--mutants`/`--why` become picks; no name means no mutants run. _Avoid_: selection, chosen mutants.

**Removed test** (of a change): a Rust test function where the change starts that it no longer has, by name, unless its body is there under a name new to it (a rename or a move). Its source lines, from the measurement of the tree the change starts from, are the change's lines too: picks over them run against the tests left (ADR 0008, slimfit#87). `landing::removed::of` is the one place they are found. _Avoid_: deleted test, dropped test.

**Mutants record**: what `slimfit ratchet` writes in `.slimfit/mutants/` of a change: its picks with their outcomes, or why none was worth one, naming the picker and the author, who differ (ADR 0008, slimfit#58). `landing::Record::of` is the one place it is built from a measurement and its rules refused; the command only writes it. _Avoid_: mutants log, landing file.

**Kept verdict**: what `slimfit keep` records in the baseline of a test, a source file, an uncovered span or a sampled survivor, with its reason: the task does not return while what it was kept against holds (a test's or function's text, a span's lines, a file's size and churn). Keeping one again replaces its own earlier verdict only. `keep::record` is the one place a verdict is built, and `keep`'s types (`Kept`, `KeptFile`, `KeptSpan`, `KeptMutant`) are the one place each says when it holds; the command only writes the baseline. _Avoid_: exemption, allowlist.

**Task list**: what `slimfit next` answers of a measurement (and a sample, when drawn): the tasks of the kind asked, cheapest first and capped per kind and suite, their counts before the cap, the redundant-test candidates, the notes on what was not measured and the `next` step. `tasks::next` is the one place it is built; the command only measures and draws the sample. _Avoid_: task queue, todo.
