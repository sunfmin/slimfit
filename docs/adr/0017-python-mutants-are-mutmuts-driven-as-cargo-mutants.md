# 17. Python's mutants are mutmut's, driven as cargo-mutants

## Context

Mutants run only on a change, each picked by name (ADR 0006, 0008). A
Python mutation tool has to be cut to a change's lines and run one mutant
by name. mutmut and cosmic-ray can both be cut so, by a driver; neither
takes a diff itself.

## Decision

- The Python preset's mutants tool is mutmut, through uv, driven by
  `src/presets/python-mutants.py` as slimfit drives cargo-mutants: it lists
  the mutants of the lines a change adds and runs the picks by name, in a
  copy of the tree.
- Not cosmic-ray: it mutates the working tree in place.

## Consequences

- mutmut runs each mutant only with the tests that ran its function, as
  cargo-mutants does here (ADR 0007).
- The driver tracks mutmut's internals; its version is pinned (`MUTMUT` in `src/tools.rs`).
