# 贡献指南

[English](CONTRIBUTING.md)

开发时使用独立的配置目录和临时工作区。测试不能使用个人账号，也不能调用真实模型提供商。仓库约定见 [AGENTS.md](AGENTS.md)。

## 环境

- Rust stable，安装 rustfmt 和 Clippy。
- Bun，WebUI 使用的包管理器版本见 [package.json](webui/package.json)。
- 真实 WebUI 需要 wasm-pack 和 `wasm32-unknown-unknown` Rust target。
- Windows 需要 MSVC，macOS 需要 Xcode 工具，Linux 需要 C/C++ 工具链。Linux 沙箱测试还需要 bubblewrap 和可用的用户命名空间。

原生客户端和 Android 的环境要求见[客户端指南](clients/app/README.md)。

## 构建与运行

在仓库根目录执行：

```sh
rustup target add wasm32-unknown-unknown
bun scripts/build-webui.ts
cargo run --locked -- --profile dev serve
```

前端脚本安装锁定版本的依赖，构建认证 WASM 和界面。Cargo 读取构建结果，不调用 Bun 或 wasm-pack。前端变更后，需要先重新构建 UI，再构建嵌入页面的服务。

仅使用网页时，首次注册通过本机 CLI 完成：停止开发服务，运行 `daat-locus --profile dev account create NAME --password-stdin`，通过标准输入提供至少 12 个字符的密码，再重新启动该配置目录的服务。原生客户端可以在应用内注册。

仅构建后端可用 `cargo build --locked --no-default-features --bin daat-locus`，它提供 API，不包含页面。

检查时在当前 shell 设置 `DAAT_LOCUS_BUILD_WEBUI=0`，跳过页面嵌入：

```powershell
$env:DAAT_LOCUS_BUILD_WEBUI='0'
```

```sh
export DAAT_LOCUS_BUILD_WEBUI=0
```

构建默认下载 models.dev 目录。离线时，把 `DAAT_LOCUS_MODEL_CATALOG_PATH` 设为已有 models.dev JSON 快照的路径；Cargo 的 `--offline` 不会阻止构建脚本发出 HTTP 请求。`DAAT_LOCUS_WEBUI_DIST` 可以指定另一个已构建的 UI 目录。

## 前端开发

mock 预览不需要服务、账号或 WASM 构建：

```sh
cd webui
bun install --frozen-lockfile
bun run dev:mock
```

打开 Vite 输出地址下的 `/__mock`。场景使用实际组件和内存数据，提供 320px、390px 的预览宽度；切换场景会重置 mock 数据。

连接开发服务并热更新时，改用 `bun run dev`。默认 API 地址为 `http://127.0.0.1:45825`；地址不同时，在启动 Vite 前设置 `DAAT_LOCUS_DAEMON_URL`。账号请求使用相同的浏览器来源。

`DAAT_LOCUS_MOCK_MACOS=1` 和 `DAAT_LOCUS_MOCK_PHONE=1` 可以选择平台 mock。mock 只能检查界面，不能证明原生输入、权限或 WebView 的实际行为。

## 检查

设置 `DAAT_LOCUS_BUILD_WEBUI=0` 后，在根目录运行与修改相关的检查：

```sh
cargo fmt --all -- --check
cargo test --locked -p daat-locus --no-default-features --lib --bins --test http_api -- --test-threads=4
cargo test --locked -p daat-locus-sandbox -p daat-locus-bash -p daat-locus-uutils -- --test-threads=4
cargo clippy --locked -p daat-locus --no-default-features --all-targets -- -D warnings
```

在 `webui` 中运行：

```sh
bun run typecheck
bun run typecheck:mock
bun test
bun run build
```

修改中转服务后，还要在 `workers/signaling` 中运行 `bun run typecheck` 和 `bun test`。[CI 配置](.github/workflows/ci.yaml) 列出各平台环境、驱动检查、单独运行的设备接管用例和原生可选测试。

互不相关的 Rust 测试最多并行四个。原生沙箱测试需要真实系统环境；未能执行的覆盖与通过的测试分别报告，不能为了通过测试改为无隔离执行。

测试应使用独立的预期结果验证行为，使用临时存储、模拟提供商和本地 fixture。真实桌面输入、提权、驱动安装和公网中转测试需要明确选定的测试环境，按需运行。

## 打包

在根目录执行，打包程序会先准备共享前端：

| 产物 | 命令 |
| --- | --- |
| 服务归档 | `cargo xtask package binary --target <triple>` |
| Windows 安装程序，在 Windows 上构建 | `cargo xtask package windows` |
| macOS 安装包，在 macOS 上构建 | `cargo xtask package macos` |
| Android 调试 APK | `cargo xtask package android --debug` |

服务归档内嵌网页界面；原生包由客户端包含页面，配套服务不重复嵌入。产物位于 `target/<triple>/<profile>/package`，可用 `--out-dir` 指定位置。签名与复用已有构建的参数见 `cargo xtask package <format> --help`。

Windows GUI 打包需要已签名的虚拟显示驱动和匹配的辅助程序；macOS、Android 发布构建需要签名凭据。构建安装包前请阅读[客户端指南](clients/app/README.md)。

清理时不要删除 `.dev/windows-signing`，其中保存本机开发签名身份。凭据、私钥、运行数据和构建产物不提交到仓库。分发时保留[归属说明](docs/license-attribution.md)要求的许可文件。

## 提交修改

围绕具体问题修改，说明用户可见的行为、原因和验证结果。界面文字同时更新中英文目录；运行时 prompt 放在 `src/prompts`，内置技能放在 `src/builtin_skills`。指令保持简短，具体工具参数由 schema 说明。

文档只说明当前版本如何使用、构建和测试。发布说明放在 GitHub Releases；不要新增架构文档、任务日志、持续变化的测试计数或中间过程截图。
