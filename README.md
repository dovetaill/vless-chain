# Chain Panel · 个人网络控制台

用一套 Web 界面管理自己的服务器、代理接入、端口转发和网站反代。复杂中转按需展开，日常操作从「我要做什么」开始。

![控制台总览](panel/docs/images/overview.png)

| 想做什么 | 打开哪里 | 配置什么 |
| --- | --- | --- |
| 给手机、电脑创建代理 | 代理接入 | 服务器、协议、监听端口 |
| 把一个端口转发到另一台机器 | 端口转发 | 入口端口、目标地址、目标端口、TCP/UDP |
| 给应用配置域名和 HTTPS | 网站反代 | 域名、后端 URL、证书 |
| 使用已有代理或购买的中转端口 | 服务器与资源 | 原节点链接，以及服务商给的入口 |
| 看用量、限制速度和月流量 | 监控与告警 | 每条线路的上传、下载上限与额度 |
| 修改密钥、绑定 TOTP / Passkey | 登录与安全 | 当前密钥、验证器或设备凭据 |

支持 VLESS/REALITY、HTTP、SOCKS5、Shadowsocks 2022、Trojan/TLS；提供本地生成的二维码、分享链接、完整 Mihomo / Clash Meta 配置、节点 YAML 和可撤销订阅。侧栏可收起，支持手机操作。

## 先读图文手册

**[打开完整图文使用手册](panel/README.md)**：从安装、接入第一台服务器，到普通转发、购买的中转入口、多跳代理、HTTPS、限速和登录恢复。页面内也有「使用手册」。

![普通端口转发表单](panel/docs/images/forward-editor.png)

## 安装

在使用 systemd 的 Linux x86_64 / aarch64 服务器上运行，支持 Ubuntu 22.04+ / Debian 12+：

```bash
curl -fsSL https://raw.githubusercontent.com/dovetaill/vless-chain/main/scripts/install-server.sh | sudo bash
```

默认安装控制台和本机 Agent。可选填写域名启用 HTTPS；留空使用公网 IP 与随机端口。安装结束显示地址和管理员密钥，本机自动接入。日常输入 `chainctl` 打开管理菜单。

在线命令安装**最新正式 Release**。升级到 v0.2.0 时，先更新各台服务器的 Agent，再更新控制台，两者应使用同一版本；详见 [v0.2.0 更新与升级说明](panel/docs/releases/v0.2.0.md)。源码构建见[构建说明](panel/docs/advanced.md#构建)。

其他自有服务器默认复制安装命令并粘贴接入码；Web SSH 自动安装作为可选方式。导入已有代理或购买的中转入口无需提供 SSH。

## 文档

- [图文手册与协议支持范围](panel/README.md)
- [高级部署、构建、验证与发布](panel/docs/advanced.md)
- [原有 SSH 脚本完整指南](SSH-GUIDE.md)
- [Metronic 主题来源与许可](panel/frontend/THEME.md)

原有 SSH 脚本与 Web 控制台各自管理所属服务。原始主题 ZIP 不属于发行包。截图来自隔离测试环境，地址和二维码仅用于说明。
