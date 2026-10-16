# 贡献指南

[English](CONTRIBUTING.md)

先阅读 [AGENTS.md](AGENTS.md) 和[架构文档](docs/architecture.md)。
Actor 和 App 是唯一核心概念；保留身份、持久消息、权限和增量接口。
不要恢复已删除的 Session、TUI、Dashboard、Sleep、Workflow、SCOPE/LSP 或分享入口。

## 构建与运行

安装 Rust、Bun、wasm-pack：

```powershell
rustup target add wasm32-unknown-unknown
cd webui
bun install --frozen-lockfile
cd ..
cargo run -- serve
```

默认 Cargo 构建会编译并内置 WebUI 和认证 WASM。打开服务输出的地址，登录和写操作
使用同源地址。前端迭代在 `webui` 中执行 `bun run build`，再构建内置客户端的服务。
`webui_fixture` 已删除。手动验证使用独立 profile 和临时工作目录，不能使用真实用户
数据替代测试夹具。

独立视觉预览：在 `webui` 运行 `bun run dev:mock`，打开
`http://127.0.0.1:5173/__mock`（端口以 Vite 输出为准）。一个 Vite 进程即可运行
实际共享 WebUI 和内存 HTTP/SSE mock，无需后台服务、账号、wasm-pack 或 Rust
编译器。场景选择器包含对话、活动、审批、队列、附件、长内容、登录和连接状态。
每个场景同时提供 320px 和 390px 的窄屏预览。
设置和 Actor 弹窗转到主窗口使用实际界面的入口。切换场景只重置 mock 数据。
预览不提供真实认证；发行构建保留 OPAQUE/WASM，并排除预览路由。验证夹具使用
`bun run typecheck:mock` 和 `bun test tests/mock-preview.test.ts`；后台行为仍以
服务契约测试为准。

后台检查设置 `$env:DAAT_LOCUS_BUILD_WEBUI='0'`，无需 Bun 或 wasm-pack。
离线构建可用 `DAAT_LOCUS_MODEL_CATALOG_PATH` 指定有效 models.dev JSON；
Cargo `--offline` 本身不会禁止构建脚本联网。详见[模型目录](docs/model-catalog.md)。

## 后端资源压力测试

单文件脚本 `scripts/perf.rs` 通过 `rust-script scripts/perf.rs` 直接运行，
包含构建、模拟服务、负载、采样和清理，启动实际的 `daat-locus serve`，
以一次性账号、profile、home 和工作区进行测试。模型服务是本机 OpenAI 兼容流式
模拟端点；驱动通过真实 OPAQUE 登录和 HTTP 接口，让多个 Actor 并发处理消息，
每个 Actor 内部串行执行。Actor 仅在启动前通过内部接口植入，不增加产品接口。

```sh
# 首次安装脚本运行器。
cargo install rust-script --locked
# 默认 release：64 个 Actor，各 50 条消息，每次回复 16 KiB，冷却 90 秒。
rust-script scripts/perf.rs
# 保持每个 Actor 的工作量一致，提高并发（也可分别运行 --actors 32 或 64）。
rust-script scripts/perf.rs --actors 128 --rounds 50 --timeout-seconds 3600
# 快速验证；debug 数据不能作为 release 基线。
rust-script scripts/perf.rs --debug-build --actors 2 --rounds 2 --idle-seconds 2 --cooldown-seconds 2
```

需要 Rust 和 rust-script。内嵌 manifest 引用本地 DL/auth crate；rust-script 管理自己的
依赖锁文件和编译缓存。被测 CLI 使用仓库锁文件构建并关闭 WebUI 嵌入。
若也要跳过脚本依赖的 DL 库的 WebUI/WASM 编译，应在运行 rust-script 前设置
`DAAT_LOCUS_BUILD_WEBUI=0`（PowerShell：`$env:DAAT_LOCUS_BUILD_WEBUI='0'`；
POSIX：`export DAAT_LOCUS_BUILD_WEBUI=0`），否则库编译需要正常的 Bun/wasm-pack 环境。
`--skip-build` 复用对应 CLI 产物，源码变化后应重建；`--debug-build` 控制被测 CLI，
rust-script 自身的 `--debug` 仅控制脚本的编译模式。脚本使用默认
`target/<profile>` 输出路径。`--model-delay-ms`、`--sample-ms`、`--timeout-seconds`
分别控制模拟响应延迟、采样周期和测量阶段总超时，初始化子进程各有 60 秒超时。
构建不计入测量。比较结果时应减少其他负载，并重复运行。
默认总超时为 1800 秒，支持 1–256 个并发 Actor；每个 Actor 跨轮保留对话，
增加轮数也会增加历史与持久化开销。默认冷却 90 秒，超过 worker 的 60 秒闲置
回收窗口。负载阶段每 10 秒输出完成进度。

报告位于 `target/perf/<唯一标识>/`，包含 `charts.svg`、`summary.json`、`samples.csv`、
`latencies.csv` 和模拟服务的标准输出/错误。Rust 脚本通过 Plotters 直接生成独立 SVG，
可离线用浏览器或图片查看器打开。图表包含按阶段着色的内存 footprint/CPU 时间曲线，以及
带 P95 标线的已完成消息延迟直方图。CPU 使用累计时间差计算区间平均值，合并阶段
边界的过短区间，避免计数精度造成尖峰。失败、超时或中断时仍保留部分采样并生成
明确标记的部分图表（前提是测量已启动且报告写入成功）。`latencies.csv` 保存已排序
的完成消息延迟，不含未完成消息；图表标注完成数与预期数。
报告记录二进制 SHA256、当前 Git 版本/脏状态、参数、平台、完成
消息数、吞吐量、P95 延迟，以及启动、登录、空闲、负载、冷却各阶段的 footprint/CPU，
RSS 作为辅助数据保留。报告 schema v2 在 CSV 中增加 `footprint_bytes`，阶段统计增加
`footprint_*_bytes`；`footprint_metric` 明确记录各平台的统计口径：

- Windows：Private Bytes（`PROCESS_MEMORY_COUNTERS_EX.PrivateUsage`），进程私有
  提交量，包括已换出的页，不包含尚未提交的地址空间预留和共享分配。这是提交成本，
  不是物理驻留量，也不等同于 macOS 的 `phys_footprint`。
- macOS：`proc_pid_rusage().ri_phys_footprint`，包含压缩内存。
- Linux：`/proc/<pid>/smaps_rollup` 的 PSS + SwapPss，按比例分摊的驻留及交换内存
  估计值；要求两个字段均存在，并有权限读取目标进程的映射统计。

主图始终使用 footprint；采样失败则报告错误，不回退到 RSS 或虚拟地址空间大小。
应在相同系统、相同口径下比较；旧 schema v1 报告只有 RSS，不能换算为 footprint。
当前 Git 版本不能证明复用的二进制来自该版本。没有 Git 元数据的源码包会将
`git_revision`、`git_dirty` 记为 null，应随报告保留源码包哈希和清单。
辅助 RSS 是驻留内存，Windows 上为
Working Set；两种内存指标的峰值/P95 均来自采样，可能漏掉短暂尖峰。CPU 为进程用户态与内核态
累计时间，100% 表示占满一个逻辑 CPU，多核工作可超过 100%。端到端延迟包含
驱动每 100ms 对单条消息状态的有界查询。脚本验证完成回复和模拟请求总数，失败、
超时或强制终止均不会生成成功结论。

仅测量 CLI 进程，排除驱动、模拟服务和账号初始化；覆盖单设备后端持久化、模型流式
处理和 HTTP 开销，不代表真实模型推理、App 工具执行、桌面 WebView、远程同步或
浏览器 SSE 扇出开销。测试期间 Actor 身份和历史仍持久保留，冷却后 footprint 或 RSS 不下降本身
不能证明泄漏。结束时先尝试 OS-owner 正常停止，失败则回收所持子进程句柄；
随后校验路径并删除所持 `TempDir`（账号、数据库和工作区），只在被 Git 忽略的 `target`
中保留报告。不可捕获的强制杀进程或断电可能中断清理。不要传入现有用户 profile。

## 代码与测试

| 位置 | 职责 |
| --- | --- |
| `src/actors` | 执行、持久化、权限与审批 |
| `src/*_app`、`src/app_instructions` | 领域工具及全部内置 App 指令 |
| `src/accounts`、`src/stage`、`src/resources` | 账号、设备协调与统一资源 |
| `src/service` | 有界 HTTP 操作及可续传事件流 |
| `webui/src/components` | 可组合界面、设置与两种固定对话呈现 |
| `webui/src/locales` | 中文、英文资源，新增文案同步维护两份 |
| `tests/http_api.rs`、`tests/http`、`tests/support` | 单一真实 HTTP 测试入口、分领域用例、隔离 OPAQUE 账号与临时存储 |
| `workers/signaling` | 可选公网 P2P 会合与信令，不掌握执行或密码授权 |
| `xtask` | 原生归档与安装包，无需 Git 子模块 |

按改动运行相应检查：

```powershell
cargo fmt --all -- --check
$env:DAAT_LOCUS_BUILD_WEBUI='0'
cargo test --locked --workspace -- --test-threads=4
cargo clippy --locked --workspace --all-targets -- -D warnings
cd webui
bun run typecheck
bun test
bun run build
```

信令修改另在 `workers/signaling` 执行 `bun install --frozen-lockfile`、`bun test`。
测试不能调用真实模型。保留有效的操作系统隔离正反例、HTTP 认证、CSRF、队列版本、
资源授权与断线续传用例。外部环境测试保持显式启用，报告前提和实际失败；不能通过
弱化隔离或私有 ACL 让受限环境的检查通过。库测试不证明完整产品或所有平台可用。

公网信令与账号同步检查使用临时身份和模拟模型。显式设置
`DAAT_LOCUS_TEST_STAGE_SIGNALING` 为待验证的 WSS 地址，再执行
`cargo test --locked --lib public_ -- --ignored --nocapture`，验证 SDP 转发、
认证 P2P 连接、消息复制和私密模型配置同步。这些检查不能证明所有 NAT 下都能直连。

浏览器偏好留在客户端，账号配置私密同步。界面文案使用翻译键，不内联双语选择函数；
语言切换即时刷新已有界面，用户消息、文件和命令原文保持不变。验证语言资源一致、
浏览器默认语言和手动选择。

Windows/macOS 共用 `clients/app/src-tauri` 中的 `daat-locus-app`，与 `daat-locus`
一起构建。后台内置共用 WebUI，不依赖 GUI；客户端单独检查不需要 Bun/WASM。
运行 `cargo test -p daat-locus-app` 和严格 Clippy，原生 CI 覆盖两个平台。
桌面验证账号登录、附件、资源预览、重复启动、隐藏/重开、独立退出客户端与正常停止。
原生代码使用固定的配套 CLI 操作，核对 profile、可执行文件与 loopback origin；
不给 WebView JavaScript 文件、进程或 shell 能力。

## 后台进程与打包

`start --open`、`status --json`、`stop`、`restart` 用本机 OS-owner IPC 管理选定
`--profile`。用临时 profile 验证进程身份、隔离、重启参数与历史保留。

```powershell
cargo xtask package binary --target <triple>
cargo xtask package windows --target x86_64-pc-windows-msvc
cargo xtask package macos --target <macOS-triple>
cargo xtask package android --target aarch64
```

所有打包命令输出 Cargo 风格的状态和可点击产物路径；重定向日志保留纯文本 URL。
`--out-dir` 可自定义产物目录。

Windows 安装包需要 WiX，macOS 安装包需要原生系统工具。
`--skip-build --release-dir <directory>` 打包既有、已核实内置 WebUI 的二进制，
Windows/macOS 同时需要配套桌面客户端。后台专用构建会被拒绝。
Release CI 在各平台原生构建；产物放进忽略的 `target`。
分发前检查依赖归属与许可证，见[归属说明](docs/license-attribution.md)。

## 修改与评审

编辑前检查 Git 状态；保留无关改动、脏子模块工作、凭据和持久 Actor 数据。
删除前解析并验证目标位置；Windows 使用原生 PowerShell 的 `-LiteralPath`。

说明具体行为、目的、验证和实际限制，保持代码、接口、配置、测试与文档一致。
稳定契约归入架构或贡献文档，不新增任务日志、截图交接记录、浮动测试计数或进度
清单；保留版本更新日志。提交标题使用英文并指出实际改动。不提交缓存、构建产物、
运行日志、凭据或未确认实验。

## Android 构建

需要 JDK 17+、Android SDK 36 / NDK 27、Bun、wasm-pack 和 Rust Android targets。
`clients/app/src-tauri/gen/android` 是维护中的 Gradle 源码；生成的 JNI 源码、原生库
和构建输出均已忽略。在 `clients/app` 执行 `bun install --frozen-lockfile`，然后使用
`bun run android:dev` 或设置 `DAAT_LOCUS_ANDROID_SIGNING=/私有路径/release.properties`
后在仓库根目录运行 `cargo xtask package android`。默认构建 ARM64 release APK；
模拟器使用 `--target x86_64`，调试签名使用 `--debug`。产物放在
`target/<Android-triple>/<release|debug>/package`，也可通过 `--out-dir` 指定。
`--skip-build` 校验并复制现有 Gradle 产物；`--apk-dir` 可指定包含
`output-metadata.json` 的其他目录。校验版本、ABI 和内嵌 WebUI 后才报告成功。
底层继续复用现有 Bun 脚本的 Tauri/Gradle 构建与签名流程。

签名配置和 keystore 必须留在仓库外，参见[客户端构建说明](clients/app/README.md#android)。
使用 Android build-tools 的 `apksigner verify --verbose --print-certs` 验证签名，并检查
原生库的 16 KiB ELF 页对齐。禁止提交签名凭据。

必须在设备上验证签名并混淆后的发布 APK，调试包启动无法覆盖 R8。
`ReleaseStartupTest` 仪器测试验证内置登录页加载，不创建账号；请使用独立测试实例。
先构建对应设备 ABI 的发布包，再从 Gradle 目录执行：

```sh
DAAT_LOCUS_ANDROID_SIGNING=/私有路径/release.properties DAAT_LOCUS_ANDROID_ABI=arm64-v8a ./gradlew :app:connectedUniversalReleaseAndroidTest -x :app:rustBuildUniversalRelease
```

x86 模拟器使用 `x86_64`，先运行 `cargo xtask package android --target x86_64`。
排除的 Rust 任务使用刚构建好的原生库，不得使用过期产物运行测试。
