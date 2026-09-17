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

zbrew 安装的软件包与 Homebrew 相同。它使用 Homebrew 的 formula 数据和 Homebrew 的
bottle。它以不同的方式存放文件，安装速度通常更快。实测数据见
[性能快照](#性能快照-performance-snapshot)。它的命令是 `zb`。

## 安装 (Install)

zbrew 可在 macOS 和 Linux 上运行。请从下面三种方式中选择一种。

### 方式 1 —— 安装脚本

1. 运行以下命令：

   ```bash
   curl -fsSL https://raw.githubusercontent.com/HernandoR/zbrew/main/install.sh | bash
   ```

2. 重启终端。也可以运行脚本打印出来的 `source` 命令。
3. 检查结果：

   ```bash
   zb --version
   ```

该脚本做四件事。它把 `zb` 和 `zbx` 二进制文件放到 `$ZBREW_BIN`（默认为
`~/.zbrew/bin`）。它运行 `zb init`。它创建 zbrew 的各个目录。它把这些目录写入
shell 配置文件中的 `PATH`。

在 macOS 上，`zb init` 可能会要求输入密码。它使用 `sudo` 创建 `/opt/zbrew`，
随后把该目录的所有权交给你。

如果不想改动 shell 配置文件，请加上 `--no-modify-path` 参数：

```bash
curl -fsSL https://raw.githubusercontent.com/HernandoR/zbrew/main/install.sh | bash -s -- --no-modify-path
```

> `https://zerobrew.rs/install` 提供的仍是**上游**的安装脚本，该域名不在本分支控制之下。
> 安装本分支请使用上面的地址。

### 方式 2 —— Homebrew

本分支有自己的 tap：

```bash
brew install HernandoR/zbrew/zbrew
```

> [!WARNING]
> 请**不要**从上游的 tap `lucasgelfond/homebrew-zerobrew` 安装。它仍停留在 0.1.1，落后当前版本
> 五个发布；用它安装 `jq` 这类普通软件包时会报 `store corruption: prefix too long`。该 tap
> 不在本分支的控制之下（[#16](https://github.com/HernandoR/zbrew/issues/16)、
> [#17](https://github.com/HernandoR/zbrew/issues/17)）。请改用上面的安装脚本，或从
> [GitHub Releases](https://github.com/HernandoR/zbrew/releases) 下载二进制文件。

### 方式 3 —— 从源码构建

1. 安装 Rust 工具链。
2. 克隆本仓库：

   ```bash
   git clone https://github.com/HernandoR/zbrew.git
   ```

3. 构建并安装二进制文件：

   ```bash
   cd zbrew
   just install            # 或：cargo install --path zb_cli --locked
   ```

`just install` 会构建 `zb` 和 `zbx`，把它们复制到 `$ZBREW_BIN`，然后运行 `zb init`。

> 在本分支发布正式版本之前，安装脚本找不到预编译的二进制，会改为从源码构建。
> 源码构建需要 Rust 工具链，并且要花上几分钟。

### Linux 说明

当系统的 glibc 低于 2.35 时，安装脚本会选择静态链接的 musl 二进制。因此 `zb` 在
Google Colab 这类环境中同样可以运行。你也可以直接下载带 `*-musl` 后缀的发布产物。

## 更新 zbrew (Update zbrew)

如果使用安装脚本，就再运行一次：

```bash
curl -fsSL https://raw.githubusercontent.com/HernandoR/zbrew/main/install.sh | bash
zb --version
```

如果通过 Homebrew 安装，就用 Homebrew 更新：

```bash
brew update && brew upgrade zbrew
```

另有两个命令名字相近，但做的事不同。`zb update` 刷新软件包元数据。`zb upgrade`
升级由 zbrew 安装的软件包。这两个命令都不会更新 `zb` 二进制文件本身。

## 快速开始 (Quick start)

下面的步骤演示 zbrew 的首次使用。

1. 安装一个软件包：

   ```bash
   zb install jq
   ```

2. 使用该软件包。命令来自你的 `PATH`：

   ```bash
   jq --version
   ```

3. 用一条命令安装多个软件包：

   ```bash
   zb install wget git
   ```

4. 查看 zbrew 安装了什么：

   ```bash
   zb list
   ```

5. 删除一个软件包：

   ```bash
   zb uninstall jq
   ```

若只想运行一次某个命令，请使用 `zbx`。`zbx` 会安装该软件包、运行命令，并且不会向
你的 `PATH` 添加任何符号链接：

```bash
zbx jq --version
```

`zb gc` 会删除 `zbx` 创建的临时软件包。它也会删除没有任何软件包引用的存储条目。

## 日常命令 (Everyday commands)

```bash
zb install jq                   # 安装单个软件包
zb install wget git             # 安装多个软件包
zb install --build-from-source jq  # 从源码构建，而不使用 bottle
zb bundle                       # 从 Brewfile 安装
zb bundle install -f myfile     # 从自定义文件安装
zb bundle dump                  # 将已安装的软件包导出到 Brewfile
zb bundle dump -f out --force   # 导出到自定义文件（覆盖）
zb uninstall jq                 # 卸载单个软件包
zb list                         # 列出已安装的软件包
zb list --all                   # 列出已安装的软件包，包括 zbx 临时安装
zb info jq                      # 显示某个已安装软件包的详情
zb update                       # 刷新缓存的软件包元数据
zb outdated                     # 列出有新版本可用的软件包
zb upgrade                      # 升级所有已过期的软件包
zb upgrade jq wget              # 升级指定的软件包
zb migrate                      # 把 Homebrew 中的软件包迁移到 zbrew
zb doctor                       # 检查安装是否存在问题
zb gc                           # 回收 zbx 临时安装和未使用的存储条目
zb reset                        # 卸载所有内容
zbx jq --version                # 运行软件包，但不链接、不保留
```

每个命令都有帮助页面。运行 `zb <command> --help` 即可阅读。

## 手册页 (Manual pages)

`zb` 自带手册页。安装程序会把 `zb.1` 以及每个子命令各自的页面写入 `$ZBREW_MAN/man1`；
该路径默认是 `$ZBREW_BIN` 旁边的 `share/man` 目录，也就是 `man` 为 `PATH` 上的命令
本来就会搜索的位置：

```bash
man zb              # 命令本身、全局选项和全部子命令
man zb-install      # 每个子命令一页，包含它自己的选项
```

手册页由正在安装的那个 `zb` 二进制生成，因此不会描述成另一个版本。也可以随时自行生成：

```bash
zb man                            # 把 zb.1 写到标准输出
zb man --output-dir ~/man/man1    # 写出 zb.1 和每个子命令的页面
```

## 与 Homebrew 的对照 (Compared with Homebrew)

zbrew 的命令沿用 Homebrew 的名称。

| 任务 | Homebrew | zbrew |
|---|---|---|
| 安装软件包 | `brew install jq` | `zb install jq` |
| 删除软件包 | `brew uninstall jq` | `zb uninstall jq` |
| 列出已安装的软件包 | `brew list` | `zb list` |
| 显示软件包详情 | `brew info jq` | `zb info jq` |
| 刷新软件包元数据 | `brew update` | `zb update` |
| 查找已过期的软件包 | `brew outdated` | `zb outdated` |
| 升级软件包 | `brew upgrade` | `zb upgrade` |
| 从 Brewfile 安装 | `brew bundle` | `zb bundle` |
| 写出 Brewfile | `brew bundle dump` | `zb bundle dump` |
| 检查安装 | `brew doctor` | `zb doctor` |
| 删除无用文件 | `brew cleanup` | `zb gc` |
| 只运行一次软件包 | —— | `zbx jq --version` |
| 从 Homebrew 迁移软件包 | —— | `zb migrate` |

有两点差别很重要。`zb info` 只读取已安装的软件包，而 `brew info` 也能读取尚未安装的
软件包。`zb migrate` 把你的 Homebrew 软件包迁移到 zbrew，之后还可以把它们从 Homebrew
中删除。`zb migrate` 在删除任何内容之前都会先询问。

有些 Homebrew 功能目前还没有对应的 zbrew 命令，例如 `brew search`、`brew tap` 和
`brew services`。这类工作请继续使用 Homebrew。

zbrew 从 Homebrew 获取三样东西：

- homebrew-core 的 formula 定义
- 预构建的 bottle（当你的平台存在对应 bottle 时）
- 软件包元数据以及存放它们的服务器

zbrew 自己增加了三样东西：

- 基于内容寻址的存储，相同的文件只保留一份
- APFS clonefile，几乎零开销地把文件复制到 cellar
- 读取 Homebrew Ruby formula 的源码构建，用于没有 bottle 的软件包

zbrew 处于实验阶段。请让 zbrew 与 Homebrew 在同一台机器上并存。**不要**删除 Homebrew
并用 zbrew 取代它，除非你能接受由此带来的风险。

## 工作原理 (How it works)

`zb install <package>` 会执行以下步骤：

1. `zb` 从 `formulae.brew.sh` 的 Homebrew API 读取软件包的元数据。`zb` 会在本地保留
   一份该元数据的副本。`zb update` 用于刷新这份副本。
2. `zb` 计算出完整的依赖列表。
3. `zb` 为列表中的每个软件包下载 bottle。下载并行执行，默认上限为 20。
4. `zb` 将每个 bottle 的 SHA-256 校验和与预期值比对。
5. `zb` 把 bottle 解压到 store 中。store 的键就是该 bottle 的校验和。需要同一个
   bottle 的两个软件包共用同一个 store 条目。
6. `zb` 把 store 条目复制到 cellar 中。在 APFS 上使用 clonefile。在其他文件系统上
   使用硬链接，或者直接复制。
7. `zb` 对复制出来的内容打补丁。bottle 中含有构建时的路径 `/opt/homebrew` 或
   `/usr/local`。`zb` 把这些路径替换为 zbrew 的 prefix。在 macOS 上，`zb` 随后会对
   改动过的二进制文件重新签名。
8. `zb` 从软件包目录向 prefix 创建符号链接。你的 shell 就能在 `<prefix>/bin` 中
   找到新的命令。被 Homebrew 标记为 keg-only 的软件包不会创建这些符号链接。

文件存放在两个目录中：

| 目录 | macOS 默认值 | Linux 默认值 |
|---|---|---|
| 数据根目录（store、cache、数据库） | `/opt/zbrew` | `~/.local/share/zbrew` |
| prefix（`bin`、`Cellar`、符号链接） | `/opt/zbrew` | `~/.local/share/zbrew/prefix` |

环境变量 `ZBREW_ROOT` 和 `ZBREW_PREFIX` 可以更改这些路径。`--root` 和 `--prefix`
参数对单条命令起同样的作用。

prefix 路径有长度上限，因为 zbrew 要把新的 prefix 写入 Homebrew 为 `/opt/homebrew`
或 `/usr/local` 构建的二进制文件中。过长的 prefix 写不进去。`zb init` 会拒绝这样的
prefix，并给出一个更短的建议值。

## 性能快照 (Performance snapshot)

下表在相同软件包上对比 Homebrew 与 zbrew。**冷启动**表示 zbrew 的 store 中还没有该
软件包的副本。**热启动**表示 store 中已有这些文件，于是 zbrew 只把它们复制到 cellar，
不再下载。这些数字来自在一台机器上进行的一次基准测试。你的数字会有所不同。
`just bench` 可以做同样的测量。

<div align="center">

| 软件包 | Homebrew | ZB (冷启动) | ZB (热启动) | 冷启动加速 | 热启动加速 |
|---------|----------|-----------|-----------|--------------|--------------|
| **总体 (前 100 名)** | 452s | 226s | 59s | **2.0x** | **7.6x** |
| ffmpeg | 3034ms | 3481ms | 688ms | 0.9x | 4.4x |
| libsodium | 2353ms | 392ms | 130ms | 6.0x | 18.1x |
| sqlite | 2876ms | 625ms | 159ms | 4.6x | 18.1x |
| tesseract | 18950ms | 5536ms | 643ms | 3.4x | 29.5x |

</div>

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
- **同步上游：** `upstream` 分支镜像 `lucasgelfond/zerobrew:main`。运行 `just upstream-sync` 刷新。运行 `just upstream-cherry-pick <sha>` 将单个上游提交挑入 `main`。详见 [CONTRIBUTING.md](./CONTRIBUTING.md#syncing-with-upstream)。
- **许可证：** 根据您的选择，在 [Apache 2.0](./LICENSE-APACHE.md) 或 [MIT](./LICENSE-MIT.md) 下获得双重许可。
</content>
</invoke>
