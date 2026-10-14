# Chain Panel · 个人网络控制台

在本机创建和管理代理，按顺序选择外部落地组成链式代理。界面分为「代理」「外部代理」「设置」。

| 想做什么 | 打开哪里 | 配置什么 |
| --- | --- | --- |
| 给手机、电脑创建代理 | 代理 → 新建代理 | 协议、本机 IP 或域名；端口与凭据自动生成 |
| 配置链式代理 | 新建或编辑代理 → 出网方式 | 按顺序选择落地 A、B |
| 管理买来的代理链接 | 外部代理 | 导入分享链接或单节点 Mihomo YAML / JSON |
| 修改密钥、绑定 TOTP / Passkey | 设置 | 当前密钥、验证器或设备凭据 |

支持 VLESS/REALITY、HTTP、SOCKS5、Shadowsocks 2022、Trojan/TLS；分享提供链接、二维码和 Mihomo 节点。VLESS、HTTP、Trojan 填写域名时默认自动申请和续期 SSL 证书，其他参数按需展开。

**[打开使用手册](panel/README.md)**：本机创建、外部代理导入、链式路径、证书与排错。

## 安装

在使用 systemd 的 Linux x86_64 / aarch64 服务器上运行，支持 Ubuntu 22.04+ / Debian 12+：

```bash
curl -fsSL https://raw.githubusercontent.com/dovetaill/vless-chain/main/scripts/install-server.sh | sudo bash
```

默认安装控制台和本机 Agent。可选填写域名启用 HTTPS；留空使用公网 IP 与随机端口。安装结束显示地址和管理员密钥，本机自动接入。日常输入 `chainctl` 打开管理菜单。

在线命令安装**最新正式 Release**。升级到 v0.2.0 时，先更新各台服务器的 Agent，再更新控制台，两者应使用同一版本；详见 [v0.2.0 更新与升级说明](panel/docs/releases/v0.2.0.md)。源码构建见[构建说明](panel/docs/advanced.md#构建)。

其他机器请自行登录 SSH，运行相同安装命令，在那台机器的面板创建代理；将分享链接导入本面板即可选择为落地。网页不再提供 SSH 远程安装、端口转发或购买中转入口功能。

## 文档

- [图文手册与协议支持范围](panel/README.md)
- [高级部署、构建、验证与发布](panel/docs/advanced.md)
- [原有 SSH 脚本完整指南](SSH-GUIDE.md)
- [Metronic 主题来源与许可](panel/frontend/THEME.md)

原有 SSH 脚本与 Web 控制台各自管理所属服务。原始主题 ZIP 不属于发行包。截图来自隔离测试环境，地址和二维码仅用于说明。
