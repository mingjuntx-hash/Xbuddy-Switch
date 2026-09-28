# xbuddy-switch

WorkBuddy（腾讯 AI 编程助手）账号切换工具。两种形态：

- **桌面 App**：下载 `.app` 双击运行（Tauri，推荐日常使用）
- **npm / webui**：`npm i -g xbuddy-switch` 后运行 `xbuddy-switch`，浏览器打开操作界面

多账号共享登录态（`workbuddy-desktop.info`），一键切换 WorkBuddy 登录账号，并支持将当前账号的会话复制给目标账号（云端归属目标）。

## 快速开始

### npm 安装（webui）

```bash
npm i -g xbuddy-switch
xbuddy-switch              # 启动本地服务 + 自动打开浏览器
xbuddy-switch status       # 终端查看当前账号
```

webui 界面与桌面 App 一致：账号管理、切换、会话复制、自动签到、token 保活、更新检查。

### 桌面 App

从 GitHub Releases 下载对应平台 `.app`（macOS）双击运行。

> **macOS 提示「已损坏，无法打开」？** 未签名应用会触发隔离机制，在终端执行一次即可：
>
> ```bash
> xattr -rd com.apple.quarantine "/Applications/XBuddy-Switch.app"
> ```

## 功能

| 模块 | 说明 |
| --- | --- |
| 账号管理 | OAuth 扫码登录、导入导出账号、删除账号 |
| 账号切换 | 一键切换 WorkBuddy 登录账号，切换过程实时显示进度 |
| 会话复制 | 把当前账号勾选的会话复制给目标账号，源账号数据不受影响 |
| 积分到期查询 | 自动查询每个账号的积分剩余量与到期时间；7 天内到期高亮，并按紧迫程度排序、标注「建议优先使用」 |
| 积分统计 | 汇总官方请求用量：总览、近 30 天趋势、模型分类、账号消耗与请求明细 |
| Token 统计 | 按来源查看 Token 总览与趋势，含构成占比、活跃热力图、项目/模型 Top 10 与会话排行 |
| CodeBuddy CLI | 与 WorkBuddy 复用同一账号库，默认账号独立；切换后立即生效，无需重启 CLI |
| CodeBuddy IDE | 支持切换 CodeBuddy IDE 桌面客户端账号，与 CodeBuddy CLI 相互独立 |
| VS Code CodeBuddy 插件 | 支持切换 VS Code 内的 CodeBuddy 插件账号；VS Code 运行时可自动关闭并在写入后重新打开 |
| JetBrains IDE 插件 | 支持切换 IntelliJ IDEA / PyCharm 内的 CodeBuddy 插件账号；一次切换写入所有装了插件的 IDE |
| 插件会话复制 | 切换插件账号时，可把当前插件账号的会话复制给目标账号（加法，源账号不变） |
| 自动签到 | 默认开启；启动时立即检查，运行期间每 30 分钟自动补签；一键全部签到；30 天签到日志 |
| Token 保活 | 惰性刷新（操作前不足阈值刷新）+ 每日保活（默认每天无条件刷新一次，阈值 >0 时仅刷新剩余不足该天数的账号），避免 refresh token 过期 |
| 自动轮换 | 后台把积分最紧迫的账号设为 CodeBuddy CLI 后续启动账号；检测到 CLI 会话运行时会跳过 |
| 自动更新 | 从 GitHub Releases 检查新版本，整包更新经签名校验 |
| 权限检测 | macOS 授权引导（App 管理 / 完全磁盘访问拖拽授权 + 自动检测） |

## 使用

1. **添加账号**：账号页 →「扫码登录」（OAuth device flow）或「从本机导入」「手动添加」
2. **切换账号**：账号卡片 →「切换」，可勾选复制当前会话
3. **查看积分与统计**：账号页自动查询各账号积分到期情况，点「刷新积分」手动更新；侧栏进入「积分统计」「Token 统计」查看用量明细
4. **切换各客户端账号**：CodeBuddy CLI、CodeBuddy IDE、VS Code CodeBuddy 插件、JetBrains IDE 插件均可在账号卡片一键切换；其中 CodeBuddy IDE 与两个插件端支持在弹窗中勾选复制当前账号的会话。CodeBuddy IDE 首次使用前需先手动打开并登录一次
5. **自动签到**：账号页可直接开关；设置页可调整保活参数、立即签到并查看日志
6. **自动轮换**：设置 → CodeBuddy CLI 自动轮换，开启后按积分紧迫程度自动设置默认账号
7. **更新**：设置 → 自动更新可检查公开 GitHub Releases 源；npm 版本也可通过 `npm update -g xbuddy-switch` 升级

### macOS 权限说明

切换账号需要写入 WorkBuddy 认证文件，macOS 要求授权「App 管理」（或「完全磁盘访问」）：

1. 首次切换报「无权限」时，点「打开系统设置」
2. 优先在 **App 管理** 里打开 XBuddy-Switch 开关；若没有，则去 **完全磁盘访问** 把 XBuddy-Switch 拖进带箭头的框
3. 授权后重启本应用生效；设置页「权限检测」可随时验证

> webui 模式：由启动服务的终端进程权限决定；若终端已授权完全磁盘访问则无需额外操作。

## 许可

[MIT](./LICENSE)
