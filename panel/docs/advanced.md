# Chain Panel 高级部署与开发

日常安装、接入和运维请先看 [上手指南](../README.md)。本页面向维护者、自动化调用和手工部署。

个人自用的多服务器 Web 管理台。Rust 控制台负责线路编排、SQLite 状态与应用任务；Rust Agent 管理独立的 Xray 子进程以及入口网关，执行限速、配额和网站反代；React + TypeScript 前端使用已购买的 Metronic Tailwind 主题组件与布局。

本目录是新增功能。不会读取、导入、覆盖或停止原有 SSH 脚本创建的服务。没有复制 3x-ui 源码，也不依赖 3x-ui 运行。

## 当前能力

- 同时管理多条独立 VLESS + REALITY + Vision 线路，每条 1–8 个受管节点，可共享物理节点，入口端口、UUID 和路径分别保存。
- TCP、UDP、TCP + UDP 端口转发；多跳通过 Xray 的 VLESS 连接传送，最终节点连接固定目标。
- 受管 HTTP、SOCKS5、SS2022、Trojan/TLS 代理；外部可导入 HTTP(S)、SOCKS5、Shadowsocks、Trojan TCP/TLS 和 VLESS TCP/raw、XHTTP TLS/REALITY 链接。
- 原生 HTTP/HTTPS 网站反代、同端口多域名、WebSocket、手工证书和 Certbot webroot 自动申请/续期。
- 默认保存并应用，支持草稿、端口冲突和循环检查、影响范围确认、逐节点应用、运行确认与失败恢复。每个 Agent 管理一个独立 Xray，多条线路共享进程；修改可能使其相关连接短暂重连。仅限速/额度变动支持在线更新。
- 真实入口 HTTP/HTTPS 探测，以及 TCP/UDP 回显探测。后两者要求目标运行回显服务，不把端口连通当作整链成功。
- 二维码、协议分享链接、完整 Mihomo YAML / 节点 YAML、可撤销订阅、购买中转入口端点替换。
- 入口字节计量、UTC 月额度、上传/下载聚合限速、日流量、服务器指标与告警。限速覆盖整条线路的并发连接，不修改宿主机 qdisc。
- 管理员密钥轮换、TOTP 与一次性恢复码、WebAuthn Passkey、同源 Cookie 会话、节点专属令牌与本地登录恢复。

没有套餐、计费、租户、自动选路或自动故障切换。当前受管入口监听 IPv4；公网 ACME 签发、跨公网路径、IPv6 入站、外部 XHTTP 的所有扩展参数和所有客户端版本没有在本机集成测试中覆盖。实际防火墙和 DNS 由使用者配置。完整数据目录包含代理凭据、证书私钥和 TOTP 密钥，权限为 700，备份保持私有。计量范围、持久化窗口和更换入口机器的账本限制见[图文手册](../README.md#8-流量月额度与带宽上限)。

## 构建

`npm ci` 报缺少锁文件时，先检查所在目录：前端清单与锁文件位于 `panel/frontend/`，仓库根目录没有 npm 项目。不要为了消除报错在根目录生成新锁文件。

需要 Linux、Rust 2024 edition 工具链、Node.js 20.19+ 或 22.12+、npm、C 编译器、pkg-config 与 OpenSSL 开发库（Ubuntu/Debian 为 libssl-dev）。WebAuthn 使用 OpenSSL，正式 Ubuntu 22.04 构建运行时需要 libssl3。运行时包括两个 Rust 二进制和 Xray；Agent 自动证书还需要 Certbot，安装器会补齐。前端静态文件编译进 `chain-panel`，不需要 Node.js 服务。

```bash
cd /home/wwwroot/vless-chain/panel/frontend
npm ci
npm run build
cd ..
cargo build --release --bins
```

构建出的 `chain-panel`、`chain-agent` 位于 Cargo 的 `release/` 目录。生产构建可设置 `CARGO_TARGET_DIR` 到专属的大容量目录，交付后清理；项目提供的验证脚本会自动清理自己的 target。

`CHAIN_WEB_ASSETS=/绝对路径/dist` 可在编译时指定前端构建位置。只编译 Agent 时不需要前端；缺少页面的控制台会在启动时报错并给出构建方法。依赖锁定于 Cargo.lock、frontend/package-lock.json。

## 构建、安装与运维脚本

在仓库根目录执行以下命令即可打开菜单，不再需要手工切换 npm/Cargo 目录。两个 `.sh` 入口及同目录 Python 实现需一起保留；支持 Python 3.10+。

```bash
./scripts/version-release.sh
./scripts/install-server.sh
```

完整的本机安装流程：

```bash
./scripts/version-release.sh check
./scripts/version-release.sh build
sudo ./scripts/install-server.sh install
sudo chainctl token
```

已有适用发行包时可跳过 `build`，直接安装；输出目录已存在时，重新构建需指定新的 `--output`。使用 root 登录时可省略 `sudo`。`release-output/` 不进入 Git，克隆源码后需要自行构建或复制已有发行包与校验文件。

首次交互安装自动选择空闲随机端口，只询问可选域名。网卡公网 IPv4 通过 `ip -j address` 获取，并优先采用本机路由选中的已分配公网源地址，不会以外部 IP 查询服务返回的代理出口地址代替入站地址。域名留空时监听 `0.0.0.0:随机端口`；填写域名时仅监听 `127.0.0.1:随机端口`，由 Nginx 提供 HTTPS。

高级非交互安装未指定监听地址时，仍默认 `127.0.0.1:8088`。例如需要 SSH 隧道时可显式这样安装：

```bash
sudo ./scripts/install-server.sh install --non-interactive --listen 127.0.0.1:8088
```

在自己电脑上运行以下命令，再打开 `http://127.0.0.1:18088`，输入 `chainctl token` 显示的管理令牌：

```bash
ssh -N -L 18088:127.0.0.1:8088 root@服务器IP
```

按 `Ctrl+C` 关闭隧道。高级自动化可用 `install --domain panel.example.com --non-interactive` 自动配置 HTTPS，或 `--domain '' --non-interactive` 自动选择 IP 和随机端口。NAT 环境通过 `--local-host` 明确公网入站地址。需要固定监听地址时增加 `--listen 0.0.0.0:8088`，并按需要配置防火墙访问范围。

域名安装查询公共 DNS 的 A/AAAA 记录并核对网卡公网地址，使用 Nginx webroot 完成 HTTP-01 验证。自动安装仅调用 apt/dnf；不下载第三方 Nginx/ACME 安装脚本。受管站点为主配置 `http` 块实际包含目录中的 `chain-panel.conf`，证书名为 `chain-panel-域名`，验证目录为 `/var/lib/chain-acme`。签证书前检查真实 HTTP 验证路径，重载后最多等待 15 秒让新站点生效；失败时显示验证 URL、HTTP 状态码或连接错误，HTTP 404/403 不再统一误报为防火墙问题。本机自检通过后仍由证书机构从公网验证 TCP 80；HTTPS 访问使用 TCP 443。同域名已有站点或 80/443 被其他程序占用时拒绝覆盖。只支持系统 Nginx 主配置及其 systemd 服务。TLS 配置失败恢复原站点与此前服务状态，已签发证书保留供重试。

`chain-panel-cert-renew.timer` 每天检查两次，Certbot 只续期该面板证书；deploy hook 也核对证书路径后才检查并重载 Nginx。面板数据、证书私钥和其他站点保持独立。已有 HTTP 安装可通过 `chainctl install --domain panel.example.com` 增加 HTTPS，并保留后端端口和本机 Agent 身份。

构建使用本机 Linux x86_64 或 aarch64 GNU 工具链，执行 TypeScript/Vite 构建与 Rust 单元测试后生成 release 二进制。自动下载固定的 Xray v26.3.27 并检查官方 ZIP 的 SHA256，打包程序、安装器、版本清单和许可说明。不会包含运行数据、原始购买 ZIP、node_modules 或 Cargo target。原始 Xray 许可随包保留；Metronic 主题许可沿用购买授权。

产物默认放在 `release-output/v0.1.0-linux-x86_64/`（依当前版本、架构变化）。包含 `chain-panel-0.1.0-linux-x86_64.tar.gz`、`release.json`、`SHA256SUMS`。这是有意保留的最终交付目录；临时源码副本、npm 缓存和 Cargo target 在成功、失败或中断后自动清理，下一次构建会重新编译。输出目录已存在时不会覆盖，请传入新的 `--output` 或更新版本。

```bash
./scripts/version-release.sh build --work-parent /大容量磁盘 --output /新的发行目录
./scripts/version-release.sh check --full          # 真实 Xray 三节点集成验证
./scripts/version-release.sh check --full --browser
./scripts/version-release.sh version              # 查看版本
./scripts/version-release.sh version --set 0.2.0   # 同步 Cargo/npm 清单与锁文件
./scripts/version-release.sh status
```

目标服务器需要 Linux、glibc、Python 3.10+、systemd、基础用户管理命令；无需 npm、Rust 或额外安装 Xray。发行包记录构建机的 glibc 版本，安装前会检查架构及 glibc 下限。需要兼容较旧发行版时，请在对应旧环境构建；目前不执行跨架构或 musl 构建。

安装器只管理 `/opt/chain` 下有归属标识的安装、`chain-panel.service`、`chain-agent.service`、各自的 `/etc/chain-*` 与 `/var/lib/chain-*`。遇到同名的非本安装器目录或服务会拒绝覆盖。默认安装控制台并自动接入本机 Agent；只需控制台时使用 `--panel-only`。独立节点机器使用：

```bash
# 在 Web 创建服务器后，将一次性令牌写入私有文件。
chmod 600 /root/node-token
sudo ./scripts/install-server.sh install --component agent \
  --panel-url https://panel.example.com \
  --node-id 网页给出的节点UUID --token-file /root/node-token
```

推荐在 Web 添加服务器后复制接入码，在安装菜单中选择「接入节点」并粘贴；接入码输入不会回显，不需要手动保存令牌文件。以下参数用于自动化部署。控制台机器需要参与线路转发时，可再安装本机 Agent；两个组件独立选择版本、启动和升级。

安装会优先选择本地发行包，没有本地包时自动下载最新正式版本。`chainctl update` 默认下载最新正式包；本地包可显式指定：

```bash
sudo ./scripts/install-server.sh install --package /发行目录/chain-panel-0.1.0-linux-x86_64.tar.gz
# 同目录必须保留 SHA256SUMS。
```

另一台机器没有源码仓库时，将发行包与 SHA256SUMS 一起复制过去，校验后解压，运行包内入口：

```bash
sha256sum -c SHA256SUMS
mkdir chain-package
tar -xzf chain-panel-0.1.0-linux-x86_64.tar.gz -C chain-package
sudo bash chain-package/scripts/install-server.sh install
```

SHA256 用于完整性校验，发行包应从自己控制的渠道获取。安装器默认启用并启动新服务；控制台安装后显示登录信息，也可用 `sudo chainctl token` 查看。自动 HTTPS 已设置安全 Cookie；手工反向代理部署增加 `--secure-cookie 1`，并保留包含端口的请求 Host。原有 SSH 管理的 Xray 服务不会被接管。

```bash
sudo chainctl status                         # 默认控制台
sudo chainctl logs --component agent          # 最近 100 条日志，不启动后台日志进程
sudo chainctl restart --component panel
sudo chainctl stop --component agent
sudo chainctl start --component agent
sudo chainctl backup --component panel
sudo chainctl update --component panel --package /新发行目录/chain-panel-0.2.0-linux-x86_64.tar.gz
sudo chainctl rollback --component panel
```

升级保留监听地址、令牌、线路数据库及节点配置；修改参数时显式传入对应选项。升级和备份会短暂停止所选组件，以获得一致的私有备份；Agent 停止会使其转发连接重连。备份位于 `/var/backups/chain-panel/`，权限 600。启动检查失败会恢复升级前的程序链接与配置；运行数据和备份保留。`rollback` 回滚程序版本，不将数据库回退到旧备份。Agent 的进程启动检查通过后，还应在 Web 确认心跳。

推荐通过 `.github/workflows/release.yml` 发布：提交版本改动后推送 `vX.Y.Z` 标签，Actions 在 Ubuntu 22.04 的 x86_64、aarch64 runner 分别原生构建（glibc 下限 2.35），校验标签与四个版本清单一致。两个架构均成功后合并 `SHA256SUMS`，创建草稿、上传并重新下载校验全部附件，最后公开正式 Release。安装器不读取 Actions 中间产物。单架构失败不会公开半套包；中间 artifact 在工作流结束时删除，Cargo/npm 临时缓存由构建器清理。

日常发布先自行完成业务代码的 Git 提交，再运行 `./scripts/version-release.sh`。菜单直接回车进入默认的 **9「发布已提交的代码」**；版本号再次回车默认递增补丁版本，例如 `0.2.1 → 0.2.2`，也可以输入指定版本，例如 `0.3.0`。查看版本文件、已有未推送提交和自动生成的中文提交正文，再输入 `y` 确认，无需重新填写业务更新说明。

向导只同步并提交 `panel/Cargo.toml`、`panel/Cargo.lock`、`panel/frontend/package.json`、`panel/frontend/package-lock.json`，随后创建附注标签，原子推送 `main` 和标签。业务代码必须提前提交：`panel/`、`scripts/`、`README.md`、`.gitignore`、`.github/` 中存在未提交改动或未忽略的新文件时停止；范围外文件保持不动。仅接受 `main` 且暂存区为空，远端存在本地未包含的提交、标签冲突或版本倒退时停止。不会自动合并、强推或部署服务器。

仅推送失败时，再选 9 会识别当前提交上未推送的版本标签，确认后继续推送，不再递增版本。其他失败保留现场，需先检查 `git status`。向导不代替测试，必须等 Actions 构建成功并公开正式 Release 后，服务器才能通过 `sudo chainctl update` 获取新版。也可用 `./scripts/version-release.sh release` 直接进入向导。

需要手工操作时：

```bash
git tag v0.1.0
git push origin main v0.1.0
```

手工上传发行包需要安装并登录 `gh`。本地构建与安装不会自动推送 Git、上传附件或发布版本；上述一键向导通过推送标签触发 Actions，以下命令则直接操作指定远端的 Release（手工脚本一次发布一个架构，双架构应使用 Actions）：

```bash
# 先提交并推送对应源码，然后从干净的发行输入重新构建。
./scripts/version-release.sh draft --repo OWNER/REPO --directory /发行目录
./scripts/version-release.sh publish --repo OWNER/REPO --directory /发行目录
```

草稿命令将发行包绑定到清单中的已推送提交；发布命令再次下载并校验草稿附件，与本地产物一致后才正式发布。未提交的本地构建可安装，不能上传为正式草稿。无 GitHub Actions 隐式触发流程。

审核安装文件而不操作系统时，可执行 `install --root /临时审核目录 --package ...`。此模式只生成离线安装树，不创建系统用户、操作 systemd 或启动服务；测试使用该模式验证升级、回滚与数据保留。

## 启动控制台

准备单独的 Xray 二进制路径，以下命令不会安装或覆盖现有 Xray。

```bash
CHAIN_DATA_DIR=/var/lib/chain-panel \
CHAIN_LISTEN=127.0.0.1:8088 \
CHAIN_XRAY_BIN=/opt/chain/bin/xray \
/opt/chain/bin/chain-panel
```

首次启动创建数据目录，并将管理令牌保存在 `/var/lib/chain-panel/admin-token`。在 Web 登录页输入令牌。默认仅监听本机，远程访问通过自己的 HTTPS 反向代理或 SSH 隧道。

正式 HTTPS 入口设置 `CHAIN_SECURE_COOKIE=1`。反向代理必须保留请求的 `Host`，例如 Nginx 的 `proxy_set_header Host $http_host;`；代理读取超时至少 60 秒，以容纳整链探测。控制台发布任务是异步操作，页面持续刷新状态。

| 控制台变量 | 默认值 / 说明 |
| --- | --- |
| `CHAIN_DATA_DIR` | `data`，只接受空目录或本程序拥有的数据目录 |
| `CHAIN_LISTEN` | `127.0.0.1:8088` |
| `CHAIN_XRAY_BIN` | `xray`，用于真实入口探测 |
| `CHAIN_SECURE_COOKIE` | `1` 开启 Secure Cookie，HTTPS 部署使用 |
| `CHAIN_APPLY_TIMEOUT` | `240`，每个节点应用 / 恢复的等待秒数，容纳首次证书申请 |
| `CHAIN_XRAY_LOG_LEVEL` | `warning`，排障可设 `debug` |

修改或删除 `admin-token` 后重启控制台可以轮换管理令牌；已登录会话会失效。控制台重启不会停止远端 Agent 已运行的流量。

## 接入服务器

1. 在「服务器与资源 → 我的服务器」添加名称、可达 IP/域名。REALITY 域名、端口和 SNI 位于高级选项，默认端口 443，独立输入。保存时生成密钥和只显示一次的节点令牌。
2. 将节点令牌写入服务器私有文件，如 `/etc/chain-agent/token`，权限 600。安装二进制至独立路径。
3. 配置环境变量并启动 Agent：

```bash
CHAIN_PANEL_URL=https://panel.example.com \
CHAIN_NODE_ID=网页给出的节点UUID \
CHAIN_NODE_TOKEN_FILE=/etc/chain-agent/token \
CHAIN_AGENT_DIR=/var/lib/chain-agent \
CHAIN_XRAY_BIN=/opt/chain/bin/xray \
/opt/chain/bin/chain-agent
```

Agent 主动连接控制台，不开放管理监听端口，不需要控制台保存 SSH 密码。除 localhost 外默认要求 HTTPS；明确可信的私网可以设置 `CHAIN_ALLOW_HTTP=1`。同一台服务器多个 Agent 必须使用不同节点 ID、令牌、数据目录和业务监听端口。

启动目录需由运行用户拥有；Agent 拒绝操作其他组件或非空无归属目录。每次应用先执行 `xray run -test -config`，随后只停止自己启动的 Xray。端口被已有服务占用时应用失败并恢复旧配置，不会结束占用者。

手工部署可参考 `deploy/` 的独立 systemd 单元。安装脚本会生成带版本链接与归属标识的独立单元；以上模板无需另行安装。Agent 停止会同时停止其 Xray；断开与控制台的连接则继续使用上次成功配置。

## 创建与修改线路

在「代理接入」或「端口转发」添加规则；默认单服务器直接出网，需要时展开中转路径，端口留空自动分配。普通转发目标地址与端口分开填写。仅改变后续路径会保留已有入口端口与 UUID，名称变化会改变分享链接名称片段。

默认保存后自动应用；会影响其他共享线路时确认影响范围。需要暂缓时勾选「仅保存草稿」。应用先尝试下游节点，多条交叉路径共享节点时不提供分布式原子切换，相关连接可能重连。API 保留 preview/publish 以兼容自动化。

失败会按快照恢复已尝试节点。控制台在发布中重启，也会转入恢复流程。离线节点的恢复配置保留，状态显示 `degraded`，待 Agent 回来确认后更新为 `rolled_back`。恢复属于尽力补偿；节点长期离线时不会显示成功。

应用完成后从「接入」扫码、复制链接或下载 Mihomo 配置；转发规则提供原业务入口地址。在更多操作或「监控与告警」按需检查连接。检查与导出使用已应用快照，草稿不会被导出为可用配置。

## 验证

`scripts/verify.py --xray /绝对路径/xray --work-parent /大容量目录` 在独立临时目录构建前后端、运行 Rust 测试和真实 Xray 集成测试，结束自动清理。可加 `--browser` 做 Chromium 桌面和移动端流程验证；浏览器及 npm 缓存也位于临时目录内。

提供 `CHAIN_TEST_MIHOMO=/已校验的/mihomo` 时，集成测试还会用真实 Mihomo 验证五类导出配置并请求本机测试站点。兼容验证使用 v1.19.32；只替换本地客户端监听端口，代理配置保持导出原样，测试 TLS CA 仅注入测试进程。

集成测试使用本机独立控制台、三个 Agent、真实 Xray、TLS 目标和 TCP/UDP 回显服务；覆盖多跳共享节点、稳定入口、外部出口、端口占用回滚、离线恢复；新增 features.py 验证 HTTP/SOCKS/SS/Trojan、HTTPS/WebSocket、多域名、SOCKS UDP 认证生命周期、八连接聚合限速、配额持久化、在线限速变更、导出/订阅、TOTP 和密钥轮换。browser.cjs 使用 Chromium 虚拟 CTAP2 验证 Passkey 注册/登录/撤销，检查桌面、手机和真实保存流程，并生成手册截图。测试不连接已有生产服务，进程组、端口和临时文件有归属记录，退出检查清理。

## Agent 入口网关与数据

编译配置的 `_chain` 元数据记录公共入口、限速、额度、网站与证书；Agent 在传给 Xray 前移除此字段。代理/转发的 Xray 入口绑定 127.0.0.1 的内部端口，公共端口由 Agent 网关监听。内部中转仍为 Xray 的 VLESS/REALITY，不增加第二套代理内核。

`traffic.json` 持久化线路计量，约每 2 秒写入并在正常退出前保存；`active.json` 保存已应用版本。Agent 上报网关能力和已占用 TCP/UDP 端口，自动端口分配避开这些端口。端口采样与实际绑定之间仍可能发生竞争，应用失败时保留占用者并回滚。控制台拒绝向未支持入口网关的旧 Agent 应用新规则，升级时先更新 Agent 再更新控制台。

自动证书在 Agent 数据目录的 `acme/` 下，Certbot 使用独立 config/work/logs/web 路径，以 Agent 用户执行；可用 `CHAIN_CERTBOT_BIN` 指定程序位置。自动证书要求 Agent 可以独占 TCP 80（或已运行自己的 80 网站），不会改动已有 Nginx。HTTPS 端口可以自定义。已有网站可以使用手工证书或在外层 Nginx 终止 TLS。

控制台订阅 token 和恢复码只保存哈希；TOTP 秘钥和业务证书需要在私有数据目录中保留原文。Passkey 使用 webauthn-rs 的挑战、来源、用户验证和签名检查，挑战有有效期且单次使用。管理员密钥轮换会退出全部会话；本地 `chainctl reset-auth` 先备份并停止控制台，再清除登录二步验证配置，保留业务数据。

主题来源及保留组件见 [主题来源与许可](../frontend/THEME.md)。四个原始购买 ZIP 保持原样。

本轮调研与设计取舍见[功能设计与参考](design-notes.md)。

## 引导安装与自动更新

`chainctl` 无参数时打开交互菜单，`chainctl help` 和 `chainctl --help` 无需管理员权限即可查看帮助。非管理员执行实际运维时自动调用 sudo；无终端且无子命令时只打印帮助。自动化调用增加 `--non-interactive`，保留原有 `--component`、`--package`、监听及令牌文件参数。

安装器可单独下载运行；在 curl 管道中会从 `/dev/tty` 读取交互输入。无参数脚本执行 `setup`：首次直接安装，已安装时显示登录信息，缺失默认本机 Agent 时重试接入。`chainctl` 无参数仍打开运维菜单。缺少 Python 时使用 apt/dnf 安装并校验 Python 3.10+。在线引导从项目 main 的固定提交下载三个辅助模块，再从 GitHub Releases 下载本机架构的正式包，检查 SHA256、清单和兼容性后才询问可选域名。下载和解压目录在退出时删除。没有正式包、架构缺失或校验失败时终止，不会静默改为源码编译。

维护者需要先推送安装器和前端改动，再按本页发布流程上传对应版本的正式包；此前本机已有包可用安装向导选择。发布兼容较旧系统的包时，应在对应旧版 glibc 环境构建。单次正式发行需要同时提供包、release.json 和 SHA256SUMS。

接入码只是将控制台地址、节点 UUID 和节点令牌合并成一份可复制凭据，不是临时兑换码；它没有单独有效期，节点重置令牌后失效。接入码不进入 shell 参数或历史，只通过隐藏输入交给向导。重新接入会保存新身份并重启 Agent，受其管理的流量可能短暂重连。HTTP 接入需要在向导中明确确认可信私网。

## Web 远程安装与本机自动接入

控制台安装默认附带本机 Agent，通过本机 API 创建节点并保存节点凭据；重复安装不覆盖已安装 Agent 的身份。控制台与 Agent 仍有独立服务与升级操作。只需控制台时使用高级参数 `--panel-only`；本机对外地址与 REALITY 目标可用 `--local-host`、`--local-target` 指定。默认 REALITY 目标为 www.microsoft.com:443，需按实际网络确认可达，之后也可在 Web 编辑。

Web 的「接入服务器」提供 SSH 安装：使用节点记录的 IP/域名，填写 root 密码或无口令私钥，检测并核对 SSH 指纹后安装。服务端以严格主机密钥校验连接，SSH 凭据只写入私有临时目录，任务结束删除，不入数据库；节点令牌通过 SSH 加密通道传递，不放在命令参数中。安装器上传控制台自身校验过的发行文件，所以控制台和节点须为兼容架构、满足包内 glibc 下限。其他架构可在节点上使用在线安装向导获取对应包。

控制台安装器会补齐 openssh-client、tar、gzip；运行环境需要 `CHAIN_RELEASE_DIR` 指向完整的已安装发行目录，该变量由安装器配置。直接运行开发二进制时不可使用 Web 传包安装。Web 只确认安装进程成功，最终接入以心跳显示在线为准；SSH 超时或失败时在节点通过 chainctl 查看状态。
