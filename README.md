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


## Install

```bash
curl -fsSL https://raw.githubusercontent.com/HernandoR/zbrew/main/install.sh | bash
```

The installer updates your shell config. After it finishes, restart your terminal
or run the `source` command it prints.

> `https://zbrew.rs/install` still serves the **upstream** installer, and this fork
> does not control that domain. Use the URL above to install this fork.

> [!WARNING]
> Do **not** install from the upstream tap `lucasgelfond/homebrew-zerobrew`. It is still on
> 0.1.1 — five releases behind — and that build fails with `store corruption: prefix too long`
> when installing ordinary packages such as `jq`. This fork does not control that tap
> ([#16](https://github.com/HernandoR/zbrew/issues/16),
> [#17](https://github.com/HernandoR/zbrew/issues/17)). Use the installer above, or download
> a binary from [GitHub Releases](https://github.com/HernandoR/zbrew/releases).

Or via Homebrew, from this fork's own tap:

```bash
brew install HernandoR/zbrew/zbrew
```

On Linux the installer picks a statically linked musl binary when your glibc is older than
2.35, so `zb` also runs in environments such as Google Colab. You can download the
`*-musl` release assets directly if you prefer.

Or build from source:

```bash
git clone https://github.com/HernandoR/zbrew.git
cd zbrew
just install            # or: cargo install --path zb_cli --locked
```

> Until this fork has published a stable release, the installer finds no prebuilt
> binary and builds from source instead, which needs a Rust toolchain and takes a
> few minutes.

## Update zbrew

If you used the standalone installer, rerun it:

```bash
curl -fsSL https://raw.githubusercontent.com/HernandoR/zbrew/main/install.sh | bash
zb --version
```

If you installed with Homebrew:

```bash
brew update && brew upgrade zbrew
```

## Quick start

```bash
zb install jq                   # install one package
zb install wget git             # install multiple
zb bundle                       # install from Brewfile
zb bundle install -f myfile     # install from custom file
zb bundle dump                  # export installed packages to Brewfile
zb bundle dump -f out --force   # dump to custom file (overwrite)
zb uninstall jq                 # uninstall one package
zb outdated                     # list packages with newer versions
zb upgrade                      # upgrade all outdated packages
zb upgrade jq wget              # upgrade specific packages
zb reset                        # uninstall everything
zb gc                           # garbage collect unused store entries
zbx jq --version                # run without linking
```

## Performance snapshot

<div align="center">

| Package | Homebrew | ZB (cold) | ZB (warm) | Cold Speedup | Warm Speedup |
|---------|----------|-----------|-----------|--------------|--------------|
| **Overall (top 100)** | 452s | 226s | 59s | **2.0x** | **7.6x** |
| ffmpeg | 3034ms | 3481ms | 688ms | 0.9x | 4.4x |
| libsodium | 2353ms | 392ms | 130ms | 6.0x | 18.1x |
| sqlite | 2876ms | 625ms | 159ms | 4.6x | 18.1x |
| tesseract | 18950ms | 5536ms | 643ms | 3.4x | 29.5x |

</div>

## Relationship with Homebrew

zbrew is more of a performance-optimized client for the Homebrew ecosystem. We rely on:
- Homebrew's formula definitions (homebrew-core)
- Homebrew's pre-built bottles when available
- Homebrew's package metadata and infrastructure

Our innovations focus on:
- Content-addressable storage for deduplication
- APFS clonefiles for zero-overhead copying
- Source build fallback using Homebrew's Ruby DSL

zbrew is experimental. We recommend running it alongside Homebrew rather than as a replacement, and do _not_ 
recommend purging homebrew and replacing it with zbrew unless you are absolutely sure about the implications of 
doing so. 

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
- **Upstream sync:** the `upstream` branch mirrors `lucasgelfond/zerobrew:main`. Run `just upstream-sync` to refresh it and `just upstream-cherry-pick <sha>` to bring individual upstream commits into `main`; see [CONTRIBUTING.md](./CONTRIBUTING.md#syncing-with-upstream).
- **License:** Dual-licensed under [Apache 2.0](./LICENSE-APACHE.md) OR [MIT](./LICENSE-MIT.md), at your choice.
