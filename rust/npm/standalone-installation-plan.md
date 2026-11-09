# 独立安装脚本、发布产物与升级方案

调研日期：2026-11-06。下文保留设计依据；本轮已实现安装脚本、原生升级事务、产物组装和发布流程，实际接口以 [standalone 使用说明](../standalone/README.md) 为准。尚未发布带安装脚本的 GitHub Release，也没有实现二进制自更新命令。

后续确定的 Unix 目录约定：完整程序与升级状态放在 `~/.zvec-grep/bin`，创建 `~/.local/bin/zg` 软链指向其中的 `current/bin/zg`。Shell 配置和 PATH 由用户维护，安装器只检查并打印指令；下文关于自动修改 Unix PATH 的内容保留为早期方案，不再是实际行为。

本轮范围仅为官方 GitHub Releases，不提供选源、镜像、OSS/CDN 或自定义下载地址。CI 生成六个基础包、两个 CUDA 包和统一清单，先上传草稿并核对 GitHub 资产摘要，再公开 Release。首次上线仍需通过真实 tagged release 的六平台和 GPU CI。

升级通过重复执行脚本完成；稳定入口为 `<install-dir>/current/bin/zg[.exe]`，原生安装锁和恢复日志协调目录切换及本安装 daemon 的停机、重启、失败恢复。已安装客户端可迁移到该稳定入口。实际 CUDA auto 策略延续 npm 的有超时 `nvidia-smi` 探测，不保证主机运行库兼容；runtime/cuBLAS/cuDNN 打包不在本轮范围。

本文以仓库现有 Rust 原生分发链路为方案底座。仓库另保留 TypeScript/npm 旧链路：根 [package.json](../../package.json) 为 0.2.1、要求 Node >=22，[publish.yml](../../.github/workflows/publish.yml) 仍执行该链路。此次未查询线上 registry latest，不能据此断言 Rust 包已经成为线上 stable 默认版本。

结论：可以复用现有原生构建增加独立安装渠道。主要工作是发布完整可运行的归档包、定义兼容性与安装状态契约、处理后台服务和客户端路径迁移；下载脚本只是其中一层。

首版建议提供 `install.sh` 和 `install.ps1`，通过重复运行同一安装器完成升级。原生自更新命令可在后续增加；它不是首版上线的前置条件。

## 1. 当前已有能力与缺口

| 项目 | 已核验的当前状态 | 对独立安装的影响 |
| --- | --- | --- |
| 平台矩阵 | macOS、Linux glibc、Windows MSVC 各有 arm64/x64，共六个平台；只有 Linux/Windows x64 有 CUDA 组件 | 首版沿用该矩阵，不自动扩展为 musl、其他架构或其他 GPU 后端 |
| 执行入口 | npm 安装后运行纯原生程序，不需要 Node launcher | 可以取消安装时对 Node/npm 的依赖，无需重写核心程序 |
| 安装布局 | 需要完整保留 `bin/zg` 与 `lib/`；Unix 使用相对 rpath，Windows 使用原生 delay-load | 不能只下载、复制一个 `zg` 可执行文件 |
| npm 产物 | 共 15 个 tarball：六个 CPU 基础包、八个 CUDA 组件包和一个元包；ORT 分片用于适应 npm 180 MiB 上传预算 | 独立渠道应在构建阶段合并完整目录，避免安装脚本复刻分片拼装逻辑 |
| 平台基线 | Linux glibc 2.31；macOS x64 14+；macOS 支持 Metal；当前没有 musl 包 | 安装器必须检查支持范围并提供明确错误 |
| GPU 依赖 | GGUF 有 CUDA 13.1 与 12.4 fallback；ORT 使用 CUDA 12 与 cuDNN 9；主机驱动及 CUDA runtime/cuBLAS/cuDNN 未全部随包提供 | “检测到 NVIDIA GPU”不等于“下载后一定能用 GPU” |
| 发布流程 | 现有流程构建、收集 Actions artifact、发布 npm canary、做六平台 registry smoke，最后 promote latest | 还需增加公开、稳定、可校验的独立发布资产与发布完成标记 |

本地依据：[平台清单](platforms.json)、[npm 发布说明](README.md)、[现有打包脚本](../scripts/npm-release.mjs)、[发布工作流](../../.github/workflows/rust-publish.yml)。平台最低要求应继续以发布清单和实际构建验证为准。

现有 GPU 安装策略可复用其语义：`auto/cpu/cuda`；先安装 CPU 基础包，再安装 CUDA 组件；auto 失败时保留 CPU，显式 cuda 失败则报错。现有载荷校验、`coreId` 和 checksums 也能作为独立产物校验的基础。

不过，当前 `nvidia-smi` 探测仅有 5 秒超时和设备存在性判断，不验证驱动、runtime 和 GPU 架构组合；组件下载通过 `npm pack`。两部分都不能直接搬进无 npm 的安装器。

本地依据：[plugins.cjs](meta/plugins.cjs) 的探测与下载逻辑（69–113 行附近）、[install.cjs](meta/install.cjs) 的安装与平台选择逻辑（383–465 行附近）。

## 2. 发布产物：先建立一份安装契约

建议沿用同一套原生 build 输出，同时生成 npm 包和 standalone 归档。两条渠道应使用同一版本、同一构建标识和同一组件清单，不分别维护依赖版本。

基础包继续采用当前可添加后端组件的同一 core，不另编译一个改变 Windows 后端加载语义的 `--cpu-only` 核心。

首版产物矩阵建议如下，名称均为示意：

| 平台 | 默认完整包 | CUDA 完整包 |
| --- | --- | --- |
| macOS arm64 / x64 | 各一个，保留现有 Metal 能力 | 无 |
| Linux glibc arm64 | 一个 | 无 |
| Linux glibc x64 | 一个 | 一个，含该平台的完整 CUDA 组件布局 |
| Windows MSVC arm64 | 一个 | 无 |
| Windows MSVC x64 | 一个 | 一个，含该平台的完整 CUDA 组件布局 |

即六个基础包加两个 CUDA 完整包。Unix 使用 tar.gz，Windows 使用 zip；每个包都保留 `bin/`、`lib/`、必要资源和许可证。此处“CUDA 完整包”指应用组件完整，不默认意味着已捆绑所有主机 CUDA 运行库。

首版优先减少用户端拼装复杂度。后续如果 CUDA 下载体积明显影响体验，再改成“基础包 + 单个完整 CUDA addon”；无需继续暴露 npm 专用的 ORT 分片。

建议增加版本化发布清单，至少记录：

- 应用版本、构建标识、发布渠道，以及清单格式版本。
- OS、架构、libc/ABI、最低系统版本与 CPU 指令集要求。
- 包的 URL、SHA256、大小、目录布局版本与组件列表。
- CPU/Metal/CUDA 能力、CUDA 组件版本、主机依赖、最低驱动及支持的 GPU 架构。
- 安装器最低兼容版本，以及可选的签名/发布证明信息。

清单应由构建元数据生成。POSIX shell 不应临时用 grep/sed 解析任意 JSON；可从同一 manifest 生成严格简单的行格式，或在版本化安装器中生成平台 → URL/hash 表。PowerShell 可原生解析 JSON。不同表示必须来自同一数据源并锁定版本，不能维护两份手写平台表。

GitHub Releases 可以提供固定文件名的 latest 下载链接。建议先取得清单并锁定版本，再按该版本的 tag URL 下载归档和校验文件，避免两次 latest 请求之间发生版本切换。发布时先上传并校验全部资产，最后更新稳定入口。[GitHub 官方说明](https://docs.github.com/en/repositories/releasing-projects-on-github/linking-to-releases)

需要新增的发布工作包括：官方 GitHub Release 资产上传、版本化清单和校验文件生成、安装脚本稳定 URL、发布完整性检查、保留旧版本，以及独立渠道的 smoke test。现有 Actions artifact 收集与 npm latest 提升不能代替这条公开下载链路。

## 3. 安装器应该做什么

安装器只负责识别环境、选择包、验证下载、准备目录和激活版本，不运行本机构建，也不要求安装 Node、npm、jq 或 Python。

拟议执行顺序：

1. 解析版本、安装目录和 `auto/cpu/cuda` 参数；读取已有 installation receipt。
2. 识别 OS、进程/目标架构与 libc，检查最低系统要求；不支持的组合直接说明原因。
3. 获取并锁定发布清单版本，按照兼容性规则选择归档。
4. 下载到 staging，验证哈希，检查解压路径边界和期望目录结构。
5. 在最终安装卷上的版本目录准备完整载荷，运行轻量启动检查；不为安装检查强制下载模型。
6. 对升级场景先准备完新版本，再协调当前受管理 daemon 停止，缩短停机窗口。
7. 切换稳定入口，更新 receipt，必要时重启受管理 daemon 并验证实际版本。
8. 检查 PATH 与客户端配置，输出安装位置、实际后端和下一步命令。

Unix 至少需要 shell、下载工具、解压和 SHA256 工具，Windows 使用 PowerShell 对应能力；“无需 npm”并不等于完全不依赖系统工具。启动时检查这些依赖并提供可操作的错误。

建议默认用户级安装，支持显式安装目录、不修改 PATH 和无人值守模式。PATH 修改需要幂等，避免每次升级重复追加。安装器应检测同名 npm 入口并显示当前实际命中的路径。

uv 官方安装器已经提供自定义目录、不修改 PATH 和 unmanaged 模式；数据目录与程序安装目录分离。这些是可参考的交互约定，不是要求复用 uv 的实现。[uv installer](https://docs.astral.sh/uv/reference/installer/)

## 4. CUDA 自动选择不能只检查“有没有 CUDA”

需要区分硬件、驱动、Toolkit 和应用运行库四个层次。`nvcc` 存在表示开发工具可用，并不能证明当前应用能够运行；缺少 `nvcc` 也不能证明 CUDA 应用无法运行。

NVIDIA 官方说明，`nvidia-smi` 显示的 CUDA Version 是驱动支持的最高 CUDA 版本，并非本机 Toolkit 的安装版本；GPU 架构支持也会随 Toolkit 版本变化。[NVIDIA 架构与驱动矩阵](https://docs.nvidia.com/datacenter/tesla/drivers/cuda-toolkit-driver-and-architecture-matrix.html)

| 用户选择 | 拟议行为 |
| --- | --- |
| `cpu` | 仅下载基础包，不探测或下载 NVIDIA 组件；macOS 现有 Metal 策略另按平台保留 |
| `auto` | 仅在目标有 CUDA 产物且设备、驱动和依赖检查通过时选择 CUDA；未知或不满足时安装基础包并解释原因 |
| `cuda` | 显式请求目标平台的 CUDA 组件；不支持的平台、下载或校验失败时报错，不静默改装基础包；保留在无 GPU 构建机预装的能力，运行库缺失单独诊断 |

脚本中的探测应只用于安装候选包选择。运行时仍需通过 Driver API、GPU 架构和实际后端加载结果确认可用性；不能根据 `nvidia-smi` 的最高版本单独决定 GGUF 13.1/12.4 或 ORT 后端。

CUDA 的小版本兼容也有条件：例如 PTX、新特性和目标架构可能提高实际驱动要求。因此发布清单中的最低驱动必须来自本项目构建与测试，不能直接把 CUDA 大版本兼容表当作全部二进制可运行的保证。[NVIDIA minor compatibility](https://docs.nvidia.com/deploy/cuda-compatibility/minor-version-compatibility.html)

首版有两种明确的产品承诺，工作量不同：

- 延续现状：发布应用 CUDA 组件，要求用户预装匹配的主机运行库；安装器准确检查并说明依赖。这条路径无需先完成 runtime 重打包。
- 仅需驱动即可使用 GPU：按实际链接依赖随包提供可再分发的 runtime/cuBLAS/cuDNN 等组件，验证库搜索路径与许可证，并在没有 Toolkit 的真实 GPU 环境测试。这些是该承诺的上线前置工作。

NVIDIA 官方建议静态链接 CUDA runtime 或随应用分发匹配的动态库；应用可以不依赖用户安装完整 Toolkit，但仍需兼容驱动。Linux 的 SONAME/rpath 和 Windows 的 DLL 搜索路径必须正确。[CUDA runtime 分发指南](https://docs.nvidia.com/cuda/archive/12.8.2/cuda-c-best-practices-guide/index.html#distributing-the-cuda-runtime-and-libraries)

ORT 的 CUDA 与 cuDNN 要求需要按组件分别校验，不能仅依据 GGUF 后端成功就判断 ORT 可用；cuDNN 8 与 9 的主版本不兼容。[ONNX Runtime CUDA requirements](https://onnxruntime.ai/docs/execution-providers/CUDA-ExecutionProvider.html)

NVIDIA 提供按组件划分的 tar/zip 和包含版本、平台、SHA256、大小的 JSON 清单；打包前需核对每个组件的许可证。安装器不应自动安装或升级显卡驱动。[NVIDIA redistributable archives](https://docs.nvidia.com/cuda/cuda-installation-guide-linux/#tarball-and-zip-archive-deliverables)

## 5. 脚本安装后如何升级

首版：用户重新运行安装脚本，默认安装当前稳定版；显式指定版本则安装该版本。再次安装同一版本应幂等，支持按需修复；指定旧版本可作为程序降级入口。

以下仅展示拟议接口，不是当前可用命令；最终参数名、URL 和版本号以实现为准：

```text
sh install.sh
sh install.sh --version <version> --accelerator auto
powershell -File install.ps1 -Version <version> -Accelerator auto
```

安装器应记住安装目录、固定的 GitHub 来源及用户选择，升级时延续它们；记录 `accelerator_requested` 与 `accelerator_actual`，避免将一次 auto 降级永久解释为用户主动选择 CPU。

后续可增加原生 `zg self update`，复用同一清单、校验、安装状态和切换协议。它不应发展成另一套平台/依赖选择逻辑；具体 CLI 语法需结合现有命令结构确定。

现有 `zg --install --upgrade-installed` 处理 Agent 集成配置迁移，不能把它描述成已实现二进制自更新。程序升级应使用独立的命令语义和帮助文案。[当前 upgrade 实现](../crates/zg-cli/src/install/upgrade.rs)

建议为 standalone 安装记录 receipt：`source/channel/version/target/install_dir/accelerator_requested/accelerator_actual/components/hashes`。该记录用于判断归属、重装、升级与卸载，不能仅凭 `zg` 所在路径猜测渠道。

npm 安装继续通过 npm 升级，standalone 自更新只处理本渠道安装。避免覆盖包管理器管理的文件；同机并存时明确提示 PATH 优先级。uv 同样将自更新限制于独立安装，包管理器安装使用原管理器升级。[uv 安装与升级](https://docs.astral.sh/uv/getting-started/installation/)

## 6. 升级事务、后台服务与回滚

建议采用“版本目录 + 稳定入口 + receipt”，程序目录与配置、模型、缓存、索引分开。Unix 可使用 symlink；Windows 可采用不需要管理员权限的原生 shim/指针方案，不能引入 Node 包装器。首版也可采用固定目录、停机后的可回滚替换；不要把 `ln -s` 描述为跨平台原子切换方案。

客户端配置必须引用稳定入口。不能直接把版本目录下的 `current_exe` 路径写入客户端配置，否则第一次升级后客户端仍会启动旧版本。

当前集成迁移把包含 `--server --stdio` 的配置视为兼容，旧绝对路径可能不会被修改；另有通过 `current_exe` 写路径的分支。standalone 上线前需要补足“旧入口 → 稳定入口”迁移，且保留用户的其他配置。[upgrade.rs](../crates/zg-cli/src/install/upgrade.rs)（498、573、699 行附近）

后台服务是升级中的实质缺口：当前 `start_server` 可以复用 ready daemon，`existing_server` 不比较应用版本，`server_status` 检查的是 daemon protocol version。磁盘上切换新程序后，server 模式可能继续执行旧进程，而 direct 模式已经使用新程序。

本地依据：[controller.rs](../crates/zg-daemon/src/controller.rs)（377、402、566 行附近）、[runtime.rs](../crates/zg-daemon/src/runtime.rs)（62 行附近的 status 版本字段）。

建议升级流程：

1. 新包下载、校验、解压和轻量检查全部完成后，才进入切换阶段。
2. 确认需要切换的是本次安装管理的本地 daemon；不要影响用户指定的远程 server。
3. 使用 graceful stop，并对正在执行的任务设定清晰的等待、超时和失败规则。
4. 完成稳定入口与 receipt 切换，启动新 daemon，核验 status 返回的应用版本及健康状态。
5. 明确已有 MCP/stdio 客户端需要如何重连；已有连接不能因磁盘文件变化自动变成新版本。
6. 激活失败时恢复旧入口与旧 receipt，按原状态恢复服务；旧版本文件延迟清理。

Windows 正在运行的 EXE/DLL 可能阻碍覆盖或删除，版本目录与停机切换要一起设计。Unix 也不能仅依赖可替换文件就忽略旧进程和旧库生命周期。

程序回滚不等于索引回滚。若新版本修改了索引格式或配置格式，恢复旧程序可能不足以恢复可用状态；需要独立定义版本兼容策略，不能承诺任意版本无损降级。

卸载默认移除本渠道程序、receipt 和自身 PATH 配置，保留用户配置、模型与索引；数据删除应作为独立的显式选项。

## 7. 工作拆分与上线顺序

| 顺序 | 工作 | 首版是否必需 |
| --- | --- | --- |
| 1 | 固定 standalone 目录契约、六平台与两种 CUDA 产物、生成唯一清单 | 是 |
| 2 | 复用原生 build 打完整归档，补齐校验、资源、许可证及公开下载发布 | 是 |
| 3 | 实现 sh/PowerShell 安装器、无 npm 下载、兼容性判定及诊断 | 是 |
| 4 | 实现 receipt、稳定入口、幂等重装、指定版本与升级失败恢复 | 是 |
| 5 | 修正客户端路径迁移，协调本地 daemon 升级并检查应用版本 | 是 |
| 6 | 补独立安装/升级矩阵测试、使用文档和 npm/standalone 共存说明 | 是 |
| 7 | 打包 GPU runtime 并在无 Toolkit 环境验证 | 仅在承诺 driver-only GPU 体验时必需 |
| 8 | 增加原生自更新命令、基础包与 addon 去重、便捷回滚入口 | 后续增强 |

独立安装验收至少覆盖：

- 六个平台在没有 Node/npm 的环境安装并启动，系统基线不满足时给出准确错误。
- CPU 模式不下载 CUDA；auto 与显式 cuda 的失败语义正确；真实 GPU 验证 GGUF 与 ORT。
- 首装、同版本重装、升级、指定旧版本、失败恢复和卸载；验证配置与索引数据保留。
- 哈希错误、缺失资产、网络中断、下载超时、并发安装及半完成目录不会破坏原版本。
- 自定义目录、空格与中文路径，以及 Windows/macOS/Linux 对路径和权限的差异。
- 已有 npm 安装与 PATH 冲突、已有客户端配置迁移、正在运行的 daemon 与长连接重连。
- 升级完成后 direct/server 返回的新版本与功能行为一致，远程 server 不受影响。

首版延续现有 GPU 主机依赖，不承诺“仅安装驱动即可使用 GPU”。仓库实现与本地测试不能代替六平台和真实 GPU 发布验证；用户安装入口要等首个包含这些资产的 GitHub Release 发布后才可用。
