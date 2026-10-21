# Daat Locus

[English](README.md)

Daat Locus 是只有一个对话入口的 Actor 系统：

```text
Actor = Agent 核心 + 安装的 App 实例
```

账号 Actor 接收请求并委派给持久 Actor。设备共享账号对话和模型上下文、提供工具，
并可接管 worker；普通 Actor 保留宿主。点击 Actor 链接打开工作对话，没有常驻侧栏、
TUI、Workflow 或 Sleep 框架。

WebUI 提供消息、附件、排队引导、紧凑活动与结果。浮动菜单承载账号和设置，另一角
保留对话搜索。界面支持中英文，默认跟随浏览器，也可在个性化中手动选择。
访客分享与公开隧道已移除。账号支持纯本地运行，或通过公网中转连接同账号设备；
中转通道保留设备认证与端到端加密，执行与协调仍属于设备。

## 运行

安装 Rust、Bun、wasm-pack：

```powershell
rustup target add wasm32-unknown-unknown
bun scripts/build-webui.ts
cargo run -- serve
```

先独立构建共享 WebUI 及认证 WASM，Cargo 再以 zstd 压缩嵌入 dist；Rust 构建脚本
不调用前端工具链。纯无头服务关闭默认 feature。打开输出地址，在本机创建账号，或在新设备
使用相同账号名和密码登录。OPAQUE 将密码留在客户端，浏览器使用 HttpOnly cookie。
`--profile NAME` 隔离本机账号。纯本地模式不启动跨设备基础设施；互联模式通过公网 WSS
中转自动发现、连接和同步。中转可部署在 CF Worker/DO 或普通服务器。

`daat-locus --help` 显示原有字符 Logo 与 clap 帮助。`serve` 前台运行；
`start --open`、`status --json`、`stop`、`restart` 通过本机 OS-owner IPC 管理
选定 profile。停止或重启保留 Actor 身份与历史。无界面首次注册使用
`account create NAME --password-stdin`，密码不能放进命令参数。

Windows/macOS 使用[统一 Tauri 客户端](clients/app/README.md)，复用现有 WebUI
与账号接口。Tauri 管理原生资源及压缩，配套后台关闭 embedded-webui；按客户端
指南构建后，将两个可执行文件放在同一目录，运行 `daat-locus-app`。`start --open` 优先打开已安装客户端。
关窗仅隐藏窗口；托盘的“退出客户端”保留后台任务，服务维护通过 CLI 完成。
后台不依赖 GUI。Linux 保留服务与浏览器入口；Android 将相同核心和 WebUI
内置到 APK，可独立运行，无需终端。参见[客户端构建](clients/app/README.md#android)。

## 权限与恢复

| 模式 | 行为 |
| --- | --- |
| 自动批准 | 独立模型只批准或拒绝，错误拒绝，不转人工 |
| 请求批准 | Owner 审批不可修改的操作与请求范围 |
| 完全访问 | 不做审批审查，仍执行身份与访问硬边界 |

额外文件范围只作用于一次操作，不能改写 Actor 的只读身份或长期授权。
工具限制、框架凭据和其他 Actor 私有数据继续受保护；原生隔离不可用时拒绝执行。
文件编辑使用 SHA256 校验；统一资源描述跨设备不传本地路径，读取仍需 Actor 授权。

执行不设消息条数、ReAct 轮次或工具批次上限。上下文满时清空工作窗口并插入历史
恢复提示，不生成摘要、不滑动裁剪。完整历史保留，中断或不明确的副作用不自动重放。
账号 worker 保持健康亲和，由已同步设备接管。新设备作为同步副本加入，不增加永久
投票依赖。账号授权节点失联时，已同步且可执行的设备在持久的本地恢复分支继续运行；
重连后自动合并对话并保留原始分支，不重放结果未知的 App 操作。

## 开发与打包

详见[贡献指南](CONTRIBUTING_zh-CN.md)、[架构](docs/architecture.md)、
[模型目录](docs/model-catalog.md)和[许可证归属](docs/license-attribution.md)。
显式 `DAAT_LOCUS_BUILD_WEBUI=0` 为后台检查跳过 WebUI 资源嵌入。

`cargo xtask package binary --target <triple>` 生成 binstall 归档；
`package windows`、`package macos` 在对应系统构建安装包。binary 归档包含内置
网页的独立服务；原生安装包包含 Tauri WebUI 与不重复嵌入页面的配套后台。项目无需 Git 子模块。
