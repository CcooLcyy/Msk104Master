# GitHub Actions 与 R2 发布

## 目标与验收

源码、Windows x64 检查与 NSIS 打包、更新签名和 R2 发布统一由 GitHub 承载。本地 `origin` 指向 `https://github.com/CcooLcyy/Msk104Master.git`。

- push、PR 和手动运行执行 Python CI 边界测试、前端测试/类型检查/构建、Rust fmt、Release Clippy、workspace 测试和 NSIS 打包。
- 普通分支、PR 或尚未开启发布时只生成未签名安装包，归档产物和日志 14 天。
- 仅本仓库 main 的 push 或手动运行，且仓库变量 `R2_PUBLISH_ENABLED=true` 时进入 `ci` 环境，使用原签名私钥生成更新包并发布 R2。其他运行使用不含发布凭据的 `checks` 环境。
- Linux 发布前再次运行 Python 测试，核对提交、版本、产物摘要，确认 main 未前进，再上传安装包及签名，最后切换 `latest.json`。同版本重编产物使用安装包与签名的内容指纹目录，避免覆盖缓存中的旧文件。
- 发布作业串行且不主动取消；旧提交、旧版本及输入不一致必须失败，不能覆盖新清单。本地不运行构建或测试，推送后不自动跟踪 CI。

## 发布配置

当前 `ci` 环境仅允许 `main`，仓库变量为 `CI_VERSION_BASE=23` 和 `R2_PUBLISH_ENABLED=true`。

1. GitHub 仓库 Settings → Environments → `ci`，Deployment branches and tags 只允许 `main`。`checks` 环境不放任何凭据。
2. 在 `ci` 环境配置 `TAURI_SIGNING_PRIVATE_KEY` 和可选的 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。签名私钥须与客户端内置公钥配对。在仓库或 `ci` 环境 Secrets 配置 `R2_ACCOUNT_ID`、`R2_ACCESS_KEY_ID`、`R2_SECRET_ACCESS_KEY`。
3. 版本序号为仓库变量 `CI_VERSION_BASE` 加本工作流的 `github.run_number`。基数固定为 23，重跑同一 run 保持版本号。缺少基数时，未签名检查以 0 为基数，签名发布失败。
4. 通过仓库变量 `R2_PUBLISH_ENABLED` 控制发布。值为 `false` 时，后续运行只执行未签名检查与打包。
5. Actions → Windows CI 与 R2 发布 → Run workflow，选择 `main`。缺少所需签名或 R2 密钥会失败，不能跳过发布检查冒充成功。

## 日常流程与产物

提交到 GitHub → Windows 检查与打包 → `windows-x64-<run_id>-<run_attempt>` 产物。开启发布的 main 再进入 Linux R2 发布，归档 `r2-manifest-<run_id>-<run_attempt>`。从 Actions 运行页的 Artifacts 下载 Windows 安装包，失败日志单独归档。

更新地址保持 `https://pub-19f3d71852b04011b120b1b814141c12.r2.dev/msk104master/ci/latest.json`，存储桶为 `mskdsp-update`，仅写 `msk104master/ci` 前缀，不改其他项目。本流程发布 CI 更新通道。

Windows 使用 GitHub 托管的 `windows-2022`、Node.js 22、Python 3.12 和 Rust 1.97.1，通过 vswhere 定位 MSVC。仅缓存 npm 下载；不恢复整个 target，避免跨工作区绝对路径身份冲突。依赖变更时更新并提交对应锁文件。

CI 运行成功仅代表该提交通过自动检查和构建；真实安装、卸载和跨版本升级仍需 Windows 桌面验收。

官方依据：[工作流语法](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)、[Windows Runner 镜像](https://github.com/actions/runner-images/blob/main/images/windows/Windows2022-Readme.md)、[产物归档](https://github.com/actions/upload-artifact)。
