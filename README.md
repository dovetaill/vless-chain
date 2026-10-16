# Chain Panel · 个人网络控制台

在本机创建和管理代理，按顺序选择外部落地组成链式代理。界面分为「代理」「外部代理」「设置」。

| 想做什么 | 打开哪里 | 配置什么 |
| --- | --- | --- |
| 给手机、电脑创建代理 | 代理 → 新建代理 | 协议、本机 IP 或域名；端口与凭据自动生成 |
| 配置链式代理 | 新建或编辑代理 → 出网方式 | 按顺序选择落地 A、B |
| 管理买来的代理链接 | 外部代理 | 导入分享链接或单节点 Mihomo YAML / JSON |
| 修改密钥、绑定 TOTP / Passkey | 设置 | 当前密钥、验证器或设备凭据 |

支持 VLESS/REALITY、HTTP、SOCKS5、Shadowsocks 2022、Trojan/TLS；分享提供链接、二维码和 Mihomo 节点。VLESS 默认使用 REALITY，可批量创建 Vision / XHTTP 与 REALITY / TLS 的组合；TLS 模式支持自动申请和续期证书。网站回落和 Nginx 统一 443 的配置见[使用说明](panel/README.md#网站回落与统一-443)。

**[打开使用手册](panel/README.md)**：本机创建、外部代理导入、链式路径、证书与排错。

## 安装

正式发行包支持 Linux（systemd / OpenRC）的 x86_64 / ARM64，以及 macOS 14+ 的 Apple Silicon（ARM64）：

```bash
curl -fsSL https://raw.githubusercontent.com/dovetaill/vless-chain/main/scripts/install-server.sh | sudo sh
```

Linux 使用静态 musl 程序包，兼容 glibc 与 Alpine；运行环境仍需 Python 3.10+ 和可用的系统软件源。macOS 使用 launchd，先安装 [Homebrew](https://brew.sh)，再从普通管理员账户执行上面的命令。以 root 登录的 Linux 可将 `sudo sh` 改为 `sh`；执行入口需要先有 curl 与 CA 证书。旧版 CentOS 若软件源没有 Python 3.10+，需先自行准备兼容 Python；不替换系统 Python。Windows 暂不支持。

默认安装控制台和本机 Agent。可选填写域名启用 HTTPS；留空使用公网 IP 与随机端口。安装结束显示地址和管理员密钥，本机自动接入。日常输入 `chainctl` 打开管理菜单。

Linux 交互安装会识别运行中的独立代理及 x-ui / 3x-ui 服务，支持逐个选择备份导入、备份停用重装或保留。自动接管支持可确认归属的 systemd 服务及独立 Docker / Compose 容器；不会按进程名直接杀进程。兼容的 VLESS、VMess、SS 配置保留凭据与入口端口；无法无损转换的配置只导入名称，在 Web「代理」标记为待手动配置。安装和导入完成后立即停止所选旧服务，不等待这些项目补齐；应用失败时恢复旧服务。旧面板的订阅、到期和额度等管理数据不迁移。原配置和恢复信息保留在 `/var/backups/chain-legacy/`，卸载新平台也不会删除这些备份。

80/443 冲突时可为新平台选择随机或手动端口。80 可用时仍可自动签发证书；80 被保留的服务占用时，提供与面板域名匹配的证书及私钥路径，使用独立 HTTPS 入口，并让原续期工具在更新证书后重载 `chain-panel-https`。兼容的 VLESS/REALITY 入口可与面板按 SNI 共用 443。多用户共用端口、WS、自定义路由、依赖旧容器的网站回落以及不受支持的 SS 算法等会转为待配置项；导入的代理 TLS 证书作为手动证书保存，续期后需在 Web 更新。

菜单 **16「卸载本机全部组件」** 可选择保留数据或清除数据。`chainctl uninstall` 删除程序、服务和管理命令，保留配置、数据库、备份及恢复记录，重新运行安装脚本可恢复；`chainctl uninstall --purge` 同时清除应用数据和服务账户。两种方式均保留系统 Nginx、其他网站及 `/etc/letsencrypt` 中的面板证书，避免重装时重复申请。卸载后的证书续期需在重装并恢复 HTTPS 后继续；原先使用统一 443 的代理还需在重装后从菜单重新启用统一入口。

在线命令安装**最新正式 Release**；跨平台包需由本次代码构建并发布后才可在线获取，未发布时使用本地对应平台发行包。升级到 v0.2.0 时，先更新各台服务器的 Agent，再更新控制台，两者应使用同一版本；详见 [v0.2.0 更新与升级说明](panel/docs/releases/v0.2.0.md)。源码构建见[构建说明](panel/docs/advanced.md#构建)。

其他机器请自行登录 SSH，运行相同安装命令，在那台机器的面板创建代理；将分享链接导入本面板即可选择为落地。网页不再提供 SSH 远程安装、端口转发或购买中转入口功能。

## 文档

- [图文手册与协议支持范围](panel/README.md)
- [高级部署、构建、验证与发布](panel/docs/advanced.md)
- [原有 SSH 脚本完整指南](SSH-GUIDE.md)
- [Metronic 主题来源与许可](panel/frontend/THEME.md)

原有 SSH 脚本与 Web 控制台各自管理所属服务。原始主题 ZIP 不属于发行包。截图来自隔离测试环境，地址和二维码仅用于说明。
