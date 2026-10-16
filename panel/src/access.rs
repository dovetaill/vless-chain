//! Client-facing protocols and exports. Internal hops remain managed automatically.
use crate::{
    compiler::{address, host_valid},
    model::*,
};
use anyhow::{Context, Result, bail, ensure};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE_NO_PAD},
};
use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use url::Url;

fn decode(s: &str) -> Result<String> {
    Ok(percent_decode_str(s).decode_utf8()?.into_owned())
}
pub(crate) fn b64(s: &str) -> Result<String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .or_else(|_| STANDARD.decode(s))
        .or_else(|_| STANDARD_NO_PAD.decode(s.trim_end_matches('=')))?;
    Ok(String::from_utf8(bytes)?)
}
pub fn external_tls(stream: &mut Value, url: &Url) -> Result<()> {
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "allowInsecure" | "insecure" => {
                ensure!(
                    matches!(value.as_ref(), "true" | "false" | "1" | "0"),
                    "allowInsecure 参数无效"
                );
                ensure!(
                    matches!(value.as_ref(), "false" | "0"),
                    "当前 Xray 不支持跳过证书验证，请使用有效证书并设置 skip-cert-verify: false"
                );
            }
            "alpn" => {
                stream["tlsSettings"]["alpn"] = json!(value.split(',').collect::<Vec<_>>());
            }
            "fp" => {
                stream["tlsSettings"]["fingerprint"] = json!(value);
            }
            _ => {}
        }
    }
    Ok(())
}
pub fn external(link: &str, tag: &str) -> Result<Value> {
    if let Some(encoded) = link.strip_prefix("vmess://") {
        let v: Value = serde_json::from_str(&b64(encoded)?)?;
        ensure!(
            v["udp"].is_null() || v["udp"].is_boolean(),
            "VMess udp 参数无效"
        );
        for field in ["net", "type", "scy", "tls", "sni", "fp", "alpn"] {
            ensure!(
                v[field].is_null() || v[field].is_string(),
                "VMess {field} 必须是文本"
            );
        }
        let host = v["add"].as_str().context("VMess 缺少地址")?;
        ensure!(host_valid(host), "VMess 地址无效");
        let port = v["port"]
            .as_u64()
            .or_else(|| v["port"].as_str()?.parse().ok())
            .filter(|p| *p > 0 && *p <= 65535)
            .context("VMess 端口无效")?;
        let uuid = uuid::Uuid::parse_str(v["id"].as_str().unwrap_or_default())?;
        ensure!(
            matches!(v["aid"].as_u64(), Some(0))
                || matches!(v["aid"].as_str(), Some("0"))
                || v["aid"].is_null(),
            "仅支持 VMess AEAD（alterId: 0）"
        );
        ensure!(
            matches!(v["net"].as_str(), None | Some("tcp" | "raw")),
            "VMess 目前支持 TCP 传输"
        );
        ensure!(
            matches!(v["type"].as_str(), None | Some("" | "none")),
            "VMess 不支持此 TCP 伪装"
        );
        let cipher = v["scy"].as_str().unwrap_or("auto");
        ensure!(
            matches!(cipher, "auto" | "aes-128-gcm" | "chacha20-poly1305"),
            "VMess 加密方式无效"
        );
        let tls = v["tls"].as_str().unwrap_or("");
        ensure!(matches!(tls, "" | "none" | "tls"), "VMess TLS 模式无效");
        ensure!(
            matches!(v["allowInsecure"], Value::Null | Value::Bool(false))
                || matches!(v["allowInsecure"].as_str(), Some("false" | "0")),
            "不支持跳过证书验证"
        );
        let mut stream = json!({"network":"tcp","security":if tls == "tls" {"tls"} else {"none"}});
        if tls == "tls" {
            stream["tlsSettings"] = json!({"serverName":v["sni"].as_str().filter(|s|!s.is_empty()).unwrap_or(host),"fingerprint":v["fp"].as_str().unwrap_or("chrome")});
            if let Some(alpn) = v["alpn"].as_str().filter(|s| !s.is_empty()) {
                stream["tlsSettings"]["alpn"] = json!(alpn.split(',').collect::<Vec<_>>());
            }
        }
        ensure!(
            v["tfo"].is_null() || v["tfo"].is_boolean(),
            "VMess tfo 参数无效"
        );
        if let Some(tfo) = v["tfo"].as_bool() {
            stream["sockopt"] = json!({"tcpFastOpen":tfo});
        }
        return Ok(
            json!({"tag":tag,"protocol":"vmess","settings":{"vnext":[{"address":host,"port":port,"users":[{"id":uuid,"security":cipher,"alterId":0}]}]},"streamSettings":stream}),
        );
    }
    let mut u = Url::parse(link).context("请输入完整代理链接")?;
    if u.scheme() == "ss" && u.host_str().is_some() && u.username().is_empty() {
        let payload = link
            .trim_start_matches("ss://")
            .split('#')
            .next()
            .unwrap_or_default();
        u = Url::parse(&format!("ss://{}", b64(payload)?))?;
    }
    let host = u
        .host_str()
        .context("链接缺少地址")?
        .trim_matches(['[', ']']);
    ensure!(host_valid(host), "代理地址无效");
    let port = u
        .port_or_known_default()
        .or_else(|| matches!(u.scheme(), "hysteria2" | "hy2" | "tuic").then_some(443))
        .context("链接缺少端口")?;
    ensure!(port > 0, "端口必须大于 0");
    let user = decode(u.username())?;
    let pass = decode(u.password().unwrap_or_default())?;
    for (key, value) in u.query_pairs().filter(|(key, _)| key == "udp") {
        ensure!(
            matches!(value.as_ref(), "true" | "false" | "1" | "0"),
            "{key} 参数无效"
        );
    }
    let mut result = match u.scheme() {
        "hysteria2" | "hy2" | "tuic" => {
            let q: std::collections::BTreeMap<_, _> = u.query_pairs().into_owned().collect();
            for key in q.keys() {
                ensure!(
                    ["sni", "alpn", "insecure", "allowInsecure", "udp"].contains(&key.as_str())
                        || (u.scheme() == "tuic"
                            && ["congestion_control", "udp_relay_mode"].contains(&key.as_str())),
                    "暂不支持此 QUIC 参数：{key}"
                );
            }
            let sni = q.get("sni").map(String::as_str).unwrap_or(host);
            ensure!(host_valid(sni), "TLS server name 无效");
            let mut stream = json!({"network":"hysteria","security":"tls","tlsSettings":{"serverName":sni,"alpn":["h3"]},"hysteriaSettings":{"version":2,"auth":if u.password().is_some() {format!("{user}:{pass}")} else {user.clone()}}});
            external_tls(&mut stream, &u)?;
            if u.scheme() == "tuic" {
                let uuid = uuid::Uuid::parse_str(&user)?;
                ensure!(!pass.is_empty(), "TUIC v5 密码不能为空");
                let congestion = q
                    .get("congestion_control")
                    .map(String::as_str)
                    .unwrap_or("bbr");
                let relay = q
                    .get("udp_relay_mode")
                    .map(String::as_str)
                    .unwrap_or("native");
                ensure!(
                    matches!(congestion, "bbr" | "cubic" | "new_reno")
                        && matches!(relay, "native" | "quic"),
                    "TUIC 参数无效"
                );
                return Ok(
                    json!({"tag":tag,"protocol":"tuic","settings":{"type":"tuic","server":host,"port":port,"uuid":uuid,"password":pass,"sni":sni,"alpn":stream["tlsSettings"]["alpn"],"skip-cert-verify":false,"udp":!matches!(q.get("udp").map(String::as_str),Some("false" | "0")),"congestion-controller":congestion,"udp-relay-mode":relay}}),
                );
            }
            ensure!(!user.is_empty(), "Hysteria 2 密码不能为空");
            json!({"tag":tag,"protocol":"hysteria","settings":{"version":2,"address":host,"port":port},"streamSettings":stream})
        }
        "http" | "https" | "socks" | "socks5" => {
            let mut server = json!({"address":host,"port":port});
            if !user.is_empty() {
                server["users"] = json!([{"user":user,"pass":pass}]);
            }
            let mut value = json!({"tag":tag,"protocol":if u.scheme().starts_with("http") {"http"} else {"socks"},"settings":{"servers":[server]}});
            if u.scheme() == "https" {
                let sni = u
                    .query_pairs()
                    .find(|(key, _)| key == "sni")
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_else(|| host.into());
                value["streamSettings"] =
                    json!({"network":"tcp","security":"tls","tlsSettings":{"serverName":sni}});
            }
            value
        }
        "trojan" => {
            ensure!(
                !user.is_empty() && u.password().is_none(),
                "Trojan 链接密码无效"
            );
            let q: std::collections::BTreeMap<_, _> = u.query_pairs().into_owned().collect();
            let network = q.get("type").map(String::as_str).unwrap_or("tcp");
            ensure!(
                matches!(network, "tcp" | "raw"),
                "Trojan 导入目前支持 TCP/TLS"
            );
            ensure!(
                q.get("security").is_none_or(|s| s == "tls"),
                "Trojan 导入需要 TLS"
            );
            json!({"tag":tag,"protocol":"trojan","settings":{"servers":[{"address":host,"port":port,"password":user}]},"streamSettings":{"network":"tcp","security":"tls","tlsSettings":{"serverName":q.get("sni").map(String::as_str).unwrap_or(host)}}})
        }
        "ss" => {
            ensure!(
                u.query_pairs()
                    .all(|(key, _)| matches!(key.as_ref(), "tfo" | "udp")),
                "暂不支持 Shadowsocks 插件参数"
            );
            let credential = if u.password().is_some() {
                format!("{user}:{pass}")
            } else {
                b64(&user)?
            };
            let (method, password) = credential
                .split_once(':')
                .context("Shadowsocks 链接缺少加密方式和密码")?;
            ensure!(
                matches!(
                    method,
                    "2022-blake3-aes-128-gcm"
                        | "2022-blake3-aes-256-gcm"
                        | "aes-128-gcm"
                        | "aes-256-gcm"
                        | "chacha20-ietf-poly1305"
                ),
                "不支持此 Shadowsocks 加密方式"
            );
            ensure!(!password.is_empty(), "Shadowsocks 密码不能为空");
            json!({"tag":tag,"protocol":"shadowsocks","settings":{"servers":[{"address":host,"port":port,"method":method,"password":password}]}})
        }
        _ => bail!("不支持此代理协议"),
    };
    if result["streamSettings"]["security"] == "tls" {
        external_tls(&mut result["streamSettings"], &u)?;
    }
    for (key, value) in u.query_pairs() {
        if key == "tfo" {
            ensure!(
                matches!(value.as_ref(), "true" | "false" | "1" | "0"),
                "tfo 参数无效"
            );
            if result.get("streamSettings").is_none() {
                result["streamSettings"] = json!({});
            }
            result["streamSettings"]["sockopt"] =
                json!({"tcpFastOpen":matches!(value.as_ref(), "true" | "1")});
        }
    }
    Ok(result)
}

pub fn validate_line(line: &Line) -> Result<()> {
    ensure!(
        line.access.host.is_empty() || host_valid(&line.access.host),
        "代理访问地址无效"
    );
    for rate in [line.limits.upload_mbps, line.limits.download_mbps] {
        ensure!(
            rate.is_none_or(|r| r.is_finite() && r >= 0.001 && r <= 1_000_000.0),
            "带宽上限须为 0.001–1000000 Mbps；留空表示不限速"
        );
    }
    ensure!(
        line.limits
            .monthly_gib
            .is_none_or(|q| q.is_finite() && q > 0.0 && q <= 1_000_000_000.0),
        "月额度须为正数 GiB；留空表示不限量"
    );
    ensure!(
        line.access.public_host.is_empty() || host_valid(&line.access.public_host),
        "外部入口地址无效"
    );
    ensure!(
        line.access.public_port.is_none_or(|p| p > 0),
        "外部入口端口无效"
    );
    ensure!(
        line.access.public_host.is_empty() == line.access.public_port.is_none(),
        "购买的中转入口需要同时填写地址和端口"
    );
    if line.kind == Kind::Vless {
        let a = &line.access;
        ensure!(
            matches!(a.transport.network.as_str(), "tcp" | "xhttp"),
            "仅支持 TCP / XHTTP 传输"
        );
        ensure!(
            a.transport.network == "tcp" || a.protocol == Protocol::Vless,
            "XHTTP 传输仅用于 VLESS"
        );
        if a.transport.network == "xhttp" {
            ensure!(
                a.transport.path.starts_with('/')
                    && a.transport.path.len() <= 256
                    && !a
                        .transport
                        .path
                        .chars()
                        .any(|c| c.is_control() || matches!(c, '?' | '#')),
                "XHTTP 路径必须以 / 开头，不含查询、片段或控制字符"
            );
            ensure!(
                a.transport.host.is_empty() || host_valid(&a.transport.host),
                "XHTTP Host 无效"
            );
            ensure!(
                matches!(
                    a.transport.mode.as_str(),
                    "auto" | "packet-up" | "stream-up" | "stream-one"
                ),
                "XHTTP 模式无效"
            );
        }
        if let Some(r) = &a.reality {
            crate::compiler::target(&r.target)?;
            ensure!(host_valid(&r.server_name), "REALITY SNI 无效");
            if let Some(keys) = &r.credentials {
                let private: [u8; 32] = URL_SAFE_NO_PAD
                    .decode(&keys.private_key)?
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("REALITY 私钥长度无效"))?;
                let public =
                    x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(private));
                ensure!(
                    URL_SAFE_NO_PAD.encode(public.as_bytes()) == keys.public_key,
                    "REALITY 公私钥不匹配"
                );
                ensure!(
                    keys.short_id.len() <= 16
                        && keys.short_id.len() % 2 == 0
                        && keys.short_id.bytes().all(|c| c.is_ascii_hexdigit()),
                    "REALITY short ID 无效"
                );
            }
        }
        if a.shared_port {
            ensure!(
                a.protocol == Protocol::Vless && line.hops.first().is_some_and(|h| h.port == 443),
                "统一 443 入口仅用于 VLESS，监听端口必须为 443"
            );
        }
        if !a.fallback.is_empty() {
            ensure!(
                a.protocol == Protocol::Vless
                    && a.transport.network == "tcp"
                    && matches!(a.tls.mode.as_str(), "auto" | "manual"),
                "网站回落用于 VLESS TCP + TLS；REALITY 自动回落到目标站点"
            );
            if a.fallback != "builtin" {
                let url = Url::parse(&a.fallback)?;
                ensure!(
                    matches!(url.scheme(), "http" | "https")
                        && url.host_str().is_some()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.query().is_none()
                        && url.fragment().is_none(),
                    "回落网站需为不含认证、查询和片段的 HTTP/HTTPS 地址"
                );
            }
        }
        match line.access.protocol {
            Protocol::Http | Protocol::Socks => {
                ensure!(
                    !line.access.username.is_empty() && !line.access.password.is_empty(),
                    "HTTP/SOCKS5 入口需要用户名和密码"
                );
                ensure!(
                    line.access.username.len() <= 128 && line.access.password.len() <= 128,
                    "用户名和密码最长 128 字节"
                );
            }
            Protocol::Shadowsocks => {
                ensure!(
                    line.access.method == "2022-blake3-aes-128-gcm",
                    "新建 Shadowsocks 使用 SS2022 AES-128-GCM"
                );
                ensure!(
                    STANDARD.decode(&line.access.password)?.len() == 16,
                    "SS2022 密钥须为 16 字节的 Base64"
                );
            }
            Protocol::Trojan | Protocol::Hysteria2 | Protocol::Tuic => {
                ensure!(!line.access.password.is_empty(), "代理密码不能为空");
                ensure!(
                    matches!(line.access.tls.mode.as_str(), "auto" | "manual"),
                    "Trojan / Hysteria 2 / TUIC 需要 TLS 证书"
                );
            }
            _ => {}
        }
    }
    if line.kind == Kind::Website {
        ensure!(
            line.hops.len() == 1 && line.external_id.is_none(),
            "网站反代请选择一台入口服务器"
        );
        let u = Url::parse(&line.target).context("网站后端必须为 HTTP/HTTPS URL")?;
        ensure!(
            matches!(u.scheme(), "http" | "https")
                && u.username().is_empty()
                && u.password().is_none()
                && u.host_str().is_some()
                && u.query().is_none()
                && u.fragment().is_none(),
            "网站后端必须为不含认证、查询和片段的 HTTP/HTTPS 地址"
        );
        ensure!(host_valid(&line.access.tls.domain), "请填写网站访问域名");
    }
    let tls = &line.access.tls;
    ensure!(
        matches!(tls.mode.as_str(), "" | "off" | "auto" | "manual"),
        "TLS 模式无效"
    );
    if line.kind == Kind::Forward {
        ensure!(
            tls.mode.is_empty() || tls.mode == "off",
            "原始转发不应终止 TLS"
        );
    }
    if matches!(tls.mode.as_str(), "manual" | "auto") {
        ensure!(host_valid(&tls.domain), "TLS 域名无效");
        if tls.mode == "auto" {
            ensure!(
                tls.domain.parse::<std::net::IpAddr>().is_err(),
                "自动证书需要公网域名"
            );
        } else {
            crate::website::tls_config(&tls.cert, &tls.key)?;
        }
    }
    Ok(())
}

pub fn fill_credentials(access: &mut Access) {
    if access.username.is_empty() {
        access.username = "chain".into();
    }
    if access.password.is_empty() {
        access.password = if access.protocol == Protocol::Shadowsocks {
            STANDARD.encode(rand::random::<[u8; 16]>())
        } else {
            secret()[..32].into()
        };
    }
    if access.protocol == Protocol::Shadowsocks {
        access.method = "2022-blake3-aes-128-gcm".into();
    }
}

pub fn inbound(line: &Line, hop: &Hop, tag: &str) -> Value {
    let a = &line.access;
    let (protocol, settings) = match a.protocol {
        Protocol::Http => (
            "http",
            json!({"accounts":[{"user":a.username,"pass":a.password}]}),
        ),
        Protocol::Socks => (
            "socks",
            json!({"auth":"password","accounts":[{"user":a.username,"pass":a.password}],"udp":true,"ip":"127.0.0.1"}),
        ),
        Protocol::Shadowsocks => (
            "shadowsocks",
            json!({"method":a.method,"password":a.password,"network":"tcp,udp"}),
        ),
        Protocol::Trojan => ("trojan", json!({"clients":[{"password":a.password}]})),
        Protocol::Vmess => ("vmess", json!({"clients":[{"id":hop.uuid,"alterId":0}]})),
        Protocol::Hysteria2 | Protocol::Tuic => (
            "socks",
            json!({"auth":"noauth","udp":true,"ip":"127.0.0.1"}),
        ),
        Protocol::Vless => unreachable!(),
    };
    json!({"tag":tag,"listen":"0.0.0.0","port":hop.port,"protocol":protocol,"settings":settings})
}

pub fn endpoint(line: &Line, node: &Node) -> (String, u16) {
    (
        if !line.access.host.is_empty() {
            line.access.host.clone()
        } else if line.access.public_host.is_empty() {
            if matches!(line.access.tls.mode.as_str(), "auto" | "manual") {
                line.access.tls.domain.clone()
            } else {
                node.host.clone()
            }
        } else {
            line.access.public_host.clone()
        },
        line.access.public_port.unwrap_or(line.hops[0].port),
    )
}
pub fn share(line: &Line, node: &Node, udp: bool) -> Result<String> {
    let a = &line.access;
    let (host, port) = endpoint(line, node);
    if a.protocol == Protocol::Vmess {
        return vmess_link(
            &json!({"name":line.name,"server":host,"port":port,"uuid":line.hops[0].uuid,"tls":matches!(a.tls.mode.as_str(),"auto"|"manual"),"servername":a.tls.domain,"udp":udp}),
        );
    }
    let scheme = match a.protocol {
        Protocol::Http if matches!(a.tls.mode.as_str(), "auto" | "manual") => "https",
        Protocol::Http => "http",
        Protocol::Socks => "socks5",
        Protocol::Shadowsocks => "ss",
        Protocol::Trojan => "trojan",
        Protocol::Hysteria2 => "hysteria2",
        Protocol::Tuic => "tuic",
        _ => bail!("VLESS 由 REALITY 导出器处理"),
    };
    let mut u = Url::parse(&format!("{scheme}://{}", address(&host, port)))?;
    match a.protocol {
        Protocol::Http | Protocol::Socks => {
            u.set_username(&a.username).ok();
            u.set_password(Some(&a.password)).ok();
            if scheme == "https" {
                u.query_pairs_mut().append_pair("sni", &a.tls.domain);
            }
        }
        Protocol::Trojan => {
            u.set_username(&a.password).ok();
            u.query_pairs_mut().extend_pairs([
                ("security", "tls"),
                ("sni", a.tls.domain.as_str()),
                ("type", "tcp"),
            ]);
        }
        Protocol::Hysteria2 | Protocol::Tuic => {
            u.set_username(if a.protocol == Protocol::Tuic {
                &line.hops[0].uuid
            } else {
                &a.password
            })
            .map_err(|_| anyhow::anyhow!("认证参数无效"))?;
            if a.protocol == Protocol::Tuic {
                u.set_password(Some(&a.password))
                    .map_err(|_| anyhow::anyhow!("密码无效"))?;
                u.query_pairs_mut()
                    .append_pair("congestion_control", "bbr")
                    .append_pair("udp_relay_mode", "native");
            }
            u.query_pairs_mut()
                .append_pair("sni", &a.tls.domain)
                .append_pair("alpn", "h3")
                .append_pair("insecure", "0");
        }
        Protocol::Shadowsocks => {
            u.set_username(&URL_SAFE_NO_PAD.encode(format!("{}:{}", a.method, a.password)))
                .ok();
        }
        _ => {}
    }
    if !udp && a.protocol != Protocol::Http {
        u.query_pairs_mut().append_pair("udp", "false");
    }
    u.set_fragment(Some(&line.name));
    Ok(u.to_string())
}

pub fn vmess_link(node: &Value) -> Result<String> {
    let v = json!({"v":"2","ps":node["name"],"add":node["server"],"port":node["port"],"id":node["uuid"],"aid":node.get("alterId").cloned().unwrap_or(json!(0)),"scy":node["cipher"].as_str().unwrap_or("auto"),"net":node["network"].as_str().unwrap_or("tcp"),"type":"none","tls":if node["tls"] == true {"tls"} else {""},"sni":node["servername"].as_str().unwrap_or(""),"fp":node["client-fingerprint"].as_str().unwrap_or("chrome"),"alpn":node["alpn"].as_array().map(|v|v.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(",")).unwrap_or_default(),"tfo":node.get("tfo"),"udp":node.get("udp"),"allowInsecure":node.get("skip-cert-verify")});
    Ok(format!(
        "vmess://{}",
        STANDARD.encode(serde_json::to_vec(&v)?)
    ))
}

pub fn mihomo_node(line: &Line, doc: &Document) -> Result<Value> {
    ensure!(line.kind == Kind::Vless, "此规则不使用代理客户端配置");
    let node = doc
        .nodes
        .iter()
        .find(|n| n.id == line.hops[0].node_id)
        .context("入口不存在")?;
    let (host, port) = endpoint(line, node);
    let a = &line.access;
    let mut v = json!({"name":line.name,"server":host,"port":port});
    match a.protocol {
        Protocol::Vless => {
            v["type"] = json!("vless");
            v["uuid"] = json!(line.hops[0].uuid);
            if a.transport.network == "tcp" {
                v["flow"] = json!("xtls-rprx-vision");
            }
            v["tls"] = json!(true);
            v["udp"] = json!(true);
            v["network"] = json!(a.transport.network);
            v["packet-encoding"] = json!("xudp");
            if a.transport.network == "xhttp" {
                v["xhttp-opts"] = json!({"path":a.transport.path,"host":a.transport.host,"mode":a.transport.mode});
            }
            v["client-fingerprint"] = json!("chrome");
            if matches!(a.tls.mode.as_str(), "auto" | "manual") {
                v["servername"] = json!(a.tls.domain);
            } else {
                v["servername"] = json!(a.server_name(node));
                let (_, public_key, short_id) = a.reality_credentials(node);
                v["reality-opts"] = json!({"public-key":public_key,"short-id":short_id});
            }
        }
        Protocol::Http | Protocol::Socks => {
            v["type"] = json!(if a.protocol == Protocol::Http {
                "http"
            } else {
                "socks5"
            });
            v["username"] = json!(a.username);
            v["password"] = json!(a.password);
            if a.protocol == Protocol::Socks {
                v["udp"] = json!(true);
            }
            if matches!(a.tls.mode.as_str(), "auto" | "manual") {
                v["tls"] = json!(true);
                v["sni"] = json!(a.tls.domain);
            }
        }
        Protocol::Shadowsocks => {
            v["type"] = json!("ss");
            v["cipher"] = json!(a.method);
            v["password"] = json!(a.password);
            v["udp"] = json!(true);
        }
        Protocol::Trojan => {
            v["type"] = json!("trojan");
            v["password"] = json!(a.password);
            v["sni"] = json!(a.tls.domain);
            v["udp"] = json!(true);
        }
        Protocol::Vmess => {
            v["type"] = json!("vmess");
            v["uuid"] = json!(line.hops[0].uuid);
            v["alterId"] = json!(0);
            v["cipher"] = json!("auto");
            v["network"] = json!("tcp");
            v["udp"] = json!(true);
            v["tls"] = json!(matches!(a.tls.mode.as_str(), "auto" | "manual"));
            if v["tls"] == true {
                v["servername"] = json!(a.tls.domain);
            }
        }
        Protocol::Hysteria2 | Protocol::Tuic => {
            v["type"] = json!(if a.protocol == Protocol::Tuic {
                "tuic"
            } else {
                "hysteria2"
            });
            v["password"] = json!(a.password);
            v["sni"] = json!(a.tls.domain);
            v["alpn"] = json!(["h3"]);
            v["skip-cert-verify"] = json!(false);
            v["udp"] = json!(true);
            if a.protocol == Protocol::Tuic {
                v["uuid"] = json!(line.hops[0].uuid);
                v["congestion-controller"] = json!("bbr");
                v["udp-relay-mode"] = json!("native");
            }
        }
    }
    if v.get("udp").is_some() {
        v["udp"] = json!(crate::compiler::udp_supported(line, doc)?);
    }
    Ok(v)
}
pub fn mihomo(lines: &[Line], doc: &Document) -> Result<String> {
    let mut names = std::collections::BTreeSet::new();
    let proxies: Vec<Value> = lines
        .iter()
        .filter(|l| l.enabled && l.kind == Kind::Vless)
        .map(|l| {
            let mut n = mihomo_node(l, doc)?;
            let name = if names.insert(l.name.clone()) {
                l.name.clone()
            } else {
                format!("{} · {}", l.name, l.id)
            };
            n["name"] = json!(name);
            Ok(n)
        })
        .collect::<Result<_>>()?;
    ensure!(!proxies.is_empty(), "没有已应用且启用的代理线路");
    let choices: Vec<_> = proxies.iter().map(|p| p["name"].clone()).collect();
    Ok(serde_yaml_ng::to_string(
        &json!({"mixed-port":7890,"allow-lan":false,"mode":"rule","log-level":"info","proxies":proxies,"proxy-groups":[{"name":"代理选择","type":"select","proxies":choices}],"rules":["MATCH,代理选择"]}),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vmess_and_quic_validate_credentials_and_transport() {
        let uuid = id();
        let node = json!({"name":"test","server":"proxy.example","port":443,"uuid":uuid,"tls":true,"servername":"origin.example"});
        let link = vmess_link(&node).unwrap();
        let out = external(&link, "test").unwrap();
        assert_eq!(out["protocol"], "vmess");
        assert_eq!(out["streamSettings"]["security"], "tls");
        for (key, value) in [
            ("aid", json!(1)),
            ("net", json!("ws")),
            ("tls", json!(true)),
            ("allowInsecure", json!(1)),
            ("allowInsecure", json!(true)),
            ("port", json!(65536)),
        ] {
            let mut data: Value =
                serde_json::from_str(&b64(link.strip_prefix("vmess://").unwrap()).unwrap())
                    .unwrap();
            data[key] = value;
            assert!(
                external(
                    &format!("vmess://{}", STANDARD.encode(data.to_string())),
                    "test"
                )
                .is_err(),
                "{key}"
            );
        }
        for scheme in ["hy2", "hysteria2", "tuic"] {
            let auth = if scheme == "tuic" {
                format!("{uuid}:password")
            } else {
                "password".into()
            };
            let link = format!("{scheme}://{auth}@proxy.example:443?sni=origin.example");
            assert!(external(&link, "test").is_ok());
            assert!(external(&format!("{link}&insecure=1"), "test").is_err());
            assert!(external(&format!("{link}&obfs=unknown"), "test").is_err());
        }
    }
    #[test]
    fn supported_external_protocols_parse_and_plugins_are_rejected() {
        assert_eq!(
            external("socks5://user:pass@proxy.example.com:1080", "a").unwrap()["protocol"],
            "socks"
        );
        assert_eq!(
            external("https://user:pass@proxy.example.com:443", "a").unwrap()["streamSettings"]["security"],
            "tls"
        );
        assert_eq!(
            external(
                "trojan://secret@proxy.example.com:443?sni=origin.example.com",
                "a"
            )
            .unwrap()["protocol"],
            "trojan"
        );
        let auth = URL_SAFE_NO_PAD.encode("aes-128-gcm:secret");
        assert!(external(&format!("ss://{auth}@proxy.example.com:8388"), "a").is_ok());
        assert!(external(&format!("ss://{auth}@proxy.example.com:8388?plugin=x"), "a").is_err());
    }
}
