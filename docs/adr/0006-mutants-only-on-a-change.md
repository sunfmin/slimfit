# 6. Mutants only on a change; redundant tests from per-test coverage

Supersedes `0004-rust-kill-matrix.md`, and what `0001`, `0002` and `0005`
still said about runs over every mutant (`--all-mutants`, `init --mutants`,
pieces, the kill matrix).

## Context

On factorium#93 (2026-11-19 02:13) a worker's `slimfit check --mutants`
fell back to a full run: 1960 mutants, each running all 213 tests with
`--no-fail-fast`, hours of work, killed. ADR 0005 already made `--mutants`
refuse rather than fall back, but `next --all-mutants` still ran every
mutant, and slimfit's own `[idle]` ran it daily (an hour or more). A full
run of factorium is many hours on this host (ADR 0002's pieces measured 4 of
64 files in 7 hours).

The owner's decision (control session, 2026-11-19 02:20): slimfit loses the
ability to run all mutants; only what finishes fast stays. And (02:35,
02:40): redundant tests are found another way, with no mutants.

## Decision

### No run mutates the whole codebase

- The only mutants run is the change's (ADR 0005): the lines changed since
  the baseline's `mutants_at`, or the merge base with the upstream. Its size
  follows the change, never the codebase.
- `--all-mutants` is gone from every command. A command line that has it is
  refused, and the fix names the line to use: `slimfit check --mutants`
  (`slimfit next --mutants` for its tasks). This refusal is the one trace
  left of the flag.
- A suite whose mutation tool takes neither a diff nor files is refused by
  `--mutants`, as before; there is no longer any other way to run it. Today
  that is every preset but Rust's (gremlins, muter, PIT, Stryker): their
  mutants run only once a suite sets `mutants_diff_flag` or
  `mutants_file_flag`.
- Surviving-mutant tasks come only from runs of a change.
- slimfit's own `factorium.toml` has no `[idle]` any more: its one step was
  the daily full run. `E2E_MUTANTS=1` runs the Rust sample's mutants of an
  injected change only (it ran every mutant of every sample).
- `slimfit doctor` finds `whole_codebase_mutants`: `--all-mutants`, or a
  mutation tool run without slimfit and without a diff, anywhere in
  `factorium.toml` (gates and `[idle]` steps) or the scripts it names.

### No kill matrix

ADR 0004 made cargo-mutants run nextest with `--no-fail-fast` and read each
mutant's log for every killing test, only so `redundant-test` could be
computed from a kill matrix. Nothing reads killers now, so:

- the Rust preset drops `--cargo-test-arg=--no-fail-fast`: a caught mutant's
  test run stops at the first failure (on factorium a caught mutant ran all
  213 tests);
- the readers keep only each mutant's status (`killed_by`, the list of known
  tests, the nextest log parser, Stryker's `killedBy`, PIT's
  `killingTests` are gone), and the samples' cargo-mutants logs with them.

### Redundant tests: coverage proposes, the agent decides on assertions

Coverage alone is too coarse. On factorium (a406723) by function coverage
86 of 207 tests reach only functions one other test reaches, and many are
not redundant: `accounts::doctor_says_when_mytokens_lists_no_account`
reaches only what `doctor_says_why_each_account_cannot_be_used_here`
reaches, yet alone checks the missing-mytokens branch. The literature
agrees (coverage-based reduction can lose much fault detection: Rothermel
et al. ~48% in one study; Chen et al., ICST 2017: assertions decide). A
model judging redundancy from test text alone over-flags (~30% false
positives, arXiv 2412.03693). Together they fit: coverage proposes, the
review decides.

1. **Candidate** (slimfit, deterministic, from per-test coverage of source
   lines, `reach.lines` in the measurement; `test_lines` before ADR 0007):
   - a test whose covered lines are a subset of one other test's; of two
     tests with the same lines, the slower is proposed (on equal time, the
     later by name);
   - a test whose lines overlap another's heavily without either being a
     subset: `|A ∩ B| / |A ∪ B| ≥ 0.8` (`MERGE_OVERLAP`, a Jaccard share of
     the lines either runs); the slower of the two is proposed, so a pair
     is named once.

   The overlap is computed without the suite's shared setup: a line more
   than half the tests run (`COMMON_LINES`, 0.5) is left out of both
   tests' lines first, and a test with no line left overlaps nothing
   (slimfit#34). The subset is of all lines: setup both run cannot make
   one test a subset of another. Each candidate names the one other test
   it most overlaps (a superset first), slowest candidate first. A test
   that runs no source line is not proposed.
2. **Decision** (the agent, by the task's steps). For every assertion of the
   candidate, find the assertion in the other test (or another named test)
   that checks the same outcome on the same setup. Then one of:
   - **delete**: every assertion maps; the mapping goes in the commit
     message;
   - **merge**: some assertion does not map, but the setups are compatible:
     fold the unmapped assertions (and the setup they need) into the other
     test, or turn both into one test over a table of cases, so the suite
     pays the setup once (a factorium scenario's cost is its world, about 50
     git processes; factorium c277f9a merged this way). The merged test
     keeps every assertion of both, each failing with its own message; the
     commit lists where each assertion went;
   - **keep**: the setups conflict (folding one changes what the other
     checks), or merging would hide which behaviour failed.
3. **Accept**: plain `slimfit check`: no new uncovered line, failures 0,
   test time not up (and for a merge, down). No mutant runs.

**Built (slimfit#32), for Rust.** The Rust preset's test run already gives
every test its own coverage profiles (ADR 0007). After the run each test's
profiles are merged (`llvm-profdata merge`) and exported as LCOV
(`llvm-cov export -format=lcov`, with the test's binary and the build's
binaries as objects, so a binary the test starts counts), as many tests at
once as there are cores; only the records that ran are kept. That one
record gives the functions each test ran (`reach.functions`, ADR 0007) and
its source lines, test code left out (`reach.lines`), in the same coverage
run: no second test run. Other presets record nothing per test: `next`
says so in `notes`, and their suites get no redundant-test task.

**Keep** is recorded with `slimfit keep <test> --reason "<why>"`, which the
task's steps name. `.slimfit/baseline.json`'s `kept` holds the suite, the
test, the reason and an FNV-1a hash of the test's text, from its `fn` line
to the brace rustfmt closes it with. The test is found by its function's
name in the suite's Rust files (of several, the one whose path names its
modules). The coverage report cannot say where, because cargo llvm-cov
leaves integration tests out of it. A candidate whose kept hash matches its
text now is not proposed; any edit inside it brings it back. `ratchet`
drops a verdict whose test changed or is gone. A test that is not found
cannot be kept: `keep` refuses, and the steps leave delete or merge.

## Consequences

- No command can run for hours on a codebase's size. A large change still
  costs its own mutants (Measured, below).
- What a deleted test killed in unchanged code is seen by nothing: a
  deletion adds no line (ADR 0005). Redundant-test deletions rest on the
  assertion mapping, not on mutants.
- Go, Swift, Kotlin and TypeScript suites run no mutants until a diff or
  file flag is set for their tool. Their readers stay, for that.
- Old files still load: serde ignores the removed fields (`killed_by`,
  `mutation_tests`, `mutated.all` in a cached measurement; the baseline's
  `mutants`, `mutant_files`, `mutants_pending` from before ADR 0005), and
  the next write drops them. No compatibility code is kept.

## Measured

`slimfit check --mutants` (this ADR's build, cargo-mutants 27.1.0, nextest
0.9.151, the Rust preset without `--no-fail-fast`) on a clone of factorium,
each commit checked out with the baseline's `mutants_at` at its parent,
2026-11-19, under factorium's `worker exclusive --background`, 1-minute load
2-19 on a shared 18-core host:

| change | lines added (all / `src` .rs) | mutants | outcome | mutants run | `check --mutants` wall |
|---|---|---|---|---|---|
| f1fc25c (factorium#110) | 69 / 32 in 4 files | 4 | 4 unviable | 3 min | 389s |
| 3b6afc1 (factorium#83) | 48 / 30 in 2 files | 16 | 2 caught, 14 unviable | 7 min | 511s |

The wall time is one instrumented test run, plus cargo-mutants' unmutated
baseline (100-118s build, 66-69s test), plus the mutants: an unviable one
about 1s, a caught one an incremental build (7s) and its test run (18s and
200s here; the second mutant made a test run long before it failed). The
whole workspace has about 1960 mutants (factorium#93).

### Per-test lines (slimfit#32)

factorium aecedfe: a clone with its build warm, 234 tests. Each run was
made under factorium's `worker exclusive`, one after the other, on
2026-11-19, with a 1-minute load of 6-14 on a shared 18-core host. Wall
time of the Rust preset's test command (build, tests, coverage report and
the per-test record):

| test command | round 1 | round 2 |
|---|---|---|
| no per-test record (`cargo llvm-cov nextest`, no runner) | 20.9s | 20.9s |
| ADR 0007 as first built: `llvm-profdata show` per profile, one at a time (functions only) | 24.9s | 25.0s |
| this ADR: `llvm-profdata merge` + `llvm-cov export` per test, one per core (functions and lines) | 22.5s | 22.4s |

So the per-test record costs about 1.5s over the plain run, less than the
functions-only record it replaces. `slimfit next --fresh` measured 23.0s
(`command_seconds`). The measurement grew to 7.7 MB (`reach.lines`), and
`reach.txt` is 16 MB. The function of each of the 234 tests was found,
for `keep`.

`slimfit next --all --kind redundant-test` counts **180** candidates (179
in an earlier run of the same tree: coverage that depends on timing). They
come from the 230 tests that run a source line: 44 subset candidates (by
function coverage, the control session found 86) and 136 by overlap alone.
factorium's scenarios all run the daemon's tick and their world's setup,
so most pairs share more than `MERGE_OVERLAP` of their lines. At 0.9 there
would be 102 overlap candidates, at 0.95 there would be 68. Each is still
decided by its assertions. Keeping one test with `slimfit keep` (0.04s,
from the cached measurement) left the count at 179, and the test was
proposed again once a line inside it changed.

### Shared setup left out of overlap (slimfit#34)

The 136 overlap candidates above were mostly setup: every scenario runs
the daemon's tick and its world's setup, so two scenarios that check
unrelated things shared 80-96% of their lines. The overlap is now computed
on the lines at most `COMMON_LINES` (half) of the suite's tests run.

Why half: on factorium aecedfe, of the 5084 lines the 230 tests run, 1261
are run by 60-80% of the tests (the world, the tick) and only 103 by
40-60%; the next band down is run by families of scenarios (the landing,
promotion and cut pipeline). The cut sits in that gap, so the count barely
moves with it: 109, 106, 107, 110, 111 candidates at 0.4, 0.45, 0.5, 0.55,
0.6 (87 at 0.25, 164 at 0.75).

Why not the subset too: the issue asked for both computed without the
common lines. Setup both tests run cannot make a subset (the candidate's
own lines must all be in the other too), so leaving it out of the subset
only adds candidates where the other test skips some setup, and it breaks
small suites, where the lines most tests run are the code under test: the
Rust sample has 4 tests, 3 of which run `add`, and its redundant copy of
the `adds` test was no longer proposed. On factorium that rule counts 108
(46 subset, 62 overlap); this one 107.

Measured: a clone of factorium at aecedfe, build warm, `slimfit next
--all --kind redundant-test --fresh` under factorium's `worker exclusive`
on 2026-11-19 (24.5s, 1-minute load 11 on a shared 18-core host); then
both builds of slimfit answered from that one cached measurement, so the
two counts see the same coverage (0.38s each):

| | subset | overlap | total |
|---|---|---|---|
| before (b6b83c0) | 45 | 133 | 178 |
| after | 45 | 62 | 107 |

(The earlier run above counted 180 = 44 + 136 on the same tree: coverage
that depends on timing.) All 71 dropped were overlap candidates; none was
added. Ten of each, decided by reading both tests' assertions (A is
proposed, B is the test it was named with):

Dropped:

| A | B | verdict |
|---|---|---|
| accounts::a_used_up_account_rests_an_hour_and_no_worker_starts_on_it_meanwhile | accounts::doctor_says_why_each_account_cannot_be_used_here_and_sessions_start_on_the_rest | keep: A checks a quota'd account's hour of rest and the start after it; B checks doctor's reason for each unusable account |
| admission::memory_disk_and_the_workers_share_each_hold_admission | watch::a_watch_home_orca_never_finds_is_given_up_after_five_more_tries | keep: A checks which bound holds admission; B counts Orca's retries for the watch's home |
| admission::settings_list_each_with_its_unit_refuse_an_unknown_name_and_bound_in_their_unit | path::an_issue_added_with_its_blockers_waits_for_them_from_the_start | keep: A checks the settings list and `no_setting`; B checks `blocked_by` at add and the start after the blocker closes |
| idle::idle_work_runs_when_the_host_is_idle_and_not_again_within_every | idle::red_idle_work_is_a_finding_and_opens_an_episode | keep: A checks when green idle work runs and reruns; B checks red idle work's finding and episode |
| one_harness::a_shell_s_prompt_is_told_from_a_harness_s_screen | one_harness::a_harness_s_question_before_its_screen_is_up_is_not_typed_at | keep: A classifies shell prompts against harness screens; B refuses a pre-screen question, then types once up |
| path::closing_an_issue_or_leaving_it_to_a_human_ends_its_worker | path::blocked_issues_and_ones_waiting_for_a_human_wait_and_the_oldest_ready_one_starts_first | keep: A checks workers ended and worktrees kept or not; B checks start order under blocks |
| queue::a_gate_run_holding_others_up_past_the_hold_bound_is_ended | path::a_red_landing_tells_the_worker_why_and_the_second_red_goes_to_the_watch | keep: A ends a gate past the hold bound; B checks the red-landing message and the second red's escalation |
| tracker::an_issue_waits_for_others_of_its_checkout | tracker::an_issue_waiting_for_a_human_is_listed_so_until_readied | keep: A checks `issue_block`'s dedupe and refusals; B checks `waiting_for_human` and `issue_ready` |
| terminals::a_close_orca_refuses_is_recorded_and_named_by_doctor | path::orca_refusing_to_remove_a_worktree_is_an_error_and_the_worktree_stays | keep: A records a refused terminal close and doctor names it; B refuses a close whose worktree removal fails |
| path::a_closed_issue_takes_no_more_commands | comment::a_comment_with_no_worker_on_the_issue_is_only_recorded | keep: A refuses a comment on a closed issue (`ended`); B records a comment with `delivery` none |

Every dropped pair shared only the world, the checkout, the kit and the
tick.

Remaining:

| A | B | kind | verdict |
|---|---|---|---|
| queue::beside_what_runs_one_heavy_command_starts_per_reading | queue::beside_what_runs_asks_in_one_second_start_one_per_settled_reading | subset | delete: B checks one start per settled reading more precisely; A adds only doctor's `started_at` null on the waiting entry |
| base::a_local_base_with_its_own_commits_is_left_and_is_a_finding | base::a_local_base_checked_out_over_changes_is_left_and_is_a_finding | overlap | merge: one flow, differing only in how the base is dirtied, the finding's text and the undo: a table of cases |
| one_harness::a_codex_worker_is_told_the_repos_rules_it_does_not_read_from_its_start_directory | one_harness::a_codex_worker_runs_on_the_host_s_model_settings_and_no_other_setting_of_theirs | subset | merge: both stage one codex worker and read its launch line; B's setup can hold A's CLAUDE.md |
| tracker::what_one_run_records_the_next_run_reads | tracker::issues_are_numbered_per_checkout_from_one | subset | merge: B numbers issues in one run; A adds only reading them back through a second run, which B's setup can take |
| admission::a_terminal_orca_no_longer_knows_is_gone | admission::orca_failing_to_answer_is_no_gone_terminal | subset | keep: A checks a handle Orca no longer lists ends `waiting`; B never drops the handle |
| line::a_cut_installs_once_brew_update_sees_its_version | terminals::a_landed_worker_s_terminal_is_closed | overlap | keep: A checks brew update's retries; B a landed worker's terminal; both run the landing pipeline |
| binary::exclusive_runs_its_words_as_given | binary::exclusive_runs_at_a_gates_priority_unless_asked_for_background | subset | keep: argv passed through vs the priority it runs at |
| line::one_landing_per_checkout_at_a_time_the_first_to_ask_first | line::a_promotion_takes_every_landing_up_to_its_tip | subset | keep: landing order vs what one promotion takes |
| watch::an_escalation_makes_the_look_due_on_the_next_tick | watch::an_escalation_becomes_an_episode_and_one_round_sent_to_the_watch | overlap | keep: when the look is due vs what the round carries |
| stall::a_heavy_queue_held_by_what_runs_is_a_finding_only_past_the_hold_bound_and_the_stall_bound | stall::a_heavy_queue_that_starts_nothing_past_the_stall_bound_is_a_finding_until_its_head_is_taken_off | subset | keep: different conditions, bounds and fix |

So the remaining ones hold real delete and merge candidates (4 of these
10), and still many keeps: the next band of shared lines is a scenario
family's pipeline (landing, promotion, cut), which about a third of the
tests run, below the cut. `slimfit keep` records those.
