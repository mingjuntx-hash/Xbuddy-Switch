# 开发指南

## 环境要求

Node.js ≥ 20、Rust stable、macOS（或 Windows/Linux）。

## 开发命令

```bash
npm install
npm run tauri dev        # 开发模式
npm run build:app        # 构建 debug .app（含前端资源补丁）
npm run build:app:release  # 构建 release .app + 签名更新包
```

## 发布新版本

签名密钥（自动更新用）存放于 `~/.wb-switch/wb-switch-updater.key`，构建脚本通过
`TAURI_SIGNING_PRIVATE_KEY` 注入。发布新版本时：

1. `npm run build:app:release` 生成 `.app.tar.gz` + `.sig`（Windows NSIS 构建会额外生成当前版本的 `*_x64-setup.exe` + `.exe.sig`）。CI 会先清掉 `target/**/release/bundle`，避免 cargo cache 把旧安装包带进 Release。
2. macOS：`UPDATE_OS=macos UPDATE_ARCH=aarch64 sh scripts/gen-update-json.sh` 生成 `latest-macos-aarch64.json`；Intel 用 `UPDATE_ARCH=x86_64`
3. Windows：`UPDATE_OS=windows UPDATE_ARCH=x86_64 sh scripts/gen-update-json.sh` 生成 `latest-windows-x86_64.json`
4. `python3 scripts/merge-update-manifests.py <产物目录>` 合并为 `latest.json`，并把 Windows 平台项写入 `latest-macos-x86_64.json`（兼容已安装的 Windows 客户端）
5. 将安装包、签名更新包、`latest*.json` 一并上传到 GitHub Release

### 签名密钥（本仓库自己的，2026-09-24 生成）

本仓库**不再使用上游作者的密钥**。当前公钥 keyid = `F2FF7C92C710BC24`，
已写入 `src-tauri/tauri.conf.json` 的 `plugins.updater.pubkey`。

| 项 | 位置 |
|---|---|
| 公钥 | `src-tauri/tauri.conf.json`（公开，随代码走） |
| 私钥 | `~/.wb-switch/wb-switch-updater.key`（在**仓库目录之外**，正常无法被提交；`.gitignore` 另有 `*.key` 防御性规则） |
| 私钥密码 | `wb-switch-dev`（与 `package.json` 的 `build:app*` 脚本一致） |

CI 发版（`.github/workflows/build.yml`）还需要在
**仓库 Settings → Secrets and variables → Actions** 配置：

| 名称 | 类型 | 值 |
|---|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | Secret | 私钥文件**全文** |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Secret | `wb-switch-dev` |
| `NPM_TOKEN` | Secret | npm 发布用；不发 npm 就改配下面那个变量 |
| `PUBLISH_NPM` | **Variable** | 设为 `false` 可跳过 npm 发布（缺 NPM_TOKEN 时必设，否则 tag 发版会先挂在 npm 那步） |

⚠️ **换密钥的后果**：`pubkey` 一旦变更，所有已安装的旧客户端都再也无法通过新包的签名校验
（因为校验用的是**客户端内置**的公钥）。所以**除非必要不要轮换**；
真要轮换，就得同时接受「老用户必须手动重装一次」。

### npm 版（webui）发布

1. 编译 server 二进制并上传 GitHub Release（`.github/workflows/build.yml` 自动执行）
2. `cd npm && npm publish`（包名 `xbuddy-switch`，postinstall 按平台从 Release 下载二进制）

## 目录结构

```
src-tauri/
  src/
    commands.rs      # Tauri command 薄包装（对应 Python 版 HTTP API）
    modules/         # 已抽离到 crates/wb-switch-core（三宿主复用）
crates/
  wb-switch-core/    # 核心逻辑：account/auth_file/oauth/process/switch/session/checkin/refresh/update/config
  wb-switch-server/  # HTTP server + CLI：axum API + rust-embed 前端
src/                 # 前端：components/pages/lib（api.ts 双通道：Tauri invoke / HTTP fetch）
npm/                 # npm 包：package.json + bin + scripts/install.js
```

## 隐私注意事项

- 仓库不提交本地数据（accounts.json、认证文件、密钥、token 由 `.gitignore` 排除）
- 发布前用 `git grep` 扫描 token 模式（`ghp_`/`npm_`/`gho_` 等）
