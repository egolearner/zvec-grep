# install.sh 工具的安装目录与 PATH 调研

调研日期：2026-11-06。范围为官方安装器的目录、PATH、交互和升级约定；仅阅读官方文档及源码，没有执行安装脚本。本文不改变当前实现或扩展 GitHub Releases 之外的下载渠道。

后续决定：Unix 程序安装到 `~/.zvec-grep/bin`，以 `~/.local/bin/zg` 软链提供入口；缺少 PATH 时仅提示用户设置，不再自动写入 shell 配置。下文对本项目自动 PATH 方案的分析是决定前的调研快照，实际行为见 [安装说明](README.md)。

## 对照结果

下面的目录是 Unix 默认值；自定义环境变量可能改变它们。

| 工具 | 默认命令目录 | PATH 处理 | 用户控制 | 脚本安装后的升级 |
| --- | --- | --- | --- | --- |
| uv | `~/.local/bin`；优先考虑 `XDG_BIN_HOME`、`XDG_DATA_HOME/../bin` | 目标目录已在当前 PATH 时跳过修改；否则生成幂等 env 文件，在 shell 配置中添加 source | `UV_INSTALL_DIR`、`UV_NO_MODIFY_PATH=1`；CI 可用 `UV_UNMANAGED_INSTALL`，同时禁用自更新 | `uv self update`，保留安装位置和 PATH 策略；包管理器安装由包管理器升级 |
| Rust/rustup | `~/.cargo/bin` | 生成 `~/.cargo/env` 等 shell 专用文件，在检测到的 shell 配置中 source | `CARGO_HOME` / `RUSTUP_HOME`；`--no-modify-path`；`-y` 跳过安装交互 | `rustup update` 更新工具链；`rustup self update` 更新管理器 |
| Deno | `~/.deno/bin` | shell installer 调用独立 shell-setup；非 CI 且 stdout 为终端时可交互配置，`-y` 可显式启用非交互配置 | `DENO_INSTALL`；`--no-modify-path`；`-y` | `deno upgrade`，可用 `--version` 指定版本 |
| Bun | `~/.bun/bin` | `command -v bun` 成功就跳过 PATH 设置；否则按 `$SHELL` 尝试追加配置，文件不可写则打印手动步骤 | `BUN_INSTALL`；当前检查的 shell 脚本未提供通用 no-modify-path 参数 | `bun upgrade`；重跑脚本并传版本可安装旧版本 |
| Starship | `/usr/local/bin` | 检查目标目录是否已在 PATH，未加入则警告；安装脚本不自动修改 PATH | `BIN_DIR` / `--bin-dir`；`-y`；目录不可写时尝试 sudo | 官方推荐重跑安装脚本，不改变应用配置 |

表格依据：[uv 存储目录](https://docs.astral.sh/uv/reference/storage/)、[uv 安装器选项](https://docs.astral.sh/uv/reference/installer/)、[uv 升级](https://docs.astral.sh/uv/getting-started/installation/#upgrading-uv)；[rustup 安装](https://rust-lang.github.io/rustup/installation/index.html)、[rustup 升级](https://rust-lang.github.io/rustup/basics.html)；[Deno 安装与升级](https://docs.deno.com/runtime/getting_started/installation/)、[Deno install.sh](https://github.com/denoland/deno_install/blob/master/install.sh)；[Bun 安装与升级](https://bun.sh/docs/installation)、[Bun install](https://bun.sh/install)；[Starship FAQ](https://starship.rs/faq/#how-do-i-install-starship-without-sudo)、[Starship 升级说明](https://starship.rs/#quick-install)。PATH 实现细节见下文源码核验。

## 哪些做法更值得参考

uv、rustup 和 Deno 的共同结构是：**程序目录中的 env 文件负责 PATH 逻辑，用户的 rc/profile 文件只负责 source 它**。这样可以同时处理多种启动文件，又不会每次 source 都重复追加 PATH。目录是否短或是否位于 `~/.local/bin`，与 shell 配置是否正确是两个问题。

### uv：通用用户命令目录，加上两层去重

核验版本为 [uv 0.13.7 的实际发布安装脚本](https://github.com/astral-sh/uv/releases/download/0.13.7/uv-installer.sh)，其生成器记录为 cargo-dist 0.32.0：

- 1399–1403 行检查当前 PATH 已包含目标目录时跳过配置修改。
- 1405–1417 行设置 `.profile`，并处理已存在的 `.bashrc`、`.bash_profile`、`.bash_login`；zsh 从 `.zshrc` / `.zshenv` 选择；fish 有独立 `conf.d/uv.env.fish`。
- `print_home_for_script` 尊重 `ZDOTDIR`；`add_install_dir_to_path` 避免重复写入同一 source 语句。
- `write_env_script_sh` / `write_env_script_fish` 在每次加载时检查目录是否已在 PATH，解决重复 source 和子 shell 继承后的累加。

这比只 grep 配置文件中的整行更完整。`UV_NO_MODIFY_PATH` 和 unmanaged 模式属于官方公开接口，后续 self update 继续尊重原有选择。[安装器选项](https://docs.astral.sh/uv/reference/installer/)、[自更新实现](https://github.com/astral-sh/uv/blob/main/crates/uv/src/commands/self_update.rs)

### rustup：明确处理 shell 启动文件差异

[`shell.rs`](https://github.com/rust-lang/rustup/blob/main/src/cli/self_update/shell.rs) 中 `Posix::rcs` 使用 `.profile`，`Bash::rcs` 处理已存在的 `.bash_profile` / `.bash_login` / `.bashrc`，`Zsh::rcs` 选择 `.zshenv` 并考虑 `ZDOTDIR`，`Fish::rcs` 使用 XDG 配置目录下的 `fish/conf.d/rustup.fish`。这并非仅根据 `$SHELL` 选择一个文件，而是检测可用 shell。

[`unix.rs::add_to_path`](https://github.com/rust-lang/rustup/blob/main/src/cli/self_update/unix.rs) 在写入前检查已有 source 命令；[`env.sh`](https://github.com/rust-lang/rustup/blob/main/src/cli/self_update/env.sh) 在运行时按冒号分隔的完整 PATH 项判断是否需要添加目录。这分别解决配置重复和运行时重复。

### Deno：把安装与可选的 shell 设置分开

[`install.sh`](https://github.com/denoland/deno_install/blob/master/install.sh) 74–76 行决定目录，94–111 行决定是否运行 shell setup。它使用新安装的 Deno 执行 JSR 的 shell-setup 模块；普通非交互/CI 调用未带 `-y` 时，不应假定一定修改配置文件。

仓库 [`shell-setup/bundled.esm.js`](https://github.com/denoland/deno_install/blob/master/shell-setup/bundled.esm.js) 中 `shEnvScript`、`addToPath`、`updateRcFile` 实现 env、source 去重和配置备份；Bash 处理现有 profile/rc，zsh 考虑 `ZDOTDIR`，fish 使用独立 conf.d 文件。官方文档也给出了删除 env source 和 fish 文件的卸载方式。[安装文档](https://docs.deno.com/runtime/getting_started/installation/)

边界：本次核对的 helper 是仓库 master，并未确认其与当时 JSR 发布版本逐字相同。该源码的 `setupShells` 中 `noModifyPath` 明确限制非交互自动配置分支，交互分支仍会询问；不能把文档中的 flag 名称扩展为所有执行模式的无条件保证。

### Bun：实现简单，但去重判断比较粗

所读 [官方安装脚本](https://bun.sh/install) 175–180 行在任何 `bun` 已可解析时提前退出 PATH 设置；194 行之后按 fish/zsh/bash 分支处理。Bash 从 `.bash_profile`、`.bashrc` 及部分 XDG 候选中选择首个可写文件；zsh 使用 `~/.zshrc`；fish 使用 `~/.config/fish/config.fish`。配置不可写或 shell 不识别时输出手动步骤。

该脚本没有逐项确认 PATH 中的 bun 是否就是刚安装的程序，追加的 PATH 语句也没有运行时去重。这说明“流行工具采用某实现”不能直接等同于“该实现覆盖所有边界”。

### Starship：安装到常见 bin，缺失 PATH 时只提示

[`install/install.sh`](https://github.com/starship/starship/blob/master/install/install.sh) 的 `check_bin_dir` 检查目录及 PATH 成员关系，未加入时发出警告；默认目录在 421–423 行为 `/usr/local/bin`。`install` 根据目录是否可写决定是否需要 sudo。用户可用 `--bin-dir ~/.local/bin` 改成用户级安装。

它后续要求用户添加 `starship init` 是启用 shell prompt 功能，不是安装器自动添加 PATH，二者不应混淆。[官方安装步骤](https://starship.rs/guide/)、[无 sudo 安装说明](https://starship.rs/faq/#how-do-i-install-starship-without-sudo)

## 对 zvec-grep 当前实现的建议

以下是结合本项目需求的设计判断，不是上述工具的共同强制标准。

1. **保留完整包目录，命令入口可以独立。** zvec-grep 有 `bin/` 和 `lib/`，可继续放在 `~/.local/share/zvec-grep/standalone/current`。Unix 可考虑在 `~/.local/bin/zg` 放稳定 symlink/launcher，而不把整套运行库迁到通用 bin。需要另外验证相对库加载、已有同名文件归属及 Windows 入口实现。XDG 规范允许用户可执行文件位于 `~/.local/bin`，但不能据此假设每台机器的 PATH 都已包含它。[XDG Base Directory Specification](https://specifications.freedesktop.org/basedir/latest/)
2. **PATH 逻辑采用 env + source，并做到两层幂等。** profile 中避免重复 source；env 每次执行都按完整目录项去重。当前 [install.sh](install.sh) 174–193 行只有整行 grep，重复 source 仍可累加 PATH。
3. **补齐 Bash、ZDOTDIR 和 fish。** 当前 zsh 写 `~/.zshrc`，其他一律 `.profile`，不能覆盖常见 Bash 非登录终端；存在 `.bash_profile` 时，Bash 登录也可能不读 `.profile`。zsh 应尊重 `ZDOTDIR`，fish 应有原生配置片段。[Bash 启动文件](https://www.gnu.org/software/bash/manual/html_node/Bash-Startup-Files.html)、[zsh 启动文件](https://zsh.sourceforge.io/Doc/Release/Files.html)、[fish_add_path](https://fishshell.com/docs/current/cmds/fish_add_path.html)
4. **区分当前可用与以后可用。** 当前 PATH 包含目录时可以尊重用户手动管理，但这不能证明所有新终端和 IDE 启动环境都已持久配置。当前 `command -v zg` 也不能单独证明命中此次安装。应显示实际命令路径，并明确报告本次修改了哪些配置文件，或因何跳过。
5. **保留关闭 PATH 修改的开关和偏好。** CI 使用明确的不修改模式；交互环境可默认自动配置，并在无法写入时给出可直接执行的命令。脚本下载、程序激活成功和 shell 配置成功应分开报告。
6. **明确当前终端生效方式。** `curl | sh` 的子进程无法修改父 shell 的环境；自动写文件后仍应提示重开终端或 source 对应 env 文件。[Bash 命令执行环境](https://www.gnu.org/software/bash/manual/html_node/Command-Execution-Environment.html)

升级首版继续采用重跑安装脚本有成熟先例（Starship）。原生自更新可后续复用同一安装事务，不需要为补齐 PATH 体验同时扩大升级范围。

## 核验快照

除 uv 固定版本外，源码链接为调研当日的 main/master 或官方动态脚本，未来可能变化。以下 SHA256 仅标识本次读取的内容，不是安装安全保证：

| 来源 | 本次读取标识 |
| --- | --- |
| uv 0.13.7 `uv-installer.sh` | `d263ee68314cf5bea36b08e6bd9e2c4563240d7adb73756cf33af5c199b92db4` |
| `https://bun.sh/install` | `04882bf41679d49d9af108657a1e5515bf04fdf2940d12c0d0b1e5d79dc53be8` |
| Deno master `shell-setup/bundled.esm.js` | `3bf232de451d984bb700230b661c21c7cee5304f9f4ad4d6af5e7f2cc3603899` |

本次没有安装这些工具，也没有验证所有 shell/OS 的实际运行结果；结论区分官方接口说明、源码观察和本项目建议。
