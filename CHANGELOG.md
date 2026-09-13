# Changelog

All notable changes to zerobrew will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.3] - 2026-09-13

### Added
- Statically linked musl builds for Linux (`x86_64` and `aarch64`). `install.sh` reads the host glibc version and prefers the musl asset below 2.35, then verifies the downloaded binary actually runs before keeping it, so `zb` works in environments such as Google Colab whose glibc is older than the one the release binaries are built against ([#10](https://github.com/HernandoR/zerobrew/issues/10))
- The release workflow regenerates the formula in the [`HernandoR/homebrew-zerobrew`](https://github.com/HernandoR/homebrew-zerobrew) tap from the release's `SHA256SUMS` and pushes it. The tap must be a separate repository, because `brew tap <user>/<repo>` resolves only to `github.com/<user>/homebrew-<repo>`; keeping the two in sync by hand is what left the upstream tap stranded five releases behind ([#16](https://github.com/HernandoR/zerobrew/issues/16), [#17](https://github.com/HernandoR/zerobrew/issues/17))
- Homebrew's `which` and `which_all`, and the `ENV` helpers `append_to_cflags`, `remove_from_cflags`, `remove`, `deparallelize` and `make_jobs`, in the Ruby formula shim, so a formula whose `install` block calls them no longer aborts the build with `undefined method` ([#20](https://github.com/HernandoR/zerobrew/issues/20))
- A weekly `upstream sync` workflow fast-forwards the `upstream` branch from `lucasgelfond/zerobrew` and opens or updates an `upstream-triage` issue listing commits that have not been adopted here, so late activity on the unmaintained original is noticed ([#23](https://github.com/HernandoR/zerobrew/issues/23))

### Changed
- The macOS CI job runs automatically when a change touches macOS-specific code, detected both by path (`macos.rs`, `macho.rs`, anything named for `darwin` or `cask`) and by content (any `.rs` file containing `cfg(target_os = "macos")`). Neither test alone is sufficient: `macos.rs` is gated at the module level and carries no `target_os` attribute of its own, while a file such as `cellar/materialize.rs` is portable apart from one `cfg` block inside it. Linux CI compiles none of that code, so relying on someone to remember the `ci-macos` label meant macOS changes merged unverified; the label remains as a manual override
- Release notes lead with `install.sh` and explain that the macOS binaries are not notarized. `zb-darwin-arm64` carries only the ad-hoc signature the linker adds and `zb-darwin-x64` has no signature at all, so a browser download trips Gatekeeper. `install.sh` and Homebrew are unaffected, because `com.apple.quarantine` is set by the downloading application and `curl` does not set it
- The READMEs warn that the upstream `lucasgelfond/homebrew-zerobrew` tap is still on 0.1.1 and that this fork does not control it ([#16](https://github.com/HernandoR/zerobrew/issues/16), [#17](https://github.com/HernandoR/zerobrew/issues/17))
- `object` 0.40 wraps the ELF and Mach-O constants in newtypes (`ProgramType`, `FileType`, `LoadCommandType`, `SectionType`). The patchers unwrap them where they are compared against raw header fields, including the fields `arwen` exposes from its own older `object` ([#76](https://github.com/HernandoR/zerobrew/pull/76))
- The release workflow runs on this fork: the build job was gated to `lucasgelfond/zerobrew` and so had never produced a binary here. A tag whose name carries a pre-release suffix (`v0.3.3-rc.1`) is published as a GitHub pre-release, leaving `releases/latest` on the last stable release ([#2](https://github.com/HernandoR/zerobrew/issues/2))
- `install.sh` clones and downloads from `HernandoR/zerobrew` instead of the upstream repository, so the installer no longer silently installs upstream's last release ([#3](https://github.com/HernandoR/zerobrew/issues/3))
- The READMEs install from `raw.githubusercontent.com/HernandoR/zerobrew/main/install.sh` and `brew install HernandoR/zerobrew/zerobrew`. `https://zerobrew.rs/install` still serves the upstream installer and this fork does not control that domain ([#3](https://github.com/HernandoR/zerobrew/issues/3), [#28](https://github.com/HernandoR/zerobrew/issues/28))
- The Homebrew compatibility workflow resolves its `setup-homebrew` action again: `Homebrew/actions` renamed its default branch from `master` to `main`, so every run failed at "Set up job" without testing anything ([#4](https://github.com/HernandoR/zerobrew/issues/4))
- The Homebrew compatibility workflow runs on Linux only by default, matching the test workflow: the macOS entry is added when the workflow is dispatched with `macos` or when a pull request carries the `ci-macos` label ([#4](https://github.com/HernandoR/zerobrew/issues/4))
- Bump MSRV to 1.96, required to build the latest `cargo-audit` in CI ([#393](https://github.com/lucasgelfond/zerobrew/pull/393))
- Refresh `Cargo.lock` for audit findings: `crossbeam-epoch` (RUSTSEC-2026-0204), `quinn-proto` (RUSTSEC-2026-0185), and `anyhow` (RUSTSEC-2026-0190) ([#393](https://github.com/lucasgelfond/zerobrew/pull/393))
- Refresh `Cargo.lock` for audit findings: `h2` 0.4.19 (RUSTSEC-2026-0258) and `chacha20` 0.10.2, replacing a yanked release ([#64](https://github.com/HernandoR/zerobrew/pull/64))
- The macOS CI job is opt-in: pull requests run the test suite on Linux, and the macOS matrix entry is added only when the pull request carries the `ci-macos` label, when the workflow is dispatched with `macos`, or when the ref is a `release-*` branch or a release tag ([#64](https://github.com/HernandoR/zerobrew/pull/64))
- Ad-hoc re-signing now passes `--preserve-metadata=entitlements,requirements,flags,runtime`, as Homebrew does, so entitlements and the hardened runtime survive patching ([#1](https://github.com/HernandoR/zerobrew/issues/1))
- `/usr/local` is only rewritten when what follows it is Homebrew's (`/Cellar/`, `/Caskroom/`, `/Homebrew/`, `/opt/`), leaving genuine system paths such as `/usr/local/lib` alone ([#1](https://github.com/HernandoR/zerobrew/issues/1))
- Walking a keg for Mach-O files reads four magic bytes per file instead of the whole file ([#1](https://github.com/HernandoR/zerobrew/issues/1))
- Security reports go to a private advisory on this repository instead of the upstream maintainer's email, and the response-time commitments written for the upstream project are replaced with a best-effort statement. Code of Conduct reports go to this repository's maintainer ([#22](https://github.com/HernandoR/zerobrew/issues/22))
- Dependabot opens weekly dependency update pull requests for the cargo workspace, for GitHub Actions and for the Eleventy site's pnpm lockfile, so advisories do not pile up between manual lockfile refreshes. Every open Dependabot alert on this repository was npm, in `site/pnpm-lock.yaml` ([#21](https://github.com/HernandoR/zerobrew/issues/21))

### Fixed
- `zb reset` refuses to delete a system directory. It clears the contents of `--root` and `--prefix` and can escalate to `sudo rm -rf`, but the only check rejected `..`, control characters and over-long paths — it accepted `/`, `/usr`, `/home` and `$HOME`. Both values come straight from the flags or from `ZEROBREW_ROOT`/`ZEROBREW_PREFIX`, so a stale environment variable was enough to wipe a machine. Paths must now be absolute, outside a list of well-known system directories, not the user's home, and at least two components deep; the check runs before the installer builds its directory tree underneath the unvalidated path
- The chunked downloader no longer holds an entire download in memory. Every chunk was kept in a map purely so the digest could be computed in offset order, although each had already been written to the blob file, so peak memory was the full file size for any bottle over the 10 MiB chunking threshold, multiplied by concurrent downloads. Only the chunk offset and length now cross the channel, and the digest is computed by streaming the finished blob back off disk — which additionally proves the bytes landed at the offsets they were meant to
- Entitlements survive patching for helper executables outside `bin`. lima 2.x runs Virtualization.framework from `libexec/lima/lima-driver-vz`, spawned as its own process and validated on its own signature, but re-signing selected only paths containing `/bin/` on the grounds that other Mach-O files inherit signing from their loader — true of libraries, not of a helper binary. `bin`, `sbin` and `libexec` are now matched, against the keg-relative path so that a prefix containing `bin` does not drag the whole keg into re-signing ([#11](https://github.com/HernandoR/zerobrew/issues/11))
- Patching a keg no longer reports "Failed to patch ELF ... parse error" for relocatable objects. The file list was built from the four-byte ELF magic alone and handed everything to the rewriter, which cannot represent the `SHT_GROUP` (COMDAT) sections every C++ object file carries — llvm ships thousands of them. Files are now classified by `e_type`, and anything that cannot hold `DT_RPATH`, `DT_RUNPATH` or `PT_INTERP` is skipped quietly; genuine failures still warn ([#13](https://github.com/HernandoR/zerobrew/issues/13))
- Uninstalling a formula whose files were already deleted by hand no longer orphans its symlinks. `unlink_keg` discovers what to remove by walking the keg directory, so an absent keg meant nothing was unlinked, and the `keg_files` rows were then deleted — leaving dangling links in the prefix that not even `zb doctor --repair` could still see. Recorded links are now removed before the transaction, and only when they are genuinely dangling or still point into the keg ([#14](https://github.com/HernandoR/zerobrew/issues/14))
- Linking is all-or-none. A failure partway through left every symlink created so far in place, and the pre-flight conflict scan did not report a keg directory landing on another keg's live file symlink, so linking deleted that link and then failed trying to read a regular file as a directory. Linking now records an undo log and replays it on any error, restoring links it had replaced, and that case is reported as a conflict ([#6](https://github.com/HernandoR/zerobrew/issues/6))
- `zb init` no longer destroys a shell config it cannot decode: a `~/.zshrc`, `~/.bash_profile` or `~/.profile` that is not valid UTF-8 used to be read as an empty file and then overwritten with nothing but the managed zerobrew block, losing everything the user had in it; such a file is now left byte-for-byte untouched and the block is printed for the user to add by hand, while a config that simply does not exist yet is still created as before ([#18](https://github.com/HernandoR/zerobrew/issues/18), [#400](https://github.com/lucasgelfond/zerobrew/pull/400))
- Mach-O patching validates where it writes: path strings are rewritten only inside the ranges the Mach-O structure declares as C string storage — load command strings and `S_CSTRING_LITERALS` sections — so a path that happens to appear in code, in a pointer table or in length-prefixed Rust/Go string data is no longer silently corrupted ([#1](https://github.com/HernandoR/zerobrew/issues/1))
- A shortened path keeps its tail: replacements rewrite the whole string instead of splicing NULs in behind the new prefix, which used to truncate `/opt/homebrew/opt/git/libexec/git-core` down to the prefix alone ([#1](https://github.com/HernandoR/zerobrew/issues/1))
- Failing to re-sign a patched binary now fails the install instead of only logging, so a keg cannot ship binaries that Gatekeeper kills at first use; `install_name_tool` and `otool` exit statuses are checked as well, where before only process startup was ([#1](https://github.com/HernandoR/zerobrew/issues/1))
- Load command paths that are too long to patch in place are rewritten with `install_name_tool`, which resizes them properly, and `LC_RPATH` entries are now patched too ([#286](https://github.com/lucasgelfond/zerobrew/issues/286))
- A keg whose patching failed is removed rather than left behind, where the next run would have mistaken it for a finished install ([#1](https://github.com/HernandoR/zerobrew/issues/1))
- Relink on upgrade/reinstall: symlinks owned by another version of the same formula — including dangling links left behind by removed kegs — are now replaced during linking instead of failing the link step as conflicts with the formula itself, which left `bin`/`opt` pointing at the old version while the DB reported the new one ([#393](https://github.com/lucasgelfond/zerobrew/pull/393))
- Versioned formulae are no longer treated as keg-only just because their name contains `@`: keg-only status is read from the formula's own `keg_only` field, as Homebrew defines it, so `gcc@13`, `python@3.12` and other versioned-but-linkable formulae have their binaries linked into the prefix again instead of staying hidden in the Cellar ([#19](https://github.com/HernandoR/zerobrew/issues/19), [#403](https://github.com/lucasgelfond/zerobrew/pull/403))
- Skipping the link step no longer claims "versioned formula" as the reason, which is no longer a reason zerobrew keeps a keg unlinked; a formula that declares a keg-only reason string still reports it, and the rest report "keg-only formula" ([#19](https://github.com/HernandoR/zerobrew/issues/19))

## [0.3.2] - 2026-06-11

### Security
- Verify SHA-256 checksums for resource and URL patch downloads in the formula build shim before extraction or application (CVE-2026-53970)

## [0.3.1] - 2026-05-30

### Fixed
- Centralize a single, sandbox-tolerant rustls `ClientConfig` in `network::tls`: prefer native roots, and fall back to the bundled webpki-roots Mozilla roots when no system trust store is available ([#375](https://github.com/lucasgelfond/zerobrew/pull/375))
- Correct migration behavior on unplannable formulas ([#380](https://github.com/lucasgelfond/zerobrew/pull/380))

### Changed
- Clarify standalone installer shell setup and update flow: surface `zb init` output, print shell-specific reload commands after shell config changes, print exact `export`/fish commands for `--no-modify-path`, report installed/updated/already-current status on reruns, and warn when an older `zb` still appears earlier in `PATH` ([#381](https://github.com/lucasgelfond/zerobrew/pull/381))
- Clarify `zb update` help/output and README update docs so users know `zb update` refreshes package metadata while the installer or Homebrew updates the `zb` binary itself ([#381](https://github.com/lucasgelfond/zerobrew/pull/381))

## [0.3.0] - 2026-05-29

### Added
- Eleventy-based homepage with responsive styling, interactive panels, benchmark/install content, and site assets ([#309](https://github.com/lucasgelfond/zerobrew/pull/309))
- `zb doctor` command with `--repair` flag for state diagnosis, recovery, orphaned store entries, and broken symlinks ([#314](https://github.com/lucasgelfond/zerobrew/pull/314))
- Chinese translation of the README ([#316](https://github.com/lucasgelfond/zerobrew/pull/316))
- `zb upgrade` command to upgrade installed packages, with `--build-from-source` and `--no-link` flags; supports upgrading all outdated packages or specific ones by name ([#369](https://github.com/lucasgelfond/zerobrew/pull/369))

### Fixed
- Validate root/prefix paths before passing to sudo to prevent shell injection ([#311](https://github.com/lucasgelfond/zerobrew/pull/311))
- Regex matches only version segments within Cellar-style paths when patching Mach-O binary strings ([#317](https://github.com/lucasgelfond/zerobrew/pull/317))
- Update vulnerable `aws-lc-sys`, `aws-lc-rs`, and `rustls-webpki` dependencies ([#318](https://github.com/lucasgelfond/zerobrew/pull/318))
- Make `just fmt` apply formatting and document the workflow ([#319](https://github.com/lucasgelfond/zerobrew/pull/319))
- Resolve formula aliases and oldnames after API 404s ([#332](https://github.com/lucasgelfond/zerobrew/pull/332))
- Skip linking `libexec` Python `site-packages` paths to avoid conflicts ([#368](https://github.com/lucasgelfond/zerobrew/pull/368))
- Make `zb upgrade` clean old cellar metadata, stay idempotent after download failures, and exit non-zero for missing requested packages ([#369](https://github.com/lucasgelfond/zerobrew/pull/369))
- Ignore stale macOS prefix environment defaults when initializing or resolving paths ([#372](https://github.com/lucasgelfond/zerobrew/pull/372))
- Resolve Linux `uses_from_macos` dependencies, rewrite Linuxbrew bottle paths, and restrict Linux bottle fallback by architecture ([#373](https://github.com/lucasgelfond/zerobrew/pull/373))

### Changed
- Split monolithic install module into focused submodules ([#312](https://github.com/lucasgelfond/zerobrew/pull/312))
- Split monolithic download module into focused submodules ([#313](https://github.com/lucasgelfond/zerobrew/pull/313))
- Document Homebrew tap installation as an alternative install method ([#325](https://github.com/lucasgelfond/zerobrew/pull/325))
- Refresh dependency lockfile entries ([#330](https://github.com/lucasgelfond/zerobrew/pull/330))
- Make migration install only leaf formulae from Homebrew ([#333](https://github.com/lucasgelfond/zerobrew/pull/333))
- Prefer direct Homebrew install instructions in README files ([#337](https://github.com/lucasgelfond/zerobrew/pull/337))
- Refresh `Cargo.lock` for audit findings ([#345](https://github.com/lucasgelfond/zerobrew/pull/345), [#363](https://github.com/lucasgelfond/zerobrew/pull/363))
- Pin release workflow Ubuntu runners to 22.04 for stability ([#352](https://github.com/lucasgelfond/zerobrew/pull/352))
- Add CLI help text for command arguments and flags ([#355](https://github.com/lucasgelfond/zerobrew/pull/355))
- Add and then revert the security scanning workflow ([#354](https://github.com/lucasgelfond/zerobrew/pull/354), [#358](https://github.com/lucasgelfond/zerobrew/pull/358))


## [0.2.1] - 2026-03-14

### Fixed
- Fix `zb outdated` panic caused by clap type mismatch between global `verbose` (u8 count) and subcommand `verbose` (bool) flags ([#308](https://github.com/lucasgelfond/zerobrew/pull/308))

## [0.2.0] - 2026-03-12

### Added
- Batch processing for `zb migrate` command ([#285](https://github.com/lucasgelfond/zerobrew/pull/285))
- `zb outdated` command with `--quiet`/`--verbose`/`--json` output modes ([#266](https://github.com/lucasgelfond/zerobrew/pull/266))
- `zb update` command ([#266](https://github.com/lucasgelfond/zerobrew/pull/266))
- Tracing-based internal logging with `-v`/`--verbose` and `-q`/`--quiet` flags ([#275](https://github.com/lucasgelfond/zerobrew/pull/275))
- Configurable UI theme and writer-based output layer ([#274](https://github.com/lucasgelfond/zerobrew/pull/274))
- Fuzzy formula suggestions on missing package errors ([#279](https://github.com/lucasgelfond/zerobrew/pull/279))
- `ZEROBREW_API_URL` support and persistent API cache ([#252](https://github.com/lucasgelfond/zerobrew/pull/252))
- Build provenance attestation in release workflow ([#247](https://github.com/lucasgelfond/zerobrew/pull/247))

### Fixed
- Added SQLite schema versioning with sequential migrations and downgrade protection([#305](https://github.com/lucasgelfond/zerobrew/pull/305))
- Global lock on installer to prevent concurrent install corruption ([#304](https://github.com/lucasgelfond/zerobrew/pull/304))
- Strip zerobrew's bin paths from `PATH` during install to prevent dyld errors on re-install ([#289](https://github.com/lucasgelfond/zerobrew/pull/289))
- Warn when Mach-O in-place patching is skipped due to prefix length mismatch (Intel Mac) ([#286](https://github.com/lucasgelfond/zerobrew/issues/286))
- Prefer compatible macOS bottle tags over newer ones ([#283](https://github.com/lucasgelfond/zerobrew/pull/283))
- Ruby syntax backwards compatibility for source builds ([#282](https://github.com/lucasgelfond/zerobrew/pull/282))
- Skip extraction on raw binaries and copy to keg bin dir directly ([#278](https://github.com/lucasgelfond/zerobrew/pull/278))
- Chunked download robustness and memory efficiency ([#270](https://github.com/lucasgelfond/zerobrew/pull/270))
- Skip libexec virtualenv metadata links to avoid cross-formula conflicts ([#248](https://github.com/lucasgelfond/zerobrew/pull/248))
- Link formulas on Linux when Homebrew marks them keg-only ([#249](https://github.com/lucasgelfond/zerobrew/pull/249))
- Preprocess resolver before parsing ([#244](https://github.com/lucasgelfond/zerobrew/pull/244))

### Changed
- Eliminate unwraps, reduce allocations, decompose install path ([#292](https://github.com/lucasgelfond/zerobrew/pull/292))
- Removed `--yes` alias from global `--auto-init` flag ([#287](https://github.com/lucasgelfond/zerobrew/pull/287))

## [0.1.2] - 2026-02-15

### Added
- Local source build fallback — compile packages from source when no bottle is available ([#212](https://github.com/lucasgelfond/zerobrew/pull/212))
- `--build-from-source` / `-s` flag for `zb install` ([#212](https://github.com/lucasgelfond/zerobrew/pull/212))
- External tap and cask support with safer install/uninstall behavior ([#203](https://github.com/lucasgelfond/zerobrew/pull/203))
- GitHub release installs with clone fallback ([#198](https://github.com/lucasgelfond/zerobrew/pull/198))
- Source-only tap formula support with scoped parsing ([#232](https://github.com/lucasgelfond/zerobrew/pull/232))
- Resolve tap formulas from `Formula/`, `HomebrewFormula/`, and repo root ([#231](https://github.com/lucasgelfond/zerobrew/pull/231))
- `zb bundle dump` subcommand with Brewfile syntax support ([#218](https://github.com/lucasgelfond/zerobrew/pull/218))

### Fixed
- Include zbx binaries in GitHub releases ([#229](https://github.com/lucasgelfond/zerobrew/pull/229))
- Preserve execute bit when patching Mach-O binary strings ([#228](https://github.com/lucasgelfond/zerobrew/pull/228))
- Skip patching when new prefix is longer than old ([#227](https://github.com/lucasgelfond/zerobrew/pull/227))
- Prevent bricked installs from link conflicts, respect keg-only formulas ([#207](https://github.com/lucasgelfond/zerobrew/pull/207))
- Default macOS prefix to `/opt/zerobrew` to stay within the 13-char Mach-O path limit ([#206](https://github.com/lucasgelfond/zerobrew/pull/206))
- Shell init management and fish support ([#200](https://github.com/lucasgelfond/zerobrew/pull/200))
- Remove `-D` flag from install since directories are already created ([#221](https://github.com/lucasgelfond/zerobrew/pull/221))
- Force static liblzma linking and verify macOS binaries ([#222](https://github.com/lucasgelfond/zerobrew/pull/222))
- Formula token normalization across crates ([#230](https://github.com/lucasgelfond/zerobrew/pull/230))
- Default macOS prefix to root on install scripts ([#239](https://github.com/lucasgelfond/zerobrew/pull/239))

### Changed
- Refreshed README with banner and star history ([#224](https://github.com/lucasgelfond/zerobrew/pull/224))

## [0.1.1] - 2026-02-08

Initial release of zerobrew - a fast, modern package manager. We're excited for our pilot release and 
want to thank all of the support from all channels, as well as all of our contributors up to this point. 

To get an idea of the initial features zerobrew supports, take a look at the [README](https://github.com/lucasgelfond/zerobrew#readme).

See the [full commit history](https://github.com/lucasgelfond/zerobrew/commits/v0.1.1) for more details.

[Unreleased]: https://github.com/HernandoR/zerobrew/compare/v0.3.3...HEAD
[0.3.3]: https://github.com/HernandoR/zerobrew/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/lucasgelfond/zerobrew/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/lucasgelfond/zerobrew/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/lucasgelfond/zerobrew/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/lucasgelfond/zerobrew/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/lucasgelfond/zerobrew/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/lucasgelfond/zerobrew/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/lucasgelfond/zerobrew/releases/tag/v0.1.1
