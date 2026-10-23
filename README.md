<h1 align="center">Chain Panel</h1>

<p align="center">自托管代理管理面板 · 链式出网 · 流量与租期管理 · 多机总控</p>

<p align="center">
  <a href="https://github.com/dovetaill/vless-chain/releases/latest"><img src="https://img.shields.io/github/v/release/dovetaill/vless-chain?label=Release&amp;color=2563eb" alt="最新正式版本"></a>
  <a href="#技术栈"><img src="https://img.shields.io/badge/Rust-Axum-dea584?logo=rust" alt="Rust / Axum"></a>
  <a href="#技术栈"><img src="https://img.shields.io/badge/React-19-149eca?logo=react&amp;logoColor=white" alt="React 19"></a>
  <a href="#安装"><img src="https://img.shields.io/badge/Platform-Linux%20%7C%20macOS-475569" alt="Linux 与 macOS"></a>
</p>

<p align="center">
  <a href="#界面预览">界面预览</a> ·
  <a href="#安装">安装</a> ·
  <a href="#开始使用">开始使用</a> ·
  <a href="panel/README.md">完整手册</a> ·
  <a href="https://github.com/dovetaill/vless-chain/releases">版本更新</a>
</p>

Chain Panel 帮你在自己的服务器上创建代理、组合出网路径，并在浏览器中管理用量、限速、到期和续费。可以只管理一台自用 VPS，也可以把多台独立面板接入总控，集中查看状态和维护。

项目仓库名为 `vless-chain`。安装后，面板负责配置与管理，本机 Agent 负责运行代理；手机和电脑通过支持相应协议的客户端连接。

## 可以用它做什么

| 使用场景 | 你可以完成的操作 |
| --- | --- |
| 给自己的设备创建代理 | 自动生成端口和凭据，复制链接、扫码或导出 Mihomo 节点 |
| 使用指定落地出网 | 导入已有代理，按顺序组成「本机 → 落地 A → 落地 B」的链式路径 |
| 管理代理用量 | 查看上传、下载、周期用量和余量，设置月额度及上下行限速 |
| 管理客户与续费 | 建立客户档案，设置报价，登记线下收款、开通和续费 |
| 让客户自行查用量 | 客户凭代理配置或私密查询链接查看用量、到期与续费信息 |
| 集中管理多台 VPS | 接入各台独立面板，汇总状态、管理代理、执行支持的批量维护 |
| 及时发现异常 | 配置代理连通性、失联、额度、到期和证书告警，通过 Telegram 或邮件接收通知 |
| 保护管理入口 | 使用管理员密钥、TOTP 动态验证码和 Passkey，支持本机恢复登录 |

客户端只需连接入口代理；链式路径在服务器端执行。总控离线不会停止各 VPS 上已经运行的代理。客户无需面板账号，收款由管理员线下确认，当前不提供在线支付。

### 支持的接入协议

| 协议 | 支持方式 | 创建前需要准备 |
| --- | --- | --- |
| VLESS | REALITY / TLS，TCP + Vision / XHTTP；支持一次创建多种组合 | REALITY 需要可用的目标地址与匹配 SNI；TLS 需要域名及证书 |
| Shadowsocks 2022 | `2022-blake3-aes-128-gcm` | 密钥可自动生成 |
| Trojan | TCP + TLS | 域名及证书，密码可自动生成 |
| VMess AEAD | TCP，`alterId: 0`，可使用 TLS | UUID 可自动生成；启用 TLS 时需要域名及证书 |
| Hysteria 2 | QUIC + TLS | 域名及证书，放行 UDP 监听端口 |
| TUIC v5 | QUIC + TLS | 域名及证书，放行 UDP 监听端口 |
| HTTP / SOCKS5 | 用户名、密码认证 | 普通入口不加密，按使用网络选择合适协议 |

自动证书支持申请与续期，也可使用自己的 PEM 证书。客户端需要支持所选协议及传输方式；链路的 UDP 能力取决于每一跳，详见[链式代理说明](panel/README.md#链式代理)。

## 界面预览

以下为仓库保留的**早期版本截图**，来自隔离测试环境，展示桌面和手机端的界面风格。图中的地址、数据和二维码仅用于演示；导航与操作以当前版本及下方说明为准。截图中的独立端口转发、网站反代和购买中转入口已从当前 Web 界面移除。

**桌面总览**

![Chain Panel 早期版本桌面总览，展示状态卡片、流量图表和服务器列表](panel/docs/images/overview.png)

<details>
<summary>展开查看登录、安全设置和移动端截图</summary>

**管理员登录**

![Chain Panel 早期版本管理员登录界面](panel/docs/images/login.png)

**登录与安全**

![Chain Panel 早期版本安全设置，展示管理员密钥、TOTP 和 Passkey](panel/docs/images/security.png)

**手机端总览**

<img src="panel/docs/images/mobile.png" width="320" alt="Chain Panel 早期版本手机端总览">

</details>

## 技术栈

| 部分 | 技术 | 作用 |
| --- | --- | --- |
| 控制台与 Agent | Rust 2024 edition、Axum 0.8、Tokio 1 | 管理 API、配置下发、代理进程管理和流量控制 |
| 数据存储 | SQLite、rusqlite | 保存面板状态、客户与收款记录、Agent 流量账本 |
| Web 前端 | React 19、TypeScript 5、Vite 7 | 浏览器中的管理界面，构建后随面板程序分发 |
| 界面组件 | Tailwind CSS 4、Radix UI、Lucide、Metronic 主题组件 | 响应式布局、表单、弹窗和图标 |
| 代理内核 | Xray、Mihomo | 执行不同协议的接入与链式出网 |
| HTTPS 与证书 | Nginx、Certbot | 面板 HTTPS、证书申请与续期、可选的统一 443 入口 |
| 安装与运维 | Shell、Python 3.10+、systemd / OpenRC / launchd | 安装发行包、管理服务、备份与升级 |

**使用正式发行包无需安装 Rust、Node.js，也无需自行编译前端或单独下载代理内核。**内核版本随发行包固定，可在「设置 → 版本与维护」查看。主题来源与使用条件见[主题许可说明](panel/frontend/THEME.md)。

## 安装

### 1. 准备服务器

| 系统 | 正式发行包架构 | 服务管理 | 说明 |
| --- | --- | --- | --- |
| Linux | x86_64、ARM64 / aarch64 | systemd 或 OpenRC | 静态 musl 包，兼容 glibc 系统与 Alpine |
| macOS 14+ | Apple Silicon / ARM64 | launchd | 先安装 [Homebrew](https://brew.sh)，从普通管理员账户使用 sudo 安装 |
| Windows / macOS Intel | 无对应正式安装支持或发行包 | — | 不适用下方一键安装流程 |

安装前需要：

- 可以通过 SSH 或本机终端操作的服务器，以及 root / sudo 权限。
- `curl` 和 CA 证书，可访问 GitHub Releases 与系统软件源。安装器会检查 Python 3.10+，并按需安装 Python、Nginx、Certbot 等依赖；旧系统的软件源可能无法提供所需版本。
- 能从客户端访问的服务器地址。使用域名自动签证书时，将域名解析到本机，并放行公网 TCP 80；面板 HTTPS 和代理监听端口也需在系统防火墙、云安全组中放行。

面板域名与代理域名按各自用途配置；创建 REALITY 代理不要求你持有代理域名，但面板公网管理仍建议使用 HTTPS。

### 2. 运行安装向导

```bash
curl -fsSL https://raw.githubusercontent.com/dovetaill/vless-chain/main/scripts/install-server.sh | sudo sh
```

Linux 已经以 root 登录时，将末尾的 `sudo sh` 换为 `sh`。

向导默认安装控制台和本机 Agent，并自动接入本机：

1. 下载适合当前系统和架构的最新正式发行包，并校验完整性。
2. 按提示填写面板域名，配置 HTTPS；留空则使用 IP 与随机端口。
3. 安装并启动服务，显示访问地址和管理员密钥。

保存安装结束时的登录信息。之后运行 `chainctl` 即可打开管理菜单，运行 `sudo chainctl access` 可再次查看访问地址。

> 在线安装获取的是[最新正式 Release](https://github.com/dovetaill/vless-chain/releases/latest)，源码中的未发布改动不会自动出现在已安装版本中。本地发行包、固定监听地址及手工部署见[高级部署文档](panel/docs/advanced.md#构建安装与运维脚本)。

### 3. 首次登录

打开安装结果中的地址，点击「管理登录」，或直接访问该地址下的 `/admin`，输入管理员密钥。网站首页是客户公共查询页，进入首页后没有看到管理菜单属于正常情况。

首次进入后台后，建议先到「设置」绑定 TOTP 或 Passkey，并保存恢复码。Passkey 在公网使用时需要固定的 HTTPS 域名；本机开发可使用 `http://localhost`。

<details>
<summary>已有 x-ui / 3x-ui、独立代理或其他网站时</summary>

Linux 交互向导可识别已有代理服务，提供备份导入、备份停用重装或保留选项。自动接管范围包括可确认归属的 systemd 服务和独立 Docker / Compose 容器；选择接管会切换服务，请先阅读向导列出的影响。

当前安装器会先预检导入配置；存在不能完整导入的项目时，保留旧服务并继续完成面板安装。旧面板的订阅、到期、额度等管理数据不迁移。原配置与恢复信息保存在 `/var/backups/chain-legacy/`，卸载新平台也会保留这份备份。

80 / 443 被其他服务占用时，按向导选择独立端口或手工证书。使用手工面板证书时，需让原续期工具在更新后重载 `chain-panel-https`。已有 HTTPS 网站需要与代理共用 443 时，参阅[网站回落与统一 443](panel/README.md#网站回落与统一-443)，不要直接覆盖已有站点配置。

原有 SSH 脚本和 Web 面板各自管理所属服务，只有明确选择导入接管时才切换旧服务；继续使用原脚本请看 [SSH 指南](SSH-GUIDE.md)。

</details>

## 开始使用

### 创建第一条代理

1. 打开「代理」，点击「新建代理」。
2. 选择协议，确认本机 IP 或域名；端口和连接凭据可以自动生成。
3. 初次自用可选择默认的 **VLESS + REALITY + Vision**，确认目标地址和 SNI，点击「检测目标与 SNI」。需要 TLS 的协议则配置域名和证书。
4. 「出网方式」保持直接出网，按需设置流量额度及上传、下载上限，点击「创建代理」。
5. 等待应用成功，再打开该代理的「分享配置」，复制链接、扫码或下载 Mihomo 节点。

创建后放行实际监听端口，并在客户端连接验证。Hysteria 2、TUIC 使用 UDP 监听端口；SOCKS5 UDP 还使用动态中继端口，具体要求见[使用手册](panel/README.md#流量计量与运营操作)。

### 配置链式出网

已有其他服务器或供应商提供的代理时，在「外部代理」导入分享链接，或单节点 Mihomo / Clash YAML、JSON。然后编辑本机代理，在「出网方式」选择链式代理，按实际流量顺序添加落地：

```text
手机 / 电脑 → 本机入口 → 落地 A → 落地 B → 互联网
```

保存并应用后，客户端继续连接本机入口。落地也可以引用其他已经启用、应用的本机代理，但不能循环引用。HTTP / HTTPS 出口只承载 TCP；Hysteria 2、TUIC 的连接需要 UDP，前置路径无法承载时会拒绝保存。

只想让另一台 VPS 作为落地时，在那台机器安装并创建代理，再将分享链接导入即可；需要集中管理多台机器时，再使用多机总控。

### 导入手机或电脑客户端

| 分享方式 | 怎么使用 |
| --- | --- |
| 分享链接 | 在支持所选协议的客户端中粘贴导入 |
| 二维码 | 使用客户端的扫码导入功能 |
| Mihomo 节点 YAML | 合并到已有配置的 `proxies` 列表，并加入需要的策略组 |

单节点 YAML 不包含客户端监听端口、DNS、规则和策略组。导入前确认客户端支持相应协议，尤其是 XHTTP、Hysteria 2 和 TUIC。分享内容包含连接凭据，只交给该代理的使用者。

### 管理客户、额度和续费

1. 在「客户与收款」创建客户档案。
2. 在代理的「出租信息」中选择客户，填写额度、有效期及月付 / 季付 / 年付报价。
3. 收到线下款项后，在「收款 / 续费」登记实收金额，确认开通或续费。设置出租信息后，首次开通前代理不可用。
4. 将代理配置或该条代理的私密查询链接交给客户，并设置统一续费联系入口。

客户访问面板首页或 `/query`，凭配置查询自己的用量、余量、到期和报价，无需管理账号。每条代理独立计算额度；到期、耗尽或手动停用后会阻断转发。自用代理可选择「设为无限期（自用）」，该操作不取消额度和限速。

租期和流量周期按北京时间计算；有效期内续费延长租期，不清零当前用量。计量口径、自然月重置和停用规则见[总览与出租管理](panel/README.md#总览与出租管理)。

### 接入多机总控与告警

先在各台 VPS 分别安装本平台，并为远端面板配置有效 HTTPS。在远端「多机总控 → 总控设置」生成接入码，然后在总控中填写远端名称、HTTPS 根地址和接入码。接入码有效期为 10 分钟，只能使用一次。

总控可集中查看 VPS、代理和外部代理，管理远端配置，并免密进入已授权面板。网页升级和批量维护目前支持 **Linux systemd**；其他平台可使用 `chainctl` 运维。接入操作不会代替 SSH 安装。

在「监控告警」启用检测策略，并配置 Telegram 或 SMTP 通知。监控默认关闭；探测会产生少量代理流量。多机同步、授权范围与告警规则见[完整说明](panel/README.md#多机总控与监控告警)。

## 日常维护

在已安装服务器的终端输入 `chainctl`，可通过菜单执行常用操作，也可以直接运行：

| 操作 | 命令 |
| --- | --- |
| 查看面板访问地址与登录信息 | `sudo chainctl access` |
| 查看管理员密钥 | `sudo chainctl token` |
| 查看面板 / Agent 状态 | `sudo chainctl status` / `sudo chainctl status --component agent` |
| 查看最近的面板 / Agent 日志 | `sudo chainctl logs` / `sudo chainctl logs --component agent` |
| 升级至最新正式版 | `sudo chainctl update` |
| 备份面板 / Agent | `sudo chainctl backup` / `sudo chainctl backup --component agent` |
| 重启面板 / Agent | `sudo chainctl restart` / `sudo chainctl restart --component agent` |
| 恢复登录二步验证设置 | `sudo chainctl reset-auth` |

升级会保留配置与数据，标准安装会同时更新已安装的本机 Agent。更新不再自动生成备份包；成功后清理 `/opt/chain/releases` 中已有的历史版本，只保留各组件当前使用的版本。仍被进程占用、校验失败或归属不明的目录会保留并提示，下次更新重试。更新失败仍恢复此前程序链接与配置；更新成功后不保留用于 `rollback` 的历史版本。升级、重启 Agent 以及 Agent 备份会短暂中断代理连接，请选择合适时间。独立部署的 Agent 应与控制台保持版本一致；从早期版本升级时先阅读对应的[更新说明](https://github.com/dovetaill/vless-chain/releases)。

首次从旧安装器迁移时，旧 `chainctl` 进程仍执行原来的升级逻辑；请安装新版后再执行一次 `chainctl update`，或直接使用新版安装脚本执行更新，以应用历史版本清理规则。已有备份包不自动删除，手工 `backup` 仍可使用。

备份保存在 `/var/backups/chain-panel/`，包含连接凭据，应私下保存。面板和 Agent 需要分别备份；手工备份 Agent 时，应停止 Agent 并复制完整数据目录，不能只保存 `traffic.json` 或单独复制 `traffic.db` 而遗漏 WAL 文件。`chainctl rollback` 只回滚程序版本，不恢复旧数据库，不能当作数据恢复命令。

### 本地升级命令报错时修复

如果 `chainctl update` 报 `ModuleNotFoundError: No module named 'chain_time'`，说明本地安装器引用了未安装的配套模块。旧安装器使用固定文件列表复制新版管理脚本时，可能漏掉新版新增模块；报错发生在下载之前，重复运行同一命令不能修复。

直接下载完整安装器并显式执行升级，可绕过损坏的本地入口：

```bash
curl -fsSL https://raw.githubusercontent.com/dovetaill/vless-chain/main/scripts/install-server.sh \
  | sudo sh -s -- update --non-interactive
```

请完整复制两行，保留第一行末尾的 `\`；root 用户可去掉 `sudo`。仅安装 Agent 的机器在末尾加 `--component agent`。此命令使用最新安装器安装最新正式 Release，保留已有配置与数据，并重启受影响组件；不代表源码中尚未发布的 Rust 迁移已经生效。

不带 `update` 参数的安装命令默认执行 `setup`；安装已完成时只显示登录信息，不更新本地管理器。确认上述升级成功后，再使用日常的 `chainctl update`。

### 卸载与重装

在 `chainctl` 菜单选择「卸载本机全部组件」，或执行：

```bash
sudo chainctl uninstall
```

默认移除程序、服务和管理命令，保留配置、数据库、备份及恢复记录；重新运行安装脚本可恢复使用。

如果确认不再保留应用数据，可使用 `sudo chainctl uninstall --purge`，该操作还会清除应用配置、数据、应用备份和服务账户。两种方式都保留系统 Nginx、其他网站、`/etc/letsencrypt` 中的面板证书，以及旧服务迁移备份。重装恢复 HTTPS 后才能继续原面板的证书续期；使用过统一 443 的代理需重新启用统一入口。

## 常见问题

**打开网址只有查询页面，后台在哪里？**

管理入口是 `/admin`，也可以点击首页的「管理登录」。安装后的地址可通过 `sudo chainctl access` 查看。

**忘记管理员密钥或丢失验证器怎么办？**

在安装面板的服务器执行 `sudo chainctl token` 查看密钥。丢失 TOTP / Passkey 时，可用恢复码，或执行 `sudo chainctl reset-auth` 清除二步验证配置并退出已有会话；原管理员密钥与代理配置保留。

**面板能打开，代理却连不上？**

先看 Agent 是否在线、配置是否应用成功；再检查代理状态、额度与到期时间，核对系统防火墙和云安全组是否放行对应的 TCP / UDP 端口。然后检查客户端协议、证书或 REALITY 目标与 SNI，并查看 Agent 日志。面板可访问只说明管理入口正常。

**为什么保存成功，页面还显示「等待同步」？**

保存与节点生效是两个步骤。离线节点恢复连接后才会应用设置，以「已同步」状态和实际连接结果为准。

**为什么服务器流量与代理用量不一样？**

网卡统计包含这台服务器的全部业务；代理额度按成功鉴权后的业务载荷统计，上传与下载合计扣除，不包含外层协议封装或网站回落流量。两者不能直接当作同一份账单。

**自动申请证书失败怎么办？**

检查域名解析、TCP 80 的公网可达性，以及已有网站是否占用了验证路径。根据页面或安装日志中的具体错误处理；已有证书时也可使用手工证书。统一 443 和网站回落的条件见[相关说明](panel/README.md#网站回落与统一-443)。

## 文档与反馈

| 文档 | 内容 |
| --- | --- |
| [完整使用手册](panel/README.md) | 协议设置、链式代理、客户查询、计量、租期及多机管理细节 |
| [高级部署与开发](panel/docs/advanced.md) | 本地发行包、源码构建、环境变量、手工部署与验证 |
| [版本更新](https://github.com/dovetaill/vless-chain/releases) | 正式发行包、版本变更和升级说明 |
| [原有 SSH 脚本指南](SSH-GUIDE.md) | 使用命令行脚本管理原有服务 |
| [主题来源与许可](panel/frontend/THEME.md) | Metronic 组件来源及使用条件；原始主题 ZIP 不随发行包分发 |

遇到问题或有功能建议，可提交 [GitHub Issue](https://github.com/dovetaill/vless-chain/issues)。请附上面板与 Agent 版本、操作系统及架构、复现步骤、错误信息和脱敏截图；不要上传管理员密钥、代理密码、私钥、完整分享链接或备份文件。
