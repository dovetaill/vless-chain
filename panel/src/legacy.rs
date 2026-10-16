//! Conservative conversion: unsupported behavior becomes a named, inactive item.
use crate::{compiler::compile, model::*};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

#[derive(Deserialize)]
pub struct Import {
    pub node_id: String,
    pub sources: Vec<Source>,
    #[serde(default)]
    pub preview: bool,
    #[serde(default)]
    pub shared_443: bool,
}

#[derive(Deserialize)]
pub struct Source {
    pub id: String,
    pub name: String,
    pub config: Value,
    #[serde(default)]
    pub error: String,
}

fn only(value: &Value, keys: &[&str]) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    let object = value.as_object().context("配置必须是对象")?;
    ensure!(
        object.keys().all(|k| keys.contains(&k.as_str())),
        "存在尚不支持的配置字段"
    );
    Ok(())
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
fn empty(value: &Value) -> bool {
    value.is_null()
        || value.as_object().is_some_and(|v| v.is_empty())
        || value.as_array().is_some_and(|v| v.is_empty())
}
fn name(value: &str) -> String {
    let mut result = String::new();
    for ch in value.trim().chars().filter(|c| !c.is_control()) {
        if result.len() + ch.len_utf8() > 120 {
            break;
        }
        result.push(ch);
    }
    if result.is_empty() {
        "旧代理".into()
    } else {
        result
    }
}

fn global(config: &Value) -> Result<()> {
    only(
        config,
        &[
            "log",
            "api",
            "stats",
            "policy",
            "inbounds",
            "outbounds",
            "routing",
            "dns",
            "remarks",
        ],
    )?;
    ensure!(empty(&config["dns"]), "自定义 DNS 需要手动配置");
    let out = config["outbounds"].as_array().context("缺少出口配置")?;
    ensure!(
        out.first().is_some_and(|o| o["protocol"] == "freedom"
            && empty(&o["settings"])
            && empty(&o["streamSettings"])),
        "非默认直连出口需要手动配置"
    );
    only(&config["routing"], &["rules", "domainStrategy"])?;
    for rule in config["routing"]["rules"].as_array().into_iter().flatten() {
        ensure!(
            rule["inboundTag"] == json!(["api"]) && rule["outboundTag"] == "api",
            "自定义路由需要手动配置"
        );
    }
    Ok(())
}

fn convert(
    doc: &mut Document,
    node_id: &str,
    inbound: &Value,
    id: String,
    title: String,
    shared: bool,
) -> Result<Line> {
    only(
        inbound,
        &[
            "tag",
            "remark",
            "listen",
            "port",
            "protocol",
            "settings",
            "streamSettings",
            "sniffing",
        ],
    )?;
    ensure!(
        matches!(text(&inbound["listen"]), "" | "0.0.0.0"),
        "特定地址或 IPv6 监听需要手动配置"
    );
    ensure!(
        empty(&inbound["sniffing"]) || inbound["sniffing"]["enabled"] == false,
        "流量嗅探需要手动配置"
    );
    let port = inbound["port"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p > 0)
        .context("监听端口无效")?;
    let mut access = Access::default();
    access.protocol = match text(&inbound["protocol"]) {
        "vless" => Protocol::Vless,
        "vmess" => Protocol::Vmess,
        "shadowsocks" => Protocol::Shadowsocks,
        _ => anyhow::bail!("此协议需要手动配置"),
    };
    let stream = &inbound["streamSettings"];
    only(
        stream,
        &[
            "network",
            "security",
            "tcpSettings",
            "rawSettings",
            "xhttpSettings",
            "realitySettings",
            "tlsSettings",
        ],
    )?;
    access.transport.network = match text(&stream["network"]) {
        "" | "tcp" | "raw" => "tcp".into(),
        "xhttp" if access.protocol == Protocol::Vless => "xhttp".into(),
        _ => anyhow::bail!("此传输方式需要手动配置"),
    };
    for key in ["tcpSettings", "rawSettings"] {
        only(&stream[key], &["header"])?;
        only(&stream[key]["header"], &["type"])?;
        ensure!(
            matches!(text(&stream[key]["header"]["type"]), "" | "none"),
            "TCP 伪装需要手动配置"
        );
    }
    if access.transport.network == "xhttp" {
        let x = &stream["xhttpSettings"];
        only(x, &["path", "host", "mode"])?;
        ensure!(
            matches!(text(&x["mode"]), "" | "auto"),
            "XHTTP 模式需要手动配置"
        );
        access.transport.path = if text(&x["path"]).is_empty() {
            "/"
        } else {
            text(&x["path"])
        }
        .into();
        access.transport.host = text(&x["host"]).into();
    }
    let settings = &inbound["settings"];
    let mut credential = crate::model::id();
    if access.protocol == Protocol::Shadowsocks {
        only(settings, &["method", "password", "network"])?;
        ensure!(
            matches!(text(&settings["network"]), "" | "tcp,udp"),
            "SS 网络范围需要手动配置"
        );
        access.method = text(&settings["method"]).into();
        access.password = text(&settings["password"]).into();
    } else {
        only(settings, &["clients", "decryption", "fallbacks"])?;
        ensure!(empty(&settings["fallbacks"]), "回落及其依赖需要手动配置");
        let clients = settings["clients"].as_array().context("缺少客户端配置")?;
        ensure!(clients.len() == 1, "同端口多用户需要手动配置");
        let client = &clients[0];
        only(client, &["id", "flow", "alterId", "email", "level"])?;
        credential = text(&client["id"]).to_owned();
        uuid::Uuid::parse_str(&credential).context("原 UUID 无效")?;
        if access.protocol == Protocol::Vless {
            ensure!(
                matches!(text(&settings["decryption"]), "" | "none"),
                "VLESS 加密设置需要手动配置"
            );
            let expected = if access.transport.network == "tcp" {
                "xtls-rprx-vision"
            } else {
                ""
            };
            ensure!(
                text(&client["flow"]) == expected,
                "原 VLESS flow 与平台不兼容"
            );
        } else {
            ensure!(
                client["alterId"].as_u64().unwrap_or(0) == 0 && text(&client["flow"]).is_empty(),
                "旧版 VMess 认证需要手动配置"
            );
        }
    }
    let node_index = doc
        .nodes
        .iter()
        .position(|n| n.id == node_id)
        .context("本机节点不存在")?;
    match text(&stream["security"]) {
        "reality" if access.protocol == Protocol::Vless => {
            let r = &stream["realitySettings"];
            only(
                r,
                &[
                    "show",
                    "dest",
                    "target",
                    "xver",
                    "serverNames",
                    "privateKey",
                    "shortIds",
                ],
            )?;
            ensure!(
                r["xver"].as_u64().unwrap_or(0) == 0,
                "REALITY PROXY protocol 需要手动配置"
            );
            let names = r["serverNames"].as_array().context("缺少 REALITY SNI")?;
            let shorts = r["shortIds"].as_array().context("缺少 REALITY short ID")?;
            ensure!(
                names.len() == 1 && shorts.len() == 1,
                "多个 SNI / short ID 需要手动配置"
            );
            let target = if r.get("target").is_some() {
                text(&r["target"])
            } else {
                text(&r["dest"])
            };
            let (host, _) = crate::compiler::target(target)?;
            ensure!(
                host.contains('.') && !host.starts_with("127.") && host != "localhost",
                "REALITY 本地或容器目标需要手动配置"
            );
            let key: [u8; 32] = URL_SAFE_NO_PAD
                .decode(text(&r["privateKey"]))?
                .try_into()
                .map_err(|_| anyhow::anyhow!("REALITY 私钥长度无效"))?;
            let private = StaticSecret::from(key);
            let reality = Reality {
                target: target.into(),
                server_name: text(&names[0]).into(),
                private_key: text(&r["privateKey"]).into(),
                public_key: URL_SAFE_NO_PAD.encode(PublicKey::from(&private).as_bytes()),
                short_id: text(&shorts[0]).into(),
            };
            ensure!(
                reality.short_id.len() <= 16
                    && reality.short_id.len() % 2 == 0
                    && reality.short_id.bytes().all(|c| c.is_ascii_hexdigit()),
                "REALITY short ID 无效"
            );
            let node = &doc.nodes[node_index];
            let used = doc.lines.iter().chain(&doc.applied_lines).any(|l| {
                l.access.protocol == Protocol::Vless
                    && !matches!(l.access.tls.mode.as_str(), "auto" | "manual")
                    && l.hops.iter().any(|h| h.node_id == node_id)
            });
            ensure!(
                !used
                    || (node.reality.private_key == reality.private_key
                        && node.reality.short_id == reality.short_id),
                "节点已使用另一组 REALITY 密钥，需要手动配置"
            );
            access.reality = Some(RealityTarget {
                target: reality.target.clone(),
                server_name: reality.server_name.clone(),
            });
            doc.nodes[node_index].reality = reality;
        }
        "tls" => {
            let tls = &stream["tlsSettings"];
            only(tls, &["certificates", "serverName", "minVersion"])?;
            ensure!(
                matches!(text(&tls["minVersion"]), "" | "1.2"),
                "TLS 最低版本需要手动配置"
            );
            let certs = tls["certificates"].as_array().context("缺少 TLS 证书")?;
            ensure!(certs.len() == 1, "多证书需要手动配置");
            only(&certs[0], &["certificate", "key"])?;
            let pem = |key: &str| -> Result<String> {
                let values = certs[0][key].as_array().context("证书必须在预检时读入")?;
                Ok(values
                    .iter()
                    .map(|v| v.as_str().context("证书格式无效"))
                    .collect::<Result<Vec<_>>>()?
                    .join("\n"))
            };
            let cert = pem("certificate")?;
            let parsed = openssl::x509::X509::from_pem(cert.as_bytes())?;
            let domain = if !text(&tls["serverName"]).is_empty() {
                text(&tls["serverName"]).to_owned()
            } else {
                parsed
                    .subject_alt_names()
                    .into_iter()
                    .flatten()
                    .find_map(|n| n.dnsname().map(str::to_owned))
                    .context("证书缺少 DNS 名称")?
            };
            access.tls = Tls {
                mode: "manual".into(),
                domain,
                cert,
                key: pem("key")?,
            };
        }
        "" | "none" if access.protocol != Protocol::Vless => {}
        _ => anyhow::bail!("此安全设置需要手动配置"),
    }
    access.shared_port = shared && port == 443 && access.protocol == Protocol::Vless;
    let line = Line {
        id,
        name: title,
        kind: Kind::Vless,
        hops: vec![Hop {
            node_id: node_id.into(),
            port,
            uuid: credential,
        }],
        external_id: None,
        chain: vec![],
        target: String::new(),
        network: "tcp".into(),
        enabled: true,
        access,
        limits: Limits::default(),
    };
    crate::access::validate_line(&line)?;
    Ok(line)
}

pub fn import(doc: &mut Document, request: &Import) -> Result<Value> {
    ensure!(
        doc.nodes.iter().any(|n| n.id == request.node_id),
        "请先接入本机节点"
    );
    ensure!(
        !request.sources.is_empty() && request.sources.len() <= 64,
        "请选择 1–64 个旧服务"
    );
    let mut imported = vec![];
    let mut pending = vec![];
    for source in &request.sources {
        ensure!(
            source.id.len() <= 256 && !source.id.is_empty(),
            "旧服务标识无效"
        );
        let entries = source.config["inbounds"]
            .as_array()
            .filter(|v| !v.is_empty())
            .cloned()
            .unwrap_or_else(|| vec![Value::Null]);
        ensure!(entries.len() <= 1024, "旧服务入口过多");
        for (index, entry) in entries.iter().enumerate() {
            if entry["tag"] == "api" && entry["protocol"] == "dokodemo-door" {
                continue;
            }
            let digest = Sha256::digest(format!("{}:{index}", source.id));
            let id = uuid::Uuid::from_slice(&digest[..16])?.to_string();
            if doc.lines.iter().any(|l| l.id == id)
                || doc.pending_imports.iter().any(|p| p.id == id)
            {
                continue;
            }
            let title = name(if !text(&entry["remark"]).is_empty() {
                text(&entry["remark"])
            } else if !text(&entry["tag"]).is_empty() {
                text(&entry["tag"])
            } else {
                &source.name
            });
            let before = doc.clone();
            let result = (|| {
                ensure!(source.error.is_empty(), "{}", source.error);
                global(&source.config)?;
                let line = convert(
                    doc,
                    &request.node_id,
                    entry,
                    id.clone(),
                    title.clone(),
                    request.shared_443,
                )?;
                doc.lines.push(line.clone());
                compile(doc)?;
                Ok::<_, anyhow::Error>(line)
            })();
            match result {
                Ok(line) => imported.push(json!({"id":id,"name":title,"port":line.hops[0].port,"protocol":line.access.protocol,
                    "can_share": line.access.protocol == Protocol::Vless && line.access.tls.mode != "manual",
                    "sni":line.access.server_name(doc.nodes.iter().find(|n| n.id == request.node_id).unwrap())})),
                Err(error) => {
                    *doc = before;
                    let item = PendingImport { id, name: title, reason: format!("{error:#}") };
                    pending.push(serde_json::to_value(&item)?);
                    doc.pending_imports.push(item);
                }
            }
        }
    }
    Ok(json!({"imported":imported,"pending":pending}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(inbound: Value) -> (Document, Import) {
        let node = Node {
            id: id(),
            name: "local".into(),
            host: "example.com".into(),
            reality: Reality::new("www.microsoft.com:443".into(), "www.microsoft.com".into()),
            token_hash: String::new(),
            seen: 0,
            report: Report::default(),
            desired: None,
        };
        let request = Import {
            node_id: node.id.clone(),
            preview: false,
            shared_443: false,
            sources: vec![Source {
                id: "systemd:xray.service:/config.json".into(),
                name: "old".into(),
                error: String::new(),
                config: json!({"inbounds":[inbound],"outbounds":[{"protocol":"freedom","settings":{}}]}),
            }],
        };
        (
            Document {
                nodes: vec![node],
                ..Default::default()
            },
            request,
        )
    }

    fn vmess() -> Value {
        json!({"tag":"original","port":23456,"protocol":"vmess","settings":{"clients":[{"id":"c88bd6c0-0194-4718-a699-a1674058767c","alterId":0}]},
            "streamSettings":{"network":"tcp","security":"none"}})
    }

    #[test]
    fn preserves_vmess_identity_and_is_idempotent() {
        let (mut doc, request) = fixture(vmess());
        let result = import(&mut doc, &request).unwrap();
        assert_eq!(result["imported"].as_array().unwrap().len(), 1);
        assert_eq!(
            doc.lines[0].hops[0].uuid,
            "c88bd6c0-0194-4718-a699-a1674058767c"
        );
        assert_eq!(doc.lines[0].hops[0].port, 23456);
        import(&mut doc, &request).unwrap();
        assert_eq!(doc.lines.len(), 1);
    }

    #[test]
    fn preserves_reality_private_key_and_public_identity() {
        let reality = Reality::new("www.microsoft.com:443".into(), "www.microsoft.com".into());
        let input = json!({"tag":"old reality","port":443,"protocol":"vless",
            "settings":{"decryption":"none","clients":[{"id":id(),"flow":"xtls-rprx-vision"}]},
            "streamSettings":{"network":"tcp","security":"reality","realitySettings":{"dest":reality.target,
            "serverNames":[reality.server_name],"shortIds":[reality.short_id],"privateKey":reality.private_key}}});
        let (mut doc, mut request) = fixture(input);
        request.shared_443 = true;
        let result = import(&mut doc, &request).unwrap();
        assert_eq!(result["pending"], json!([]));
        assert_eq!(doc.nodes[0].reality.private_key, reality.private_key);
        assert_eq!(doc.nodes[0].reality.public_key, reality.public_key);
        assert_eq!(doc.nodes[0].reality.short_id, reality.short_id);
        assert!(doc.lines[0].access.shared_port);
    }

    #[test]
    fn unsupported_clients_and_routes_only_create_names() {
        let mut inbound = vmess();
        inbound["settings"]["clients"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":id()}));
        let (mut doc, mut request) = fixture(inbound);
        let result = import(&mut doc, &request).unwrap();
        assert_eq!(result["imported"], json!([]));
        assert_eq!(doc.pending_imports[0].name, "original");
        assert!(doc.lines.is_empty());
        doc.pending_imports.clear();
        request.sources[0].config["inbounds"] = json!([vmess()]);
        request.sources[0].config["routing"] =
            json!({"rules":[{"domain":["example.com"],"outboundTag":"blocked"}]});
        import(&mut doc, &request).unwrap();
        assert!(doc.lines.is_empty());
        assert!(doc.pending_imports[0].reason.contains("路由"));
    }

    #[test]
    fn preserves_supported_ss_password_without_generating_credentials() {
        let password = "YWJjZGVmZ2hpamtsbW5vcA==";
        let (mut doc, request) = fixture(json!({"tag":"SS","port":23456,"protocol":"shadowsocks",
            "settings":{"method":"2022-blake3-aes-128-gcm","password":password,"network":"tcp,udp"}}));
        import(&mut doc, &request).unwrap();
        assert!(doc.pending_imports.is_empty());
        assert_eq!(doc.lines[0].access.password, password);
    }

    #[test]
    fn unknown_transport_does_not_silently_become_tcp() {
        let mut inbound = vmess();
        inbound["streamSettings"]["network"] = json!("ws");
        let (mut doc, request) = fixture(inbound);
        import(&mut doc, &request).unwrap();
        assert!(doc.lines.is_empty());
        assert!(doc.pending_imports[0].reason.contains("传输"));
    }
}
