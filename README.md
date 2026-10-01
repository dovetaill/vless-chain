# VLESS / Xray 原生实例管理

本文介绍 `xray-vless-native-v3.sh` 的五个菜单功能和使用示例。脚本支持 Debian/Ubuntu、CentOS/RHEL 系和 Alpine，使用 systemd 或 OpenRC 管理服务。

如果你要让 **A 既直接出网，又作为 B 的前置入口**，看下面[实战示例](#实战示例)中的**场景 3**。它会说明 A 上的两个实例怎样创建，以及客户端该用哪个链接。

## 首次运行

使用 root 用户，在脚本所在目录执行：

```sh
bash ./xray-vless-native-v3.sh
```

Alpine 默认使用 BusyBox `sh`，首次运行前先安装 Bash：

```sh
apk add --no-cache bash
bash ./xray-vless-native-v3.sh
```

脚本使用了 Bash 数组等语法，请使用 `bash` 启动。用 `sh xray-vless-native-v3.sh` 会出现语法错误。

进入后会看到：

```text
VLESS 实例管理
  1) 安装落地 VLESS（直接出网）
  2) 新增租户入口 VLESS（粘贴落地配置）
  3) 列出所有实例和绑定
  4) 选择实例进行运维
  5) 导入旧版部署
  0) 退出
```

下面用两台服务器举例：B 是落地服务器，A 是入口服务器。示例中的域名、伪装目标和端口需要按实际情况填写；本机节点域名应解析到对应服务器，客户端访问的端口需要放行。

**角色属于实例，一台机器可以运行多个实例。** 例如 A 上的 `a-direct` 可以直接出网，`a-to-b` 可以转发到 B。它们使用不同的监听端口和 UUID，客户端通过不同的分享链接选择线路。

## 1）安装落地 VLESS（直接出网）

这个功能是在当前服务器新建一个 VLESS 节点，由这台服务器直接访问 Internet。它可以供客户端直接连接，也可以作为其他入口节点的下一跳。

```text
客户端 → B 落地服务器 → Internet
```

例如，你希望 B 服务器作为最终出口，给它取一个管理编号 `landing01`。在 B 上选择菜单 `1`，或者执行：

```sh
bash ./xray-vless-native-v3.sh install direct landing01
```

按提示填写，下面是使用 REALITY 和外部伪装目标的示例：

| 提示 | 示例填写 |
| --- | --- |
| 租户显示名称 | B 落地 |
| 节点域名 | `landing.example.com`，替换为指向 B 的域名 |
| 入口模式 | `2`，VLESS + REALITY + Vision |
| REALITY 伪装目标 | `2`，指定外部 TLS 站点 |
| REALITY target | `www.example.com:443`，替换为实际可用的 TLS 伪装目标 |
| REALITY 监听端口 | `8443`，也可以选择其他空闲端口 |

REALITY 使用外部伪装目标时不需要本地 TLS 证书。如果选择 TLS、dual 或本机 HTTPS 伪装站，则继续按提示配置证书。

HTTP-01 除了需要正确的 DNS 记录，还需要 Nginx 能读到验证文件。v3 默认把新实例的网站和验证文件放在 `/var/lib/xray-chain-manager`，避免部分系统的 `/var/www` 权限为 `700` 时阻断 Nginx 访问。已有实例继续使用登记的路径；已有证书续期继续使用原来的 webroot。可通过 `MANAGER_WEB_ROOT` 指定其他位置，但所有父目录都必须允许 Nginx 工作用户穿过。

申请前脚本会分别检查本机 Nginx 和域名访问，要求返回 HTTP 200 且内容匹配。若提示本机自检失败，查看 `nginx -T` 和 Nginx 错误日志；`Permission denied` 表示需要检查整个目录链的权限。脚本不会自动修改 `/var/www` 或 `/var/lib/nginx/proxy` 的权限、所有者，也不会修改 Nginx 的工作用户。

安装成功后会生成节点分享链接。之后可以随时查看：

```sh
bash ./xray-vless-native-v3.sh links landing01
```

把生成的 `vless://...` 链接导入客户端，就能连接 B。需要搭建入口时，也可以把这个链接复制到 A 上，作为菜单 `2` 的落地配置。

## 2）新增租户入口 VLESS（粘贴落地配置）

这个功能是在当前服务器创建一个入口节点，将客户端的代理流量继续交给指定的落地节点。

```text
客户端 → A 入口服务器 → B 落地服务器 → Internet
```

例如 B 已经部署好 `landing01`，现在要在 A 上创建管理编号为 `tenant01` 的入口。在 A 上选择菜单 `2`，或者执行：

```sh
bash ./xray-vless-native-v3.sh install relay tenant01
```

按提示填写：

| 提示 | 示例填写 |
| --- | --- |
| 租户显示名称 | 租户 01 入口 |
| 节点域名 | `edge.example.com`，替换为指向 A 的域名 |
| 入口模式 | `2`，VLESS + REALITY + Vision |
| REALITY 伪装目标 | `2`，指定外部 TLS 站点 |
| REALITY target | `www.example.com:443`，替换为实际可用的 TLS 伪装目标 |
| REALITY 监听端口 | `9443`，使用 A 上的空闲端口 |
| 落地配置 | 粘贴 B 上生成的完整 `vless://...` 链接 |

脚本会解析落地地址、端口、UUID、安全方式等信息，并显示出来让你确认。字段不完整时，会提示补填。

落地配置支持单条 VLESS 链接，也支持单个 YAML/JSON 节点；粘贴多行节点时，最后另起一行输入 `END`。目前支持的落地传输是 TCP/raw 或 XHTTP，安全方式是 TLS 或 REALITY。

创建完成后，在 A 上查看入口分享链接：

```sh
bash ./xray-vless-native-v3.sh links tenant01
```

把 **A 的入口分享链接** 发给使用这个入口的客户端。客户端连接 A，A 再使用你粘贴的落地配置连接 B。

如果还要给另一个租户创建入口，可以再次选择菜单 `2`，或执行：

```sh
bash ./xray-vless-native-v3.sh install relay tenant02
```

例如给 `tenant02` 使用端口 `9444`，并粘贴同一个 B 的链接。每个新建实例都有独立的 UUID、配置和服务；同机实例的监听端口需要不同，可以绑定相同或不同的落地。

## 3）列出所有实例和绑定

这个功能用于查看**当前服务器上已经登记的实例**，包括编号、名称、角色、监听端口、运行状态和绑定的落地。

选择菜单 `3`，或者执行：

```sh
bash ./xray-vless-native-v3.sh list
```

例如，在 A 上创建两个入口后，可能显示：

```text
编号               名称             角色     端口         运行状态   落地
tenant01           租户 01 入口      relay    9443         running    landing.example.com:8443
tenant02           租户 02 入口      relay    9444         stopped    landing.example.com:8443
```

这表示两个入口都绑定了同一个落地，其中 `tenant01` 正在运行，`tenant02` 已停止。直接出网的 `direct` 实例在“落地”一栏显示 `Internet`。

A 和 B 的实例记录分别保存在各自服务器上。要查看 B 的 `landing01`，需要在 B 上执行这条命令。

## 4）选择实例进行运维

这个功能用于管理一个已经创建或导入的实例。

选择菜单 `4`，再选择要管理的实例；也可以直接指定编号：

```sh
bash ./xray-vless-native-v3.sh manage tenant01
```

进入后可以查看状态和绑定、查看日志、启动、停止、重启、设置开机自启、查看分享链接。入口实例还会显示“更换绑定落地”。

常用操作也可以直接执行：

```sh
# 查看状态和绑定
bash ./xray-vless-native-v3.sh status tenant01

# 查看最近日志
bash ./xray-vless-native-v3.sh logs tenant01

# 持续查看日志，Ctrl+C 结束查看
bash ./xray-vless-native-v3.sh follow tenant01

# 启动、停止或重启指定实例
bash ./xray-vless-native-v3.sh start tenant01
bash ./xray-vless-native-v3.sh stop tenant01
bash ./xray-vless-native-v3.sh restart tenant01

# 开启或关闭开机自启
bash ./xray-vless-native-v3.sh enable tenant01
bash ./xray-vless-native-v3.sh disable tenant01

# 查看分享链接
bash ./xray-vless-native-v3.sh links tenant01
```

“停止”只改变当前运行状态；“关闭开机自启”只改变自启设置。例如，要让实例现在停止且重启服务器后也不自动启动，需要执行 `stop` 和 `disable` 两个操作。

如果要把 `tenant01` 的落地从 B 换成 C，选择该实例的“更换绑定落地”，或者执行：

```sh
bash ./xray-vless-native-v3.sh upstream tenant01
```

然后粘贴 C 的完整节点配置并确认。更换落地会保留这个入口的 UUID 和监听端口；正在运行的入口会重启应用配置，已停止的入口保持停止状态。配置校验或应用失败时，脚本会尝试恢复原配置。

这些命令作用于指定的实例。例如 `stop tenant01` 会停止 `tenant01`，`tenant02` 由它自己的服务继续运行。

## 5）导入旧版部署

这个功能是把**当前服务器上已经安装好的旧 Xray 原生部署，登记到新版脚本里**，方便通过同一个菜单管理。

例如，你以前部署过一个节点：

- 配置目录：`/etc/xray-chain`，里面有 `config.json`。
- 服务名：`xray-chain`，由 systemd 或 OpenRC 管理。
- 希望在新版菜单里使用的编号：`legacy`。

选择菜单 `5`，按提示填写这三项；或者执行：

```sh
bash ./xray-vless-native-v3.sh import legacy /etc/xray-chain xray-chain
```

脚本会读取旧配置，检查原服务是否使用这个配置文件，并显示部署信息让你确认。旧配置没有记录对外域名时，会提示补填。

**导入保留原来的 UUID、端口、配置目录和服务。导入操作本身不会重装或重启节点，只新增管理记录。**

之后可以这样管理这个旧节点：

```sh
# 进入旧节点的运维菜单
bash ./xray-vless-native-v3.sh manage legacy

# 查看旧节点状态
bash ./xray-vless-native-v3.sh status legacy

# 重启旧节点（在确实需要重启时执行）
bash ./xray-vless-native-v3.sh restart legacy
```

当前导入要求旧配置的入站是 VLESS + TCP/raw + TLS 或 REALITY，且只有一个 UUID；dual 两个入站可以共用这个 UUID。路由也需要符合脚本可识别的直接出网或中转结构，不是所有第三方 Xray 配置都能导入。

已有的 systemd/OpenRC 服务是导入条件。项目中 `xray-vless-chain-v2.sh` 使用的 Docker 容器部署，不能通过这个菜单直接转换为原生服务。

如果你是第一次安装，没有旧节点，直接选择菜单 `1` 或 `2` 即可。

## 实战示例

下面六个场景按操作顺序说明：在哪台机器执行、粘贴谁的链接，以及客户端最终从哪里出网。

| 场景 | 流量路线 | 客户端使用的链接 | 最终出网机器 |
| --- | --- | --- | --- |
| 1. 单机直连 A | 客户端 → A → Internet | A 的直接出网实例 | A |
| 2. A 前置、B 落地 | 客户端 → A → B → Internet | A 的入口实例 | B |
| 3. A 兼任直接出网和 B 的前置 | 客户端 → A → Internet；或客户端 → A → B → Internet | 按需要选 A 上的两个实例 | A 或 B |
| 4. B 前置、A 落地 | 客户端 → B → A → Internet | B 的入口实例 | A |
| 5. A 上的租户分别绑定 B、C | 客户端 → A → B/C → Internet | A 上对应租户的入口实例 | B 或 C |
| 6. 三台机器串联 | 客户端 → A → B → C → Internet | A 的入口实例 | C |

### 实战示例的共同准备

每台参与的机器都需要有 `xray-vless-native-v3.sh`，并在各自机器上执行命令。脚本只管理当前机器上的实例，需要分别登录 A、B、C 操作。

这些示例统一使用 REALITY + 外部伪装目标，运行时仍会提示填写显示名称、实际的 REALITY 伪装目标，并确认端口和落地配置。

- 把 `a.example.com`、`b.example.com`、`c.example.com` 分别替换为解析到 A、B、C 的真实域名。
- 提示输入 `REALITY target` 时，填写实际可用的 TLS 伪装目标，格式为 `主机名:443`。这个字段填写伪装目标，不是用来指定下一跳；下一跳由后面粘贴的落地配置决定。
- 下例的 `8443`、`9443`、`9444` 是示例监听端口。确认它们在对应机器上空闲，并放行相应 TCP 端口；同一台机器不能让两个实例监听同一个端口。
- 查看分享信息时，只复制其中完整的 `vless://...` 那一行，保留链接中的公钥、SNI、short ID 等参数。
- 六个场景可以独立阅读。如果相应实例已经创建，直接使用它的 `links` 命令，不要再次安装同名实例。已有实例使用其他编号或端口时，按实际部署替换。

### 场景 1：只有 A，一台机器直接出网

目标：客户端连接 A，最终使用 A 的出口。

```text
客户端 → A:8443（a-direct）→ Internet（A 出口）
```

**在 A 上执行：**

```sh
DOMAIN=a.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote REALITY_PORT=8443 \
  bash ./xray-vless-native-v3.sh install direct a-direct
```

填写实际的伪装目标并完成安装，然后在 A 上查看链接：

```sh
bash ./xray-vless-native-v3.sh links a-direct
```

**客户端操作：** 把 `a-direct` 的 VLESS 链接导入客户端并选中使用。这个场景只需要 A 上的直接出网实例。

### 场景 2：A 是前置入口，B 是最终落地

目标：客户端先连接 A，由 A 转发到 B，最终使用 B 的出口。创建顺序是先 B、再 A。

```text
客户端 → A:9443（a-to-b）→ B:8443（b-direct）→ Internet（B 出口）
```

**第一步，在 B 上创建直接出网实例：**

```sh
DOMAIN=b.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote REALITY_PORT=8443 \
  bash ./xray-vless-native-v3.sh install direct b-direct

bash ./xray-vless-native-v3.sh links b-direct
```

复制 B 输出的完整 VLESS 链接。如果 B 已经有可用的落地节点，也可以直接使用现有节点的完整链接。

**第二步，在 A 上创建绑定 B 的入口：**

```sh
DOMAIN=a.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote REALITY_PORT=9443 \
  bash ./xray-vless-native-v3.sh install relay a-to-b
```

提示“落地配置”时，粘贴**第一步 B 的链接**，确认显示的落地地址是 B。

完成后，在 A 上查看入口链接：

```sh
bash ./xray-vless-native-v3.sh links a-to-b
```

**客户端操作：** 导入并使用 A 的 `a-to-b` 链接。A 通过 B 的节点完成转发。

### 场景 3：A 既直接出网，也作为 B 的前置入口

这就是“A 是落地，同时又是 B 的前置入口”的用法。**在 A 上运行两个独立实例，分别提供两条线路。**

```text
线路一：客户端 → A:8443（a-direct）→ Internet（A 出口）
线路二：客户端 → A:9443（a-to-b）→ B:8443（b-direct）→ Internet（B 出口）
```

| 机器 | 实例编号 | 角色 | 示例端口 | 用途 |
| --- | --- | --- | --- | --- |
| A | `a-direct` | direct | `8443` | 使用 A 自己的出口 |
| A | `a-to-b` | relay | `9443` | 作为 B 的前置入口 |
| B | `b-direct` | direct | `8443` | 为第二条线路提供最终出口 |

A 和 B 是不同机器，所以都使用 `8443` 不冲突；A 上的两个实例使用 `8443` 和 `9443` 两个不同端口。A 的两个实例也可以使用同一个指向 A 的域名。

**第一步，在 A 上创建直接出网实例：**

```sh
DOMAIN=a.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote REALITY_PORT=8443 \
  bash ./xray-vless-native-v3.sh install direct a-direct
```

如果 A 已经有落地实例，这一步直接复用现有实例即可。例如，已有编号为 `landing01` 的落地，就继续用 `landing01` 管理和查看链接。需要登记旧原生部署时，先使用菜单 `5` 导入。

**第二步，在 B 上创建落地并复制链接：**

```sh
DOMAIN=b.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote REALITY_PORT=8443 \
  bash ./xray-vless-native-v3.sh install direct b-direct

bash ./xray-vless-native-v3.sh links b-direct
```

如果 B 已经部署好落地，直接复制现有节点的完整 VLESS 链接。

**第三步，回到 A 上，新增绑定 B 的入口：**

```sh
DOMAIN=a.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote REALITY_PORT=9443 \
  bash ./xray-vless-native-v3.sh install relay a-to-b
```

提示“落地配置”时，粘贴**第二步 B 的链接**。这会创建 A 上的第二个实例，原来的直接出网实例继续使用原配置和服务。

**第四步，在 A 上分别取得两条线路的链接：**

```sh
# 线路一：从 A 直接出网
bash ./xray-vless-native-v3.sh links a-direct

# 线路二：经过 A，再从 B 出网
bash ./xray-vless-native-v3.sh links a-to-b

# 查看 A 上的两个实例及绑定
bash ./xray-vless-native-v3.sh list
```

如果第一步复用了已有实例，请把第一条命令中的 `a-direct` 换成原来的编号。

**客户端操作：** 把这两个 VLESS 链接都导入客户端，可以分别命名为“A 直出”和“A → B”。选择“A 直出”时从 A 出网；选择“A → B”时从 B 出网。

如果 B 换了新节点，在 A 上执行：

```sh
bash ./xray-vless-native-v3.sh upstream a-to-b
```

粘贴 B 的新链接即可更新第二条线路。`upstream` 命令用于 relay 实例；已有的 `direct` 实例要同时提供中转线路时，需要另外创建 relay 实例。

### 场景 4：B 是前置入口，A 是最终落地

目标：客户端先连接 B，转发到 A，最终使用 A 的出口。这是与场景 2 相反的方向。

```text
客户端 → B:9443（b-to-a）→ A:8443（a-direct）→ Internet（A 出口）
```

**第一步，在 A 上准备直接出网实例并复制链接：**

```sh
DOMAIN=a.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote REALITY_PORT=8443 \
  bash ./xray-vless-native-v3.sh install direct a-direct

bash ./xray-vless-native-v3.sh links a-direct
```

如果 A 已有这个实例，直接执行 `links` 命令。

**第二步，在 B 上创建入口：**

```sh
DOMAIN=b.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote REALITY_PORT=9443 \
  bash ./xray-vless-native-v3.sh install relay b-to-a
```

提示“落地配置”时，粘贴**A 的 `a-direct` 链接**，然后在 B 上查看入口链接：

```sh
bash ./xray-vless-native-v3.sh links b-to-a
```

**客户端操作：** 使用 B 的 `b-to-a` 链接，从 A 出网。A 的 `a-direct` 仍然可以供其他客户端直接连接。如果 A 还运行了场景 3 的 `a-to-b`，这里仍要给 B 粘贴 `a-direct` 的链接，避免把 A、B 配成互相转发的循环。

### 场景 5：A 上两个租户，分别使用 B 和 C 的出口

目标：同一台入口服务器 A 提供两条线路，租户 01 从 B 出网，租户 02 从 C 出网。

```text
租户 01 → A:9443（tenant01）→ B:8443（b-direct）→ Internet（B 出口）
租户 02 → A:9444（tenant02）→ C:8443（c-direct）→ Internet（C 出口）
```

**第一步，在 B 上创建落地并复制链接：**

```sh
DOMAIN=b.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote REALITY_PORT=8443 \
  bash ./xray-vless-native-v3.sh install direct b-direct

bash ./xray-vless-native-v3.sh links b-direct
```

**第二步，在 C 上创建落地并复制链接：**

```sh
DOMAIN=c.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote REALITY_PORT=8443 \
  bash ./xray-vless-native-v3.sh install direct c-direct

bash ./xray-vless-native-v3.sh links c-direct
```

B 或 C 已有节点时，直接使用各自现有的完整 VLESS 链接。

**第三步，在 A 上创建租户 01 入口：**

```sh
DOMAIN=a.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote REALITY_PORT=9443 \
  bash ./xray-vless-native-v3.sh install relay tenant01
```

提示“落地配置”时，粘贴 **B 的链接**。

**第四步，仍在 A 上创建租户 02 入口：**

```sh
DOMAIN=a.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote REALITY_PORT=9444 \
  bash ./xray-vless-native-v3.sh install relay tenant02
```

提示“落地配置”时，粘贴 **C 的链接**。然后在 A 上查看两条入口链接和绑定：

```sh
bash ./xray-vless-native-v3.sh links tenant01
bash ./xray-vless-native-v3.sh links tenant02
bash ./xray-vless-native-v3.sh list
```

**客户端操作：** 租户 01 使用 `tenant01` 的入口链接，租户 02 使用 `tenant02` 的入口链接。如果两个租户都要从 B 出网，创建 `tenant02` 时也粘贴 B 的链接即可，A 上仍使用两个不同的监听端口。

### 场景 6：A → B → C，三台机器串联

目标：A 是第一跳，B 是中间转发，C 是最终出口。创建顺序是 **先 C、再 B、最后 A**，每个入口绑定它的下一跳。

```text
客户端 → A:9443（a-to-b）→ B:9443（b-to-c）→ C:8443（c-direct）→ Internet（C 出口）
```

**第一步，在 C 上创建最终落地并复制链接：**

```sh
DOMAIN=c.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote REALITY_PORT=8443 \
  bash ./xray-vless-native-v3.sh install direct c-direct

bash ./xray-vless-native-v3.sh links c-direct
```

**第二步，在 B 上创建绑定 C 的入口：**

```sh
DOMAIN=b.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote REALITY_PORT=9443 \
  bash ./xray-vless-native-v3.sh install relay b-to-c
```

提示“落地配置”时，粘贴 **C 的 `c-direct` 链接**。完成后在 B 上查看：

```sh
bash ./xray-vless-native-v3.sh links b-to-c
```

**第三步，在 A 上创建绑定 B 的入口：**

```sh
DOMAIN=a.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote REALITY_PORT=9443 \
  bash ./xray-vless-native-v3.sh install relay a-to-b
```

提示“落地配置”时，粘贴 **第二步 B 的 `b-to-c` 链接**，然后在 A 上查看：

```sh
bash ./xray-vless-native-v3.sh links a-to-b
```

**客户端操作：** 使用 A 的 `a-to-b` 链接，依次经过 A、B、C，最终从 C 出网。这里 A 绑定的是 B 的中转实例 `b-to-c`；如果绑定 B 的直接出网实例 `b-direct`，流量就会从 B 出网。

如果已经按场景 2 创建了 A 的 `a-to-b`，在 B 上新建 `b-to-c` 后，可以在 A 上用 `upstream a-to-b` 粘贴 B 的 `b-to-c` 链接，更新下一跳。这样 B 原来的直接出网实例也可以继续使用。

### 怎样确认当前实例和线路

在每台机器上查看本机实例，确认编号、角色、端口和绑定符合你选择的场景：

```sh
bash ./xray-vless-native-v3.sh list
```

例如，在场景 3 的 A 上分别查看两个实例：

```sh
bash ./xray-vless-native-v3.sh status a-direct
bash ./xray-vless-native-v3.sh status a-to-b
```

`a-direct` 应是 `direct`；`a-to-b` 应是 `relay`，显示绑定到 B 的域名和落地端口。

最终在客户端选中对应线路，让浏览器或查询工具的流量实际经过代理，再查看出口 IP。使用 `a-direct` 时应看到 A 的出口 IP，使用 `a-to-b` 时应看到 B 的出口 IP；三跳线路应看到 C 的出口 IP。仅有入口服务处于运行状态，还不能证明后面的整条链路已经连通。

## 实例编号和文件位置

实例编号用于命令行指定管理对象，例如 `landing01`、`tenant01`、`tenant02` 或 `legacy`。使用 1–40 位英文字母、数字、下划线和短横线，以英文字母或数字开头，同一台服务器上的编号不能重复。

默认情况下，新建的 `tenant01` 实例使用：

- 配置和管理记录：`/etc/xray-chain-manager/instances/tenant01/`。
- 服务名：`xray-chain-tenant01`。
- 网站目录：`/var/www/xray-chain-manager/instances/tenant01/`。

导入的旧实例继续使用原来的配置目录和服务，新版登记文件默认保存在 `/etc/xray-chain-manager/instances/legacy/state.json`。
