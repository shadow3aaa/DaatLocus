# 贡献指南

[English](CONTRIBUTING.md)

开发时使用独立的配置目录和临时工作区。测试不能使用个人账号，也不能调用真实模型提供商。仓库约定见 [AGENTS.md](AGENTS.md)。

## 环境

- Rust stable，安装 rustfmt 和 Clippy。
- Bun，WebUI 使用的包管理器版本见 [package.json](webui/package.json)。
- 真实 WebUI 需要 wasm-pack 和 `wasm32-unknown-unknown` Rust target。
- Windows 需要 MSVC，macOS 需要 Xcode 工具，Linux 需要 C/C++ 工具链。Linux 沙箱测试还需要 bubblewrap 和可用的用户命名空间。

原生客户端和 Android 的环境要求见[客户端指南](clients/app/README.md)。

## Workspace 文件系统

设备支持原生挂载时，非 Core Actor 可使用 Workspace App。选择 `default` 在原目录工作，或用 `fork` 和 `select` 创建并选择共享的隔离分支。用 `commit` 保存版本，`history`、`diff` 和 `read` 查看历史及内容，从保存的版本创建分支以恢复早期工作。`merge` 将两个引用合并到新分支，`publish` 将分支或保存版本整合到原目录。发生冲突时，使用返回的固定版本和显式路径解决结果继续处理。创建协作 Actor 时，将选中的执行目录作为它的 workspace。

- Windows 使用与服务架构匹配的 WinFsp 2.2 Beta 5 或更新的 2.x 版本。通过管理员授权安装官方签名驱动，服务仍以普通 OS 用户身份运行。文件符号链接、目录符号链接和 junction 会持久化保留类型及目标；指向项目内部的绝对路径链接在挂载时重定位到分支内，发布时再指回 default。项目外部链接保持原生目标语义，并继续受设备沙箱约束。发布符号链接到原生 NTFS 时，需要 OS 用户通常的符号链接创建权限或开发者模式。目前不支持硬链接和备用数据流。Daat Locus 不导入文件系统驱动证书，也不启用测试签名。[WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos](https://github.com/winfsp/winfsp)。
  Windows 安装器通过管理员授权安装 WinFsp，并为本机 cmd 启用 UNC 工作目录（系统级 `DisableUNCCheck=1`）。Workspace 分支使用本地 WinFsp 网络卷。
- Linux 需要可访问的 `/dev/fuse`、可用的 `fusermount3` 或 `fusermount`，以及挂载到 `/sys/fs/fuse/connections` 的 fusectl，并允许挂载用户访问自己连接的 abort 控制。通过发行版常规管理员流程安装 FUSE 软件包。Workspace 挂载不请求 `allow_other`。
- macOS 和 Android 暂无 Workspace 投影后端，不暴露 Workspace App；普通执行目录继续可用。

修改原生文件系统依赖后重启服务。

## 构建与运行

在仓库根目录执行：

```sh
rustup target add wasm32-unknown-unknown
bun scripts/build-webui.ts
cargo run --locked -- --profile dev serve
```

前端变更后，需要先重新构建 UI，再构建嵌入页面的服务。

仅使用网页时，首次注册通过本机 CLI 完成：停止开发服务，运行 `daat-locus --profile dev account create NAME --password-stdin`，通过标准输入提供至少 12 个字符的密码，再重新启动该配置目录的服务。原生客户端可以在应用内注册。

仅构建后端可用 `cargo build --locked --no-default-features --bin daat-locus`，它提供 API，不包含页面。

本地维护 CLI 可列出会话，并向会话 Core 发送消息，无需浏览器 cookie：

```sh
daat-locus sessions list --json
printf '%s' '状态检查' | daat-locus sessions send SESSION_ID --message-stdin
daat-locus sessions messages SESSION_ID --limit 20
```

使用 `--archived` 改为列出已归档会话，`--after` 翻页，`--profile NAME` 选择 profile。消息从 stdin 读取，不会出现在进程参数中。`messages` 也接受 worker ID 用于观察；只有会话 Core 接受 `send`。

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

打开 Vite 输出地址下的 `/__mock`。

连接开发服务并热更新时，改用 `bun run dev`。默认 API 地址为 `http://127.0.0.1:45825`；地址不同时，在启动 Vite 前设置 `DAAT_LOCUS_DAEMON_URL`。账号请求使用相同的浏览器来源。

`DAAT_LOCUS_MOCK_MACOS=1` 和 `DAAT_LOCUS_MOCK_PHONE=1` 可以选择平台 mock。

## 打包

在根目录执行，打包程序会先准备共享前端：

| 产物 | 命令 |
| --- | --- |
| 服务归档 | `cargo xtask package binary --target <triple>` |
| Windows 安装程序，在 Windows 上构建 | `cargo xtask package windows` |
| macOS 安装包，在 macOS 上构建 | `cargo xtask package macos` |
| Android 调试 APK | `cargo xtask package android --debug` |

产物位于 `target/<triple>/<profile>/package`，可用 `--out-dir` 指定位置。签名与复用已有构建的参数见 `cargo xtask package <format> --help`。

Windows 版本保留 BrowserUse，不包含桌面 GUI 控制或虚拟显示。当前 Setup 可执行文件未签名；公开分发时可另行添加 Authenticode 签名。macOS、Android 发布构建仍需签名凭据。构建安装包前请阅读[客户端指南](clients/app/README.md)。

构建清理时不要删除本机凭据、私钥或运行数据；不要将它们及构建产物提交到仓库。分发时保留[归属说明](docs/license-attribution.md)要求的许可文件。

## 提交修改

围绕具体问题修改，说明用户可见的行为、原因和验证结果。界面文字同时更新中英文目录；运行时 prompt 放在 `src/prompts`，内置技能放在 `src/builtin_skills`。指令保持简短，具体工具参数由 schema 说明。

文档只说明当前版本如何使用、构建和测试。发布说明放在 GitHub Releases；不要新增架构文档、任务日志、持续变化的测试计数或中间过程截图。
