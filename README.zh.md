<div align="center">

<h2>zbrew</h2>

<p align="center">
  <a href="README.md">English</a> ·
  <strong>中文</strong>
</p>

[![Lint](https://github.com/HernandoR/zbrew/actions/workflows/ci.yml/badge.svg)](https://github.com/HernandoR/zbrew/actions/workflows/ci.yml)
[![Test](https://github.com/HernandoR/zbrew/actions/workflows/test.yml/badge.svg)](https://github.com/HernandoR/zbrew/actions/workflows/test.yml)
[![Release](https://img.shields.io/github/v/release/HernandoR/zbrew?display_name=tag)](https://github.com/HernandoR/zbrew/releases)
[![Discord](https://img.shields.io/badge/Discord-Join-5865F2?logo=discord&logoColor=white)](https://discord.gg/ZaPYwm9zaw)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE-MIT.md)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](./LICENSE-APACHE.md)

<img alt="zbrew demo" src="./assets/zb-demo.gif" />

<p><strong>zbrew 为 macOS 和 Linux 上的 Homebrew 软件包带来了类似 uv 的架构。</strong></p>

</div>

> [!NOTE]
> 本仓库是 [lucasgelfond/zerobrew](https://github.com/lucasgelfond/zerobrew) 的**维护分支（fork）**。
> 原作者已将上游标记为不再维护。本分支的目标是让 zbrew 持续可用：整理上游未关闭的 bug、
> 合入上游未合并的修复，并保持依赖更新。规划见本仓库的 [milestones](https://github.com/HernandoR/zbrew/milestones)，
> 实时进度见 [项目看板](https://github.com/users/HernandoR/projects/3)，其中跟踪了全部上游未关闭的 bug、功能请求和未合并的上游 PR。
> 安全问题请在本仓库提交私密的 [security advisory](https://github.com/HernandoR/zbrew/security/advisories/new)。

## 安装 (Install)

```bash
curl -fsSL https://raw.githubusercontent.com/HernandoR/zbrew/main/install.sh | bash
```

安装程序会更新你的 shell 配置。完成后，重启终端，或运行它打印的 `source` 命令。

> `https://zerobrew.rs/install` 提供的仍是**上游**的安装脚本，该域名不在本分支控制之下。
> 安装本分支请使用上面的地址。

> [!WARNING]
> 请**不要**从上游的 tap `lucasgelfond/homebrew-zerobrew` 安装。它仍停留在 0.1.1，落后当前版本
> 五个发布；用它安装 `jq` 这类普通软件包时会报 `store corruption: prefix too long`。该 tap
> 不在本分支的控制之下（[#16](https://github.com/HernandoR/zbrew/issues/16)、
> [#17](https://github.com/HernandoR/zbrew/issues/17)）。请改用上面的安装脚本，或从
> [GitHub Releases](https://github.com/HernandoR/zbrew/releases) 下载二进制文件。

或通过 Homebrew 安装（本分支自己的 tap）：

```bash
brew install HernandoR/zbrew/zbrew
```

在 Linux 上，如果系统的 glibc 低于 2.35，安装脚本会自动选择静态链接的 musl 二进制，
因此 `zb` 在 Google Colab 这类环境中同样可以运行。你也可以直接下载带 `*-musl` 后缀的发布产物。

或从源码构建：

```bash
git clone https://github.com/HernandoR/zbrew.git
cd zbrew
just install            # 或：cargo install --path zb_cli --locked
```

> 在本分支发布正式版本之前，安装脚本找不到预编译的二进制，会回退到源码构建：
> 这需要 Rust 工具链，并且要花上几分钟。

## 更新 zbrew (Update zbrew)

如果使用独立安装脚本，重新运行：

```bash
curl -fsSL https://raw.githubusercontent.com/HernandoR/zbrew/main/install.sh | bash
zb --version
```

如果通过 Homebrew 安装：

```bash
brew update && brew upgrade zbrew
```

`zb update` 只刷新软件包元数据。`zb upgrade` 升级通过 zbrew 安装的软件包。它们都不会更新 `zb` 二进制文件本身。

## 快速开始 (Quick start)

```bash
zb install jq                   # 安装单个软件包
zb install wget git             # 安装多个软件包
zb bundle                       # 从 Brewfile 安装
zb bundle install -f myfile     # 从自定义文件安装
zb bundle dump                  # 将已安装的软件包导出到 Brewfile
zb bundle dump -f out --force   # 导出到自定义文件（覆盖）
zb uninstall jq                 # 卸载单个软件包
zb outdated                     # 列出有新版本可用的软件包
zb upgrade                      # 升级所有已过期的软件包
zb upgrade jq wget              # 升级指定的软件包
zb reset                        # 卸载所有内容
zb gc                           # 回收 zbx 临时安装和未使用的存储条目
zbx jq --version                # 在不链接、不保留的情况下运行
zb list --all                   # 列出已安装的软件包，包括 zbx 临时安装
```

## 性能快照 (Performance snapshot)

<div align="center">

| 软件包 | Homebrew | ZB (冷启动) | ZB (热启动) | 冷启动加速 | 热启动加速 |
|---------|----------|-----------|-----------|--------------|--------------|
| **总体 (前 100 名)** | 452s | 226s | 59s | **2.0x** | **7.6x** |
| ffmpeg | 3034ms | 3481ms | 688ms | 0.9x | 4.4x |
| libsodium | 2353ms | 392ms | 130ms | 6.0x | 18.1x |
| sqlite | 2876ms | 625ms | 159ms | 4.6x | 18.1x |
| tesseract | 18950ms | 5536ms | 643ms | 3.4x | 29.5x |

</div>

## 与 Homebrew 的关系 (Relationship with Homebrew)

zbrew 更像是一个针对 Homebrew 生态系统进行性能优化的客户端。我们依赖于：
- Homebrew 的 formula 定义 (homebrew-core)
- Homebrew 提供的预构建 bottle（如果可用）
- Homebrew 的软件包元数据和基础设施

我们的创新重点在于：
- 用于去重的基于内容的寻址存储 (Content-addressable storage)
- 用于零开销复制的 APFS clonefiles
- 使用 Homebrew 的 Ruby DSL 的源码编译回退 (Source build fallback)

zbrew 处于实验阶段。我们建议将其与 Homebrew 并行运行，而不是作为替代品。除非您完全确定其影响，否则**不**建议清除 Homebrew 并将其替换为 zbrew。

## 项目状态 (Project status)

<div align="center">
  <a href="https://star-history.dera.page/#HernandoR/zbrew&Date">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="https://star-history.dera.page/svg?repos=HernandoR/zbrew&type=Date&theme=dark" />
      <img alt="Star History Chart" src="https://star-history.dera.page/svg?repos=HernandoR/zbrew&type=Date" />
    </picture>
  </a>
</div>

- **状态：** 处于实验阶段，但对于许多常见的 Homebrew formulas 已经非常有用。
- **反馈：** 如果遇到不兼容问题，请在[本分支提出 issue 或 PR](https://github.com/HernandoR/zbrew/issues)。
- **Roadmap：** [milestones](https://github.com/HernandoR/zbrew/milestones) 与[项目看板](https://github.com/users/HernandoR/projects/3)。
- **同步上游：** `upstream` 分支镜像 `lucasgelfond/zerobrew:main`。运行 `just upstream-sync` 刷新，`just upstream-cherry-pick <sha>` 将单个上游提交挑入 `main`；详见 [CONTRIBUTING.md](./CONTRIBUTING.md#syncing-with-upstream)。
- **许可证：** 根据您的选择，在 [Apache 2.0](./LICENSE-APACHE.md) 或 [MIT](./LICENSE-MIT.md) 下获得双重许可。
