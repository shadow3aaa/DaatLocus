<div align="center">

<img src="assets/logo.svg" alt="Daat Locus Logo" style="width:220px; height:auto;" />

# Daat Locus

<p align="center">
  <img src="assets/preview-tui.png" alt="preview-tui" width="45%" />
  <img src="assets/preview-webui.png" alt="preview-webui" width="45%" />
</p>

[![English][readme-en-badge]][readme-en-url]
[![Crates.io][crates-badge]][crates-url]
[![CI][ci-badge]][ci-url]
[![License][license-badge]][license-url]

</div>

[readme-en-badge]: https://img.shields.io/badge/README-English-blue.svg?style=for-the-badge
[readme-en-url]: README.md
[crates-badge]: https://img.shields.io/crates/v/daat-locus?style=for-the-badge
[crates-url]: https://crates.io/crates/daat-locus
[ci-badge]: https://img.shields.io/github/actions/workflow/status/shadow3aaa/DaatLocus/ci.yml?style=for-the-badge&label=CI
[ci-url]: https://github.com/shadow3aaa/DaatLocus/actions/workflows/ci.yml
[license-badge]: https://img.shields.io/badge/License-Apache%202.0-blue.svg?style=for-the-badge
[license-url]: LICENSE

## 这是什么

Daat Locus 是一个长期运行在本地的 agent runtime。

## 快速开始

推荐使用 `cargo-binstall` 安装，它会下载与你的平台匹配的 GitHub Release 预编译二进制。

目前 daat-locus 只在 Windows 和 MacOS 上测试过，不过 Linux 应该也能正常工作。

```bash
cargo install cargo-binstall
cargo binstall daat-locus

# 或者直接使用 cargo install，这会从源码构建并需要 Bun
cargo install

daat-locus
```

第一次启动时，Daat Locus 会进入交互式引导流程。

### 源码构建

源码构建需要 Bun，因为 `build.rs` 会构建并嵌入 WebUI。

请从 <https://bun.sh/> 安装 Bun，并确保它位于 `PATH` 中，然后再构建。

```bash
git clone https://github.com/shadow3aaa/DaatLocus
cd DaatLocus
cargo run
```

## 常用入口

```bash
daat-locus help                # 显示帮助信息
daat-locus run                 # 打开前台 runtime flow
daat-locus code <project-dir>  # 选择或创建项目作用域 session
daat-locus attach              # attach 到已有 daemon
daat-locus send "..."          # 发送一次消息并等待回复
daat-locus config              # 打开交互式配置菜单
```

## 文档

- [English README](README.md)
- [架构说明](docs/architecture_zh-CN.md)
- [配置](docs/configuration_zh-CN.md)
- [Semantic Code Operation & Propagation Engine](crates/scope-engine/README.md)
- [贡献指南](CONTRIBUTING_zh-CN.md)
- [内置 SOP primitives](workflows/README.md)

## 许可证

Daat Locus 使用 [Apache License 2.0](LICENSE)。
