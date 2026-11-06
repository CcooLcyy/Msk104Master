# Msk104Master

基于 **Tauri 2 + Rust + React/TypeScript** 的独立 IEC104 模拟主站。

当前版本 `0.1.0` 包含中文工作台、连接参数校验与首版独立 IEC104 协议内核。内核提供 TCP 会话、链路管理、总召及常用遥信/遥测解析；**桌面界面尚未接入内核，安装版仍不能建立 104 连接**。遥控、遥调尚未实现；界面不产生模拟接收数据，也不会自动连接设备。

## 工程结构

```text
crates/iec104-core/  独立 Rust 核心，含配置、报文编解码与 TCP 会话
src-tauri/          Tauri 桌面进程、中文日志与命令桥接
src/                React/TypeScript 工作台
scripts/ci/         Windows CI 脚本
docs/               设计与验收边界
```

界面和后续自动化接口共用 Rust 核心。主站独立于 DSP 下位机、DSP 上位机和测试上位机，不依赖被测 DSP 协议实现。

## GitHub Actions 与 CI

- [GitHub 项目](https://github.com/CcooLcyy/Msk104Master) 默认分支为 `main`；[Actions](https://github.com/CcooLcyy/Msk104Master/actions) 负责 Windows 检查、NSIS 打包和 R2 更新发布。
- push、PR 与手动运行执行前端测试/类型检查/构建、Rust 格式、Release Clippy、workspace 测试与安装包构建；日志和交付产物保存 14 天。
- 只有本仓库 main 的 push/手动运行且 `R2_PUBLISH_ENABLED=true` 才使用 `ci` 环境密钥签名并发布 R2。未开启发布、其他分支及 PR 只生成未签名安装包。
- [GitLab 项目](http://192.168.10.198:8929/mskdsp/msk104master) 保留源码和未签名检查构建，R2 发布任务已删除。
- 本地不主动构建、测试或安装环境；相关校验统一通过对应提交的 CI 执行。

首次迁移需要配置 GitHub 环境 Secrets 与版本基数，详见 [GitHub Actions 流程与配置](docs/GitHub-Actions.md)。依赖由 `package-lock.json` 和根目录 `Cargo.lock` 锁定，日常使用 `npm ci` 与 Cargo `--locked`。

Windows 检查、测试和打包统一使用 x64 Release；GitHub 使用独立托管 Runner，不恢复整个 target，仍保留引用登记和磁盘检查。原持久 Runner 缓存规则见 [CI 缓存治理](docs/CI缓存治理.md)。

## 后续开发入口

在开发环境获得授权并具备 Node.js、Rust、Windows C++ Build Tools 与 WebView2 后，Tauri 开发入口为 `npm run tauri -- dev`。工程常用脚本是 `npm run build` 和 `npm run tauri -- build --no-bundle`；默认由 CI 调用。

首版内核的范围、接口及验证边界见 [协议内核首版](docs/协议内核首版.md)。后续将桌面与自动化接口接入同一会话，再独立实现控制命令。初始骨架验收记录见 [工程设计](docs/工程设计.md)。

Tauri 配置与平台依赖参考官方 [配置说明](https://v2.tauri.app/reference/config/) 和 [前置条件](https://v2.tauri.app/start/prerequisites/)。

## 安装与更新

在成功流水线的产物中下载 `package/installer/` 下的 NSIS 安装器，运行安装后打开主站。主站打开时及此后每分钟自动检查更新，也可手动“检查更新”；发现新版本立即下载安装，无需确认。安装前校验签名，Windows 安装器会关闭应用并中断现有连接，完成升级。旧版本首次升级到本版本仍需点击原有的“下载并安装更新”。

R2 使用与 slaveSim 相同的存储桶，独立更新地址为 [主站 CI 更新清单](https://pub-19f3d71852b04011b120b1b814141c12.r2.dev/msk104master/ci/latest.json)。该通道用于持续交付，尚未设立 Stable 通道。

详见 [安装与更新](docs/安装与更新.md) 和 [验收范围](docs/安装与更新验收.md)。CI 成功不等于已经完成人工安装、卸载和跨版本升级验收。
