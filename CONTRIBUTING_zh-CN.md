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

## Windows Virtual Workspace

Windows x64 服务的 ComputerUse 是普通 Actor 的设备 App，Core 不直接持有。
一项记忆的桌面操作授权明确允许 GUI 越出项目文件沙箱，不扩大 Files/Terminal 权限。
交还窗口先撤销引用再卸载 Hook 和放置；已开始的命令未结束时保持待交还。
构建匹配的 Hook DLL 和 hook_lab、安装签名驱动后，在解锁且前台的终端中保持光标
静止执行真实 App 测试。覆盖完整窗口 PNG 尺寸、放大区域输入命中、尺寸/位置调整、
适应空闲区域、最大化恢复、旧图像撤权，以及调整期间前台/焦点/捕获/光标不变：

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

驱动和 helper 使用 SHA256 与 RFC3161 时间戳；默认 DigiCert 服务失败后回退 Sectigo。
打包可用 `--windows-timestamp-server <URL>`、单独构建驱动可用
`--timestamp-server <URL>` 覆盖端点；显式指定其他端点时失败即终止。
服务/客户端 EXE、MSI 与最外层 setup EXE 不做 Authenticode 签名。

Windows CI 以 `--features driver-setup` 运行信任保留/回滚测试。管理员原生往返测试
需先关闭工作区，使用上述完整卸载移除捆绑驱动与证书，并在隔离的 Windows 测试环境运行：

```powershell
$env:DAAT_LOCUS_SIGNED_IDD_PACKAGE="$PWD/target/virtual-workspace-driver"
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
# 可使用 CurrentUser/My 中已存在的可信证书签名：
cargo run --locked -p xtask -- idd-driver --certificate-thumbprint '<thumbprint>'
```

xtask 将固定版本/SHA256 的微软 WDK NuGet 包还原到 `target/virtual-workspace-deps`，
编译 UMDF DLL，用 Inf2Cat 校验并生成目录包；产物位于 `target/virtual-workspace-driver`。
xtask 同时执行 Universal InfVerif 与 ApiValidator，DLL 使用静态 CRT。
驱动使用独立 Cargo 构建图和已保存的锁文件，普通 lib 构建不调用 WDK；
`--wdk-root` 可指定现有 WDK 内容根目录。发行时一起分发
INF、DLL、已签名 CAT 和 `LICENSE.microsoft`；未签名构建不能作为可安装发行包。
DLL 签名在生成 CAT 之前完成，使用 SHA256 与 RFC3161 时间戳。捆绑发行使用专用
自签名身份，公钥位于 lib 的 `certificates/virtual-display.cer`，由安装器显式建立信任；
这不等于微软认证，也不消除首次下载提示。须从未信任该证书的干净目标 Windows 开始，
通过真实安装器验证安装和加载，包括 Secure Boot/内存完整性配置；准确记录不兼容项。
xtask 不安装驱动、不修改证书存储或 Windows 签名策略。

发行 helper 通过正常管理员授权导入嵌入的固定公钥证书，再创建并绑定持久 PnP 适配器。
运行时直接打开设备接口，通过 IOCTL 创建归属文件对象的监视器，
不提权、不使用管道/心跳/额外服务。不要求已有虚拟屏、手工导证书、编辑注册表或
修改显示设置。下面只有安装命令需要 sudo：

```powershell
cargo build --locked -p daat-locus-virtual-workspace --features driver-setup --bin daat-locus-display-setup
sudo --chdir "$PWD" "$PWD/target/debug/daat-locus-display-setup.exe" install --package "$PWD/target/virtual-workspace-driver"
cargo run --locked -p daat-locus-virtual-workspace --example workspace-debug -- screen --width 1920 --height 1080 --seconds 10
cargo run --locked -p daat-locus-virtual-workspace --example workspace-debug -- lab
# 没有配套驱动时，仅验证原生 WM_POINTER 输入：
cargo run --locked -p daat-locus-virtual-workspace --example workspace-debug -- pointer-lab --self-test
```

库调用方通过 `DriverPackage::open(...).install()` 部署驱动，
服务宿主在 App 外保留 `SharedDisplay::global()`，Windows 服务在对外就绪及激活 worker
之前调用 `initialize()` 创建屏幕并等待布局稳定，不预占窗口区域。驱动/桌面前置条件
不可用时记录诊断，账号服务仍可用；ComputerUse 使用时仍返回实际原生错误。
账号维护命令不创建屏幕。普通库构造保持延迟创建，调用方可显式 `initialize()`，
也可等首次 `reserve(width, height)` 创建屏幕。`DisplayRegion` 为窗口保留互不重叠的位置，
直到物理交还完成。
App 使用 `region.display().id` attach 借用工作区；最后一个区域释放后屏幕保持启用，
最后的服务持有者和窗口区域都释放后才移除。这是进程内共享，不是跨 profile
进程的屏幕代理。在已解锁且装有签名驱动的测试机上，串行运行
`cargo test --locked -p daat-locus-virtual-workspace --test native_shared_display -- --ignored --nocapture --test-threads=1`
验证空闲复用、并发区域、首次创建之后的光标/拓扑稳定和最后持有者清理；
还检查位置尚未确认时新旧区域同时保留，其他客户端不能提前复用任一位置。
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

`WindowInput` 通常选择 Hook，仅在接管时遇到签名策略限制才选择 UIA。UIA 显式关闭
AutoSetFocus，保留允许安装的 CBT 守卫，支持可访问性点击、垂直滚动和关闭；不声称
支持通用按键、插入位置文本或拖曳。失败/未知效果不触发后端重试。应用仍可能不兼容，
必须检查实际结果与桌面回执。`native_hosted` 除跨进程 Hook 路由外，也验证真实 Win32
BUTTON 上的 UIA 操作。

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

通用模型指令维护在 `src/app_instructions/orchestration.md`。Core 和安装管理型
Actors App 的 hosted Actor 自动加载它；修改后重新构建后台。场景指导应保持
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

连接性能测量可启用 `daat_locus::stage` 的 debug 日志；`native_dial`、
`offer_candidates`、`answer_candidates` 和 `ice_dtls_sctp` 分阶段记录耗时和结果，
不记录 SDP 或凭据。内容完成日志记录原始字节与耗时，传输计数区分原始内容字节和
编码后的协议字节。同一网络重复比较成功率、建链耗时中位数／尾部值与校验后的
内容吞吐，并记录发起方向、IPv4／IPv6、网络切换和丢包；本机回环测试不能代表蜂窝性能。

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

## MCP 验证

使用隔离的本地 MCP 服务器验证协议、schema、凭据、修订和不确定效果。
运行 `cargo test --lib mcp::tests`、`cargo test --test http_api apps::`，并运行
Actor 权限与执行循环的相关测试。Windows 原生 stdio 测试同时验证工作区读取、
私有存储拒绝、显式环境变量和子进程回收，无需真实账号或公网 MCP 服务器。
WebUI mock 场景包含应用列表、空列表、加载中和加载失败；连接表单使用
`https://unavailable.example/mcp` 模拟连接失败，mock 不会访问这个地址。


## 原生进程沙箱验证

Windows 已改用 MXC/PSEC，不再修改宿主文件 ACL。通用 Windows runner 验证不可用时
明确拒绝受限执行；原生正反例需要实际支持 PSEC deny paths 的主机。配置仓库变量
`MXC_RUNNER` 为已准备好的 Windows runner 标签后，CI 的独立原生任务会先探测能力，
再执行沙箱、Terminal 和 MCP 测试；缺少能力必须失败，跳过任务不算原生验证通过。
具体本机命令见 CONTRIBUTING.md 的 MCP validation。

Linux CI 安装 bwrap 并允许 user namespace，显式运行全部 ignored 沙箱测试。后端需要
支持 pidfd/openat2 的 Linux 5.6+、支持 `--disable-userns` 和 `--seccomp` 的 bwrap，
以及已审核的 x86-64/AArch64 syscall ABI。测试覆盖有限读写根、缺失路径、并发挂载、
私有设备/procfs、内核接口过滤和子进程清理；syscall 探针使用 rustc 编译。
macOS 的原生 Seatbelt 测试随正常测试执行，包含特殊路径字符、未来私有路径、
符号链接越界及祖先目录改名。维护 socket 用例会初始化 OS-owner 控制目录，
本机显式运行，macOS CI 必须执行；用例只删除自己创建的唯一 socket。
Linux 同时验证受限进程不能凭相同 UID 获得 HTTP 初始化权限、普通独立 Owner 进程仍可使用。
交叉编译不等于这些原生行为已通过验证。
