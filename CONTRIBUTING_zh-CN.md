# 贡献指南

[English](CONTRIBUTING.md)

先阅读 [AGENTS.md](AGENTS.md) 和[架构文档](docs/architecture.md)。
Actor 和 App 是唯一核心概念；保留身份、持久消息、权限和增量接口。
不要恢复已删除的 Session、TUI、Dashboard、Sleep、Workflow、SCOPE/LSP 或分享入口。

## 构建与运行

安装 Rust、Bun、wasm-pack：

```powershell
rustup target add wasm32-unknown-unknown
bun scripts/build-webui.ts
cargo run -- serve
```

`bun scripts/build-webui.ts` 独立构建共享 WebUI 和认证 WASM，输出 `webui/dist`。
Cargo 只消费产物，构建脚本不调用 Bun 或 wasm-pack。默认 `embedded-webui` feature
将资源以 zstd 压缩后嵌入独立网页服务；`DAAT_LOCUS_WEBUI_DIST` 可指定产物目录。
打开服务输出的地址，登录和写操作仍然同源。前端变化后先重建 dist，再构建服务。
Tauri 使用同一 dist，通过 frontendDist 和框架内置压缩管理原生资源；配套后台使用
`--no-default-features`，Android 核心也关闭默认 feature，避免重复嵌入。
Tauri/xtask 打包会先准备前端；纯无头服务使用 `cargo run --no-default-features -- serve`。
`webui_fixture` 已删除。手动验证使用独立 profile 和临时工作目录，不能使用真实用户
数据替代测试夹具。

独立视觉预览：在 `webui` 运行 `bun run dev:mock`，打开
`http://127.0.0.1:5173/__mock`（端口以 Vite 输出为准）。一个 Vite 进程即可运行
实际共享 WebUI 和内存 HTTP/SSE mock，无需后台服务、账号、wasm-pack 或 Rust
编译器。场景选择器包含对话、活动、审批、队列、附件、长内容、登录和连接状态。
每个场景同时提供 320px 和 390px 的窄屏预览。
设置使用实际界面的入口。Actor 链接在桌面打开对话框，在窄屏打开带返回按钮的
全屏对话。切换场景只重置 mock 数据。
运行 `bun run dev:mock` 时设置 `DAAT_LOCUS_MOCK_MACOS=1`，可预览 macOS 标题区
避让和 GUI 设置。窗口命令与隐私授权均为模拟；原生三色按钮和窗口拖动仍需使用
实际 macOS 客户端验证。
预览不提供真实认证；发行构建保留 OPAQUE/WASM，并排除预览路由。验证夹具使用
`bun run typecheck:mock` 和 `bun test tests/mock-preview.test.ts`；后台行为仍以
服务契约测试为准。

后台检查设置 `$env:DAAT_LOCUS_BUILD_WEBUI='0'`，无需 Bun 或 wasm-pack。
离线构建可用 `DAAT_LOCUS_MODEL_CATALOG_PATH` 指定有效 models.dev JSON；
Cargo `--offline` 本身不会禁止构建脚本联网。详见[模型目录](docs/model-catalog.md)。

独立 Rust 测试夹具最多并发四个，避免其他夹具的磁盘与密码学初始化挤占真实租约、
心跳的执行时间。CI 在其他库与 HTTP 测试之后，以独立步骤运行三设备连续接管用例，
并设置 `DAAT_LOCUS_TEST_STAGE_TRACE=1`。多设备夹具与服务一样使用多线程 Tokio 运行时，
避免一台设备的同步存储操作阻塞所有设备的心跳。从并行测试池中排除此用例时，必须保留这个
显式执行步骤，不能减少协调回归覆盖。

多设备测试使用隔离的回环中转，复用生产通道复用和设备 TLS。Rust 中转夹具只模拟注册；Worker 与普通服务器测试验证真实中转认证。
存储调度回归在单 worker 的多线程 runtime 上暂停事务并排队读取，验证控制任务仍可执行；
通道回归验证读、写、鉴权错误的首个原因不会被后续关闭覆盖。

## macOS ComputerUse

macOS 与 Windows 使用同一个 `computer_use` App。独立原生库
`daat-locus-computer-use` 由桌面客户端持有，服务只链接不依赖 GUI 框架的
`daat-locus-client-protocol::computer_use`。构建需要 Xcode 或 Command Line
Tools 的 macOS SDK 和 Swift 编译器；先按正常流程构建共享 WebUI/认证 WASM。
客户端仍支持 macOS 11，实时窗口截图需要 macOS 14+。

客户端通过运行时绑定的私有 `CGVirtualDisplay` 类创建一块真实虚拟屏。
开关需要两项 TCC 授权；“高级”中的像素分辨率和 1×/2× 缩放确认后在原显示器上生效。
实测需验证实际在线枚举、模式尺寸、窗口迁入及归还、预览 PNG 和屏幕移除，不能仅以
返回显示器编号作为成功。Owner 查看不创建屏幕、不请求授权。私有 API 缺失或模式
未确认时拒绝输入。ABI 参考
[Chromium 虚拟屏实现](https://chromium.googlesource.com/chromium/src/+/1b72286560637710932b4ef43908d0d6a121a432/ui/display/mac/test/virtual_display_util_mac.mm)。

```bash
cargo test --locked -p daat-locus-client-protocol -p daat-locus-computer-use
DAAT_LOCUS_BUILD_WEBUI=0 cargo test --locked -p daat-locus-app
DAAT_LOCUS_BUILD_WEBUI=0 cargo clippy --locked -p daat-locus-app -p daat-locus-computer-use --all-targets -- -D warnings
xcrun swift-format lint --strict crates/daat-locus-computer-use/native/macos.swift crates/daat-locus-computer-use/tests/fixtures/NativeFixture.swift
```

原生 GUI 实测为显式 opt-in，需要交互式桌面。使用独立安装的测试 `.app` 与
`mac-test-*` 临时 profile，通过原生界面创建临时账号，在“GUI 操作”中启用并
授予辅助功能、屏幕录制；授权对象应为安装后的客户端，而非服务或终端。
系统认证由操作者完成；系统提示重启时退出并重新打开客户端。ad-hoc 签名构建
替换后可能需要重新授权，必须检查实际状态。先完成安装再授权，实测期间固定测试产物。
若开关已开启但 TCC 仍报告旧代码签名不匹配，需要从两处隐私列表移除旧测试条目，
再添加安装后的 `.app`；仅关闭再开启可能保留旧签名要求。
不修改 TCC 数据库或降低系统防护。

将 `crates/daat-locus-computer-use/tests/fixtures/NativeFixture.swift` 编译为
独立 AppKit `.app`，可执行文件为 `native-fixture`，使用例如
`io.daatlocus.fixture.macos` 的 bundle ID，避免落入自身应用排除前缀
`io.shadow3aaa.daatlocus` 及其子标识，也不能使用旧的 `io.daat-locus.*`。
只打开一个初始状态的测试窗口，并用其他应用遮挡它。

```bash
DAAT_LOCUS_BUILD_WEBUI=0 cargo build --locked --no-default-features -p daat-locus -p daat-locus-app --features daat-locus-app/custom-protocol,daat-locus-app/native-contract
# 将 companion 二进制放入独立测试 bundle 并签名后，从 LaunchServices 启动。
open -n -a '/Applications/Daat Locus GUI Test.app' --args --profile mac-test-native --native-gui-contract /private/tmp/dl-native-result.json
```

隐藏参数仅在 `native-contract` feature 下提供。测试 bundle 也可以在
`LSEnvironment` 中设置 `DAAT_LOCUS_HOME` 为以 `mac-test-` 开头的隔离目录，
并通过 `DAAT_LOCUS_NATIVE_CONTRACT` 指定结果路径。检查最多等待五分钟以完成
真实设置与 TCC 前置条件，然后写出 JSON；前置条件不可用或断言失败时
`passed:false`，应用启动成功不能代替检查报告。运行期间不要操作桌面。
检查覆盖实际 PNG、Unicode 编辑、按钮效果、滚动、滑块、窗口尺寸、实际关闭及
重新后台启动，以及旧图像和过期 epoch 拒绝、取消、Actor 隔离与引用撤销，并比较前台进程和真实指针。
这些结果不替代 App 审批和 schema 测试。普通 CI 覆盖无需系统授权的 IPC、坐标
和 Cookie 回归；GUI runner 必须另行配置交互会话和实际权限。

## Sudo 验证

Sudo 协议、进程树清理与审查回归随 Windows/Linux/macOS 普通后端测试运行。
真实提权单独显式运行：先编译无默认 feature 的 `daat-locus`，将
`DAAT_LOCUS_TEST_COMPANION` 设为该二进制的绝对路径，再运行
`cargo test --no-default-features --lib native_elevation_runs_only_the_helper -- --ignored --nocapture --test-threads=1`。
操作者需完成真实 UAC/polkit/macOS 授权；测试确认 helper 的系统身份与服务身份保持，
普通用户协议 fixture 不能代替提权证据。Windows 已具备 MXC/PSEC 与打包 winget 时可运行
`cargo test --no-default-features --lib sudo_runs_packaged_winget -- --ignored --nocapture --test-threads=1`，
只读取版本，不安装软件或请求 UAC。
Unix CI 通过 runner 的免密 sudo 运行
`root_helper_reads_protected_files_without_elevating_the_service`，验证真实 root helper
可读取受保护文件而调用方保持普通身份；此用例不代替交互式 polkit/macOS 授权框验证。

## Windows Virtual Workspace

窗口观察按需使用 WGC 捕获目标 HWND，被覆盖也不混入其他窗口像素；取帧后关闭会话。
布局优先空位，空间不足时层叠，fit 使用整个工作区，不缩小或挪动其他窗口。
在解锁桌面保持光标静止，运行不依赖 IDD/Hook DLL 的原生回归：

```powershell
cargo test --locked -p daat-locus-virtual-workspace --test native_window_capture -- --ignored --nocapture --test-threads=1
```

该用例验证覆盖窗口下的已知像素、子控件命中、禁用/模态拒绝、输入状态保持和重复捕获清理。
生命周期回归在隔离子进程中交替使用短命 MTA/STA 调用线程，核对已知像素、COM DLL
清理后的模块驻留及子进程退出状态，以捕获 DLL 卸载后的原生回调崩溃。
还验证最小化窗口可以被发现、扫描不会还原窗口，且截图和输入仍被拒绝。
下列 Actor 与计算器用例还验证覆盖窗口下的真实 Hook/UIA 输入，仍需签名 IDD。

Windows x64 服务的 ComputerUse 是普通 Actor 的设备 App，Core 不直接持有。
一项记忆的桌面操作授权明确允许 GUI 越出项目文件沙箱，不扩大 Files/Terminal 权限。
交还窗口先撤销引用再卸载 Hook 和放置；已开始的命令未结束时保持待交还。
构建匹配的 Hook DLL 和 hook_lab、安装签名驱动后，在解锁且前台的终端中保持光标
静止执行真实 App 测试。覆盖完整窗口 PNG 尺寸、放大区域输入命中、尺寸/位置调整、
适应整个工作区、最大化恢复、旧图像撤权，以及调整期间前台/焦点/捕获/光标不变：

```powershell
$env:DAAT_LOCUS_BUILD_WEBUI='0'
cargo test --locked --no-default-features --lib computer_use_app
cargo test --locked --no-default-features --lib computer_use_app::windows::tests::native_actor_take_input_give_revocation_and_inflight_handoff -- --ignored --exact --nocapture --test-threads=1
```

启动位置另需构建当前 CLI 与 `launch_target`，在解锁桌面中保持光标静止、串行运行：

```powershell
cargo build --locked --no-default-features --bin daat-locus
cargo build --locked -p daat-locus-virtual-workspace --example launch_target
cargo test --locked -p daat-locus-virtual-workspace --test native_launch initial_visible_window -- --ignored --nocapture --test-threads=1
cargo test --locked --no-default-features --lib computer_use_app::windows::launch_tests -- --ignored --nocapture --test-threads=1
```

前者检查 EXE 第一次 WM_SHOWWINDOW 的位置，而不是事后移动；后者通过真实清理环境的
启动子命令，使用带空格/中文的路径，验证自动接管、受保护环境变量排除及已有窗口保持。
Shell 显示器提示不是强制隔离：单独 opt-in 的
`calculator_shell_activation_respects_the_virtual_monitor_hint` 兼容性检查目前失败，
计算器会在物理屏显示并激活；不得将其描述为已修复或通过前置条件跳过掩盖。
无法确认的启动继续返回 outcome_unknown。

Windows 发行构建同时携带并验签 Hook DLL、部署 helper 与 IDD 包。
`cargo xtask package windows --windows-certificate-thumbprint <CurrentUser/My指纹>`
完成构建与签名；`--idd-package <目录>` 可提供已有签名驱动包，helper 仍需有效签名。
发行包使用固定的 `Daat Locus Virtual Display` 自签身份，SHA1 指纹为
`C2D252AEFAF27171C36AF1BD6DAF2FDC5441E462`；其他身份的 CAT 在导入信任前拒绝。
签名机需已信任该公钥以完成 SignTool 策略验签；首次引导时可先用 helper 安装
已生成的签名包，再重新验签。私钥和可恢复备份保存在私有 secrets 仓库，安装包只含公钥。
WiX Burn 对机器级驱动安装提权，客户端 MSI 保持 per-user。执行驱动/MSI 前，缓存的
非提权 CLI 通过 OS-owner IPC 停止当前安装在默认及命名 profile 中运行的服务，等待
进程和客户端退出，并阻止安装期间重新启动。完成后按原参数恢复原来运行的服务和
客户端；回滚时先停止恢复的进程，待 MSI 回滚后恢复旧安装。安装器崩溃不永久阻止
启动，重试仍保留此前停止的服务参数。其他用户或外部工作区仍可能导致真实重启要求。
执行 `cargo test --locked -p daat-locus --test installer_upgrade -- --test-threads=1`
验证进程退出、启动阻止和原参数恢复；该套件的 ignored 用例额外要求可工作的签名 IDD，
在已解锁且光标静止的桌面单独运行，以核对真实屏幕移除。安装 WiX 6/Bal，将
`DAAT_LOCUS_TEST_COMPANION` 设为新编译的 `daat-locus.exe` 后执行
`cargo test --locked -p xtask -- service_lifecycle_surrounds_msi_and_restores_the_service_after_rollback --ignored --nocapture --test-threads=1`，
验证真实临时 MSI 的成功及失败回滚路径。Windows CI 显式准备并运行此回归，
不安装驱动、不接触账号 profile。
使用 setup EXE 完成协调升级；单独运行 MSI 不执行 Bundle 的服务协调和驱动安装步骤。
安装页说明将导入
机器 Root/TrustedPublisher 信任；CAT 哈希和证书存在状态检测未变版本。
InstallCondition 在驱动和信任已满足时跳过整个永久前置包，Repair（Burn action 8）
仍执行修复，实际部署保留安装文件缓存供维护。跳过路径不请求机器缓存，也不无条件
计划缓存清理，避免 Burn 6 在无提权执行的 per-user 安装计划中写入 ProgramData 失败；
不放宽系统缓存 ACL。
卸载客户端保留共享适配器/信任；需完整清理时，提权运行
`daat-locus-display-setup.exe uninstall`，移除该身份所有驱动版本后只删除安装器新增的
证书项。需要重启时保留信任，重启后再次运行卸载完成清理。已有证书不删除。
不增加后台服务，不修改启动、Secure Boot、内存完整性或签名强制策略。
实测 Windows 可在新增/移除 IDD 拓扑时把系统光标移至主屏中央，物理输出模式未变
也可能发生；输入隔离测试从屏幕配置完成后取基准，生命周期测试仍检查前台、焦点、
capture、引用撤销与物理输出几何。后端不恢复用户光标来掩盖系统行为。同步模态
输入/WM_CLOSE 处理器可使输入/交还保持 pending 直到退出；任意应用兼容性仍需专项验证。
归档用户解压后只需一次 `sudo ./daat-locus-display-setup.exe install`。
`--skip-build` 仍执行全部必需签名检查。

发行驱动和 helper 使用 SHA256 与 RFC3161 时间戳；默认 DigiCert 服务失败后回退 Sectigo。
打包可用 `--windows-timestamp-server <URL>`、单独构建驱动可用
`--timestamp-server <URL>` 覆盖端点；显式指定其他端点时失败即终止。
服务/客户端 EXE、MSI 与最外层 setup EXE 不做 Authenticode 签名。

Windows CI 以 `--features driver-setup` 运行信任保留/回滚测试。管理员原生往返测试
需先关闭工作区，使用上述完整卸载移除捆绑驱动与证书，并在隔离的 Windows 测试环境运行：

```powershell
$env:DAAT_LOCUS_SIGNED_IDD_PACKAGE="$PWD/target/virtual-workspace-driver-debug"
cargo test --locked -p daat-locus-virtual-workspace --features driver-setup --lib windows::driver_trust::tests::native_bundled_installation_rejects_tampering_and_cleans_trust -- --ignored --exact --nocapture --test-threads=1
```

该测试拒绝篡改 INF 并检查信任回滚，再安装/修复、创建真实屏幕、完整卸载驱动与信任。
结束时驱动未安装，按需重新安装；缺少提权或签名包明确失败，不能据此证明所有系统策略兼容。

另外，在安装 WiX 6/Bal 扩展的**非提权** Windows 终端执行缓存回归：

```powershell
cargo test --locked -p xtask computer_use::tests::present_machine_driver_does_not_require_machine_cache_for_user_install -- --ignored --exact --nocapture
```

该测试使用生产驱动包编排、受控的“驱动已存在”状态和独立临时 per-user MSI，
真正运行 Burn 安装/卸载并核对文件内容及删除结果；不安装驱动或改变机器信任/缓存权限。
完整产品安装和 Repair 仍需验证，归档提取及 layout 检查不能代替 Apply。

独立 lib crate 位于 `crates/daat-locus-virtual-workspace`，`driver/` 包含配套
UMDF/IddCx 驱动源码。lib 构建不构建后台/WebUI；Windows 服务通过 `computer_use`
设备 App 接入，Core 仍仅持有 Actors App。托管屏幕
后端需要 Windows 10 1903+ x64：

```powershell
cargo test --locked -p daat-locus-virtual-workspace --all-targets
cargo clippy --locked -p daat-locus-virtual-workspace --all-targets -- -D warnings
cargo run --locked -p daat-locus-virtual-workspace --example workspace-debug -- probe
```

使用 stable Rust、MSVC x64 工具、Windows SDK 与 libclang 构建配套 Rust 驱动，
libclang 只用于生成 WDK 绑定，不编译 C++ 驱动代码：

```powershell
cargo run --locked -p xtask -- idd-driver
cargo run --locked -p xtask -- idd-driver --check
cargo fmt --manifest-path crates/daat-locus-virtual-workspace/driver/Cargo.toml -- --check
# 发行构建必须提供 CurrentUser/My 中已存在的可信证书：
cargo run --locked -p xtask -- idd-driver --release --certificate-thumbprint '<thumbprint>'
```

xtask 将固定版本/SHA256 的微软 WDK NuGet 包还原到 `target/virtual-workspace-deps`，
编译 UMDF DLL，用 Inf2Cat 校验并生成目录包。默认生成本机签名的开发驱动及匹配的
安装 helper，放在 `target/virtual-workspace-driver-debug`。首次构建在已被 Git 忽略的
`.dev/windows-signing/` 创建代码签名证书和私钥备份，后续复用同一身份。
目录及文件仅允许当前 Windows 所有者访问；PFX 使用随机密码，密码通过 Windows
CurrentUser DPAPI 保护，不能直接复制给其他账户或电脑。CurrentUser/My 的私钥缺失时
从备份恢复；文件不完整、损坏或证书过期时明确报错，不悄悄更换身份。
日常清理请保留此目录。`--check`/`--test` 不生成证书。两种模式的驱动均使用优化后的
静态 release CRT 以满足 Universal API 校验；`--release` 选择发行签名及原有的
`target/virtual-workspace-driver` 输出目录，缺失证书时绝不回退到开发签名。
开发签名采用 SHA256，不访问时间戳服务；显式传入 `--certificate-thumbprint` 则使用
该身份，并将对应公钥嵌入 debug helper。直接 debug 构建 `driver-setup` feature
也复用本机开发身份；release helper 始终嵌入仓库里的发行公钥，忽略
`DAAT_LOCUS_DEVELOPMENT_CERTIFICATE` 覆盖。
xtask 同时执行 Universal InfVerif 与 ApiValidator，DLL 使用静态 CRT。
驱动使用独立 Cargo 构建图和已保存的锁文件，普通 lib 构建不调用 WDK；
`--wdk-root` 可指定现有 WDK 内容根目录。发行时一起分发
INF、DLL、已签名 CAT 和 `LICENSE.microsoft`；未签名构建不能作为可安装发行包。
DLL 签名在生成 CAT 之前完成，使用 SHA256 与 RFC3161 时间戳。捆绑发行使用专用
自签名身份，公钥位于 lib 的 `certificates/virtual-display.cer`，由安装器显式建立信任；
这不等于微软认证，也不消除首次下载提示。须从未信任该证书的干净目标 Windows 开始，
通过真实安装器验证安装和加载，包括 Secure Boot/内存完整性配置；准确记录不兼容项。
构建只将签名私钥导入 CurrentUser/My，不安装驱动、不导入 Root/TrustedPublisher
或修改 Windows 签名策略。信任仅由提权的安装 helper 建立，按证书分别记录归属。
构建不会替换本机已安装的驱动。

发行 helper 通过正常管理员授权导入嵌入的固定公钥证书，再创建并绑定持久 PnP 适配器。
运行时直接打开设备接口，通过 IOCTL 创建归属文件对象的监视器，
不提权、不使用管道/心跳/额外服务。不要求已有虚拟屏、手工导证书、编辑注册表或
修改显示设置。下面只有安装命令需要 sudo：

```powershell
# idd-driver 已构建嵌入匹配开发公钥的 helper：
sudo --chdir "$PWD" "$PWD/target/virtual-workspace-driver-debug/daat-locus-display-setup.exe" install
cargo run --locked -p daat-locus-virtual-workspace --example workspace-debug -- screen --width 1920 --height 1080 --seconds 10
cargo run --locked -p daat-locus-virtual-workspace --example workspace-debug -- lab
# 没有配套驱动时，仅验证原生 WM_POINTER 输入：
cargo run --locked -p daat-locus-virtual-workspace --example workspace-debug -- pointer-lab --self-test
```

库调用方通过 `DriverPackage::open(...).install()` 部署驱动，
服务宿主在 App 外保留 `SharedDisplay::global()`，Windows 服务在对外就绪及激活 worker
之前，在本机 GUI 操作已启用时创建屏幕并等待布局稳定，不预占窗口区域。驱动/桌面前置条件
不可用时记录诊断，账号服务仍可用；ComputerUse 使用时仍返回实际原生错误。
账号维护命令不创建屏幕。普通库构造保持延迟创建，调用方可显式 `initialize()`，
也可等首次 `reserve(width, height)` 创建屏幕。`DisplayRegion` 为窗口保留可重叠的放置租约，
直到物理交还完成。
Windows 的开关和驱动支持的虚拟屏分辨率位于「设置 → GUI 操作」，同一分类在 Android
显示 PhoneUse 内容。偏好按服务 profile 保存在 `computer-use.json`，缺省启用且使用
1920×1080；关闭后下次启动不建屏。关闭先归还窗口再拔屏，调整分辨率先归还窗口并
撤销引用，再在原输出上重配。交还未完成、显示或持久化失败时拒绝新输入，由 Owner 重试。
App 使用 `region.display().id` attach 借用工作区；最后一个区域释放后屏幕保持启用，
最后的服务持有者和窗口区域都释放后才移除。这是进程内共享，不是跨 profile
进程的屏幕代理。在已解锁且装有签名驱动的测试机上，串行运行
`cargo test --locked -p daat-locus-virtual-workspace --test native_shared_display -- --ignored --nocapture --test-threads=1`
验证空闲复用、并发与重叠区域、首次创建之后的光标/拓扑稳定和最后持有者清理；
还检查位置尚未确认时新旧租约均保持有效，其他窗口可以与之重叠。
同一用例还验证未申请窗口区域时即可初始化，以及重复初始化不改变显示拓扑。
在同一桌面设置 `$env:DAAT_LOCUS_BUILD_WEBUI='0'` 后串行运行
`cargo test --locked -p daat-locus --test computer_use_startup -- --ignored --nocapture --test-threads=1`，
验证真实服务在报告就绪前创建输出、无需任何 Actor 工具调用即可空闲保持，退出后移除。

短期诊断工作区仍使用
`VirtualWorkspace::create(DisplayOptions::default())` 创建拥有的屏幕，
`configure(...)` 修改分辨率/位置，`close()` 等待移除。析构或进程退出也会拔除
自己的屏幕，由 WDF 文件清理回调完成；`attach` 借用的输出不被删除。适配器支持
最多 8 个独立 workspace 文件。设备/电源停机退休代次，恢复后不自动重建旧屏幕。
正常 close/Drop 先停用自己的路径并保留其余模式，再拔除屏幕，避免热拔除
重排其他输出；进程被强杀时仍通过文件清理回收监视器，布局由 Windows 处理。
自动位置选择物理屏相邻的空位、优先右侧；相邻位置被占满时可传入显式位置。
保留物理屏启用。
`lab --display '<probe 打印的精确 GDI 名称>'` 仅用于可选的已有屏幕诊断。

夹具在同一进程、同一 GUI 线程创建两个一次性窗口。交互模式下点击 Human 窗口
开始；`--self-test` 只激活自建 Human 窗口并有界运行断言。默认接收窗口自行处理
PA_NOACTIVATE；`--default-activation` 改用系统默认激活行为。与 `--self-test`
组合时，明确要求观察到激活并验证检测结果。夹具不移动、控制或关闭现有用户窗口。
自动断言期间保持物理鼠标静止；交互模式允许用户移动并报告变化，不声称变化来源。
Windows 前台锁策略可能拒绝自动激活夹具；从获得焦点的本机终端运行，或使用
点击启动模式。自动激活被拒绝属于前置条件失败。

需要由真实点击开始、同时保留自动断言时，运行
`workspace-debug lab --self-test --click-to-start`，在两分钟内点击自建 Human
窗口内部。断言检查捕获与原生 pointer 输入，结束后释放屏幕。增加
`--default-activation` 可在自建 IDD 屏幕上测量默认激活行为。
`--default-activation --noactivate-window` 还会让自建接收窗口带有
WS_EX_NOACTIVATE；断言仍要求检测到激活，因为该样式不能抑制默认
WM_POINTERACTIVATE 行为。

原生集成测试默认 ignored，要求已解锁的交互桌面；缺少前置条件时明确失败，
不提前返回成功。逐项执行，避免多个夹具争抢前台。前两项只需物理屏；生命周期
和完整工作区用例需要已经安装的已签名配套驱动，运行无需管理员，会自行创建并释放屏幕，不需要
监视器环境变量。生命周期测试断言创建、调整和回收时原有输出保持不变，完整
工作区用例还检查 IDD 显示中的实际夹具颜色：

```powershell
cargo test --locked -p daat-locus-virtual-workspace --test native_workspace native_pointer_metadata_and_input_preservation -- --ignored --exact --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_workspace default_pointer_activation_is_detected -- --ignored --exact --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_workspace default_pointer_activation_on_owned_display_is_detected -- --ignored --exact --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_workspace noactivate_style_does_not_prevent_default_pointer_activation_on_owned_display -- --ignored --exact --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_workspace owned_display_lifecycle_preserves_existing_outputs -- --ignored --exact --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_workspace closing_one_workspace_preserves_another_on_the_same_adapter -- --ignored --exact --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_workspace owned_monitor_is_removed_when_client_process_is_terminated -- --ignored --exact --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_workspace same_process_same_thread_pointer_delivery_preserves_human_input -- --ignored --exact --nocapture --test-threads=1
```

CI 在各 OS 检查 Rust crate，Windows 构建驱动并校验目录包；托管 CI 不加载
未签名驱动，也未配置交互式 IDD 桌面，不能把 ignored 集成测试报告为通过。pointer-aware 夹具
成功不代表任意应用兼容或独立键盘焦点。Windows 决定 pointer 路由与激活；
不能用 PostMessage 伪造 pointer ID，或恢复用户焦点掩盖失败。
参见[输入 API](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-injectsyntheticpointerinput)
和[激活契约](https://learn.microsoft.com/en-us/windows/win32/inputmsg/wm-pointeractivate)。

可选的 `hook/` Cargo 成员构建 Rust DLL，由 Windows 加载到所选 GUI 线程的
目标进程内；无需修改目标源码或重启应用。这一实验明确允许 DLL 注入。
DLL 与夹具须使用相同 profile/架构构建；在获得焦点的本机终端中执行，保持光标静止：

```powershell
cargo build --locked -p daat-locus-workspace-hook
cargo build --locked -p daat-locus-virtual-workspace --example hook_lab
./target/debug/examples/hook_lab.exe --dll ./target/debug/daat_locus_workspace_hook.dll
# 自动运行测试，保留实时预览窗口：
./target/debug/examples/hook_lab.exe --run-tests
# 自动验证后关闭；仍可使用不依赖驱动的诊断模式：
./target/debug/examples/hook_lab.exe --self-test
./target/debug/examples/hook_lab.exe --physical-screen --self-test
cargo test --locked -p daat-locus-virtual-workspace --test native_hook -- --ignored --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_hosted -- --ignored --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_calculator -- --ignored --nocapture --test-threads=1
```

默认自行创建 1280x720 IDD 屏幕并打开实时预览，点击 Run tests 开始。
DLL 默认取 examples 目录同一 profile 下的构建产物。原生 GUI 线程以 50ms
定时器捕获、绘制实际 BGRA，缩放窗口时保持画面比例。测试阶段和结果显示在
窗口内，成功或失败后继续显示画面，关闭时回收夹具和自有屏幕。预览不转发输入。
`--self-test` 要求物理光标静止并在验证后退出；交互模式允许物理光标移动，仍
检查焦点/capture。`--physical-screen` 保留不依赖驱动的独立双窗口诊断。

控制端启动独立夹具进程，其两个普通 Win32 窗口共用一个 GUI 线程。验证 CBT
激活/焦点否决、标准 EDIT 的 Unicode 文本/导航/Shift 选择、标准 BUTTON 消息
点击、标准滑块从 20 拖至约 80、用户文本/焦点/光标保持、同线程兄弟控件拒绝、修饰键状态恢复、热卸载/重挂、
超时取消无延迟重放、已有用户 capture 时拒绝鼠标操作，以及控制进程死亡后的策略释放。前台激活被拒绝属于前置条件
失败。自有屏幕用例需要已安装的签名驱动，物理屏用例无需驱动；托管 CI 不运行
这些 opt-in 桌面用例，但在 Windows 编译 DLL。

`native_hosted` 在自有 IDD 屏上创建独立框架/内容进程，验证跨进程中文输入与点击、
内容进程更换、无关兄弟窗口拒绝和全部输入路由撤权。`native_calculator` 启动真实的
新计算器实例，仅关闭本次启动识别出的新窗口，不复用或终止已有窗口/进程。
测试端 UI Automation 只读取按钮位置与结果；生产 `WindowInput` 选择 UIA 并调用
实际控件，必须显示 7 + 5 = 12，同时保持前台、焦点、光标和 capture。测试先准备
自己的计算器/用户夹具窗口；临时样式与输入队列准备均在基线前恢复，操作后不恢复焦点。
还检查不支持的通用键盘/插入文本明确拒绝，撤权后不能派发。另一个用例验证纯 Hook
在输入前返回类型化签名策略错误，并回收部分安装的租约。不得关闭系统或应用缓解策略
来让测试通过。普通测试将这些外部环境用例标为 ignored；显式运行时，缺少应用、驱动
或交互前置条件会明确失败。

`WindowInput` 通常选择 Hook；接管时发现签名策略限制，则仅为受限接收者选择 UIA，
其他表面保留 Hook 输入。`backend_for` / `supported_actions_for` 在输入前解析接收者。
UIA 显式关闭
AutoSetFocus，保留允许安装的 CBT 守卫，支持可访问性点击、垂直滚动和关闭；不声称
支持通用按键、插入位置文本或拖曳。失败/未知效果不触发后端重试。应用仍可能不兼容，
必须检查实际结果与桌面回执。`native_hosted` 除跨进程 Hook 路由外，也验证真实 Win32
BUTTON 上的 UIA 操作。
无需 IDD 的 `native_hosted` 用例
`restricted_child_keeps_native_routes_and_skips_disabled_uia_decoration` 对临时子进程
设置真实 Microsoft-only 签名策略，检查普通兄弟表面的中文输入、禁用装饰层下的
UIA 命中、重叠宿主兄弟窗口的正确路由，以及禁用按钮仍被拒绝。
`native_window_capture` 的 DPI 缩小用例检查应用重排后的实际截图尺寸，并拒绝超出
预留区域的窗口。

调用方持有 `WindowHook::attach(&window, absolute_dll_path)` 或初始
`workspace.hook(...)`，用 `text`、`key`、`mouse` 向明确控件 HWND 派发操作。
close/Drop 禁用策略并卸载 Hook，不承诺 DLL 立即完全卸载。仅支持同架构目标；
受保护/提权应用可能拒绝安装或加载。这是同步消息派发，不是独立硬件输入：
尚无 Raw Input、accelerator 消息循环、GetFocus/GetCursorPos 虚拟化和 OLE 拖放。
`drag(control, path, timeout)` 在一次回调内完成 2..=128 个 signed-16-bit
客户区坐标组成的左键拖曳，不 sleep、不跨命令按住；起点必须在客户区内，后续点
允许移出。异常结束派发 WM_CANCELMODE 并释放目标 capture。
已有 capture 时拒绝鼠标操作；原生控件可能在单次点击/拖曳内短暂取得真实 capture，不能
据此宣称支持用户同时拖拽或任意软件。`--native-touch` 单独测量默认窗口处理的
真实触摸；CBT 可阻止激活，但兼容转换仍可能移动系统光标，不能作为保持光标的
后备输入路径。未知效果必须先观察，不自动重放。

`cargo xtask idd-driver --test` 使用驱动相同的 WDK 与静态 CRT 配置运行单元测试，
包括清理/错误状态分类。

定位驱动内部阶段时，仅在实验期间启用 ETW provider：

```powershell
sudo logman start DaatLocusIdd -p '{70b75adc-3a20-4a68-99a4-bc5249203ca0}' -ets -f bincirc -max 64 -o "$PWD/target/idd-diagnostics.etl"
# 执行需要诊断的安装或屏幕操作。
sudo logman stop DaatLocusIdd -ets
```

事件只含阶段与状态码，不含应用数据。没有 provider 元数据时，
`Get-WinEvent -Path ... -Oldest` 的 XML 可能把 EventWriteString 的 UTF-16
内容放在 `EventPayload`。跟踪文件保留在已忽略的 `target` 下。
循环跟踪最多保留 64 MiB。电源回调记录进入/退出、前一/目标 WDF 电源状态、诊断用
D0 周期与获取锁的位置；适配器初始化/异步完成、屏幕移除、对象删除及帧线程停止/
join 也有可关联的阶段事件。驱动对象句柄和每个帧线程的编号关联回调，时间、进程与
线程由 ETW 提供。失败保留 NTSTATUS/HRESULT 类型和原始错误码；异常交换链失败
在启用跟踪时额外查询 D3D 设备移除原因。停止信号、交换链放弃及图形设备停止作为
正常清理记录，不逐帧记录成功或 E_PENDING 等待。未启用跟踪时跳过格式化和分配。
这些日志不改变电源、所有权或重试策略，安装新版驱动后才会产生新增事件；不自动
收集崩溃 dump。
没有 provider manifest 时，可用 PowerShell 7 解码字符串事件：

```powershell
Get-WinEvent -Path "$PWD/target/idd-diagnostics.etl" -Oldest |
  Where-Object ProviderId -eq ([guid]'70b75adc-3a20-4a68-99a4-bc5249203ca0') |
  ForEach-Object {
    $record = $_
    $eventXml = [xml]$record.ToXml()
    $payload = $eventXml.Event.ProcessingErrorData.EventPayload
    $message = if ($payload) {
      [Text.Encoding]::Unicode.GetString([Convert]::FromHexString($payload)).TrimEnd([char]0)
    } else { $record.Message }
    [pscustomobject]@{ Time=$record.TimeCreated; PID=$record.ProcessId; TID=$record.ThreadId; Level=$record.Level; Message=$message }
  } | Format-Table -Wrap
```

这种回退中的 `ProcessingErrorData` 表示读取端缺少元数据，不是驱动失败；应查看解码
后的事件状态和级别。

## Actor 编排评测

通用模型指令维护在 `src/app_instructions/actors.md`。runner 为 Core 和普通
Actor 独立注入它，不依赖 App 安装集合；修改后重新构建后台。场景指导应保持
通用，区分编排失误与工具、环境、权限、运行时故障，在对应层修复问题，不能为
每个失败都追加提示词规则。

模型能力 benchmark 应比较基线与单项策略修改，固定任务版本、模型与提供商设置、
工具、审批模式、环境，以及总时间和 token 预算。成本统计包含全部协作者和自动
审批。使用一次性账号、profile 和获准的隔离任务工作区；独立样本之间不复用
Actor 历史或已修改的代码目录，同一样本内保留相关 Actor 上下文以评估复用。
真实模型评测必须显式运行，不进入普通测试。

记录源码版本及未提交差异、策略哈希、任务 ID、结果、耗时、用量和委派／验证
失败，保留失败与超时样本。采用规则前，比较重复运行和留出任务集的逐任务结果；
不把题目专用解法、隐藏测试预期或 benchmark 身份写入模型指令。生成报告放在
被忽略的输出目录，不提交源码。当前仓库没有 DeepSWE／模型能力评测适配器；
下方模拟负载脚本测量后台资源开销，不能衡量解题能力。

## 设备连接测量

连接测量启用 `daat_locus::stage` debug 日志，比较成功率、建链延迟和校验后的内容吞吐。
覆盖断网重连、网络切换与慢接收方，不记录帧正文或凭据；回环测试不能代表蜂窝性能。

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
脚本依赖的 DL 库与被测 CLI 都关闭默认 feature，无需前端 dist、Bun 或 wasm-pack。
`--skip-build` 复用对应 CLI 产物，源码变化后应重建；`--debug-build` 控制被测 CLI，
rust-script 自身的 `--debug` 仅控制脚本的编译模式。脚本使用默认
`target/<profile>` 输出路径。`--model-delay-ms`、`--sample-ms`、`--timeout-seconds`
分别控制模拟响应延迟、采样周期和测量阶段总超时，初始化子进程各有 60 秒超时。
构建不计入测量。比较结果时应减少其他负载，并重复运行。
默认总超时为 1800 秒，并发 Actor 数量只要求为正整数；每个 Actor 跨轮保留对话，
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
| `src/*_app` | 领域工具与 schema |
| `src/app_instructions`、`src/builtin_skills` | 角色指令与按需内置指导 |
| `src/accounts`、`src/stage`、`src/resources` | 账号、设备协调与统一资源 |
| `src/service` | 有界 HTTP 操作及可续传事件流 |
| `webui/src/components` | 可组合界面、设置与两种固定对话呈现 |
| `webui/src/locales` | 中文、英文资源，新增文案同步维护两份 |
| `tests/http_api.rs`、`tests/http`、`tests/support` | 单一真实 HTTP 测试入口、分领域用例、隔离 OPAQUE 账号与临时存储 |
| `workers/signaling` | 公网中转的 CF Worker/DO 与普通服务器实现，不掌握执行或密码授权 |
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

中转修改另在 `workers/signaling` 执行 `bun install --frozen-lockfile`、`bun test`。
测试不能调用真实模型。保留有效的操作系统隔离正反例、HTTP 认证、CSRF、队列版本、
资源授权与断线续传用例。外部环境测试保持显式启用，报告前提和实际失败；不能通过
弱化隔离或私有 ACL 让受限环境的检查通过。库测试不证明完整产品或所有平台可用。

在 `workers/signaling` 运行 `bun run typecheck` 和 `bun test`，验证普通服务器版。
设置 `DAAT_LOCUS_TEST_RELAY_WORKER=1` 后运行 `bun test tests/local.test.ts`，验证本地真实 DO。
测试使用独立临时目录和回环端口。公网验证显式设置 `DAAT_LOCUS_TEST_RELAY`，执行
`cargo test --locked --lib public_ -- --ignored --nocapture`，覆盖设备认证通道、消息与模型配置同步。
密码登录加入测试另外使用 `DAAT_LOCUS_TEST_ACCOUNT_RELAY`，不依赖真实账号或模型。
仅诊断 fake-IP DNS 时，可用 `DAAT_LOCUS_TEST_RELAY_RESOLVE=relay.example=203.0.113.1`
覆盖库测试中该主机的解析地址；仍使用原域名、TLS SNI 和证书校验。
必须比较两条路径，并单独报告默认路径的失败。

CF 部署在 `workers/signaling` 执行 `bunx wrangler deploy`。保留原有 `SignalGroup`、
`AccountRoom` 绑定和撤销存储，协议 2 使用有界字节通道替换 SDP 信令。旧客户端需要升级，
迁移时停止旧直连版本。普通服务器在相同目录执行 `bun run server.ts`，前方配置支持
WebSocket Upgrade 的 HTTPS 反向代理。`PORT` 默认 8787，`RELAY_LISTEN` 默认回环，
`RELAY_DATABASE` 指定持久 SQLite 路径，重启保留数据库并限制其 OS 访问权限。
`GROUP_ROOTS` 和 `REVOKED_STAGES` 提供与 CF 相同的可选准入策略。
两种实现均提供 `/health` 协议版本查询。DL 显式配置 `mode=relay` 和公网 WSS origin；
`mode=local` 不启动任何跨设备基础设施。中转不保存 Actor 历史、租约或业务提交。

浏览器偏好留在客户端，账号配置私密同步。界面文案使用翻译键，不内联双语选择函数；
语言切换即时刷新已有界面，用户消息、文件和命令原文保持不变。验证语言资源一致、
浏览器默认语言和手动选择。

Windows/macOS 共用 `clients/app/src-tauri` 中的 `daat-locus-app`，与 `daat-locus`
一起构建。先准备共享 dist，原生客户端启用 `custom-protocol`，配套后台使用
`--no-default-features`，不依赖 GUI，也不重复嵌入资源；客户端开发检查可使用 devUrl。
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
macOS 打包会签名配套服务和完整应用包，并校验应用包完整性。默认自动生成并复用
`~/.daat-locus-private/macos-signing/` 中的本地签名身份，不随 checkout 改变。
首次使用可能需要系统认证，仅建立用户级代码签名信任。独立钥匙串会追加到既有
搜索列表，签名时解锁、完成后锁定；密码和加密 PKCS#12 备份只保存在私有目录中。
在其他 Mac 上继续使用同一身份时，应备份并迁移整个目录。身份缺失、损坏或过期
时打包失败，不会悄悄换证书。可用 `--macos-signing-identity <钥匙串签名身份>`
指定已有身份。后续使用同一证书更新时保留代码身份与 macOS 隐私授权；从旧的
ad-hoc 版本迁移时需要重新授权一次，可能需在系统设置中移除再添加应用。
本地签名不能替代 Developer ID 分发和公证。
发布工作流在临时 runner 上显式保留 ad-hoc 签名（`--macos-signing-identity -`），
这些产物不保证跨构建保留隐私授权。
`--skip-build --release-dir <directory>` 打包既有、已核实的产物：binary 归档需要
内置 WebUI 的独立服务；Windows/macOS 安装包需要带原生资源的客户端和不重复
嵌入页面的配套后台。缺失客户端或重复资源都会被拒绝。
`.github/workflows` 只保留两个工作流：`ci.yaml` 检查 debug 编译、测试、警告和格式，
不使用签名私钥；`release.yaml` 在 main 推送、`v*` 标签推送或对这些 ref 手动触发时，
对 Linux、macOS、Windows 和 Android 执行 release 构建、打包和签名校验。
main 构建只在临时 checkout 的 patch 版本后附加 `-dev.YYYYMMDDHHMMSS`（UTC），
所有平台共用本轮时间戳，已校验安装包上传 Actions artifacts，不发布 GitHub Release。
程序版本、Android 版本名及产物文件名携带完整后缀；Windows/macOS 安装器数字版本字段
和 Android versionCode 保留基础版本。版本标签必须匹配仓库版本，保留原版本号，
全部平台构建通过后发布 GitHub Release。

仓库 Actions Secrets 需要以下四项，二进制文件按 base64 编码：

- `WINDOWS_IDD_PFX_BASE64`、`WINDOWS_IDD_PFX_PASSWORD`：固定发行身份的 PFX 和密码。
- `ANDROID_KEYSTORE_BASE64`、`ANDROID_KEYSTORE_PASSWORD`：Android release PKCS12 和密码。

签名备份仍保留在专用私有仓库。Windows runner 将私钥导入 CurrentUser/My，公钥只在
当前用户的 Root/TrustedPublisher 建立验签信任，退出时删除本次导入项；不安装驱动，
不修改系统签名策略。Windows 构建校验驱动 DLL/CAT、INF 目录成员、Hook/helper 签名和
固定身份。Android 恢复权限为 0700/0600 的临时目录/文件，使用 `daat-locus` 别名生成
release APK，验证签名证书 SHA256 与 APK 16 KiB 对齐，最后删除签名文件。
这些检查不替代 Windows 真机驱动加载或 Android 真机启动验证。
分发前检查依赖归属与许可证，见[归属说明](docs/license-attribution.md)。

## 修改与评审

编辑前检查 Git 状态；保留无关改动、脏子模块工作、凭据和持久 Actor 数据。
删除前解析并验证目标位置；Windows 使用原生 PowerShell 的 `-LiteralPath`。

说明具体行为、目的、验证和实际限制，保持代码、接口、配置、测试与文档一致。
稳定契约归入架构或贡献文档，不新增任务日志、截图交接记录、浮动测试计数或进度
清单；保留版本更新日志。提交标题使用英文并指出实际改动。不提交缓存、构建产物、
运行日志、凭据或未确认实验。

## Android 构建

应用 ID 与 Java/Kotlin 命名空间统一为 `io.shadow3aaa.daatlocus`。相较旧包名
`io.daat_locus.app`，Android 会把它作为新应用安装；旧安装保留数据，系统不会
自动跨包迁移私有数据或授权。构建验证使用独立模拟器／测试设备，不卸载个人旧安装。

需要 JDK 17+、Android SDK 37.0 / NDK 27.2.12479018、Bun、wasm-pack 和 Rust Android targets。
项目固定 AGP 9.2.1、Kotlin 2.3.21、Gradle 9.4.1，以满足
librootkotlinx 2.0.0-beta02；Shizuku API/provider 使用 13.1.5。
PhoneUse 的显示隔离要求 Android 15/API 35 以上。
`clients/app/src-tauri/gen/android` 是维护中的 Gradle 源码；生成的 JNI 源码、原生库
和构建输出均已忽略。在 `clients/app` 执行 `bun install --frozen-lockfile`，然后使用
`bun run android:dev` 或设置 `DAAT_LOCUS_ANDROID_SIGNING=/私有路径/release.properties`
后在仓库根目录运行 `cargo xtask package android`。默认构建 ARM64 release APK；
模拟器使用 `--target x86_64`，调试签名使用 `--debug`。产物放在
`target/<Android-triple>/<release|debug>/package`，也可通过 `--out-dir` 指定。
`--skip-build` 校验并复制现有 Gradle 产物；`--apk-dir` 可指定包含
`output-metadata.json` 的其他目录。校验版本、ABI 和内嵌 WebUI 后才报告成功。
底层继续复用现有 Bun 脚本的 Tauri/Gradle 构建与签名流程。

Gradle 从 Cargo 解析 JVM 验证组件版本，将对应 AAR 的 CRL 域名白名单与
`app/network_security_config.xml` 中 DL 的精确本地通信规则合并，生成最终资源。
不要在 `src/main/res` 放置同名静态资源，否则 Android 会覆盖上游 XML，阻断 CRL
下载。明文默认仍禁止，证书验证与签名吊销信息检查保持启用。

签名配置和 keystore 必须留在仓库外，参见[客户端构建说明](clients/app/README.md#android)。
使用 Android build-tools 的 `apksigner verify --verbose --print-certs` 验证签名，并检查
原生库的 16 KiB ELF 页对齐。禁止提交签名凭据。

必须在设备上验证签名并混淆后的发布 APK，调试包启动无法覆盖 R8。
`ReleaseStartupTest` 仪器测试验证内置登录页加载、最终 APK 的 CRL／本地域名规则及
默认禁止明文，不创建账号；请使用独立测试实例。
先构建对应设备 ABI 的发布包，再从 Gradle 目录执行：

```sh
DAAT_LOCUS_ANDROID_SIGNING=/私有路径/release.properties DAAT_LOCUS_ANDROID_ABI=arm64-v8a ./gradlew :app:connectedUniversalReleaseAndroidTest -x :app:rustBuildUniversalRelease
```

PhoneUse 使用两个可丢弃模拟器，分别预置并启动 shell UID 的 Shizuku 与支持标准
`su` 的 Root 管理器，为测试签名的 DL 安装授予权限。不能使用个人账号或清除已有
设备数据。构建设备 APK 后，在 Gradle 目录构建 `:phone-use-fixture:assembleDebug`
与 `:app:assembleUniversalReleaseAndroidTest -x :app:rustBuildUniversalRelease`。
从仓库根目录运行 `python3 clients/app/scripts/test-phone-use.py --help`，传入 serial、
method、三个 APK 路径、`phone-test-*` 账号及环境变量
`DAAT_LOCUS_PHONE_TEST_PASSWORD`。测试通过真实 OPAQUE 页面创建/登录匹配的临时
账号，拒绝其他账号。夹具是独立无权限 APK，不打入 DL。测试涵盖 Owner 校验、
真实 UID、截图、Unicode、按键、主屏焦点/IME 保持、旧状态/代次与取消拒绝、
Actor 隔离、裁剪、释放/接管、其他 Actor 任务保留、任务关闭后立即重启、detach/重置
和关闭开关后的拒绝。还会检查横竖屏 edge-to-edge 几何、设置/抽屉控件、弹窗边界及
真实键盘展开/收起，验证没有残留或重复留白。显示参数仅在点击应用后生效；测试验证
原地调整保留屏幕 ID 和任务内容、拒绝旧观察引用，并在进程重启后保留参数。
运行器分三阶段执行，每阶段之间强制停止 App：原生契约测试结束时保持启用；
第一次重启验证复用已有 Root/Shizuku 授权自动恢复连接，然后主动关闭；
第二次重启验证仍保持关闭并拒绝工具调用。
迭代可用 `-PdaatTestBuildType=debug`，最终还必须覆盖 R8；缺少前提时测试失败。完整链路通过本机确定性模型服务驱动真实 Core 创建普通 Actor，执行 PhoneUse 工具并验证模型图片和认证资源接口；不连接外部模型服务。
CI 的 `android-phone-use` job 通过 `PHONE_USE_RUNNER`、
`PHONE_USE_SHIZUKU_SERIAL`、`PHONE_USE_ROOT_SERIAL` 及私有
`PHONE_USE_TEST_SIGNING`（properties 路径）、`PHONE_USE_TEST_PASSWORD` 运行两种方式。
需预置 SDK/NDK、JDK 17+、模拟器授权与临时账号；未配置 runner 表示缺少原生覆盖。

x86 模拟器使用 `x86_64`，先运行 `cargo xtask package android --target x86_64`。
排除的 Rust 任务使用刚构建好的原生库，不得使用过期产物运行测试。

## MCP 验证

使用隔离的本地 MCP 服务器验证协议、schema、凭据、修订和不确定效果。
运行 `cargo test --lib mcp::tests`、`cargo test --test http_api apps::`，并运行
Actor 权限与执行循环的相关测试。原生 Terminal 契约验证工作区外读取和程序执行、
工作区内可写及工作区外拒写，同时 Files 读取仍受作用域限制。Windows 原生 stdio
测试验证工作区外 MCP 程序启动、宿主读取、范围外拒写、显式环境变量和子进程回收，
无需真实账号或公网 MCP 服务器。
WebUI mock 场景包含应用列表、空列表、加载中和加载失败；连接表单使用
`https://unavailable.example/mcp` 模拟连接失败，mock 不会访问这个地址。


## 原生进程沙箱验证

原生沙箱、Terminal、MCP stdio 与 Actor 审批集成测试在 Windows、Linux 和 macOS
的正常测试套件中运行，无需专用 runner 或 `--ignored`。各用例先调用共享的原生能力
探针；只有明确确认平台前提不受支持时，才直接向 stderr 输出原因并从测试返回。
这表示缺少原生覆盖，不能算作隔离验证通过。探针输出格式异常、探针错误、权限错误
和真实沙箱启动失败都必须使测试失败，不能跳过。运行时仍在无法隔离时明确拒绝，
不会回退为宿主执行。

Windows 使用当前固定版本 MXC SDK 的 PSEC 与 deny paths 能力探针。主机明确缺少
这些能力时才跳过原生用例，探针错误仍失败；无需升级 SDK，也不改变运行时权限、
环境变量处理或审批行为。本机定向测试命令见 CONTRIBUTING.md 的 MCP validation。

Linux CI 仍安装 bwrap 并允许 user namespace。后端需要支持 pidfd/openat2 的
Linux 5.6+、支持 `--disable-userns` 和 `--seccomp` 的 bwrap，以及已审核的
x86-64/AArch64 syscall ABI。bwrap 不存在、已知缺少必需选项、内核缺少必需 syscall
或 syscall ABI 不受支持时可以跳过；创建 namespace 被拒绝属于失败，不能算平台
不支持。测试覆盖有限读写根、缺失路径、并发挂载、私有设备/procfs、内核接口过滤和
子进程清理；syscall 探针使用 rustc 编译。Linux 同时验证受限进程不能凭相同 UID
获得 HTTP 初始化权限，普通独立 Owner 进程仍可使用。

macOS 仅在 `/usr/bin/sandbox-exec` 不存在时跳过原生用例，否则随正常测试验证
Seatbelt，包含特殊路径字符、未来私有路径、符号链接越界、子进程清理及祖先目录
改名。交叉编译不等于这些原生行为已通过验证。GUI、外部服务及会改变
系统状态的测试仍保留原有显式启用方式与前提要求。

