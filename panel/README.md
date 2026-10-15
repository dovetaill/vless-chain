# Chain Panel 使用说明

在网页创建代理、选择落地路径，然后复制到客户端。登录后默认进入「代理」列表。

## 安装与登录

```bash
curl -fsSL https://raw.githubusercontent.com/dovetaill/vless-chain/main/scripts/install-server.sh | sudo bash
```

安装需要 Linux、systemd，支持 x86_64 / aarch64；推荐 Ubuntu 22.04+ / Debian 12+。安装脚本下载正式 Release，开发中的界面与功能需使用对应源码构建，见[高级部署与开发](docs/advanced.md#构建)。

保存安装结束时显示的控制台地址和管理员密钥。填写面板域名可配置 Nginx、HTTPS 与证书续期；公网登录请使用 HTTPS。Passkey 需要固定 HTTPS 域名，本机开发可用 `http://localhost`。

## 创建代理

1. 点击「新建代理」。本机就是入口，无需选择服务器。
2. 选择协议，确认本机 IP 或域名；名称、端口和凭据可以自动生成。
3. 默认直接出网；需要链式代理时，按顺序选择落地。
4. 点击「创建代理」，等待应用成功后复制分享链接或 Mihomo 节点。

| 协议 | 设置 |
| --- | --- |
| VLESS | REALITY + Vision、TLS + Vision、REALITY + XHTTP、TLS + XHTTP；默认 REALITY + Vision |
| Shadowsocks 2022 | 自动生成密钥，使用 `2022-blake3-aes-128-gcm` |
| Trojan | TLS，需要域名和证书，密码可自动生成 |
| VMess AEAD | TCP，alterId 为 0；域名默认使用 TLS，自动生成 UUID |
| Hysteria 2 | QUIC + TLS，需要证书，自动生成认证密码；放行 UDP 监听端口 |
| TUIC v5 | QUIC + TLS，需要证书，自动生成 UUID 和密码；放行 UDP 监听端口 |
| HTTP / SOCKS5 | 用户名密码；普通入口不加密 |

TLS 模式可自动申请证书，也可填写自己的 PEM 证书链与私钥。REALITY 无需申请自己的证书：在表单填写目标地址（域名/IP 加端口）和该目标证书覆盖的 SNI。「检测目标与 SNI」从控制台所在机器检查 TLS 1.3、证书/SNI、H2 和耗时；检查通过不等于所有客户端网络均可达。

Hysteria 2、TUIC v5 接入及包含 TUIC、Hysteria 2、Shadowsocks 或 HTTPS 的出网链由发行包内的 Mihomo 内核运行，随 Agent 启停；每条链独立配置连接路径，仍使用面板的流量统计、限速和出网链路。其他接入协议使用 Xray。升级时需同时更新控制台与 Agent。当前 Xray 的 REALITY 启动包含目标采样，Agent 为此预留 6 秒再开放入口；这不代替目标可用性检查，目标网络异常仍会导致连接失败。

### 一次创建多种 VLESS 组合

勾选「一次创建多种组合」，选择所需组合。默认分别分配端口；TLS 组合需要解析到本机的域名和有效证书。批量保存是原子的，任意一条无效时不保存其他条。应用成功后在列表逐条点击「分享」，取得对应链接、二维码或 Mihomo 节点。

XHTTP 可设置路径、Host 和模式；其分享配置不带 Vision 的 `flow`。REALITY 组合包含公钥、short-id 和所选 SNI。批量创建沿用同一组 REALITY 目标/SNI 和 TLS 域名。

### 网站回落与统一 443

TCP + TLS + Vision 可在「TLS 网站回落」填写 `builtin` 显示内置主页，或填写自己的 HTTP/HTTPS 网站地址；留空关闭。普通浏览器使用该代理域名访问同一端口即可看到网站。REALITY 的非认证连接由 Xray 交给所选目标网站；这里的 TLS 网站回落不用于 XHTTP。

本机已有 Nginx HTTPS 网站时，先安装包含此功能的控制台及 Agent，在服务器执行：

```bash
sudo chainctl ingress-enable
```

也可在 `chainctl` 菜单选择「启用统一 443」。随后展开代理表单的监听端口，勾选「使用统一 443 入口」。Nginx 使用 `ssl_preread` 读取 SNI，不解密代理 TLS；已存在的网站转到本机内部 TLS 端口，继续使用原证书和站点配置，并通过 PROXY protocol 保留客户端 IP。TCP 80 的证书验证及 UDP 443 不迁移。

每个 SNI 对应一个入口。REALITY 与 TLS 可以使用不同 SNI 共用 443；同一个 SNI 的 TCP 和 XHTTP 请使用独立端口，或分别创建并填写不同 SNI。不能使用现有网站的域名抢占它的入口。

自动接管支持系统 Nginx 的普通 IPv4 HTTPS 监听（及其 IPv6 监听）；已有 `stream`、自定义真实 IP 信任配置、指定 IP 的 443 绑定会明确拒绝，避免覆盖。缺少 stream 模块时由系统软件包安装。启用前保存配置，校验或重载失败恢复原文件；后续手工修改受管文件会阻止自动恢复，需先核对差异。

先停用或改回独立端口并应用共享入口代理，再执行 `sudo chainctl ingress-disable` 恢复原 Nginx 监听。正在使用共享入口时关闭分流会使这些代理断开。

## 本机与外部代理

安装自动接入控制台所在机器；本机记录缺失时，创建流程会恢复安装器保存的身份，无需额外添加服务器。旧安装缺少恢复信息时先执行 `sudo chainctl update`。

其他机器请自行通过 SSH 登录并运行安装命令，在对应面板创建代理后复制链接。「外部代理」只管理现有代理链接，不保存 SSH 凭据，也不执行远程安装。

点击「导入外部代理」，可粘贴协议分享链接、Mihomo / Clash 单节点 YAML 或 JSON，支持单对象、单元素列表和只有一条节点的 `proxies:` 列表。名称可自动读取；导入后「链接 / Mihomo」提供两种格式。

## 链式代理

例如「客户端 → 本机 → 落地 A → 落地 B → 互联网」：新建或编辑代理，选择「链式代理」，依次选择 A、B。可选择外部代理或其他已启用、已应用的本机代理；禁止循环引用。可以同时创建多条独立本机代理，各自使用不同端口和路径。

HTTP/HTTPS 出口仅支持 TCP；经它连接 Shadowsocks 或 SOCKS5 时也不支持 UDP。VLESS、VMess、Trojan 可通过 TCP 隧道传递 UDP。Hysteria 2、TUIC 连接本身需要 UDP，前置路径不支持 UDP 时会拒绝保存，不能靠换证书或打开客户端 UDP 开关解决。

分享和订阅会按整条路径（含本机代理引用）标记 UDP 能力；仅支持 TCP 的节点导出 `udp: false`，导入后也会保留此限制。

当前 Mihomo 的 Hysteria 2 实现限制单个 UDP 数据报为 4096 字节，后续 UDP 协议的封装也计入此上限。因此 Hysteria 2 → SS2022（本项目默认 AES-128、IPv4 目标）双向业务载荷上限为 4038 字节，Hysteria 2 → SOCKS5（IPv4 目标）为 4086 字节；地址格式、加密算法或更多封装会改变可用大小。超限数据报会丢弃，`udp: true` 不代表支持任意大小的数据报。这是[当前内核限制](https://github.com/MetaCubeX/sing-quic/blob/1c242664697a/hysteria2/internal/protocol/proxy.go#L25)，不是可通过面板开关提高的额度。

修改或删除被引用的代理前先处理引用关系。端口转发和购买中转入口功能已移除。

## 导入 Mihomo / Clash 客户端

代理应用成功后点击「分享」：

- **Mihomo 节点**：复制单条节点 YAML，或下载 `proxy.yaml`，合并到已有配置的 `proxies` 列表，并加入所需策略组。
- **链接 / 二维码**：复制分享链接，或用支持相应协议的客户端扫码。二维码在浏览器本地生成。

节点 YAML 只包含这一条代理，不包含客户端监听端口、DNS、规则或策略组。请使用支持相应协议的 Mihomo / Clash Meta 客户端；分享内容含连接凭据，妥善保存。草稿、停用和已删除的代理不能作为可用节点导出。

## 使用限制与排错

安装器安装 Nginx 和 Certbot，由 Nginx 提供 TCP 80 证书验证；Agent 不再争用 80 端口。域名需解析到本机，TCP 80 和代理监听端口需放行系统防火墙及云安全组。已有同域名站点必须能提供 ACME 验证路径，否则申请失败会显示具体错误，不会显示创建成功。

外部代理仅支持已实现的链接组合，不支持的传输或插件会报错。检查失败时先看服务器在线状态与应用结果，再核对端口、证书、目标可达性和客户端协议支持；修改路径或协议可能让共享服务器上的连接短暂重连，应用失败会尝试恢复旧配置。

无法登录时可在控制台服务器运行 `sudo chainctl token` 查看密钥，或运行 `sudo chainctl reset-auth` 重置 TOTP / Passkey，保留代理配置。更多部署细节见[高级文档](docs/advanced.md)。
