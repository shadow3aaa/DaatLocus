<p align="center">
  <img src="assets/wordmark.svg" alt="DaatLocus" width="640">
</p>
<p align="center"><sub>All in one 的 Agent 平台</sub></p>
<p align="center"><a href="README.md">English</a></p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/home-dark.png">
    <img src="assets/screenshots/home.png" alt="Daat Locus WebUI：在一个对话中并行推进任务" width="1120">
  </picture>
</p>

## 功能

- 独特而简便的统一交互方式，一个窗口便可并行推进一切任务
- 让任何设备互联形成强大的分布式 Agent 网络，当然也可单机使用
- 适配多个平台的虚拟屏 ComputerUse，完全不受打扰地让 Agent 完成 GUI 操作
- 强大的 Agent 浏览器，也可作为普通浏览器使用
- 超低的内存占用，即使多个 Agent 同时工作
- 优雅而简洁的界面
- 支援绝大多数 LLM 提供商，包括 ChatGPT 订阅
- 完善的 MCP 支持

## 安装

从 [Releases](https://github.com/shadow3aaa/DaatLocus/releases) 下载对应平台的软件包。

| 平台 | 软件包 |
| --- | --- |
| Windows x64 | 桌面安装程序，使用 WebView2 |
| macOS | 桌面安装包，使用系统 WebKit |
| Linux | 服务归档，通过浏览器打开界面 |
| Android | 内置服务的 APK |

浏览器和电脑操作需要原生客户端。Linux 支持服务和 HTTP 网页读取，沙箱命令需要 bubblewrap 和可用的用户命名空间。Android 没有终端和 Sudo。

从源码构建网页入口时，安装 [Rust](https://rustup.rs/)、[Bun](https://bun.sh/)、[wasm-pack](https://rustwasm.github.io/wasm-pack/installer/) 和平台构建工具，然后运行：

```sh
git clone https://github.com/shadow3aaa/DaatLocus.git
cd DaatLocus
rustup target add wasm32-unknown-unknown
bun scripts/build-webui.ts
cargo run --locked -- serve
```

打开服务输出的地址。桌面应用和 APK 的构建方法见[原生客户端指南](clients/app/README.md)。

## 互联

推荐用 **Cloudflare Workers** 部署中转，无需自备服务器。在 `workers/signaling/wrangler.jsonc` 填入自己的 Cloudflare 账号和域名，然后运行：

```sh
cd workers/signaling
bun install --frozen-lockfile
bunx wrangler login
bunx wrangler deploy
```

部署后，在每台设备的账号设置中选择中转模式，填写同一个公网 WSS 地址，再登录同一个 Daat Locus 账号。单机使用无需部署中转。更多配置见[中转部署说明](workers/signaling/README.md)。

## 许可证

本项目使用 [Apache-2.0](LICENSE)。所使用的部分开源项目的许可证说明见[第三方归属](docs/license-attribution.md)和 [NOTICE](NOTICE)。
