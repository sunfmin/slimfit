# 13. Releases are signed and notarized by the tap's workflow

## Context

Until slimfit#100 a cut (`packaging/release.sh cut`) built on the cutting
host, for aarch64 only, and published from there: the archive to the tap's
release, the formula to the tap. The binary was only ad hoc signed
(`adhoc,linker-signed`). It ran through brew, but Intel Macs could not run it,
and Gatekeeper rejected a copy downloaded in a browser (`spctl`: "rejected").

The owner, 2026-11-22: 「用我的 developer ID 签上是不是更好？把所有的工具都变成
universal。」「把公开的工具全部在 GitHub CI 上签。」「这个密钥的环境变量可以统一配置到
账号上面吗？让所有仓库都能看到。」 A personal account has no account-wide secrets.
sunfmin/mytokens already signs and notarizes in Actions (its ADR-0017), with
the secrets in its own repo.

## Decision

Invariant: **the Developer ID identity and the notary key live in one repo,
sunfmin/homebrew-tap, and only its `release` workflow signs.** slimfit holds
no signing secret and no release workflow.

- **slimfit builds.** `packaging/release.sh build <vX.Y.Z> <outdir>` builds
  `aarch64-apple-darwin` and `x86_64-apple-darwin` with `SLIMFIT_RELEASE`
  set, checks the binary says the version, `lipo -create`s them, and leaves
  the universal `slimfit` and `slimfit.rb` in `<outdir>`. The formula has no
  `depends_on arch`; its `url` and `sha256` are `@URL@` and `@SHA256@`, for
  the tap to fill in, so the release's and the archive's names live only in
  the tap. `dry` (in the gate) and `cut` run the same `build`.
- **The tap signs and publishes.** Its `.github/workflows/release.yml`
  (contract in its `.github/workflows/README.md`) checks out slimfit at the
  tag, runs `build`, signs with hardened runtime and a secure timestamp,
  notarizes, verifies, and publishes the release `slimfit-v<X.Y.Z>` (one
  archive, `slimfit-<X.Y.Z>-macos-universal.tar.gz`) and `Formula/slimfit.rb`
  with its own `GITHUB_TOKEN`. A bare binary cannot be stapled: Gatekeeper
  checks its ticket online.
- **The cut waits.** `cut` builds here first, pushes the tag, runs
  `gh workflow run release.yml -R sunfmin/homebrew-tap -f repo=slimfit -f
  tag=<tag>`, finds that run (the newest named `slimfit <tag>`, newer than any
  before the dispatch) and waits for it, bounded by `RELEASE_WAIT_LIMIT`
  (2400 s from the cut's start); a failed run fails the cut and prints its `--log-failed`.
- **factorium's tag.** factorium tags HEAD after the cut's steps with
  `git tag -a` and pushes it, and that push fails on a tag origin already
  holds. So the cut takes away the tag it pushed when it ends, however it
  ends; a tag already on origin at HEAD is left alone, and one on another
  commit fails the cut.

## Consequences

- One copy of the secrets serves every tool; factorium, truefit, sesame and
  rolesmith follow the same contract, each adding only a `build`.
- Anyone who can change the tap's workflow on `main` can use the identity:
  the tap's push access, who can already publish every formula.
- The gate builds two more release targets (`dry`), and a cut takes the CI
  run's minutes: `[cut] limit` covers the host's build plus the wait.
- A cut no longer needs a tap token or a deploy key on the host: only `gh`
  logged in as a user who may push slimfit and dispatch the tap's workflow.
