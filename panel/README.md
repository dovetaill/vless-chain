# Chain Panel

个人自用的多服务器 Web 管理台。Rust 控制台负责线路编排、SQLite 状态与发布任务，Rust Agent 管理独立的 Xray 子进程，React + TypeScript 前端使用已购买的 Metronic Tailwind 主题组件。

本目录是新增功能。不会读取、导入、覆盖或停止原有 SSH 脚本创建的服务。没有复制 3x-ui 源码，也不依赖 3x-ui 运行。

## 当前能力

- 同时管理多条独立 VLESS + REALITY + Vision 线路，每条 1–8 个受管节点，可共享物理节点，入口端口、UUID 和路径分别保存。
- TCP、UDP、TCP + UDP 端口转发；多跳通过 Xray 的 VLESS 连接传送，最终节点连接固定目标。
- 外部 VLESS 落地导入：TCP/raw 或 XHTTP，TLS 或 REALITY。外部服务只作为出口，不受 Agent 管理。
- 草稿、端口冲突和循环检查、影响范围预览、逐节点应用、运行确认、失败恢复。每个 Agent 管理一个独立 Xray，多条新线路共享该进程；修改会使其相关连接短暂重连。
- 真实入口 HTTP/HTTPS 探测，以及 TCP/UDP 回显探测。后两者要求目标运行回显服务，不把端口连通当作整链成功。
- 管理令牌登录、同源 Cookie 会话、节点专属令牌、一次性展示、令牌重置、发布记录和重启恢复。

没有套餐、计费、订阅中心、租户、自动选路或自动故障切换。当前受管入口监听 IPv4；没有验证跨公网环境、IPv6 入站、外部 XHTTP 的所有扩展参数或不同版本客户端。实际网络中的防火墙、端口放行和 REALITY TLS 目标由使用者配置。完整数据目录含线路凭据与 REALITY 私钥，权限为 700，备份应保持私有。

## 构建

`npm ci` 报缺少锁文件时，先检查所在目录：前端清单与锁文件位于 `panel/frontend/`，仓库根目录没有 npm 项目。不要为了消除报错在根目录生成新锁文件。

需要 Linux、Rust 2024 edition 工具链、Node.js 20.19+ 或 22.12+，以及 npm。运行时只需要两个 Rust 二进制和 Xray；前端静态文件编译进 `chain-panel`，不需要 Node.js 服务。

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

安装后默认监听 `127.0.0.1:8088`。在自己电脑上运行以下命令，再打开 `http://127.0.0.1:18088`，输入 `chainctl token` 显示的管理令牌：

```bash
ssh -N -L 18088:127.0.0.1:8088 root@服务器IP
```

按 `Ctrl+C` 关闭隧道。使用域名访问时配置 HTTPS 反向代理；需要直接监听所有 IPv4 地址时，安装命令增加 `--listen 0.0.0.0:8088`，并按需要配置防火墙访问范围。

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

安装器只管理 `/opt/chain` 下有归属标识的安装、`chain-panel.service`、`chain-agent.service`、各自的 `/etc/chain-*` 与 `/var/lib/chain-*`。遇到同名的非本安装器目录或服务会拒绝覆盖。默认安装控制台；节点机器使用：

```bash
# 在 Web 创建服务器后，将一次性令牌写入私有文件。
chmod 600 /root/node-token
sudo ./scripts/install-server.sh install --component agent \
  --panel-url https://panel.example.com \
  --node-id 网页给出的节点UUID --token-file /root/node-token
```

也可以在安装菜单中粘贴 Agent 参数，令牌输入不会回显。控制台机器需要参与线路转发时，可再安装本机 Agent；两个组件独立选择版本、启动和升级。

从仓库安装时自动选择 `release-output/` 中本机架构最近生成的包，也可显式指定：

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

SHA256 用于完整性校验，发行包应从自己控制的渠道获取。安装器默认启用并启动所选的新服务；控制台安装后显示令牌文件位置，可用 `sudo chainctl token` 查看。默认本机访问；HTTPS 反向代理部署增加 `--secure-cookie 1`，并保留请求 Host。原有 SSH 管理的 Xray 服务不会被接管。

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

GitHub 操作需要额外安装并登录 `gh`。构建与安装不会自动推送 Git、上传附件或发布版本；以下命令才会操作指定远端仓库：

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
| `CHAIN_APPLY_TIMEOUT` | `35`，每个节点应用 / 恢复的等待秒数 |
| `CHAIN_XRAY_LOG_LEVEL` | `warning`，排障可设 `debug` |

修改或删除 `admin-token` 后重启控制台可以轮换管理令牌；已登录会话会失效。控制台重启不会停止远端 Agent 已运行的流量。

## 接入服务器

1. 在「服务器」添加名称、可达 IP/域名、REALITY 目标 `host:port` 和对应 SNI。保存时生成密钥以及只显示一次的节点令牌。
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

在「代理线路」或「端口转发」添加路径，按顺序选择入口、中转和出口，端口留空自动分配。每条线路独立入口；仅改变后续路径会保留已有入口端口与 UUID。线路名变化会改变分享链接的名称片段。

保存只生成草稿。点击「预览并发布」查看受影响节点及共享实例的线路，然后发布。发布先尝试下游节点；多条交叉路径共享节点时不提供分布式原子切换，相关连接可能重连。

失败会按快照恢复已尝试节点。控制台在发布中重启，也会转入恢复流程。离线节点的恢复配置保留，状态显示 `degraded`，待 Agent 回来确认后更新为 `rolled_back`。恢复属于尽力补偿；节点长期离线时不会显示成功。

发布完成后从「接入」复制 VLESS 链接，或端口转发入口地址。再执行「诊断」验证实际路径。诊断使用已发布快照，尚未发布的修改不会进入探测。

## 验证

`scripts/verify.py --xray /绝对路径/xray --work-parent /大容量目录` 在独立临时目录构建前后端、运行 Rust 测试和真实 Xray 集成测试，结束自动清理。可加 `--browser` 做 Chromium 桌面和移动端流程验证；浏览器及 npm 缓存也位于临时目录内。

集成测试使用本机独立控制台、三个 Agent、真实 Xray、TLS 1.3 目标和 TCP/UDP 回显服务；覆盖多跳与共享节点、稳定入口、外部 VLESS 落地、端口占用恢复、重启和离线节点恢复。测试不会连接现有生产服务。进程组、端口和临时文件有归属记录，退出后检查并清理。

主题来源及保留组件见 [frontend/THEME.md](frontend/THEME.md)。四个原始购买 ZIP 保持原样。
