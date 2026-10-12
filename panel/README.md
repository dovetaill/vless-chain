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

需要 Linux、Rust 2024 edition 工具链、Node.js 20.19+ 或 22.12+，以及 npm。运行时只需要两个 Rust 二进制和 Xray；前端静态文件编译进 `chain-panel`，不需要 Node.js 服务。

```bash
cd panel/frontend
npm ci
npm run build
cd ..
cargo build --release --bins
```

构建出的 `chain-panel`、`chain-agent` 位于 Cargo 的 `release/` 目录。生产构建可设置 `CARGO_TARGET_DIR` 到专属的大容量目录，交付后清理；项目提供的验证脚本会自动清理自己的 target。

`CHAIN_WEB_ASSETS=/绝对路径/dist` 可在编译时指定前端构建位置。只编译 Agent 时不需要前端；缺少页面的控制台会在启动时报错并给出构建方法。依赖锁定于 Cargo.lock、frontend/package-lock.json。

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

可参考 `deploy/` 的独立 systemd 单元。仅提供模板，未安装到本机。Agent 停止会同时停止其 Xray；断开与控制台的连接则继续使用上次成功配置。

## 创建与修改线路

在「代理线路」或「端口转发」添加路径，按顺序选择入口、中转和出口，端口留空自动分配。每条线路独立入口；仅改变后续路径会保留已有入口端口与 UUID。线路名变化会改变分享链接的名称片段。

保存只生成草稿。点击「预览并发布」查看受影响节点及共享实例的线路，然后发布。发布先尝试下游节点；多条交叉路径共享节点时不提供分布式原子切换，相关连接可能重连。

失败会按快照恢复已尝试节点。控制台在发布中重启，也会转入恢复流程。离线节点的恢复配置保留，状态显示 `degraded`，待 Agent 回来确认后更新为 `rolled_back`。恢复属于尽力补偿；节点长期离线时不会显示成功。

发布完成后从「接入」复制 VLESS 链接，或端口转发入口地址。再执行「诊断」验证实际路径。诊断使用已发布快照，尚未发布的修改不会进入探测。

## 验证

`scripts/verify.py --xray /绝对路径/xray --work-parent /大容量目录` 在独立临时目录构建前后端、运行 Rust 测试和真实 Xray 集成测试，结束自动清理。可加 `--browser` 做 Chromium 桌面和移动端流程验证；浏览器及 npm 缓存也位于临时目录内。

集成测试使用本机独立控制台、三个 Agent、真实 Xray、TLS 1.3 目标和 TCP/UDP 回显服务；覆盖多跳与共享节点、稳定入口、外部 VLESS 落地、端口占用恢复、重启和离线节点恢复。测试不会连接现有生产服务。进程组、端口和临时文件有归属记录，退出后检查并清理。

主题来源及保留组件见 [frontend/THEME.md](frontend/THEME.md)。四个原始购买 ZIP 保持原样。
