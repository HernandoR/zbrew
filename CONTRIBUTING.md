# Contributing to zbrew

Thanks for your interest in contributing to zbrew! This document provides guidelines for contributing to the project.

## Licensing

By contributing to zbrew, you agree your contributions will be dual-licensed under either [Apache](./LICENSE-APACHE.md) OR [MIT](./LICENSE-MIT.md), at the licensee's choice.

## Soft Prerequisites

- Rust 1.90 or later
- Access to either a macOS or Linux machine

## A note on LLM usage
While we encourage the use of LLM's for thinking through problems, helping with tests, and even writing code, we simply 
cannot accept or tolerate PRs with no clear guidance or thought put into them.

**_Please understand_** that we reserve the right to simply close your PR if it exhibits clear indicators 
of heavy LLM usage. We understand you are excited to contribute but the code must reach a level of quality
that's typically achieved through thoughtful engagement in the community and the issues/agenda of zbrew- NOT
by throwing a prompt into an LLM and opening a PR with no direction.

If you ever need help or want to walk through an issue or idea that you have with one of the maintainers, feel free to join 
the [community discord](https://discord.gg/TVatsQBFJt); we would be more than happy to assist you.

## Project Structure

zbrew is organized as a Cargo workspace with three crates, all under `crates/`:

- `crates/zb_core`: Core data models and domain logic (formula resolution, bottle selection)
- `crates/zb_io`: I/O operations (API client, downloads, extraction, installation)
- `crates/zb_cli`: Command-line interface

Any changes you make that touch several crates should be organized properly. See [commit hygiene](#commit-hygiene)

## General Development Workflow

We prefer that a PR is linked to an open issue or previously discussed through other channels. 
If you are introducing changes that aren't otherwise reported or tracking please either reach 
out in the Discord to give us a heads up or open an issue first to discuss your changes.

**General flow:**
1. Fork the repo
2. Make your changes and ensure, at the _least_:
   - Code is formatted: `cargo fmt --all`
   - No clippy warnings: `cargo clippy --workspace --all-targets -- -D warnings`
   - Unit tests pass: `cargo test --workspace`
   - Integration tests pass: `cargo test --workspace -- --ignored` (or `just test` for all tests)
> [!NOTE] 
> These will run in CI but it's best you clean up your code _before_ opening a PR to ensure a quick 
> turnaround!

> [!IMPORTANT]
> CI tests every pull request on Linux. The macOS job runs automatically when a change touches
> macOS-specific code — Mach-O patching, codesigning, cask installation — because Linux CI does
> not compile any of it. It also always runs on `release-*` branches and release tags.
>
> Detection is by path (`macos.rs`, `macho.rs`, anything with `darwin` or `cask` in the name) and
> by content (any `.rs` file containing `cfg(target_os = "macos")`). Neither test alone is enough:
> `macos.rs` is gated at the module level and so contains no `target_os` attribute of its own,
> while a file like `cellar/materialize.rs` is portable apart from one `cfg` block inside it.
>
> Add the `ci-macos` label to run the macOS job on a change that detection does not flag. macOS
> runners are free on public repositories, so this is not about cost — the macOS concurrency
> allowance is small, and running the job on changes that cannot affect it only makes other
> people queue.

### Using Just

This project includes a `Justfile`, Install [just](https://github.com/casey/just) and use these commands instead of `cargo` (for ease of development):

- `just build` Check formatting, lint, then build the binary (Builds debug binary)
- `just install` Build and install zb to $HOME/.local/bin (Customizable with `$ZBREW_BIN`), along with the man pages `zb man` generates (Customizable with `$ZBREW_MAN`)
- `just uninstall` Remove all zbrew installations and configurations
- `just fmt` Format code with rustfmt
- `just fmt-check` Check code formatting
- `just lint` Run clippy with strict warnings
- `just test` Run all workspace tests (unit & integration)

Before creating a PR make sure you `build` your changes and `test` them.

The man pages are rendered from the `clap` command tree in `crates/zb_cli/src/cli.rs`, so a new
subcommand, option or help string documents itself — there is no checked-in roff to edit.
`zb man` writes `zb.1` to stdout, `zb man --output-dir DIR` writes it and a page per
subcommand.

3. Write tests for new functionality. Each module should have accompanying tests.
4. Commit your changes with clear, descriptive commit messages (see below)
5. Push to your fork and submit a pull request.

## Commit hygiene
We ask that you follow the format below for commits:
```bash
[fix / feat]($crate): description
```

for instance:
```bash
fix(zb_cli): foo bar moo baz
```
Allowed prefixes:
```bash
fix      # -> fixes a bug or regression
feat     # -> new feature
chore    # -> housekeeping (deps, typos in docs, etc.)
tests    # -> added, changed or removed tests
ci       # -> changes to ci
refactor # -> refactored code
perf     # -> performance related
build    # -> changes to build system (i.e. ext deps, tooling, scripts, etc)
```

Generally speaking, we also ask that you please write isolated, [atomic commits](https://en.wikipedia.org/wiki/Atomic_commit). 
This means if you are approaching a PR that touches various parts of the codebase for example, ensure that your commits
are contained and cleanly separated, properly describing/notating which commits belong where.


## Testing

- Unit tests should be colocated with the code in `mod tests` blocks
- Use `tempfile` for filesystem tests
- Use `wiremock` for HTTP mocking in integration tests
- Tests should be deterministic and not rely on external network access

## Running Benchmarks

To benchmark performance:

```bash
just bench --full
```

This runs a 100-package installation suite comparing zbrew to Homebrew. This is especially crucial to run if you are 
planning on contributing to performance/optimization related changes.

Useful options:

```bash
just bench --quick
just bench --dry-run
just bench --full results/
just bench --format csv --output benchmark.csv
just bench --log bench.log
```

Notes:
- Defaults to the quick package list (22 packages); use `--full` for all 100.
- `--full [dir]` writes all formats (txt/json/csv/html) to the directory.
- `--output` infers format from file extension when `--format` is omitted.
- Output includes cold + warm cache speedups per package.

### macOS Homebrew permissions

On macOS, Homebrew should be installed with a user-writable prefix. If `just bench` fails with a permission error, fix it by running:

```bash
sudo chown -R "$(whoami)" "$(brew --prefix)"
```

## Questions?

For further questions, open an issue on GitHub.

## Releases and the Homebrew tap

Pushing a `v*` tag runs `.github/workflows/release.yml`, which builds every
target, publishes the GitHub release, and then regenerates the formula in the
[`HernandoR/homebrew-zbrew`](https://github.com/HernandoR/homebrew-zbrew)
tap.

The tap has to be a separate repository because `brew tap <user>/<repo>`
resolves only to `github.com/<user>/homebrew-<repo>`. Keeping the two in sync
by hand is what stranded the upstream tap five releases behind (#16, #17), so
`.github/scripts/generate_formula.py` writes the formula from the release's
`SHA256SUMS` and the workflow pushes it.

On Linux the generator prefers the statically linked `*-musl` assets, which
have no glibc floor (#10), and falls back to the glibc assets when a release
does not carry musl builds.

This needs one repository secret:

| Secret | Value |
| --- | --- |
| `TAP_GITHUB_TOKEN` | A fine-grained PAT scoped to `HernandoR/homebrew-zbrew` with **Contents: read and write**, and no other permission or repository. |

`GITHUB_TOKEN` cannot be used: it is scoped to this repository and cannot push
to the tap. When the secret is absent the release still succeeds and the job
logs a warning, so the tap can be updated by hand.

To change the formula, edit the generator — the file in the tap is overwritten
on every release.

## Syncing with upstream

This repository is a maintained fork of
[lucasgelfond/zerobrew](https://github.com/lucasgelfond/zerobrew). Upstream is
mirrored on the `upstream` branch of this repository so that upstream commits can
be reviewed and cherry-picked from the same remote.

Branch layout:

- `main` — the fork's development branch. Fork releases are cut from here.
- `upstream` — a pristine mirror of `lucasgelfond/zerobrew:main`. Never commit
  to it directly; only fast-forward it.

Recipes (they add the `lucasgelfond` git remote on first use):

```bash
just upstream-sync              # fast-forward `upstream` to lucasgelfond/main and push to origin
just upstream-diff              # list upstream commits missing from the current branch
just upstream-cherry-pick <sha> # cherry-pick upstream commit(s) into the current branch
```

Adopting an unmerged upstream pull request:

```bash
gh pr checkout <number> -R lucasgelfond/zerobrew -b adopt/pr-<number>
# rebase onto main, resolve conflicts, then open a PR against this fork's main
```

Every open upstream issue and adoptable upstream PR has a tracking issue in this
repository (label `upstream-pr` for PRs), grouped into [milestones](https://github.com/HernandoR/zbrew/milestones) and shown on the
[project board](https://github.com/users/HernandoR/projects/3). Reference both the tracking issue and the upstream number in
the commit message (for example `Upstream: lucasgelfond/zerobrew#393`) so the board stays traceable.
