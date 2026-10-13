# 功能设计与参考

本轮以个人多服务器管理为范围。调研使用 Tavily 检索，并核对项目官方文档；下面区分外部资料与本项目的选择。

## 将用户任务与内部协议分开

GOST 将 HTTP 代理和固定 TCP 转发区分为不同服务类型，并独立表达后续转发链。[GOST 命令行模型](https://gost.run/reference/configuration/cmd)

本项目据此拆出「代理接入」「端口转发」「网站反代」三个入口。原有 Xray 继续承载代理与内部中转，表单不再要求普通端口转发用户先理解 VLESS。资源页区分自有服务器与外部端点；服务商出售的转发端口只是端点映射，不能被当成拥有系统管理权限的服务器。

## 整条线路的带宽上限

GOST 明确区分服务级限速与连接级限速，二者的限制范围不同。[GOST 限速限流](https://gost.run/concepts/limiter)

本项目采用入口网关中的共享方向预算：同一线路的 TCP/UDP 和并发连接合计受限；上传与下载各有预算。网关直接控制数据传输，监控负责展示。配额账本负责月总量，它与速度上限分开。没有修改宿主机网卡队列，也没有额外引入 GOST 运行进程。

## 客户端交付

Mihomo 节点配置要求明确的类型、服务器、端口及唯一名称，UDP 支持需要相应字段。[Mihomo 节点通用字段](https://wiki.metacubex.one/en/config/proxies/)

本项目提供完整配置与节点片段两种导出；独立订阅只读取已应用快照，并支持撤销。二维码由浏览器本地生成。兼容测试使用校验过的 [Mihomo v1.19.32](https://github.com/MetaCubeX/mihomo/releases/tag/v1.19.32)，将导出配置用于实际请求，不以 YAML 能解析作为唯一验收条件。

## 安全与运行边界

Passkey 使用锁定版本的 webauthn-rs；该库的 Passkey 注册和认证流程要求用户验证。[Webauthn API](https://docs.rs/webauthn-rs/0.5.5/webauthn_rs/struct.Webauthn.html)

本项目保留单管理员模式，密钥、TOTP 与恢复码完成传统登录；Passkey 是经过设备验证的替代登录方式。域名绑定、短时单次挑战、凭据撤销和服务器本地恢复均有明确操作入口。没有加入套餐、支付、多租户计费或自动故障切换。

网站由 Agent 提供独立 HTTP(S) 监听和 WebSocket 转发，自动证书使用自己的 Certbot webroot 目录。已存在的 Nginx 和业务端口不被接管；已有控制台 HTTPS 占用 80/443 时应选择其他端口、手工证书或另一台入口机器。

完整支持范围、计量口径与操作截图见[使用手册](../README.md)。
