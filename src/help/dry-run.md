What `slimfit init` or `slimfit ratchet` would write, asked without writing it.

```sh
slimfit init --dry-run
slimfit ratchet --dry-run
```

A dry run measures as the command does (the same tests, and the same mutants with the same flags) and answers as it does, plus `would_write`: each file it would change, by its path in the repo, with the file's whole new text. A file it would leave as it is is not listed: `{}` means nothing would change. `init` can write `slimfit.toml`, `.gitignore` and `.slimfit/baseline.json`; `ratchet` only `.slimfit/baseline.json`.

It writes none of them, and nothing else of slimfit's: its commands write their reports to a directory outside the repo, removed after, and the measurement is not cached in `.slimfit/out/`. The tools it runs build as they always do (`target/`, `build/`, `.build/` and the like).

- A refusal is the same as without `--dry-run`: a `ratchet` that would refuse answers the same error and exits 2.
- It takes a cached measurement as the command does, but caches none: when it measured, the real run after it measures again, at the command's cost again (see [costs](help:costs)).
