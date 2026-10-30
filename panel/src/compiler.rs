use crate::model::*;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

pub fn host_valid(host: &str) -> bool {
    !host.is_empty()
        && !host
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '/' | '@' | '?' | '#' | '\\'))
        && (host.parse::<std::net::IpAddr>().is_ok()
            || (host.len() <= 253
                && host.split('.').all(|s| {
                    !s.is_empty()
                        && s.len() <= 63
                        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                })))
}
pub fn target(value: &str) -> Result<(String, u16)> {
    let u = Url::parse(&format!("tcp://{value}"))?;
    ensure!(
        u.username().is_empty()
            && u.password().is_none()
            && u.path().is_empty()
            && u.query().is_none()
            && u.fragment().is_none(),
        "目标必须为 host:port"
    );
    let host = u
        .host_str()
        .context("目标缺少地址")?
        .trim_matches(['[', ']'])
        .to_string();
    ensure!(host_valid(&host), "目标地址无效");
    let port = u.port().context("目标缺少端口")?;
    ensure!(port > 0, "端口必须大于 0");
    Ok((host, port))
}
pub fn address(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}
pub fn external_outbound(link: &str, tag: &str) -> Result<Value> {
    if link.starts_with("mihomo://") {
        return crate::external::native_outbound(link, tag);
    }
    if let Some(outbound) = crate::external::extended_outbound(link, tag)? { return Ok(outbound); }
    if !link.starts_with("vless://") {
        return crate::access::external(link, tag);
    }
    let u = Url::parse(link).context("VLESS 链接格式错误")?;
    ensure!(
        u.scheme() == "vless" && u.password().is_none(),
        "仅支持 vless:// 链接"
    );
    let uuid = uuid::Uuid::parse_str(u.username())
        .context("VLESS UUID 无效")?
        .to_string();
    let host = u
        .host_str()
        .context("链接缺少地址")?
        .trim_matches(['[', ']']);
    ensure!(host_valid(host), "链接地址无效");
    let port = u.port().context("链接缺少端口")?;
    ensure!(port > 0, "链接端口无效");
    let q: BTreeMap<String, String> = u.query_pairs().into_owned().collect();
    if let Some(udp) = q.get("udp") {
        ensure!(
            matches!(udp.as_str(), "true" | "false" | "1" | "0"),
            "udp 参数无效"
        );
    }
    let get = |key: &str, default: &str| q.get(key).cloned().unwrap_or_else(|| default.to_string());
    ensure!(
        get("encryption", "none") == "none",
        "仅支持 encryption=none"
    );
    let network = get("type", "tcp");
    ensure!(
        matches!(network.as_str(), "tcp" | "raw" | "xhttp"),
        "首版外部落地支持 TCP/raw 或 XHTTP"
    );
    let security = get("security", "");
    let mut stream = json!({"network": network, "security": security});
    match security.as_str() {
        "reality" => {
            let key = get("pbk", "");
            ensure!(
                !key.is_empty() && !get("sni", "").is_empty(),
                "REALITY 链接缺少 pbk 或 sni"
            );
            stream["realitySettings"] = json!({"serverName": get("sni", ""), "publicKey": key,
                "shortId": get("sid", ""), "fingerprint": get("fp", "chrome"), "spiderX": get("spx", "/")});
            if !get("pqv", "").is_empty() {
                stream["realitySettings"]["mldsa65Verify"] = json!(get("pqv", ""));
            }
        }
        "tls" => {
            stream["tlsSettings"] =
                json!({"serverName": get("sni", host), "fingerprint": get("fp", "chrome")});
            crate::access::external_tls(&mut stream, &u)?;
        }
        _ => bail!("外部落地必须使用 TLS 或 REALITY"),
    }
    if network == "xhttp" {
        stream["xhttpSettings"] =
            json!({"path": get("path", "/"), "host": get("host", ""), "mode": get("mode", "auto")});
        if let Some(extra) = q.get("extra") {
            let extra: Value =
                serde_json::from_str(extra).context("XHTTP extra 必须是 JSON 对象")?;
            ensure!(extra.is_object(), "XHTTP extra 必须是 JSON 对象");
            stream["xhttpSettings"]["extra"] = extra;
        }
    }
    if let Some(tfo) = q.get("tfo") {
        ensure!(
            matches!(tfo.as_str(), "true" | "false" | "1" | "0"),
            "tfo 参数无效"
        );
        stream["sockopt"] = json!({"tcpFastOpen": matches!(tfo.as_str(), "true" | "1")});
    }
    let flow = get("flow", "");
    ensure!(
        flow.is_empty() || flow == "xtls-rprx-vision",
        "不支持此 VLESS flow"
    );
    ensure!(
        network != "xhttp" || flow.is_empty(),
        "XHTTP 不应设置 Vision flow"
    );
    Ok(
        json!({"tag":tag,"protocol":"vless","settings":{"vnext":[{"address":host,"port":port,
        "users":[{"id":uuid,"encryption":"none","flow":flow}]}]},"streamSettings":stream}),
    )
}
pub fn share(line: &Line, doc: &Document) -> Result<String> {
    ensure!(
        line.kind == Kind::Vless,
        "端口转发使用入口地址，无 VLESS 链接"
    );
    let h = line.hops.first().context("线路没有入口")?;
    let node = doc
        .nodes
        .iter()
        .find(|n| n.id == h.node_id)
        .context("入口节点不存在")?;
    if line.access.protocol != Protocol::Vless {
        return crate::access::share(line, node, udp_supported(line, doc)?);
    }
    let host = if !line.access.host.is_empty() {
        &line.access.host
    } else if line.access.public_host.is_empty() {
        if matches!(line.access.tls.mode.as_str(), "auto" | "manual") {
            &line.access.tls.domain
        } else {
            &node.host
        }
    } else {
        &line.access.public_host
    };
    let mut u = Url::parse(&format!(
        "vless://{}@{}",
        h.uuid,
        address(host, line.access.public_port.unwrap_or(h.port))
    ))?;
    let tls = matches!(line.access.tls.mode.as_str(), "auto" | "manual");
    u.query_pairs_mut().extend_pairs([
        ("type", line.access.transport.network.as_str()),
        ("security", if tls { "tls" } else { "reality" }),
        ("encryption", "none"),
        (
            "sni",
            if tls {
                &line.access.tls.domain
            } else {
                line.access.server_name(node)
            },
        ),
        ("fp", "chrome"),
    ]);
    if line.access.transport.network == "tcp" {
        u.query_pairs_mut().append_pair("flow", "xtls-rprx-vision");
    } else {
        u.query_pairs_mut().extend_pairs([
            ("path", line.access.transport.path.as_str()),
            ("host", line.access.transport.host.as_str()),
            ("mode", line.access.transport.mode.as_str()),
        ]);
    }
    if !tls {
        let (_, public_key, short_id) = line.access.reality_credentials(node);
        u.query_pairs_mut()
            .extend_pairs([("pbk", public_key), ("sid", short_id)]);
    }
    if !udp_supported(line, doc)? {
        u.query_pairs_mut().append_pair("udp", "false");
    }
    u.set_fragment(Some(&line.name));
    Ok(u.to_string())
}
pub fn empty_config() -> Value {
    json!({"log":{"loglevel":log_level()}, "inbounds":[], "outbounds":[{"tag":"blocked","protocol":"blackhole"}], "routing":{"rules":[]}})
}
fn vless_in(node: &Node, hop: &Hop, tag: &str) -> Value {
    json!({"tag":tag,"listen":"0.0.0.0","port":hop.port,"protocol":"vless", "settings":{"clients":[{"id":hop.uuid,"flow":"xtls-rprx-vision"}],"decryption":"none"},
        "streamSettings":{"network":"tcp","security":"reality","realitySettings":{"show":false,"target":node.reality.target,
            "serverNames":[node.reality.server_name],"privateKey":node.reality.private_key,"shortIds":[node.reality.short_id]}}})
}
pub fn guard_reality(config: &mut Value, inbound: &mut Value, port: u16) -> Result<()> {
    let reality = &mut inbound["streamSettings"]["realitySettings"];
    let destination = reality["target"].as_str().context("缺少 REALITY target")?.to_string();
    target(&destination)?;
    let names = reality["serverNames"].as_array().context("缺少 REALITY SNI")?;
    let domains: Vec<_> = names.iter().filter_map(Value::as_str).map(|name| format!("full:{name}")).collect();
    ensure!(!domains.is_empty(), "缺少 REALITY SNI");
    reality["target"] = json!(format!("127.0.0.1:{port}"));
    let tag = format!("{}-reality-target", inbound["tag"].as_str().context("缺少入站 tag")?);
    // A domain destination would match the allow rule even when sniffing finds no SNI.
    config["inbounds"].as_array_mut().unwrap().push(json!({
        "tag":tag,"listen":"127.0.0.1","port":port,"protocol":"dokodemo-door",
        "settings":{"address":"127.0.0.1","port":9,"network":"tcp"},
        "sniffing":{"enabled":true,"destOverride":["tls"],"routeOnly":true}
    }));
    config["outbounds"].as_array_mut().unwrap().push(json!({
        "tag":tag,"protocol":"freedom","settings":{"redirect":destination}
    }));
    config["routing"]["rules"].as_array_mut().unwrap().extend([
        json!({"type":"field","inboundTag":[tag],"domain":domains,"outboundTag":tag}),
        json!({"type":"field","inboundTag":[tag],"outboundTag":"blocked"}),
    ]);
    Ok(())
}
fn vless_out(node: &Node, hop: &Hop, tag: &str) -> Value {
    json!({"tag":tag,"protocol":"vless","settings":{"vnext":[{"address":node.host,"port":hop.port,
        "users":[{"id":hop.uuid,"encryption":"none","flow":"xtls-rprx-vision"}]}]},
        "streamSettings":{"network":"tcp","security":"reality","realitySettings":{"serverName":node.reality.server_name,
            "publicKey":node.reality.public_key,"shortId":node.reality.short_id,"fingerprint":"chrome"}}})
}
fn backend_port(reserved: &mut BTreeSet<(String, u16)>, node: &Node) -> Result<u16> {
    let range = &node.report.system["ephemeral_port_range"];
    let ephemeral = range[0]
        .as_u64()
        .zip(range[1].as_u64())
        .filter(|(low, high)| *low > 0 && low <= high && *high <= 65535)
        .unwrap_or((32768, 60999));
    // macOS reserves 49152–65535 for outgoing connections; keep Linux's existing choices first.
    let port = (50000..65535).chain(30000..50000)
        .find(|p| {
            !(ephemeral.0..=ephemeral.1).contains(&u64::from(*p))
                && !reserved.iter().any(|(_, used)| used == p)
        })
        .context("没有可供内部接入使用的端口：30000–65534 必须避开已用端口及系统临时端口范围")?;
    reserved.insert((node.id.clone(), port));
    Ok(port)
}
fn mihomo_config(config: &mut Value) -> &mut Value {
    config.as_object_mut().unwrap().entry("_mihomo").or_insert_with(|| json!({"mode":"rule","log-level":"warning","listeners":[],"proxies":[],"rules":["MATCH,REJECT"]}))
}
pub fn needs_mihomo(outbound: &Value) -> bool {
    // Keep SS handshakes, QUIC sessions and nested HTTPS TLS in the same core.
    matches!(
        outbound["protocol"].as_str(),
        Some("tuic" | "hysteria" | "shadowsocks" | "mihomo")
    ) || outbound["protocol"] == "http" && outbound["streamSettings"]["security"] == "tls"
}
fn udp_after(udp: bool, protocol: &str, supported: bool, index: usize) -> Result<bool> {
    if matches!(protocol, "hysteria2" | "tuic" | "hysteria" | "wireguard" | "shadowquic") {
        ensure!(
            udp,
            "第 {} 跳 {} 需要 UDP，前面的代理路径仅支持 TCP；请调整链式顺序",
            index + 1,
            protocol
        );
    }
    Ok(supported
        && match protocol {
            "http" => false,
            "socks" | "socks5" | "shadowsocks" | "ss" => udp,
            _ => true,
        })
}
pub fn udp_supported(line: &Line, doc: &Document) -> Result<bool> {
    fn visit(line: &Line, doc: &Document, depth: usize) -> Result<bool> {
        ensure!(depth < 16, "链式代理不能循环引用或超过 16 层");
        let mut udp = true;
        for (index, reference) in line.proxy_chain().iter().enumerate() {
            let (protocol, supported) = if let Some(id) = reference.strip_prefix("line:") {
                let next = doc
                    .lines
                    .iter()
                    .find(|l| l.id == id)
                    .context("引用的代理不存在")?;
                (
                    serde_json::to_value(&next.access.protocol)?
                        .as_str()
                        .unwrap()
                        .to_owned(),
                    visit(next, doc, depth + 1)?,
                )
            } else {
                let proxy = doc
                    .externals
                    .iter()
                    .find(|e| Some(e.id.as_str()) == reference.strip_prefix("external:"))
                    .context("外部代理不存在")?;
                let proxy = crate::external::mihomo(&proxy.url, "capability")?;
                (
                    proxy["type"].as_str().unwrap().to_owned(),
                    proxy["udp"].as_bool().unwrap_or(true),
                )
            };
            udp = udp_after(udp, &protocol, supported, index)?;
        }
        Ok(udp && line.access.protocol != Protocol::Http)
    }
    visit(line, doc, 0)
}
pub fn compile(doc: &Document) -> Result<BTreeMap<String, Value>> {
    fn check_chain(line: &Line, doc: &Document, visited: &mut BTreeSet<String>) -> Result<()> {
        ensure!(
            visited.len() < 16 && visited.insert(line.id.clone()),
            "链式代理不能循环引用"
        );
        let chain = line.proxy_chain();
        ensure!(chain.len() <= 8, "最多选择 8 个链式代理");
        let mut unique = BTreeSet::new();
        for reference in chain {
            ensure!(
                unique.insert(reference.clone()),
                "链式代理不能重复选择同一个代理"
            );
            if let Some(id) = reference.strip_prefix("line:") {
                ensure!(crate::plans::for_line(doc, id).is_none(), "客户套餐线路不能作为链式出口，以免重复记账");
                let next = doc
                    .lines
                    .iter()
                    .find(|l| l.id == id && l.kind == Kind::Vless)
                    .context("引用的代理不存在")?;
                check_chain(next, doc, visited)?;
            } else {
                ensure!(reference.starts_with("external:"), "链式代理引用无效");
            }
        }
        visited.remove(&line.id);
        Ok(())
    }
    for line in &doc.lines {
        check_chain(line, doc, &mut BTreeSet::new())?;
    }
    let mut configs: BTreeMap<String, Value> = doc
        .nodes
        .iter()
        .map(|n| (n.id.clone(), empty_config()))
        .collect();
    let mut ports = BTreeSet::new();
    let mut shared_names = BTreeSet::new();
    let mut websites: BTreeMap<(String, u16), (bool, BTreeSet<String>)> = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut reserved: BTreeSet<(String, u16)> = doc
        .lines
        .iter()
        .flat_map(|l| l.hops.iter().map(|h| (h.node_id.clone(), h.port)))
        .collect();
    for profile in doc.legacy_configs.iter().filter(|p| p.enabled) {
        reserved.extend(
            crate::legacy_config::ports(&profile.config)?
                .into_iter()
                .map(|port| (profile.node_id.clone(), port)),
        );
    }
    let mut ordered: Vec<_> = doc.lines.iter().collect();
    ordered.sort_by(|a, b| a.id.cmp(&b.id));
    for line in ordered {
        ensure!(ids.insert(&line.id), "线路 ID 重复");
        ensure!(
            !line.name.trim().is_empty() && line.name.len() <= 120,
            "线路名称不能为空或超过 120 字节"
        );
        ensure!(
            !line.hops.is_empty() && line.hops.len() <= 8,
            "每条线路需要 1–8 个受管节点"
        );
        ensure!(
            matches!(line.network.as_str(), "tcp" | "udp" | "tcp,udp"),
            "转发网络无效"
        );
        let mut visited = BTreeSet::new();
        for h in &line.hops {
            ensure!(
                visited.insert(&h.node_id),
                "线路 {} 包含循环或重复节点",
                line.name
            );
            ensure!(h.port > 0, "端口必须大于 0");
            uuid::Uuid::parse_str(&h.uuid).context("节点接入 UUID 无效")?;
            ensure!(configs.contains_key(&h.node_id), "线路引用了不存在的节点");
            // Reserve disabled lines too: enabling must not silently steal another line's port.
            let fresh = ports.insert((&h.node_id, h.port));
            if line.kind == Kind::Website {
                let key = (h.node_id.clone(), h.port);
                let tls = matches!(line.access.tls.mode.as_str(), "auto" | "manual");
                ensure!(
                    fresh || websites.contains_key(&key),
                    "网站端口 {} 被其他类型的线路占用",
                    h.port
                );
                let (mode, domains) = websites.entry(key).or_insert((tls, BTreeSet::new()));
                ensure!(*mode == tls, "同一网站监听端口不能混用 HTTP 和 HTTPS");
                ensure!(
                    domains.insert(line.access.tls.domain.to_ascii_lowercase()),
                    "同一监听端口的网站域名重复"
                );
            } else if line.access.shared_port
                && line
                    .hops
                    .first()
                    .is_some_and(|first| std::ptr::eq(first, h))
            {
                ensure!(
                    !doc.lines.iter().any(|other| !other.access.shared_port
                        && other
                            .hops
                            .iter()
                            .any(|hop| hop.node_id == h.node_id && hop.port == h.port)),
                    "443 已被独立入口占用"
                );
                let node = doc.nodes.iter().find(|n| n.id == h.node_id).unwrap();
                let name = line.access.server_name(node).to_ascii_lowercase();
                ensure!(
                    shared_names.insert((h.node_id.clone(), name.clone())),
                    "统一 443 的 SNI {name} 重复；不同传输请使用不同 SNI 或独立端口"
                );
            } else {
                ensure!(fresh, "同一节点端口 {} 被多条线路占用", h.port);
            }
        }
        let forward = if line.kind == Kind::Forward {
            Some(target(&line.target)?)
        } else {
            None
        };
        let chain = line.proxy_chain();
        let mut external = Vec::new();
        let mut links = Vec::new();
        for (index, reference) in chain.iter().enumerate() {
            let link = if let Some(id) = reference.strip_prefix("external:") {
                doc.externals
                    .iter()
                    .find(|e| e.id == id)
                    .context("外部代理不存在")?
                    .url
                    .clone()
            } else {
                let next = doc
                    .lines
                    .iter()
                    .find(|l| Some(l.id.as_str()) == reference.strip_prefix("line:"))
                    .context("引用的代理不存在")?;
                share(next, doc)?
            };
            let tag = if index + 1 == chain.len() {
                format!("line-{}-out", line.id)
            } else {
                format!("line-{}-chain-{index}", line.id)
            };
            let mut outbound = external_outbound(&link, &tag)?;
            if index > 0 {
                outbound["streamSettings"]["sockopt"]["dialerProxy"] =
                    json!(format!("line-{}-chain-{}", line.id, index - 1));
            }
            external.push(outbound);
            links.push(link);
        }
        udp_supported(line, doc)?;
        let sidecar_chain = external.iter().any(needs_mihomo);
        crate::access::validate_line(line)?;
        if !line.enabled && line.kind != Kind::Vless {
            continue;
        }
        for (i, hop) in line.hops.iter().enumerate() {
            let node = doc.nodes.iter().find(|n| n.id == hop.node_id).unwrap();
            let in_tag = format!("line-{}-in", line.id);
            let out_tag = format!("line-{}-out", line.id);
            let mut meter = None;
            let mut inbound = if i == 0 && line.kind == Kind::Forward {
                let (host, port) = forward.as_ref().unwrap();
                json!({"tag":in_tag,"listen":"0.0.0.0","port":hop.port,"protocol":"dokodemo-door",
                    "settings":{"address":host,"port":port,"network":line.network}})
            } else if i == 0 && line.kind == Kind::Vless && line.access.protocol != Protocol::Vless
            {
                crate::access::inbound(line, hop, &in_tag)
            } else {
                vless_in(node, hop, &in_tag)
            };
            if i == 0
                && line.kind == Kind::Vless
                && line.access.protocol == Protocol::Vless
                && matches!(line.access.tls.mode.as_str(), "auto" | "manual")
            {
                let tls = &line.access.tls;
                let certificates = if tls.mode == "manual" {
                    json!([{"certificate":tls.cert.lines().collect::<Vec<_>>(),"key":tls.key.lines().collect::<Vec<_>>() }])
                } else {
                    json!([])
                };
                inbound["streamSettings"] = json!({"network":"tcp","security":"tls",
                    "tlsSettings":{"minVersion":"1.2","certificates":certificates}});
            }
            let mut outbound = if let Some(next) = line.hops.get(i + 1) {
                vless_out(
                    doc.nodes.iter().find(|n| n.id == next.node_id).unwrap(),
                    next,
                    &out_tag,
                )
            } else if let Some(last) = external.last() {
                last.clone()
            } else {
                json!({"tag":out_tag,"protocol":"freedom"})
            };
            let config = configs.get_mut(&node.id).unwrap();
            if i + 1 == line.hops.len() && sidecar_chain {
                // One bridge per chain keeps each hop's dialer and QUIC sessions isolated.
                let port = backend_port(&mut reserved, node)?;
                let sidecar = mihomo_config(config);
                for (index, (link, original)) in links.iter().zip(&external).enumerate() {
                    let mut proxy =
                        crate::external::mihomo(link, original["tag"].as_str().unwrap())?;
                    if proxy["type"] != "http" && proxy.get("udp").is_none() {
                        proxy["udp"] = json!(true);
                    }
                    if index > 0 {
                        let previous = &external[index - 1];
                        proxy["dialer-proxy"] = previous["tag"].clone();
                        if proxy["type"] == "hysteria2"
                            && previous["protocol"] == "shadowsocks"
                            && previous["settings"]["servers"][0]["method"].as_str().is_some_and(|m| m.starts_with("2022-"))
                        {
                            // QUIC's receive buffer excludes SS2022 overhead. A SOCKS hop strips
                            // that framing before QUIC reads it, including larger PMTU probes.
                            let bridge_port = backend_port(&mut reserved, node)?;
                            let bridge = format!("{}-udp-bridge", original["tag"].as_str().unwrap());
                            sidecar["listeners"].as_array_mut().unwrap().push(json!({"name":bridge,"type":"socks","listen":"127.0.0.1","port":bridge_port,"udp":true,"proxy":previous["tag"]}));
                            sidecar["proxies"].as_array_mut().unwrap().push(json!({"name":bridge,"type":"socks5","server":"127.0.0.1","port":bridge_port,"udp":true}));
                            proxy["dialer-proxy"] = json!(bridge);
                        }
                    }
                    sidecar["proxies"].as_array_mut().unwrap().push(proxy);
                }
                sidecar["listeners"].as_array_mut().unwrap().push(json!({"name":out_tag,"type":"socks","listen":"127.0.0.1","port":port,"udp":true,"proxy":out_tag}));
                outbound = json!({"tag":out_tag,"protocol":"socks","settings":{"servers":[{"address":"127.0.0.1","port":port}]}});
            }
            if i == 0 {
                if line.kind == Kind::Vless && line.access.protocol == Protocol::Vless {
                    if let Some(reality) = &line.access.reality {
                        if inbound["streamSettings"]["security"] == "reality" {
                            let (private_key, _, short_id) = line.access.reality_credentials(node);
                            inbound["streamSettings"]["realitySettings"]["privateKey"] =
                                json!(private_key);
                            inbound["streamSettings"]["realitySettings"]["shortIds"] =
                                json!([short_id]);
                            inbound["streamSettings"]["realitySettings"]["target"] =
                                json!(reality.target);
                            inbound["streamSettings"]["realitySettings"]["serverNames"] =
                                json!([reality.server_name]);
                            for (key, value) in [
                                ("limitFallbackUpload", &reality.limit_fallback_upload),
                                ("limitFallbackDownload", &reality.limit_fallback_download),
                            ] {
                                if let Some(value) = value {
                                    inbound["streamSettings"]["realitySettings"][key] =
                                        json!(value);
                                }
                            }
                        }
                    }
                    if line.access.transport.network == "xhttp" {
                        inbound["settings"]["clients"][0]
                            .as_object_mut()
                            .unwrap()
                            .remove("flow");
                        inbound["streamSettings"]["network"] = json!("xhttp");
                        inbound["streamSettings"]["xhttpSettings"] = json!({"path":line.access.transport.path,"host":line.access.transport.host,"mode":"auto"});
                    }
                }
                let fallback_port = if !line.access.fallback.is_empty() {
                    let port = backend_port(&mut reserved, node)?;
                    inbound["settings"]["fallbacks"] =
                        json!([{"dest":format!("127.0.0.1:{port}"),"xver":0}]);
                    inbound["streamSettings"]["tlsSettings"]["alpn"] = json!(["h2", "http/1.1"]);
                    Some(port)
                } else {
                    None
                };
                let port = if line.access.shared_port {
                    let port = backend_port(&mut reserved, node)?;
                    if config.get("_ingress").is_none() {
                        config["_ingress"] = json!([]);
                    }
                    config["_ingress"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"sni":line.access.server_name(node),"port":port}));
                    port
                } else {
                    hop.port
                };
                let backend = backend_port(&mut reserved, node)?;
                inbound["listen"] = json!("127.0.0.1");
                inbound["port"] = json!(backend);
                if matches!(line.access.protocol, Protocol::Tuic | Protocol::Hysteria2)
                    && line.kind == Kind::Vless
                {
                    let socks = backend_port(&mut reserved, node)?;
                    inbound["port"] = json!(socks);
                    let sidecar = mihomo_config(config);
                    let mut listener = json!({"name":in_tag,"type":if line.access.protocol == Protocol::Tuic {"tuic"} else {"hysteria2"},"listen":"127.0.0.1","port":backend,"users":{&hop.uuid:&line.access.password},"certificate":line.access.tls.cert,"private-key":line.access.tls.key,"alpn":["h3"],"proxy":in_tag});
                    if line.access.protocol == Protocol::Tuic {
                        listener["congestion-controller"] = json!("bbr");
                    }
                    sidecar["listeners"].as_array_mut().unwrap().push(listener);
                    sidecar["proxies"].as_array_mut().unwrap().push(json!({"name":in_tag,"type":"socks5","server":"127.0.0.1","port":socks,"udp":true}));
                }
                if config.get("_chain").is_none() {
                    config["_chain"] = json!([]);
                }
                if line.kind == Kind::Vless {
                    meter = Some(crate::meter::Spec {
                        port: backend_port(&mut reserved, node)?,
                        backend: backend_port(&mut reserved, node)?,
                        password: hop.uuid.clone(),
                    });
                }
                config["_chain"].as_array_mut().unwrap().push(json!({
                    "id":line.id,"port":port,"backend":backend,"kind":line.kind,
                    "listen":if line.access.shared_port {"127.0.0.1"} else {"0.0.0.0"},
                    "proxy_protocol":line.access.shared_port,
                    "fallback_port":fallback_port,"fallback_target":line.access.fallback,
                    "protocol":line.access.protocol,"network":if line.kind == Kind::Forward {line.network.as_str()} else if matches!(line.access.protocol,Protocol::Hysteria2|Protocol::Tuic) {"udp"} else if matches!(line.access.protocol,Protocol::Socks|Protocol::Shadowsocks) {"tcp,udp"} else {"tcp"},
                    "limits":line.limits,"tls":line.access.tls,"target":line.target,
                    "enabled":line.enabled,"lease":crate::plans::lease(doc,&line.id).map(Rental::policy),"meter":meter,
                    "multiplier":crate::resources::multiplier(doc,line)?,
                    "resource_ids":crate::resources::pools(doc,line),
                    "access_policy":crate::rules::compile(doc,&line.id)?,
                }));
                if line.kind == Kind::Website {
                    continue;
                }
            }
            let guard = if inbound["streamSettings"]["security"] == "reality" {
                Some(backend_port(&mut reserved, node)?)
            } else { None };
            // Keep the public inbound first for existing consumers of compiled configs.
            let index = config["inbounds"].as_array().unwrap().len();
            config["inbounds"].as_array_mut().unwrap().push(inbound.clone());
            config["outbounds"].as_array_mut().unwrap().push(outbound);
            if i + 1 == line.hops.len() && external.len() > 1 && !sidecar_chain {
                config["outbounds"]
                    .as_array_mut()
                    .unwrap()
                    .extend_from_slice(&external[..external.len() - 1]);
            }
            if let Some(port) = guard {
                guard_reality(config, &mut inbound, port)?;
                config["inbounds"][index] = inbound;
            }
            if let Some(meter) = meter {
                let bridge_in = format!("line-{}-meter-in", line.id);
                let bridge_out = format!("line-{}-meter-out", line.id);
                config["inbounds"].as_array_mut().unwrap().push(json!({
                    "tag":bridge_in,"listen":"127.0.0.1","port":meter.backend,"protocol":"socks",
                    "settings":{"auth":"password","accounts":[{"user":line.id,"pass":meter.password}],"udp":true,"ip":"127.0.0.1"}
                }));
                config["outbounds"].as_array_mut().unwrap().push(json!({
                    "tag":bridge_out,"protocol":"socks","settings":{"servers":[{"address":"127.0.0.1","port":meter.port,"users":[{"user":line.id,"pass":meter.password}]}]}
                }));
                config["routing"]["rules"].as_array_mut().unwrap().extend([
                    json!({"type":"field","inboundTag":[in_tag],"outboundTag":bridge_out}),
                    json!({"type":"field","inboundTag":[bridge_in],"outboundTag":out_tag}),
                ]);
            } else {
                config["routing"]["rules"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"type":"field","inboundTag":[in_tag],"outboundTag":out_tag}));
            }
        }
    }
    crate::legacy_config::attach(doc, &mut configs)?;
    Ok(configs)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Document {
        let mut d = Document::default();
        for n in ["a", "b", "c"] {
            d.nodes.push(Node {
                id: n.into(),
                name: n.into(),
                host: format!("{n}.example.com"),
                reality: Reality::new("example.com:443".into(), "example.com".into()),
                token_hash: String::new(),
                seen: 0,
                report: Report::default(),
                desired: None,
            });
        }
        d.lines.push(Line {
            id: id(),
            name: "线路一".into(),
            kind: Kind::Vless,
            hops: ["a", "b", "c"]
                .iter()
                .map(|n| Hop {
                    node_id: n.to_string(),
                    port: 24443,
                    uuid: id(),
                })
                .collect(),
            external_id: None,
            chain: vec![],
            target: String::new(),
            network: "tcp".into(),
            enabled: true,
            access: Access::default(),
            limits: Limits::default(),
        });
        d
    }
    #[test]
    fn reality_empty_node_requires_an_explicit_line_target() {
        let mut d = fixture();
        d.nodes.truncate(1);
        d.nodes[0].reality.target.clear();
        d.nodes[0].reality.server_name.clear();
        d.lines[0].hops.truncate(1);
        assert!(compile(&d).is_err());
        d.lines[0].access.reality = Some(serde_json::from_value(json!({"target":"cover.example:443","server_name":"cover.example"})).unwrap());
        assert!(compile(&d).is_ok());
        d.lines[0].access.reality.as_mut().unwrap().server_name.clear();
        assert!(compile(&d).is_err());
        d.lines[0].access.reality = None;
        d.lines[0].access.protocol = Protocol::Shadowsocks;
        d.lines[0].access.method = "aes-128-gcm".into();
        d.lines[0].access.password = "test-password".into();
        assert!(compile(&d).is_ok());
        d.lines.clear();
        assert!(compile(&d).is_ok());
    }

    #[test]
    fn reality_guard_covers_each_hop_without_changing_client_sni() {
        let d = fixture();
        let configs = compile(&d).unwrap();
        for config in configs.values() {
            let inbound = &config["inbounds"][0];
            let guard = &config["inbounds"][1];
            assert_eq!(inbound["streamSettings"]["realitySettings"]["serverNames"], json!(["example.com"]));
            assert_eq!(guard["listen"], "127.0.0.1");
            assert_eq!(guard["settings"]["address"], "127.0.0.1");
            assert_eq!(guard["sniffing"]["routeOnly"], true);
            assert_eq!(config["routing"]["rules"][0]["domain"], json!(["full:example.com"]));
            assert_eq!(config["routing"]["rules"][1]["outboundTag"], "blocked");
            assert_eq!(config["outbounds"][2]["settings"]["redirect"], "example.com:443");
        }
    }
    #[test]
    fn backend_ports_cannot_collide_with_kernel_ephemeral_connections() {
        let mut d = fixture();
        let mut reserved = BTreeSet::new();
        assert_eq!(backend_port(&mut reserved, &d.nodes[0]).unwrap(), 61000);
        assert_eq!(backend_port(&mut reserved, &d.nodes[0]).unwrap(), 61001);
        d.nodes[0].report.system = json!({"ephemeral_port_range":[60000,65535]});
        assert_eq!(backend_port(&mut reserved, &d.nodes[0]).unwrap(), 50000);
        d.nodes[0].report.system["ephemeral_port_range"] = json!([49152, 65535]);
        assert_eq!(backend_port(&mut reserved, &d.nodes[0]).unwrap(), 30000);
        d.nodes[0].report.system["ephemeral_port_range"] = json!([30000, 65535]);
        assert!(
            backend_port(&mut reserved, &d.nodes[0])
                .unwrap_err()
                .to_string()
                .contains("系统临时端口")
        );
    }

    #[test]
    fn authenticated_payload_crosses_meter_and_operational_updates_keep_core_config() {
        let mut d = fixture();
        d.lines[0].hops.truncate(1);
        let active = compile(&d).unwrap();
        let config = &active["a"];
        let spec = crate::gateway::specs(config).unwrap().remove(0);
        let meter = spec.meter.as_ref().unwrap();
        let id = &d.lines[0].id;
        let rules = config["routing"]["rules"].as_array().unwrap();
        assert!(
            rules
                .iter()
                .any(|r| r["inboundTag"][0] == format!("line-{id}-in")
                    && r["outboundTag"] == format!("line-{id}-meter-out"))
        );
        assert!(
            rules
                .iter()
                .any(|r| r["inboundTag"][0] == format!("line-{id}-meter-in")
                    && r["outboundTag"] == format!("line-{id}-out"))
        );
        assert_ne!(meter.port, meter.backend);
        assert_ne!(meter.port, spec.port);
        assert_eq!(meter.password, d.lines[0].hops[0].uuid);
        d.lines[0].enabled = false;
        d.lines[0].limits.monthly_gib = Some(100.0);
        let stopped = compile(&d).unwrap();
        assert_eq!(
            crate::gateway::xray_config(config),
            crate::gateway::xray_config(&stopped["a"])
        );
        assert!(crate::gateway::same_listeners(
            &[spec],
            &crate::gateway::specs(&stopped["a"]).unwrap()
        ));
    }
    #[test]
    fn udp_capability_follows_local_references_and_exports() {
        let mut d = fixture();
        d.lines[0].hops.truncate(1);
        d.externals.push(External {
            id: "http".into(),
            name: "http".into(),
            url: "http://proxy.example:8080".into(),
        });
        d.externals.push(External {
            id: "hy2".into(),
            name: "hy2".into(),
            url: "hysteria2://secret@quic.example:443".into(),
        });
        d.lines[0].chain = vec!["external:http".into()];
        assert!(!udp_supported(&d.lines[0], &d).unwrap());
        assert_eq!(
            crate::access::mihomo_node(&d.lines[0], &d).unwrap()["udp"],
            false
        );
        assert_eq!(
            crate::external::mihomo(&share(&d.lines[0], &d).unwrap(), "test").unwrap()["udp"],
            false
        );
        let mut second = d.lines[0].clone();
        second.id = id();
        second.hops[0].port += 1;
        second.chain = vec![format!("line:{}", d.lines[0].id), "external:hy2".into()];
        d.lines.push(second);
        assert!(compile(&d).unwrap_err().to_string().contains("需要 UDP"));
        d.lines[0].chain.clear();
        assert!(udp_supported(&d.lines[1], &d).unwrap());
        assert!(compile(&d).is_ok());
    }
    #[test]
    fn quic_ingress_uses_udp_listener_and_loopback_socks_bridge() {
        for (protocol, kind) in [(Protocol::Hysteria2, "hysteria2"), (Protocol::Tuic, "tuic")] {
            let mut d = fixture();
            d.lines[0].hops.truncate(1);
            d.lines[0].access.protocol = protocol;
            d.lines[0].access.password = "test-password".into();
            d.lines[0].access.tls.mode = "auto".into();
            d.lines[0].access.tls.domain = "cert.example.com".into();
            let c = compile(&d).unwrap();
            let listener = &c["a"]["_mihomo"]["listeners"][0];
            assert_eq!(listener["type"], kind);
            assert_eq!(listener["listen"], "127.0.0.1");
            assert_eq!(listener["port"], c["a"]["_chain"][0]["backend"]);
            assert_eq!(c["a"]["inbounds"][0]["protocol"], "socks");
            assert_eq!(
                c["a"]["inbounds"][0]["port"],
                c["a"]["_mihomo"]["proxies"][0]["port"]
            );
            assert_eq!(c["a"]["_chain"][0]["network"], "udp");
        }
    }
    #[test]
    fn native_imports_compile_to_sidecar() {
        let nodes: Vec<Value> = serde_json::from_str(include_str!("external/fixtures.json")).unwrap();
        for mut node in nodes {
            let mut doc = fixture();
            doc.lines[0].hops.truncate(1);
            let (url,name) = crate::external::import(&node.to_string()).unwrap();
            doc.externals.push(External { id:"native".into(),name,url });
            doc.lines[0].chain = vec!["external:native".into()];
            let configs = compile(&doc).unwrap();
            let proxy = &configs["a"]["_mihomo"]["proxies"][0];
            node["name"] = proxy["name"].clone();
            if node.get("udp").is_none() { node["udp"] = json!(true); }
            assert_eq!(proxy,&node);
            assert!(configs["a"]["outbounds"].as_array().unwrap().iter().all(|v| v["protocol"] != "mihomo"));
        }
    }
    #[test]
    fn quic_requires_udp_from_the_complete_preceding_path() {
        let http = udp_after(true, "http", true, 0).unwrap();
        let socks = udp_after(http, "socks", true, 1).unwrap();
        let vless = udp_after(http, "vless", true, 1).unwrap();
        for protocol in ["hysteria2", "tuic"] {
            assert!(udp_after(http, protocol, true, 1).is_err());
            assert!(udp_after(socks, protocol, true, 2).is_err());
            assert!(udp_after(vless, protocol, true, 2).unwrap());
        }
        assert!(!socks);
        assert!(!udp_after(true, "vless", false, 0).unwrap());
    }
    #[test]
    fn https_export_preserves_sni_and_uses_managed_chain() {
        let mut d = fixture();
        d.lines[0].hops.truncate(1);
        d.lines[0].access.protocol = Protocol::Http;
        d.lines[0].access.host = "192.0.2.1".into();
        d.lines[0].access.username = "user".into();
        d.lines[0].access.password = "password".into();
        d.lines[0].access.tls.mode = "auto".into();
        d.lines[0].access.tls.domain = "cert.example.com".into();
        let link = share(&d.lines[0], &d).unwrap();
        let out = external_outbound(&link, "test").unwrap();
        assert_eq!(out["settings"]["servers"][0]["address"], "192.0.2.1");
        assert_eq!(
            out["streamSettings"]["tlsSettings"]["serverName"],
            "cert.example.com"
        );
        assert_eq!(
            crate::external::mihomo(&link, "test").unwrap()["sni"],
            "cert.example.com"
        );
        d.externals.push(External {
            id: "https".into(),
            name: "https".into(),
            url: link,
        });
        d.lines[0].chain = vec!["external:https".into()];
        let c = compile(&d).unwrap();
        assert_eq!(c["a"]["outbounds"][1]["protocol"], "socks");
        assert_eq!(c["a"]["_mihomo"]["proxies"][0]["sni"], "cert.example.com");
        for protocol in ["shadowsocks", "hysteria", "tuic"] {
            assert!(needs_mihomo(&json!({"protocol":protocol})));
        }
        assert!(!needs_mihomo(&json!({"protocol":"vless"})));
    }
    #[test]
    fn ss2022_hysteria_udp_bridge_preserves_the_complete_chain() {
        let mut d = fixture();
        d.lines[0].hops.truncate(1);
        for (id, url) in [
            ("first", "socks5://127.0.0.1:1080"),
            ("ss", "ss://2022-blake3-aes-128-gcm:YWFhYWFhYWFhYWFhYWFhYQ%3D%3D@ss.example.com:443"),
            ("hy", "hysteria2://secret@hy.example.com:443"),
        ] {
            d.externals.push(External { id: id.into(), name: id.into(), url: url.into() });
        }
        d.lines[0].chain = vec!["external:first".into(), "external:ss".into(), "external:hy".into()];
        let c = compile(&d).unwrap();
        let sidecar = &c["a"]["_mihomo"];
        let proxies = sidecar["proxies"].as_array().unwrap();
        let ss = proxies.iter().find(|p| p["type"] == "ss").unwrap();
        let hy = proxies.iter().find(|p| p["type"] == "hysteria2").unwrap();
        assert_eq!(ss["dialer-proxy"], proxies[0]["name"]);
        let bridge = proxies.iter().find(|p| p["name"] == hy["dialer-proxy"]).unwrap();
        assert_eq!(bridge["type"], "socks5");
        assert_eq!(bridge["server"], "127.0.0.1");
        let listener = sidecar["listeners"].as_array().unwrap().iter().find(|l| l["port"] == bridge["port"]).unwrap();
        assert_eq!(listener["listen"], "127.0.0.1");
        assert_eq!(listener["proxy"], ss["name"]);
        assert_eq!(listener["udp"], true);
        assert_ne!(listener["port"], c["a"]["_chain"][0]["port"]);
        // This internal compatibility bridge is not an additional billable hop.
        assert_eq!(c["a"]["_chain"][0]["multiplier"], 4);
    }
    #[test]
    fn xhttp_and_custom_reality_roundtrip_without_vision_flow() {
        let mut d = fixture();
        d.lines[0].hops.truncate(1);
        d.lines[0].access.transport = Transport {
            network: "xhttp".into(),
            path: "/proxy/test".into(),
            host: "cover.example.com".into(),
            mode: "packet-up".into(),
        };
        d.lines[0].access.reality = Some(RealityTarget {
            target: "cover.example.com:8443".into(),
            server_name: "cover.example.com".into(),
            credentials: None,
            limit_fallback_upload: None,
            limit_fallback_download: None,
        });
        let c = compile(&d).unwrap();
        let inbound = &c["a"]["inbounds"][0];
        assert_eq!(inbound["streamSettings"]["network"], "xhttp");
        assert_eq!(
            inbound["streamSettings"]["realitySettings"]["target"],
            format!("127.0.0.1:{}", c["a"]["inbounds"][1]["port"])
        );
        assert!(c["a"]["outbounds"].as_array().unwrap().iter().any(|out| out["settings"]["redirect"] == "cover.example.com:8443"));
        assert!(inbound["settings"]["clients"][0].get("flow").is_none());
        let node = crate::access::mihomo_node(&d.lines[0], &d).unwrap();
        assert_eq!(node["servername"], "cover.example.com");
        assert_eq!(node["xhttp-opts"]["path"], "/proxy/test");
        assert!(node.get("flow").is_none());
        let link = share(&d.lines[0], &d).unwrap();
        assert!(!link.contains("flow="));
        let outbound = external_outbound(&link, "test").unwrap();
        assert_eq!(
            outbound["streamSettings"]["xhttpSettings"]["mode"],
            "packet-up"
        );
    }
    #[test]
    fn shared_443_keeps_unique_sni_and_loopback_backends() {
        let mut d = fixture();
        d.lines[0].hops.truncate(1);
        d.lines[0].hops[0].port = 443;
        d.lines[0].access.shared_port = true;
        let mut tls = d.lines[0].clone();
        tls.id = id();
        tls.access.tls = Tls {
            mode: "auto".into(),
            domain: "own.example.com".into(),
            ..Default::default()
        };
        tls.access.fallback = "builtin".into();
        d.lines.push(tls);
        let c = compile(&d).unwrap();
        assert_eq!(c["a"]["_ingress"].as_array().unwrap().len(), 2);
        for spec in crate::gateway::specs(&c["a"]).unwrap() {
            assert_eq!(spec.listen, "127.0.0.1");
            assert_ne!(spec.port, 443);
        }
        let duplicate = d.lines[0].access.clone();
        d.lines[1].access = duplicate;
        assert!(compile(&d).unwrap_err().to_string().contains("SNI"));
        d.lines[1].access.shared_port = false;
        assert!(compile(&d).is_err());
    }
    #[test]
    fn ordered_proxy_chain_uses_each_transport_and_rejects_cycles() {
        let mut d = fixture();
        d.lines[0].hops.truncate(1);
        for name in ["one", "two"] {
            d.externals.push(External {
                id: name.into(),
                name: name.into(),
                url: format!(
                    "vless://{}@{name}.example.com:443?security=tls&type=tcp",
                    id()
                ),
            });
        }
        d.lines[0].chain = vec!["external:one".into(), "external:two".into()];
        let c = compile(&d).unwrap();
        let out = &c["a"]["outbounds"];
        assert_eq!(out[1]["settings"]["vnext"][0]["address"], "two.example.com");
        assert_eq!(out[2]["settings"]["vnext"][0]["address"], "one.example.com");
        assert_eq!(
            out[1]["streamSettings"]["sockopt"]["dialerProxy"],
            out[2]["tag"]
        );
        d.lines[0].chain = vec![format!("line:{}", d.lines[0].id)];
        assert!(compile(&d).unwrap_err().to_string().contains("循环"));
        d.lines[0].chain = vec!["external:one".into(), "external:one".into()];
        assert!(compile(&d).is_err());
    }
    #[test]
    fn legacy_documents_keep_vless_defaults_and_credentials() {
        let document = fixture();
        let original = share(&document.lines[0], &document).unwrap();
        let mut value = serde_json::to_value(document).unwrap();
        value.as_object_mut().unwrap().remove("security");
        value.as_object_mut().unwrap().remove("subscriptions");
        for line in value["lines"].as_array_mut().unwrap() {
            line.as_object_mut().unwrap().remove("access");
            line.as_object_mut().unwrap().remove("limits");
        }
        for node in value["nodes"].as_array_mut().unwrap() {
            node["report"].as_object_mut().unwrap().remove("traffic");
            node["report"].as_object_mut().unwrap().remove("system");
        }
        let restored: Document = serde_json::from_value(value).unwrap();
        assert_eq!(share(&restored.lines[0], &restored).unwrap(), original);
        assert_eq!(restored.lines[0].access.protocol, Protocol::Vless);
        assert!(restored.lines[0].limits.monthly_gib.is_none());
        compile(&restored).unwrap();
    }
    #[test]
    fn three_hops_route_through_actual_next_node() {
        let d = fixture();
        let c = compile(&d).unwrap();
        assert_eq!(
            c["a"]["outbounds"][1]["settings"]["vnext"][0]["address"],
            "b.example.com"
        );
        assert_eq!(
            c["b"]["outbounds"][1]["settings"]["vnext"][0]["address"],
            "c.example.com"
        );
        assert_eq!(c["c"]["outbounds"][1]["protocol"], "freedom");
        assert_eq!(c["a"]["outbounds"][0]["protocol"], "blackhole");
    }
    #[test]
    fn reject_cycles_missing_nodes_and_port_collisions() {
        let mut d = fixture();
        d.lines[0].hops[2].node_id = "a".into();
        assert!(compile(&d).is_err());
        d.lines[0].hops[2].node_id = "missing".into();
        assert!(compile(&d).is_err());
        d = fixture();
        let mut second = d.lines[0].clone();
        second.id = id();
        d.lines.push(second);
        assert!(compile(&d).is_err());
        for h in &mut d.lines[1].hops {
            h.port += 1;
        }
        assert!(compile(&d).is_ok());
    }
    #[test]
    fn forwarding_keeps_destination_across_chain() {
        let mut d = fixture();
        d.lines[0].kind = Kind::Forward;
        d.lines[0].target = "[::1]:5353".into();
        d.lines[0].network = "tcp,udp".into();
        let c = compile(&d).unwrap();
        assert_eq!(c["a"]["inbounds"][0]["settings"]["address"], "::1");
        assert_eq!(c["a"]["inbounds"][0]["settings"]["network"], "tcp,udp");
        assert_eq!(c["b"]["inbounds"][0]["protocol"], "vless");
    }
    #[test]
    fn exported_share_roundtrips_and_entry_credentials_stay_stable() {
        let mut d = fixture();
        let before = share(&d.lines[0], &d).unwrap();
        d.lines[0].hops.swap(1, 2);
        assert_eq!(share(&d.lines[0], &d).unwrap(), before);
        let parsed = external_outbound(&before, "test").unwrap();
        assert_eq!(
            parsed["settings"]["vnext"][0]["users"][0]["id"],
            d.lines[0].hops[0].uuid
        );
    }
    #[test]
    fn tls_vision_entry_keeps_internal_reality_chain_and_exports_tls() {
        let mut d = fixture();
        d.lines[0].access.tls = Tls {
            mode: "auto".into(),
            domain: "entry.example.com".into(),
            ..Tls::default()
        };
        let configs = compile(&d).unwrap();
        assert_eq!(
            configs["a"]["inbounds"][0]["streamSettings"]["security"],
            "tls"
        );
        assert_eq!(
            configs["b"]["inbounds"][0]["streamSettings"]["security"],
            "reality"
        );
        assert_eq!(
            configs["a"]["outbounds"][1]["streamSettings"]["security"],
            "reality"
        );
        let link = share(&d.lines[0], &d).unwrap();
        let outbound = external_outbound(&link, "test").unwrap();
        assert_eq!(outbound["streamSettings"]["security"], "tls");
        assert_eq!(
            outbound["streamSettings"]["tlsSettings"]["serverName"],
            "entry.example.com"
        );
        assert!(!link.contains("pbk=") && !link.contains("sid="));
        let node = crate::access::mihomo_node(&d.lines[0], &d).unwrap();
        assert_eq!(node["servername"], "entry.example.com");
        assert_eq!(node["flow"], "xtls-rprx-vision");
        assert!(node.get("reality-opts").is_none());
        d.lines[0].access.tls.domain = "127.0.0.1".into();
        assert!(compile(&d).is_err());
        d.lines[0].access.tls.mode = "manual".into();
        assert!(compile(&d).is_err());
    }
    #[test]
    fn domain_entry_defaults_to_reality_and_exports_matching_keys() {
        let mut d = fixture();
        d.lines[0].access.host = "proxy.example.com".into();
        let c = compile(&d).unwrap();
        assert_eq!(
            c["a"]["inbounds"][0]["streamSettings"]["security"],
            "reality"
        );
        let node = crate::access::mihomo_node(&d.lines[0], &d).unwrap();
        assert_eq!(node["server"], "proxy.example.com");
        assert_eq!(node["servername"], d.nodes[0].reality.server_name);
        assert_eq!(
            node["reality-opts"]["public-key"],
            d.nodes[0].reality.public_key
        );
        assert_eq!(
            node["reality-opts"]["short-id"],
            d.nodes[0].reality.short_id
        );
        let out = external_outbound(&share(&d.lines[0], &d).unwrap(), "test").unwrap();
        assert_eq!(out["streamSettings"]["security"], "reality");
    }
    #[test]
    fn rejects_plaintext_external_and_bad_destination() {
        assert!(
            external_outbound(
                &format!("vless://{}@example.com:443?security=none", id()),
                "x"
            )
            .is_err()
        );
        for s in [
            "example.com",
            "user@example.com:80",
            "example.com:80/path",
            "example.com:0",
        ] {
            assert!(target(s).is_err(), "{s}");
        }
    }
}
