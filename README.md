# Msk104Master

基于 **Tauri 2 + Rust + React/TypeScript** 的独立 IEC104 模拟主站。

当前版本 `0.1.0` 交付工程骨架、中文工作台与连接参数校验。**尚未实现 104 协议连接、总召、遥控或遥调**；界面不产生模拟接收数据，也不会自动连接设备。

## 工程结构

```text
crates/iec104-core/  独立 Rust 核心，目前包含连接配置与校验
src-tauri/          Tauri 桌面进程、中文日志与命令桥接
src/                React/TypeScript 工作台
scripts/ci/         Windows CI 脚本
docs/               设计与验收边界
```

界面和后续自动化接口共用 Rust 核心。主站独立于 DSP 下位机、DSP 上位机和测试上位机，不依赖被测 DSP 协议实现。

## GitLab 与 CI

- [GitLab 项目](http://192.168.10.198:8929/mskdsp/msk104master)，默认分支 `main`。
- [流水线](http://192.168.10.198:8929/mskdsp/msk104master/-/pipelines)：复用现有 Windows x64 Shell Runner，标签 `windows / amd64 / shell`。
- 执行前端类型检查与构建、Rust 格式检查、Clippy、workspace 单元测试，以及 Tauri Windows 发布构建。
- CI 保存 Windows x64 NSIS 安装包、可执行文件、构建记录与签名；受保护 main 成功构建后发布 R2 测试通道。
- 本地不主动构建、测试或安装环境；相关校验统一通过对应提交的 CI 执行。

依赖由 `package-lock.json` 和根目录 `Cargo.lock` 锁定。首次引导流水线生成锁文件后回收提交；后续 CI 使用 `npm ci` 和 Cargo `--locked`。

Windows CI 的检查、测试和打包统一使用 x64 Release；缓存按实际构建引用保留，不设置大小上限。完整成功后回收确认不再使用的历史产物，失败时保护上次成功所需的缓存，并归档引用、冗余与磁盘可用空间报告。GitLab 产物的 14 天有效期与 Runner 缓存回收分别管理，详见 [CI 缓存治理](docs/CI缓存治理.md)。

## 后续开发入口

在开发环境获得授权并具备 Node.js、Rust、Windows C++ Build Tools 与 WebView2 后，Tauri 开发入口为 `npm run tauri -- dev`。工程常用脚本是 `npm run build` 和 `npm run tauri -- build --no-bundle`；默认由 CI 调用。

下一步先确定协议库及许可、链路状态机、总召和控制命令的验收标准，再实现协议收发与自动化服务。详见 [工程设计](docs/工程设计.md)。

Tauri 配置与平台依赖参考官方 [配置说明](https://v2.tauri.app/reference/config/) 和 [前置条件](https://v2.tauri.app/start/prerequisites/)。

## 安装与更新

在成功流水线的产物中下载 `package/installer/` 下的 NSIS 安装器，运行安装后打开主站。主站侧栏提供“检查更新”，发现新版本后可“下载并安装更新”；安装前校验签名，Windows 安装器会关闭应用并完成升级。

R2 使用与 slaveSim 相同的存储桶，独立更新地址为 [主站 CI 更新清单](https://pub-19f3d71852b04011b120b1b814141c12.r2.dev/msk104master/ci/latest.json)。该通道用于持续交付，尚未设立 Stable 通道。

详见 [安装与更新](docs/安装与更新.md) 和 [验收范围](docs/安装与更新验收.md)。CI 成功不等于已经完成人工安装、卸载和跨版本升级验收。
