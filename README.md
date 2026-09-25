<div align="center">

<h2>zbrew</h2>

<p align="center">
  <strong>English</strong> ·
  <a href="README.zh.md">中文</a>
</p>

[![Lint](https://github.com/HernandoR/zbrew/actions/workflows/ci.yml/badge.svg)](https://github.com/HernandoR/zbrew/actions/workflows/ci.yml)
[![Test](https://github.com/HernandoR/zbrew/actions/workflows/test.yml/badge.svg)](https://github.com/HernandoR/zbrew/actions/workflows/test.yml)
[![Release](https://img.shields.io/github/v/release/HernandoR/zbrew?display_name=tag)](https://github.com/HernandoR/zbrew/releases)
[![Discord](https://img.shields.io/badge/Discord-Join-5865F2?logo=discord&logoColor=white)](https://discord.gg/ZaPYwm9zaw)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE-MIT.md)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](./LICENSE-APACHE.md)

<img alt="zbrew demo" src="./assets/zb-demo.gif" />

<p><strong>zbrew brings uv-style architecture to Homebrew packages on macOS and Linux.</strong></p>

</div>

> [!NOTE]
> This repository is a **maintained fork** of [lucasgelfond/zerobrew](https://github.com/lucasgelfond/zerobrew),
> which its original authors have marked as unmaintained. The goal of this fork is to keep zbrew
> working: triage the open upstream bugs, land the unmerged upstream fixes, and keep dependencies current.
> The roadmap lives in the [milestones](https://github.com/HernandoR/zbrew/milestones) of this repository and on the
> [project board](https://github.com/users/HernandoR/projects/3), which tracks every open upstream bug, feature request and unmerged upstream PR.
> Security issues: please open a private
> [security advisory](https://github.com/HernandoR/zbrew/security/advisories/new) on this repository.

zbrew installs the same packages as Homebrew. It uses the Homebrew formula data
and the Homebrew bottles. It stores the files differently, and it usually
installs them faster. See [Performance snapshot](#performance-snapshot) for
measured times. The command is `zb`.

## Install

zbrew runs on macOS and on Linux. Select one of the three methods below.

### Method 1 — the install script

1. Run this command:

   ```bash
   curl -fsSL https://raw.githubusercontent.com/HernandoR/zbrew/main/install.sh | bash
   ```

2. Restart your terminal. As an alternative, run the `source` command that the
   script prints.
3. Check the result:

   ```bash
   zb --version
   ```

The script does four things. It puts the `zb` and `zbx` binaries in `$ZBREW_BIN`
(default `~/.zbrew/bin`). It runs `zb init`. It creates the zbrew directories.
It adds those directories to `PATH` in your shell config files.

On macOS, `zb init` can ask for your password. It uses `sudo` to create
`/opt/zbrew`, and then it makes you the owner of that directory.

To keep your shell config files unchanged, add the `--no-modify-path` flag:

```bash
curl -fsSL https://raw.githubusercontent.com/HernandoR/zbrew/main/install.sh | bash -s -- --no-modify-path
```

> `https://zerobrew.rs/install` still serves the **upstream** installer, and this fork
> does not control that domain. Use the URL above to install this fork.

### Method 2 — Homebrew

This fork has its own tap:

```bash
brew install HernandoR/zbrew/zbrew
```

> [!WARNING]
> Do **not** install from the upstream tap `lucasgelfond/homebrew-zerobrew`. It is still on
> 0.1.1 — five releases behind — and that build fails with `store corruption: prefix too long`
> when installing ordinary packages such as `jq`. This fork does not control that tap
> ([#16](https://github.com/HernandoR/zbrew/issues/16),
> [#17](https://github.com/HernandoR/zbrew/issues/17)). Use the installer above, or download
> a binary from [GitHub Releases](https://github.com/HernandoR/zbrew/releases).

### Method 3 — build from source

1. Install a Rust toolchain.
2. Clone this repository:

   ```bash
   git clone https://github.com/HernandoR/zbrew.git
   ```

3. Build and install the binaries:

   ```bash
   cd zbrew
   just install            # or: cargo install --path crates/zb_cli --locked
   ```

The `just install` recipe builds `zb` and `zbx`, copies them to `$ZBREW_BIN`,
and then runs `zb init`.

> Until this fork has published a stable release, the install script finds no prebuilt
> binary and builds from source instead. A source build needs a Rust toolchain and takes a
> few minutes.

### Linux notes

The install script selects a statically linked musl binary when your glibc is older
than 2.35. For this reason `zb` also runs in environments such as Google Colab. You
can download the `*-musl` release assets directly if you prefer.

## Update zbrew

If you used the install script, run it again:

```bash
curl -fsSL https://raw.githubusercontent.com/HernandoR/zbrew/main/install.sh | bash
zb --version
```

If you installed with Homebrew, use Homebrew:

```bash
brew update && brew upgrade zbrew
```

Two other commands have similar names, but they do different work. `zb update`
refreshes the package metadata. `zb upgrade` upgrades the packages that zbrew
installed. Neither command updates the `zb` binary.

## Quick start

The steps below show the first use of zbrew.

1. Install a package:

   ```bash
   zb install jq
   ```

2. Use the package. The command comes from your `PATH`:

   ```bash
   jq --version
   ```

3. Install more packages in one command:

   ```bash
   zb install wget git
   ```

4. See what zbrew installed:

   ```bash
   zb list
   ```

5. Remove a package:

   ```bash
   zb uninstall jq
   ```

To run a command one time, use `zbx`. `zbx` installs the package, runs the
command, and adds no symlinks to your `PATH`:

```bash
zbx jq --version
```

`zb gc` deletes the temporary packages that `zbx` created. It also deletes the
store entries that no package uses.

## Everyday commands

```bash
zb install jq                   # install one package
zb install wget git             # install multiple packages
zb install --build-from-source jq  # build from source instead of a bottle
zb bundle                       # install from Brewfile
zb bundle install -f myfile     # install from a custom file
zb bundle dump                  # export installed packages to Brewfile
zb bundle dump -f out --force   # dump to a custom file (overwrite)
zb uninstall jq                 # uninstall one package
zb list                         # list installed packages
zb list --all                   # list installed packages, zbx temporaries included
zb info jq                      # show details of an installed package
zb update                       # refresh the cached package metadata
zb outdated                     # list packages with newer versions
zb upgrade                      # upgrade all outdated packages
zb upgrade jq wget              # upgrade specific packages
zb migrate                      # copy your Homebrew packages into zbrew
zb doctor                       # check the installation for problems
zb gc                           # reclaim zbx temporary installs and unused store entries
zb reset                        # uninstall everything
zbx jq --version                # run a package without linking or keeping it
```

Each command has a help page. Run `zb <command> --help` to read it.

## Manual pages

`zb` ships its own manual. The installer writes `zb.1` and a page per subcommand to
`$ZBREW_MAN/man1`, which defaults to the `share/man` directory beside `$ZBREW_BIN` —
the one `man` already searches for a command it finds on your `PATH`:

```bash
man zb              # the command, its global options and every subcommand
man zb-install      # one page per subcommand, its own options included
```

The pages are generated by the `zb` binary being installed, so the manual cannot
describe a version other than the one you have. Generate them anywhere with:

```bash
zb man                            # write zb.1 to stdout
zb man --output-dir ~/man/man1    # write zb.1 and a page per subcommand
```

## Compared with Homebrew

The zbrew commands use the Homebrew names.

| Task | Homebrew | zbrew |
|---|---|---|
| Install a package | `brew install jq` | `zb install jq` |
| Remove a package | `brew uninstall jq` | `zb uninstall jq` |
| List installed packages | `brew list` | `zb list` |
| Show package details | `brew info jq` | `zb info jq` |
| Refresh the package metadata | `brew update` | `zb update` |
| Find outdated packages | `brew outdated` | `zb outdated` |
| Upgrade packages | `brew upgrade` | `zb upgrade` |
| Install from a Brewfile | `brew bundle` | `zb bundle` |
| Write a Brewfile | `brew bundle dump` | `zb bundle dump` |
| Check the installation | `brew doctor` | `zb doctor` |
| Delete unused files | `brew cleanup` | `zb gc` |
| Run a package one time | — | `zbx jq --version` |
| Copy packages from Homebrew | — | `zb migrate` |

Two differences are important. `zb info` reads an installed package, but
`brew info` also reads a package that you did not install. `zb migrate` copies
your Homebrew packages into zbrew, and it can then remove them from Homebrew.
`zb migrate` asks before it removes anything.

Some Homebrew features have no zbrew command yet. The list includes
`brew search`, `brew tap` and `brew services`. Use Homebrew for that work.

zbrew takes three things from Homebrew:

- the formula definitions from homebrew-core
- the prebuilt bottles, when a bottle exists for your platform
- the package metadata and the servers that hold it

zbrew adds three things of its own:

- a content-addressed store, which keeps one copy of identical files
- APFS clonefiles, which copy a file into the cellar at almost no cost
- a source build that reads the Homebrew Ruby formula, for packages with no bottle

zbrew is experimental. Run zbrew and Homebrew together on the same machine. Do
**not** delete Homebrew and replace it with zbrew, unless you accept the risk.

### Mirrors

zbrew reads the same mirror environment variables as Homebrew, so a shell
profile that already points `brew` at a mirror points `zb` at it too.

| Variable | Effect | Default |
|---|---|---|
| `HOMEBREW_API_DOMAIN` | Base URL for formula and cask metadata | `https://formulae.brew.sh/api` |
| `HOMEBREW_ARTIFACT_DOMAIN` | Prefix for every download, bottles included | — |
| `HOMEBREW_ARTIFACT_DOMAIN_NO_FALLBACK` | Fail instead of trying the default URL | unset |

A mirror is preferred and the default is the fallback, so a mirror that is
down, out of date, or missing a file does not stop zbrew finding the package.
Metadata requests try the mirror first and only then the default. Bottle
downloads are a race rather than a strict order: zbrew opens several
connections at once and keeps whichever answers first, so the default domain
is still contacted even when the mirror is healthy. Set
`HOMEBREW_ARTIFACT_DOMAIN_NO_FALLBACK` when a request must never leave the
mirror — it leaves exactly one candidate, so there is no race.

`HOMEBREW_BOTTLE_DOMAIN` is **not** supported yet. Homebrew serves flat
`name--version.tag.bottle.tar.gz` files from a non-GitHub-Packages bottle
domain, and zbrew cannot yet produce that filename; rather than rewrite URLs
the mirrors would answer with a 404, zbrew ignores the variable. Use
`HOMEBREW_ARTIFACT_DOMAIN` with a registry-proxying mirror instead. Tracked in
[#120](https://github.com/HernandoR/zbrew/issues/120).

`HOMEBREW_ARTIFACT_DOMAIN` prefixes an ordinary download URL whole, so
`https://example.com/foo.tar.gz` becomes
`$HOMEBREW_ARTIFACT_DOMAIN/https://example.com/foo.tar.gz`. A bottle URL is
different: the registry host is replaced, so
`https://ghcr.io/v2/homebrew/core/jq/manifests/1.7` becomes
`$HOMEBREW_ARTIFACT_DOMAIN/v2/homebrew/core/jq/manifests/1.7`. If the value
already contains a `/v2` path, that segment is not repeated.

An example that uses one mirror for both metadata and bottles:

```bash
export HOMEBREW_API_DOMAIN=https://mirrors.example.edu/homebrew-bottles/api
export HOMEBREW_BOTTLE_DOMAIN=https://mirrors.example.edu/homebrew-bottles
```

zbrew also keeps its own `ZBREW_API_URL`, which names the formula metadata
base directly. It wins over `HOMEBREW_API_DOMAIN` and gets no fallback.


## How it works

`zb install <package>` does these steps:

1. `zb` reads the metadata of the package from the Homebrew API at
   `formulae.brew.sh`. `zb` keeps a local copy of that metadata. `zb update`
   refreshes the copy.
2. `zb` calculates the full list of dependencies.
3. `zb` downloads a bottle for each package in the list. The downloads run in
   parallel. The default limit is 20.
4. `zb` compares the SHA-256 checksum of each bottle against the expected value.
5. `zb` extracts the bottle into the store. The store key is the checksum of the
   bottle. Two packages that need the same bottle share one store entry.
6. `zb` copies the store entry into the cellar. On APFS it uses clonefile. On
   other filesystems it uses hard links, or a plain copy.
7. `zb` patches the copy. A bottle contains the build-time paths `/opt/homebrew`
   or `/usr/local`. `zb` replaces those paths with the zbrew prefix. On macOS,
   `zb` then signs the changed binaries again.
8. `zb` creates symlinks from the package directory into the prefix. Your shell
   finds the new commands in `<prefix>/bin`. A keg-only package gets no such
   symlinks, because Homebrew marks it as keg-only.

The files live in two directories:

| Directory | macOS default | Linux default |
|---|---|---|
| Data root (store, cache, database) | `/opt/zbrew` | `~/.local/share/zbrew` |
| Prefix (`bin`, `Cellar`, symlinks) | `/opt/zbrew` | `~/.local/share/zbrew/prefix` |

The environment variables `ZBREW_ROOT` and `ZBREW_PREFIX` change these paths.
The flags `--root` and `--prefix` do the same for one command.

The prefix path has a length limit, because zbrew writes the new prefix into
binaries that Homebrew built for `/opt/homebrew` or `/usr/local`. A long prefix
does not fit. `zb init` refuses such a prefix and suggests a shorter one.

## Performance snapshot

The table compares Homebrew against zbrew for the same packages. **Cold** means
that the zbrew store holds no copy of the package. **Warm** means that the store
already holds the files, so zbrew copies them into the cellar and does no
download. These numbers come from a benchmark run on one machine. Your numbers
will be different. The `just bench` recipe makes the same measurement.

<div align="center">

| Package | Homebrew | ZB (cold) | ZB (warm) | Cold Speedup | Warm Speedup |
|---------|----------|-----------|-----------|--------------|--------------|
| **Overall (top 100)** | 452s | 226s | 59s | **2.0x** | **7.6x** |
| ffmpeg | 3034ms | 3481ms | 688ms | 0.9x | 4.4x |
| libsodium | 2353ms | 392ms | 130ms | 6.0x | 18.1x |
| sqlite | 2876ms | 625ms | 159ms | 4.6x | 18.1x |
| tesseract | 18950ms | 5536ms | 643ms | 3.4x | 29.5x |

</div>

## Project status

<div align="center">
  <a href="https://star-history.dera.page/#HernandoR/zbrew&Date">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="https://star-history.dera.page/svg?repos=HernandoR/zbrew&type=Date&theme=dark" />
      <img alt="Star History Chart" src="https://star-history.dera.page/svg?repos=HernandoR/zbrew&type=Date" />
    </picture>
  </a>
</div>

- **Status:** Experimental, but already useful for many common Homebrew formulas.
- **Feedback:** If you hit incompatibilities, please open an [issue or PR on this fork](https://github.com/HernandoR/zbrew/issues).
- **Roadmap:** [milestones](https://github.com/HernandoR/zbrew/milestones) and the [project board](https://github.com/users/HernandoR/projects/3).
- **Upstream sync:** the `upstream` branch mirrors `lucasgelfond/zerobrew:main`. Run `just upstream-sync` to refresh it. Run `just upstream-cherry-pick <sha>` to bring individual upstream commits into `main`. See [CONTRIBUTING.md](./CONTRIBUTING.md#syncing-with-upstream).
- **License:** Dual-licensed under [Apache 2.0](./LICENSE-APACHE.md) OR [MIT](./LICENSE-MIT.md), at your choice.
</content>
</invoke>
