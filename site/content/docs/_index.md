---
title: "zbrew documentation"
description: "Install zbrew, run your first package, and learn how the store, the cellar and the prefix fit together."
---

zbrew installs the same packages as Homebrew. It uses the Homebrew formula data
and the Homebrew bottles. It stores the files differently, and it usually
installs them faster. The command is `zb`.

zbrew runs on macOS and on Linux. It is experimental: run zbrew and Homebrew
together on the same machine, and do **not** delete Homebrew and replace it with
zbrew unless you accept the risk.

## Install

Pick one of the three methods below.

### Method 1 — the install script

Run this command:

```bash
curl -fsSL https://raw.githubusercontent.com/HernandoR/zbrew/main/install.sh | bash
```

Restart your terminal, or run the `source` command that the script prints. Then
check the result:

```bash
zb --version
```

The script does four things:

- puts the `zb` and `zbx` binaries in `$ZBREW_BIN` (default `~/.zbrew/bin`)
- runs `zb init`
- creates the zbrew directories
- adds those directories to `PATH` in your shell config files

On macOS, `zb init` can ask for your password. It uses `sudo` to create
`/opt/zbrew`, and then it makes you the owner of that directory.

To keep your shell config files unchanged, add the `--no-modify-path` flag:

```bash
curl -fsSL https://raw.githubusercontent.com/HernandoR/zbrew/main/install.sh | bash -s -- --no-modify-path
```

{{< callout note >}}
`https://zerobrew.rs/install` still serves the **upstream** installer, and this
fork does not control that domain. Use the URL above to install this fork.
{{< /callout >}}

### Method 2 — Homebrew

This fork has its own tap:

```bash
brew install HernandoR/zbrew/zbrew
```

{{< callout warning >}}
Do **not** install from the upstream tap `lucasgelfond/homebrew-zerobrew`. It is
still on 0.1.1 — five releases behind — and that build fails with
`store corruption: prefix too long` when installing ordinary packages such as
`jq`. This fork does not control that tap
([#16](https://github.com/HernandoR/zbrew/issues/16),
[#17](https://github.com/HernandoR/zbrew/issues/17)). Use the installer above,
or download a binary from
[GitHub Releases](https://github.com/HernandoR/zbrew/releases).
{{< /callout >}}

### Method 3 — build from source

Install a Rust toolchain, then:

```bash
git clone https://github.com/HernandoR/zbrew.git
cd zbrew
just install            # or: cargo install --path crates/zb_cli --locked
```

The `just install` recipe builds `zb` and `zbx`, copies them to `$ZBREW_BIN`,
and then runs `zb init`.

{{< callout note >}}
Until this fork has published a stable release, the install script finds no
prebuilt binary and builds from source instead. A source build needs a Rust
toolchain and takes a few minutes.
{{< /callout >}}

### Linux notes

The install script selects a statically linked musl binary when your glibc is
older than 2.35. For this reason `zb` also runs in environments such as Google
Colab. You can download the `*-musl` release assets directly if you prefer.

### Update zbrew

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

Each command has a help page. Run `zb <command> --help` to read it.

### Install and remove

```bash
zb install jq                      # install one package
zb install wget git                # install multiple packages
zb install --build-from-source jq  # build from source instead of a bottle
zb install --cask docker-desktop   # install a cask by its Homebrew token
zb uninstall jq                    # uninstall one package
zb uninstall --cask docker-desktop # uninstall a cask by its token
zbx jq --version                   # run a package without linking or keeping it
```

### Inspect

```bash
zb list                            # list installed packages
zb list --all                      # list installed packages, zbx temporaries included
zb info jq                         # show details of an installed package
zb outdated                        # list packages with newer versions
zb doctor                          # check the installation for problems
```

### Update and upgrade

```bash
zb update                          # refresh the cached package metadata
zb upgrade                         # upgrade all outdated packages
zb upgrade jq wget                 # upgrade specific packages
```

### Brewfiles

```bash
zb bundle                          # install from Brewfile
zb bundle install -f myfile        # install from a custom file
zb bundle dump                     # export installed packages to Brewfile
zb bundle dump -f out --force      # dump to a custom file (overwrite)
```

### Casks

A cask is a Homebrew package that ships a prebuilt application rather than a
bottle. Of the artifact kinds a cask declares, zbrew installs two: a `binary`,
which lands in the prefix like any other command, and an `app`, which is moved
into your app directory as a real `.app` bundle with a symlink left in the keg
pointing at it. `zb uninstall` follows that symlink to remove the application
again.

```bash
zb install --cask iterm2               # an application
zb install --cask visual-studio-code   # an application and its `code` command
zb uninstall --cask iterm2             # uninstall it, .app bundle included
```

`ZBREW_APPDIR` sets where `.app` bundles go — `/Applications` on macOS,
`$ZBREW_PREFIX/Applications` elsewhere. A name already taken in the app
directory is reported as a conflict, never overwritten.

Two limits are worth knowing. A cask whose download is a `.dmg` disk image
cannot be installed — zbrew unpacks tar and zip — and neither can one that
installs a `pkg`; both are refused with an error saying so. And the other
artifact kinds a cask declares (`manpage`, the shell completions, `zap`,
`uninstall`) are skipped, so a cask's manual page and completions do not
arrive.

### Migrate and clean up

```bash
zb migrate                         # copy your Homebrew packages into zbrew
zb gc                              # reclaim zbx temporaries and unused store entries
zb reset                           # uninstall everything
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
your Homebrew packages into zbrew, and it can then remove them from Homebrew;
it asks before it removes anything.

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

### Where the files live

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

| Package | Homebrew | ZB (cold) | ZB (warm) | Cold speedup | Warm speedup |
|---|---|---|---|---|---|
| **Overall (top 100)** | 452s | 226s | 59s | **2.0x** | **7.6x** |
| ffmpeg | 3034ms | 3481ms | 688ms | 0.9x | 4.4x |
| libsodium | 2353ms | 392ms | 130ms | 6.0x | 18.1x |
| sqlite | 2876ms | 625ms | 159ms | 4.6x | 18.1x |
| tesseract | 18950ms | 5536ms | 643ms | 3.4x | 29.5x |

## Project status

zbrew is experimental, but already useful for many common Homebrew formulas.

This repository is a maintained fork of
[lucasgelfond/zerobrew](https://github.com/lucasgelfond/zerobrew), which its
original authors have marked as unmaintained. The goal of this fork is to keep
zbrew working: triage the open upstream bugs, land the unmerged upstream fixes,
and keep dependencies current.

- **Feedback:** if you hit incompatibilities, open an
  [issue or PR](https://github.com/HernandoR/zbrew/issues).
- **Roadmap:** the [milestones](https://github.com/HernandoR/zbrew/milestones)
  and the [project board](https://github.com/users/HernandoR/projects/3).
- **Security:** open a private
  [security advisory](https://github.com/HernandoR/zbrew/security/advisories/new).
- **Community:** [join the Discord](https://discord.gg/ZaPYwm9zaw).
- **License:** dual-licensed under Apache 2.0 OR MIT, at your choice.
