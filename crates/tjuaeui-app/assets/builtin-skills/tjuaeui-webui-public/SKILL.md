---
name: tjuaeui-webui-public
description: 通过 Cloudflare Quick Tunnel 将本机 TjuaeUI WebUI 临时暴露到公网。负责检测服务、安装 cloudflared、创建并验证公网地址，并明确说明临时地址、进程存活、密码保护和流量经 Cloudflare 中转等限制。适用于用户希望从外网访问或临时分享 TjuaeUI 的场景。
---

> **平台提示：**以下命令按 macOS / Linux 的 bash/zsh 编写。Windows 上应转换
> 为 PowerShell；`python3` 改为 `python` 或 `py`，`~/` 改为
> `$env:USERPROFILE\`，`/tmp` 改为 `$env:TEMP`。若没有直接等价语法，优先使用
> 内置文件或 HTTP 工具。

# TjuaeUI WebUI 公网临时访问

目标是用尽量少的用户操作，把本机 WebUI 暴露为临时公网 URL。除桌面端开关外，
检测、安装、启动和验证都应由智能体完成；除非用户要求，不要把原始命令甩给用户。

## 已知事实

- WebUI 默认监听 `25808`，但可通过 `--port` 修改，因此先探测，不要只依赖默认值。
- WebUI 使用用户名、密码和 JWT 认证。发布到公网后，强密码是主要保护手段。
- 桌面 WebUI 只能通过 Electron IPC 启动，智能体无法代替用户切换开关。
- Tunnel 使用 Cloudflare Quick Tunnel，无需账号，但地址随机且临时。
- 必须为 `cloudflared` 指定 `--protocol http2`，以避免网络阻断 QUIC 后长期返回
  HTTP 530。

## 流程

### 1. 检测 WebUI

```bash
curl -s -o /dev/null -w "%{http_code}" --max-time 5 http://127.0.0.1:25808/
```

- `200`：服务可用，继续安装 Tunnel。
- `000` 或连接拒绝：进入下一步。

### 2. 让用户开启 WebUI

只在服务未运行时提示：

> WebUI 尚未运行，我无法代替你切换桌面开关。请打开“设置 → WebUI”，启用
> WebUI；若还需要局域网访问，同时启用“允许远程访问”。完成后告诉我，我会
> 继续。

用户完成后重新探测，只有收到 `200` 才继续。

### 3. 安装 `cloudflared`

先执行 `command -v cloudflared`。缺失时，从 Cloudflare 官方 GitHub Release
下载当前平台的预编译二进制，不依赖系统包管理器。

macOS / Linux 示例：

```bash
mkdir -p ~/.tjuae/tools && cd ~/.tjuae/tools
OS=$(uname -s); ARCH=$(uname -m)
case "$OS" in Darwin) goos=darwin;; Linux) goos=linux;; esac
case "$ARCH" in arm64|aarch64) goarch=arm64;; x86_64|amd64) goarch=amd64;; esac
curl -fsSL -o cf.tgz "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-${goos}-${goarch}.tgz"
tar xzf cf.tgz && rm cf.tgz
./cloudflared --version
```

Windows 下载 `cloudflared-windows-<arch>.exe`。下载后验证
`cloudflared --version`，不要运行来源不明的二进制。

### 4. 创建 Tunnel

以长驻后台进程启动：

```bash
cloudflared tunnel --protocol http2 --url http://127.0.0.1:25808
```

若使用刚下载的文件，调用 `~/.tjuae/tools/cloudflared`。观察输出并确认：

1. 得到 `https://<random-words>.trycloudflare.com`；
2. 出现 `Registered tunnel connection ... protocol=http2`。

若公网地址持续返回 530，先确认命令确实包含 `--protocol http2`。

### 5. 从公网验证

交付 URL 前必须验证：

```bash
curl -s -o /dev/null -w "%{http_code}" --max-time 20 "<public-url>/"
```

新 Tunnel 可能短暂返回 530/000，可间隔几秒重试两到三次。只有获得 `200` 才算
成功。还可确认页面确实属于 TjuaeUI：

```bash
curl -s --max-time 20 "<public-url>/" | grep -i "<title>TjuaeUI</title>"
```

### 6. 检查凭据强度

交付公网地址前，主动确认用户已设置强且唯一的密码。默认、空白或弱密码不得用于
公网访问。需要修改时，可在本地桌面模式下调用：

```bash
curl -s -X POST http://127.0.0.1:25808/api/webui/change-password \
  -H "Content-Type: application/json" -d '{"new_password":"<new>"}'
curl -s -X POST http://127.0.0.1:25808/api/webui/change-username \
  -H "Content-Type: application/json" -d '{"new_username":"<new>"}'
```

这些接口只在 `tjuaecore` 的本地桌面模式可用；独立服务器模式会返回 `403`，
应使用部署环境自己的凭据管理方式。不得在聊天或日志中回显密码。

### 7. 交付并说明限制

给出已验证的公网 URL，同时明确：

- 打开后使用 WebUI 用户名和密码登录。
- 地址随机且临时，Tunnel 或 WebUI 重启后会失效并变化。
- 进程退出、电脑休眠或关机后地址不可用。
- Quick Tunnel 适合临时分享和测试，不适合关键或长期生产服务。
- 知道 URL 的人都能访问登录页；随机 URL 不是安全边界。
- 流量经过 Cloudflare 边缘并在那里终止 TLS。
- 使用结束后应停止 Tunnel，并在不再需要时关闭 WebUI。

只有用户体验过临时地址并明确要求固定地址时，才说明 Named Tunnel 方案。固定
域名需要 Cloudflare 账号和用户拥有的域名，未经明确授权不要配置。

## 工作方式

- 技术操作由智能体完成，用户只负责无法自动完成的桌面开关。
- 未验证返回 `200` 前，不得承诺地址可用。
- 在交付时主动说明临时性和安全权衡，不要等故障发生后再补充。
