# BJUTNet · 校园网自动登录（Windows）

面向 **北京工业大学（BJUT）** 校园网的 Windows 自动登录工具：一个面板 + 一个后台服务，装一次之后
**开机自己检测、自己登录、掉线自己重登**，面板只是可选的查看/修改入口。

> ⚠️ 个人项目，**与学校官方无关**。仅在你自己合法持有的账号上使用；请遵守学校网络使用规定。

## 功能

- **自动识别接入方式**：宿舍有线（Dr.COM eportal）与公共无线 `bjut_wifi`（`/drcom/login`）两套协议
- **用户标记优先**：门户探测不准时，面板里一键"把当前网络标记为校园网"（SSID / 网关 / 网段任一匹配）
- **掉线即重登**：在线每 5 秒探测（可调到 3 秒），实测掉线到重登约 3 秒
- **开机即登录**：服务普通自启（非延迟），没有 IPv4 时每 2 秒重试，网络一就绪立刻登录
- **严格在线判定**：只有 `generate_204` 返回 204 且空 body 才算在线；被门户劫持（200 登录页）不算"有网"
- **密码安全**：密码用 Windows DPAPI 加密存放在 `secrets.dat`，`config.json` 里没有明文密码；日志里账号打码
- **面板（无托盘）**：状态 / 设置 / 日志 / 关于；暂停 30 分钟 / 2 小时 / 直到恢复；「重连并验证」；「关闭服务」
- **单文件安装包**：`BJUTNetSetup-x.y.z.exe`，解包 + 注册服务 + 快捷方式 + 卸载项；卸载器不依赖安装包

## 目录结构

```
crates/
  cn-core/       # 共享库：配置 / 日志 / DPAPI / 网卡 / WLAN / HTTP 探测 / 门户协议 / IPC / 服务控制
  cn-svc/        # Windows 服务（BJUTNetSvc.exe）：状态机 + 登录 + IPC 服务端 + 诊断
  cn-gui/        # 面板（BJUTNet.exe，eframe/egui）
  cn-installer/  # 安装包（BJUTNetSetup.exe，自解压 + 向导）
  cn-uninstaller/# 卸载器（BJUTNetUninstall.exe）
  cn-packager/   # 打包器（cn-pack.exe，把 dist 打成单文件安装包）
assets/icon.ico  # 程序图标（会嵌进 4 个 exe）
build/bundle.sh  # 一键出安装包
tests/           # 本地假 AC（mock_portal.py）+ 端到端脚本（m2/m3/m4/m5）
```

## 构建

前置：Windows 10/11 + Rust stable（MSVC 工具链）+ Windows SDK（`rc.exe`，用于嵌图标/版本信息）。

```bash
cargo build --release            # 产出 target/release/*.exe
bash build/bundle.sh 1.1.3       # 组装 dist/ 并打成 release/BJUTNetSetup-1.1.3.exe
```

> `Cargo.lock` 已提交。若使用离线缓存，`cargo build --release --offline` 需要缓存里已有锁定版本。

运行测试（不需要真实校园网，用本地假 AC）：

```bash
cargo test --offline --workspace
bash tests/m5_fixes_e2e.sh       # 在线判定/图标/网络标记/暂停/掉线重登/关闭服务
bash tests/m2_e2e.sh             # 登录自愈/密码错暂停/账号后缀
bash tests/m3_e2e.sh             # IPC 与面板联动
bash tests/m4_e2e.sh 1.1.3       # 安装包自检
```

## 安装与使用

1. 运行 `BJUTNetSetup-x.y.z.exe`（会请求管理员权限；只注册**服务**开机自启，面板不自启）。
2. 安装完成后面板会自动打开。在「设置」里填学号 + 密码 → 保存（密码经 IPC 交给**服务进程**加密落盘）。
3. 想立刻确认密码对不对：状态页点 **「重连并验证」**（会注销当前会话再登录，断网几秒）。
4. 之后关掉面板即可：服务会在开机后自己检测/登录，掉线自动重登。

卸载：设置 → 应用 → 「BJUTNet 校园网自动登录」，或安装目录下的 `BJUTNetUninstall.exe`。

## 配置

数据目录：`%ProgramData%\BJUTNet\`

| 文件 | 说明 |
|---|---|
| `config.json` | 非敏感配置（账号名、节奏、门户地址、用户标记的网络、日志级别等） |
| `secrets.dat` | DPAPI 加密的学号/密码（绑定服务身份） |
| `logs\svc-yyyyMMdd.log` | 服务日志（账号打码，URL 中的密码会脱敏） |

常用字段：

```json
{
  "account": "20250000",
  "checkIntervalSec": 5,        // 掉线检测间隔（秒，最小 3）
  "offlineIntervalSec": 2,      // 离线/开机重试间隔（秒）
  "probeTimeoutSec": 3,
  "backoff": [2, 4, 8, 15, 30, 60],
  "maxPasswordFails": 3,
  "pauseMinutes": 10,
  "logLevel": "INFO",           // ERR / WARN / INFO / DEBUG
  "dorm":   { "base": "http://10.21.221.98:801" },
  "public": { "base": "https://wlgn.bjut.edu.cn" },
  "trustedNetworks": [
    { "kind": "dorm", "name": "我的宿舍网", "ssid": "", "gateway": "10.126.95.254", "subnet": "10.126.64.0/19" }
  ]
}
```

面板「设置」页可以直接改常用项，保存后服务热重载（200 ms 内生效，无需重启）。

## 命令行（排障用）

```bash
BJUTNetSvc.exe --probe                 # 完整诊断（网卡/判别/两个门户/loadConfig/在线会话）
BJUTNetSvc.exe --login-once            # 判别 + 真实登录一次
BJUTNetSvc.exe --logout                # 注销（宿舍门户）
BJUTNetSvc.exe --status --logs         # 通过 IPC 读取运行中服务的状态/日志
BJUTNetSvc.exe --console --debug       # 前台跑服务（开发/排障）
BJUTNetSvc.exe --install / --uninstall # 手工装/删服务（管理员）
```

## 实现要点

- **在线判定**：`generate_204` 必须返回 `204` 且空 body；200/3xx（门户劫持页）视为"被劫持"=需要登录。
- **账号参数**：`user_account=学号@campus` 里的 `@` **不能** URL 编码（本 AC 按字面解析，`%40` 会被拒）。
- **登录后复核**：登录成功后再打一次 204，不通只警告、下一轮继续复核，避免 AC 延迟下发策略误判。
- **状态机**：`未配置 → 检测 → 登录 → 在线`；网络类失败指数退避，账号密码类累计到阈值才暂停；成功清零。
- **IPC**：命名管道 + 显式 DACL（SYSTEM/管理员全权、普通用户读写）+ 长度前缀 JSON；
  命令包括状态/日志/诊断/保存凭据/改配置/重连/暂停/恢复/重连并验证/标记网络/关闭服务。
- **睡眠唤醒**：比较墙钟与单调时钟，发现睡眠则立即重新探测。

## 已知限制

- 只适配 BJUT 的两个门户（地址在 `config.json`，换了 AC 需要自行调整）；
- 公共无线的注销端点不可靠，所以「关闭服务/暂停」在无线上只影响本程序；
- 掉线发现依赖 HTTP 探测（门户会话掉线不会触发系统事件），默认 5 秒，最低 3 秒；
- 安装需要管理员权限（写 `Program Files`、注册服务）。

## 关于 AI 辅助

本项目在 AI 编程助手的协助下完成：需求拆解、协议分析、代码编写、测试脚本与文档整理都有 AI 参与
（主模型为 DeepSeek 系列，运行在 DeepSeek Harness 中），再由作者在真实校园网环境里逐项验证。

- 关键行为（登录、注销、网络判别、安装、掉线重登、开机自启）都在真实环境或本地假 AC 上实际跑过，
  `tests/` 里的脚本可复现；
- AI 生成的内容可能存在错误或过时信息，使用前请自行审阅代码与配置；
- 欢迎通过 Issue 指出问题或提交 PR。
