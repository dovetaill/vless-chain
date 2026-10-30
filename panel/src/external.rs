//! Import one proxy in share-link or Mihomo format, using the same outbound validation.
use crate::compiler::{address, external_outbound, host_valid};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use url::Url;
mod native;

fn text<'a>(node: &'a Value, key: &str, fallback: &'a str) -> Result<&'a str> {
    match node.get(key) {
        None | Some(Value::Null) => Ok(fallback),
        Some(Value::String(value)) => Ok(value),
        _ => bail!("{key} 必须是文本"),
    }
}
fn flag(node: &Value, key: &str, fallback: bool) -> Result<bool> {
    match node.get(key) {
        None | Some(Value::Null) => Ok(fallback),
        Some(Value::Bool(value)) => Ok(*value),
        _ => bail!("{key} 必须是 true 或 false"),
    }
}

pub fn import(input: &str) -> Result<(String, String)> {
    let input = input.trim();
    if input.starts_with("mihomo://") {
        let node = native::decode(input)?;
        return Ok((native::encode(&node)?, text(&node,"name",text(&node,"server","代理")?)?.into()));
    }
    if let Some(node) = native::share_node(input)? {
        return Ok((native::encode(&node)?,text(&node,"name",text(&node,"server","代理")?)?.into()));
    }
    if [
        "vless://",
        "vmess://",
        "hysteria2://",
        "hy2://",
        "tuic://",
        "ss://",
        "trojan://",
        "http://",
        "https://",
        "socks://",
        "socks5://",
    ]
    .iter()
    .any(|p| input.starts_with(p))
    {
        external_outbound(input, "check")?;
        if input.starts_with("vmess://") {
            let encoded = input.trim_start_matches("vmess://").trim_end_matches('=');
            let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(encoded)
                .or_else(|_| URL_SAFE_NO_PAD.decode(encoded))?;
            let data: Value = serde_json::from_slice(&bytes)?;
            return Ok((input.into(), data["ps"].as_str().unwrap_or_default().into()));
        }
        let url = Url::parse(input)?;
        let name = percent_decode_str(url.fragment().unwrap_or(""))
            .decode_utf8()?
            .into_owned();
        return Ok((input.into(), name));
    }
    let mut node: Value =
        serde_yaml_ng::from_str(input).context("请输入分享链接或单个 Mihomo YAML/JSON 节点")?;
    if let Some(proxies) = node.get_mut("proxies") {
        node = proxies.take();
    }
    if let Some(nodes) = node.as_array_mut() {
        ensure!(nodes.len() == 1, "一次导入一个代理，请粘贴单个 Mihomo 节点");
        node = nodes.remove(0);
    }
    let fields = node.as_object().context("Mihomo 节点必须是对象")?;
    let kind = text(&node, "type", "")?;
    ensure!(
        matches!(
            kind,
            "vless" | "trojan" | "ss" | "ssr" | "snell" | "wireguard" | "shadowquic"
                | "http" | "socks5" | "vmess" | "hysteria" | "hysteria2" | "tuic" | "anytls"
        ),
        "不支持此代理协议"
    );
    // Preserve the complete Mihomo node so plugin and transport fields reach the sidecar intact.
    if matches!(kind, "ssr" | "snell" | "wireguard" | "shadowquic" | "anytls" | "hysteria") {
        return Ok((native::encode(&node)?, text(&node, "name", text(&node,"server","代理")?)?.into()));
    }
    if node.get("plugin").is_some() || node.get("plugin-opts").is_some()
        || matches!(text(&node,"network","tcp")?, "ws" | "grpc" | "h2" | "http")
        || kind == "hysteria2" && fields.keys().any(|k| ["obfs","ports","hop-interval","up","down","udp-mtu","handshake-timeout"].contains(&k.as_str()))
        || kind == "socks5" && flag(&node,"tls",false)? {
        return Ok((native::encode(&node)?,text(&node,"name",text(&node,"server","代理")?)?.into()));
    }
    // Reject unsupported settings instead of silently changing how the proxy connects.
    let common = ["name", "type", "server", "port", "udp", "tfo", "ip-version"];
    let specific: &[&str] = match kind {
        "vless" => &[
            "uuid",
            "tls",
            "flow",
            "network",
            "servername",
            "sni",
            "client-fingerprint",
            "skip-cert-verify",
            "reality-opts",
            "alpn",
            "xhttp-opts",
            "encryption",
            "packet-encoding",
        ],
        "trojan" => &[
            "password",
            "network",
            "tls",
            "sni",
            "servername",
            "client-fingerprint",
            "skip-cert-verify",
            "alpn",
        ],
        "ss" => &["cipher", "password"],
        "vmess" => &[
            "uuid",
            "alterId",
            "cipher",
            "network",
            "tls",
            "servername",
            "client-fingerprint",
            "skip-cert-verify",
            "alpn",
        ],
        "hysteria2" => &["password", "sni", "skip-cert-verify", "alpn"],
        "tuic" => &[
            "uuid",
            "password",
            "sni",
            "skip-cert-verify",
            "alpn",
            "congestion-controller",
            "udp-relay-mode",
        ],
        _ => &[
            "username",
            "password",
            "tls",
            "sni",
            "client-fingerprint",
            "skip-cert-verify",
            "alpn",
        ],
    };
    for key in fields.keys() {
        ensure!(
            common.contains(&key.as_str()) || specific.contains(&key.as_str()),
            "暂不能转换 Mihomo 参数：{key}"
        );
    }
    ensure!(
        text(&node, "ip-version", "dual")? == "dual",
        "暂不支持指定 ip-version"
    );
    ensure!(
        text(&node, "encryption", "none")? == "none",
        "仅支持 encryption: none"
    );
    ensure!(
        text(&node, "packet-encoding", "xudp")? == "xudp",
        "仅支持 packet-encoding: xudp"
    );
    let host = text(&node, "server", "")?;
    ensure!(host_valid(host), "代理地址无效");
    let port = node["port"]
        .as_u64()
        .filter(|p| *p > 0 && *p <= 65535)
        .context("port 必须是 1–65535 的整数")? as u16;
    if kind == "vmess" {
        flag(&node, "tls", false)?;
        for field in [
            "uuid",
            "cipher",
            "network",
            "servername",
            "client-fingerprint",
        ] {
            text(&node, field, "")?;
        }
        flag(&node, "tfo", false)?;
        flag(&node, "udp", true)?;
        ensure!(
            !flag(&node, "skip-cert-verify", false)?,
            "不支持跳过证书验证"
        );
        if let Some(alpn) = node.get("alpn") {
            ensure!(
                alpn.as_array()
                    .is_some_and(|v| v.iter().all(Value::is_string)),
                "alpn 必须是文本数组"
            );
        }
        let link = crate::access::vmess_link(&node)?;
        external_outbound(&link, "check")?;
        return Ok((link, text(&node, "name", "")?.into()));
    }
    let tls = flag(
        &node,
        "tls",
        matches!(kind, "trojan" | "hysteria2" | "tuic"),
    )?;
    ensure!(kind != "socks5" || !tls, "SOCKS5 导入暂不支持 TLS");
    let scheme = if kind == "http" && tls { "https" } else { kind };
    let mut url = Url::parse(&format!("{scheme}://{}", address(host, port)))?;
    let user = match kind {
        "vless" => text(&node, "uuid", "")?.to_owned(),
        "trojan" | "hysteria2" => text(&node, "password", "")?.to_owned(),
        "tuic" => text(&node, "uuid", "")?.to_owned(),
        "ss" => URL_SAFE_NO_PAD.encode(format!(
            "{}:{}",
            text(&node, "cipher", "")?,
            text(&node, "password", "")?
        )),
        _ => text(&node, "username", "")?.to_owned(),
    };
    url.set_username(&user)
        .map_err(|_| anyhow::anyhow!("代理账号无效"))?;
    if matches!(kind, "http" | "socks5" | "tuic") && !user.is_empty() {
        url.set_password(Some(text(&node, "password", "")?))
            .map_err(|_| anyhow::anyhow!("代理密码无效"))?;
    }
    let reality = node.get("reality-opts").filter(|v| !v.is_null());
    if kind == "vless" {
        ensure!(tls, "VLESS 落地需要 tls: true");
        url.query_pairs_mut()
            .append_pair("encryption", "none")
            .append_pair(
                "security",
                if reality.is_some() { "reality" } else { "tls" },
            )
            .append_pair("flow", text(&node, "flow", "")?);
    }
    if matches!(kind, "vless" | "trojan") {
        url.query_pairs_mut()
            .append_pair("type", text(&node, "network", "tcp")?);
    }
    if tls {
        url.query_pairs_mut()
            .append_pair("sni", text(&node, "servername", text(&node, "sni", host)?)?)
            .append_pair(
                "allowInsecure",
                if flag(&node, "skip-cert-verify", false)? {
                    "true"
                } else {
                    "false"
                },
            );
        if !matches!(kind, "hysteria2" | "tuic") {
            url.query_pairs_mut()
                .append_pair("fp", text(&node, "client-fingerprint", "chrome")?);
        }
        if let Some(alpn) = node.get("alpn") {
            let values = alpn
                .as_array()
                .context("alpn 必须是文本数组")?
                .iter()
                .map(|v| v.as_str().context("alpn 必须是文本数组"))
                .collect::<Result<Vec<_>>>()?;
            url.query_pairs_mut().append_pair("alpn", &values.join(","));
        }
    }
    if kind == "tuic" {
        url.query_pairs_mut()
            .append_pair(
                "congestion_control",
                text(&node, "congestion-controller", "bbr")?,
            )
            .append_pair("udp_relay_mode", text(&node, "udp-relay-mode", "native")?);
    }
    if let Some(reality) = reality {
        for key in reality
            .as_object()
            .context("reality-opts 必须是对象")?
            .keys()
        {
            ensure!(
                ["public-key", "short-id", "mldsa65-verify"].contains(&key.as_str()),
                "暂不能转换 reality-opts 参数：{key}"
            );
        }
        url.query_pairs_mut()
            .append_pair("pbk", text(reality, "public-key", "")?)
            .append_pair("sid", text(reality, "short-id", "")?)
            .append_pair("pqv", text(reality, "mldsa65-verify", "")?);
    }
    if let Some(xhttp) = node.get("xhttp-opts") {
        for key in xhttp.as_object().context("xhttp-opts 必须是对象")?.keys() {
            ensure!(
                ["path", "host", "mode", "extra"].contains(&key.as_str()),
                "暂不能转换 xhttp-opts 参数：{key}"
            );
        }
        ensure!(
            text(&node, "network", "tcp")? == "xhttp",
            "xhttp-opts 需要 network: xhttp"
        );
        for (key, default) in [("path", "/"), ("host", ""), ("mode", "auto")] {
            url.query_pairs_mut()
                .append_pair(key, text(xhttp, key, default)?);
        }
        if let Some(extra) = xhttp.get("extra") {
            ensure!(extra.is_object(), "XHTTP extra 必须是对象");
            url.query_pairs_mut()
                .append_pair("extra", &serde_json::to_string(extra)?);
        }
    }
    for key in ["tfo", "udp"] {
        if node.get(key).is_some() {
            if matches!(kind, "hysteria2" | "tuic") {
                ensure!(key == "udp", "QUIC 节点不支持此参数：{key}");
            }
            url.query_pairs_mut().append_pair(
                key,
                if flag(&node, key, false)? {
                    "true"
                } else {
                    "false"
                },
            );
        }
    }
    let name = text(&node, "name", "")?.to_owned();
    url.set_fragment(Some(&name));
    external_outbound(url.as_str(), "check")?;
    Ok((url.into(), name))
}

pub fn mihomo(link: &str, name: &str) -> Result<Value> {
    if let Some(mut node) = native::share_node(link)? {
        node["name"] = json!(name);
        return Ok(node);
    }
    if link.starts_with("mihomo://") {
        let mut node = native::decode(link)?;
        node["name"] = json!(name);
        return Ok(node);
    }
    let out = external_outbound(link, "export")?;
    let q: std::collections::BTreeMap<String, String> = if link.starts_with("vmess://") {
        let value: Value =
            serde_json::from_str(&crate::access::b64(link.trim_start_matches("vmess://"))?)?;
        value["udp"]
            .as_bool()
            .map(|udp| [("udp".into(), udp.to_string())].into_iter().collect())
            .unwrap_or_default()
    } else {
        Url::parse(link)?.query_pairs().into_owned().collect()
    };
    let protocol = out["protocol"].as_str().context("代理协议无效")?;
    if protocol == "tuic" {
        let mut node = out["settings"].clone();
        node["name"] = json!(name);
        return Ok(node);
    }
    let server = if matches!(protocol, "vless" | "vmess") {
        &out["settings"]["vnext"][0]
    } else if protocol == "hysteria" {
        &out["settings"]
    } else {
        &out["settings"]["servers"][0]
    };
    let mut node = json!({"name":name,"type":match protocol {"shadowsocks"=>"ss","socks"=>"socks5","hysteria"=>"hysteria2",other=>other},"server":server["address"],"port":server["port"]});
    match protocol {
        "vless" => {
            node["uuid"] = server["users"][0]["id"].clone();
            node["flow"] = server["users"][0]["flow"].clone();
            node["network"] = out["streamSettings"]["network"].clone();
        }
        "vmess" => {
            node["uuid"] = server["users"][0]["id"].clone();
            node["alterId"] = json!(0);
            node["cipher"] = server["users"][0]["security"].clone();
            node["network"] = json!("tcp");
            node["tls"] = json!(out["streamSettings"]["security"] == "tls");
            node["udp"] = json!(true);
            if let Some(tfo) = out["streamSettings"]["sockopt"].get("tcpFastOpen") {
                node["tfo"] = tfo.clone();
            }
        }
        "hysteria" => {
            node["password"] = out["streamSettings"]["hysteriaSettings"]["auth"].clone();
            node["udp"] = json!(true);
        }
        "shadowsocks" => {
            node["cipher"] = server["method"].clone();
            node["password"] = server["password"].clone();
        }
        "trojan" => {
            node["password"] = server["password"].clone();
        }
        _ => {
            if let Some(user) = server["users"].as_array().and_then(|v| v.first()) {
                node["username"] = user["user"].clone();
                node["password"] = user["pass"].clone();
            }
        }
    }
    let stream = &out["streamSettings"];
    if matches!(stream["security"].as_str(), Some("tls" | "reality")) {
        if protocol != "hysteria" {
            node["tls"] = json!(true);
        }
        let settings = if stream["security"] == "reality" {
            &stream["realitySettings"]
        } else {
            &stream["tlsSettings"]
        };
        node[if matches!(protocol, "vless" | "vmess") {
            "servername"
        } else {
            "sni"
        }] = settings["serverName"].clone();
        if let Some(fp) = settings.get("fingerprint") {
            node["client-fingerprint"] = fp.clone();
        }
        if let Some(alpn) = settings.get("alpn") {
            node["alpn"] = alpn.clone();
        }
        node["skip-cert-verify"] = settings
            .get("allowInsecure")
            .cloned()
            .unwrap_or(json!(false));
        if stream["security"] == "reality" {
            node["reality-opts"] =
                json!({"public-key":settings["publicKey"],"short-id":settings["shortId"]});
            if let Some(pqv) = settings.get("mldsa65Verify") {
                node["reality-opts"]["mldsa65-verify"] = pqv.clone();
            }
        }
    }
    if let Some(xhttp) = stream.get("xhttpSettings") {
        node["xhttp-opts"] = xhttp.clone();
    }
    for key in ["tfo", "udp"] {
        if let Some(value) = q.get(key) {
            node[key] = json!(value == "true" || value == "1");
        }
    }
    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mihomo_vless_import_and_export_preserve_connection_settings() {
        let input = "- {name: landing-a, server: a.example.com, port: 8443, type: vless, uuid: 00000000-0000-4000-8000-000000000001, tls: true, tfo: false, flow: xtls-rprx-vision, skip-cert-verify: false, servername: a.example.com, client-fingerprint: chrome, network: tcp}";
        let (link, name) = import(input).unwrap();
        let node = mihomo(&link, &name).unwrap();
        assert_eq!(node, serde_yaml_ng::from_str::<Value>(input).unwrap()[0]);
        let (again, _) = import(&format!("proxies:\n  - {}", node)).unwrap();
        assert_eq!(
            external_outbound(&again, "x").unwrap(),
            external_outbound(&link, "x").unwrap()
        );
        assert!(import(&format!("{input}\n{input}")).is_err());
        assert!(import(&input.replace("network: tcp", "network: invalid")).is_err());
        assert!(import(&input.replace("tls: true", "tls: true, dialer-proxy: hidden")).is_err());
        assert!(import(&input.replace("port: 8443", "port: 0")).is_err());
        assert!(
            import(&input.replace("skip-cert-verify: false", "skip-cert-verify: true")).is_err()
        );
    }
    #[test]
    fn other_protocols_and_security_flags_round_trip() {
        for input in [
            "{name: ss, type: ss, server: proxy.example, port: 8388, cipher: aes-128-gcm, password: secret, tfo: false}",
            "{name: socks, type: socks5, server: proxy.example, port: 1080, username: user, password: secret}",
            "{name: http, type: http, server: proxy.example, port: 8080, username: user, password: secret}",
            "{name: trojan, type: trojan, server: proxy.example, port: 443, password: secret, skip-cert-verify: false, alpn: [h2]}",
            "{name: vmess, type: vmess, server: proxy.example, port: 443, uuid: 00000000-0000-4000-8000-000000000001, alterId: 0, cipher: auto, tls: true, servername: origin.example, alpn: [h2], tfo: false}",
            "{name: hy2, type: hysteria2, server: proxy.example, port: 443, password: 'secret:@?#', sni: origin.example, alpn: [h3]}",
            "{name: tuic, type: tuic, server: proxy.example, port: 443, uuid: 00000000-0000-4000-8000-000000000001, password: 'secret:@?#', sni: origin.example, alpn: [h3]}",
        ] {
            let (link, name) = import(input).unwrap();
            let node = mihomo(&link, &name).unwrap();
            let (again, _) = import(&node.to_string()).unwrap();
            assert_eq!(
                external_outbound(&again, "x").unwrap(),
                external_outbound(&link, "x").unwrap()
            );
            let mut disabled = node;
            disabled["udp"] = json!(false);
            let (link, _) = import(&disabled.to_string()).unwrap();
            assert_eq!(mihomo(&link, "test").unwrap()["udp"], false);
        }
    }

    #[test]
    fn mihomo_only_protocols_round_trip_without_dropping_fields() {
        let input = "{name: ssr, type: ssr, server: proxy.example, port: 443, cipher: aes-128-cfb, password: secret, obfs: tls1.2_ticket_auth, obfs-param: example.com, protocol: origin, protocol-param: user, udp: true}";
        let (link, name) = import(input).unwrap();
        assert!(link.starts_with("mihomo://"));
        let node = mihomo(&link, &name).unwrap();
        assert_eq!(node["type"], "ssr");
        assert_eq!(node["obfs-param"], "example.com");
        assert_eq!(node["protocol-param"], "user");
    }
}

pub fn native_outbound(link: &str, tag: &str) -> Result<Value> {
    let node = native::decode(link)?;
    Ok(json!({"tag":tag,"protocol":"mihomo","settings":node}))
}

pub fn extended_outbound(link: &str, tag: &str) -> Result<Option<Value>> {
    Ok(native::share_node(link)?.map(|node| json!({"tag":tag,"protocol":"mihomo","settings":node})))
}
