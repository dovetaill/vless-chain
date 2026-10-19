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
    #[serde(default)]
    pub preserve_config: bool,
    pub node_id: String,
    pub sources: Vec<Source>,
    #[serde(default)]
    pub preview: bool,
    #[serde(default)]
    pub shared_443: bool,
}

#[derive(Deserialize)]
pub struct Source {
    #[serde(default)]
    pub source_inbounds: Vec<Value>,
    #[serde(default)]
    pub asset_dir: String,
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
    let unknown: Vec<_> = object
        .keys()
        .filter(|k| !keys.contains(&k.as_str()))
        .collect();
    ensure!(
        unknown.is_empty(),
        "尚不支持的配置字段：{}",
        unknown.into_iter().cloned().collect::<Vec<_>>().join("、")
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

fn outbound_link(outbound: &Value) -> Result<String> {
    only(outbound, &["tag", "protocol", "settings", "streamSettings"])?;
    ensure!(
        outbound["protocol"] == "vless",
        "仅支持直连或单个 VLESS 出口"
    );
    let settings = &outbound["settings"];
    let (server, user) = if let Some(servers) = settings.get("vnext") {
        only(settings, &["vnext"])?;
        let servers = servers.as_array().context("VLESS vnext 必须是数组")?;
        ensure!(servers.len() == 1, "多个 VLESS 出口需要手动配置");
        let server = &servers[0];
        only(server, &["address", "port", "users"])?;
        let users = server["users"].as_array().context("缺少 VLESS 出口用户")?;
        ensure!(users.len() == 1, "多个 VLESS 出口用户需要手动配置");
        only(&users[0], &["id", "encryption", "flow"])?;
        (server, &users[0])
    } else {
        only(settings, &["address", "port", "id", "encryption", "flow"])?;
        (settings, settings)
    };
    let host = server["address"].as_str().context("VLESS 出口缺少地址")?;
    ensure!(crate::compiler::host_valid(host), "VLESS 出口地址无效");
    let port = server["port"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p > 0)
        .context("VLESS 出口端口无效")?;
    let uuid = uuid::Uuid::parse_str(text(&user["id"]))?;
    let mut url = url::Url::parse(&format!(
        "vless://{uuid}@{}",
        crate::compiler::address(host, port)
    ))?;
    let stream = &outbound["streamSettings"];
    only(
        stream,
        &[
            "network",
            "method",
            "security",
            "tcpSettings",
            "rawSettings",
            "tlsSettings",
            "realitySettings",
            "xhttpSettings",
        ],
    )?;
    for key in ["tcpSettings", "rawSettings"] {
        only(&stream[key], &["header"])?;
        only(&stream[key]["header"], &["type"])?;
        ensure!(
            matches!(text(&stream[key]["header"]["type"]), "" | "none"),
            "出口 TCP 伪装需要手动配置"
        );
    }
    let mut pair = |key: &str, value: &Value| -> Result<()> {
        if !value.is_null() {
            url.query_pairs_mut()
                .append_pair(key, value.as_str().context("VLESS 出口参数必须是文本")?);
        }
        Ok(())
    };
    pair("encryption", &user["encryption"])?;
    pair("flow", &user["flow"])?;
    pair(
        "type",
        stream
            .get("method")
            .filter(|v| !v.is_null())
            .unwrap_or(&stream["network"]),
    )?;
    pair("security", &stream["security"])?;
    match text(&stream["security"]) {
        "reality" => {
            ensure!(empty(&stream["tlsSettings"]), "出口包含多组安全设置");
            let r = &stream["realitySettings"];
            only(
                r,
                &[
                    "serverName",
                    "publicKey",
                    "password",
                    "shortId",
                    "fingerprint",
                    "spiderX",
                    "mldsa65Verify",
                ],
            )?;
            ensure!(
                r["publicKey"].is_null()
                    || r["password"].is_null()
                    || r["publicKey"] == r["password"],
                "出口 REALITY 公钥不一致"
            );
            for (field, query) in [
                ("serverName", "sni"),
                ("shortId", "sid"),
                ("fingerprint", "fp"),
                ("spiderX", "spx"),
                ("mldsa65Verify", "pqv"),
            ] {
                pair(query, &r[field])?;
            }
            pair(
                "pbk",
                r.get("publicKey")
                    .filter(|v| !v.is_null())
                    .unwrap_or(&r["password"]),
            )?;
        }
        "tls" => {
            ensure!(empty(&stream["realitySettings"]), "出口包含多组安全设置");
            let tls = &stream["tlsSettings"];
            only(tls, &["serverName", "fingerprint", "allowInsecure", "alpn"])?;
            pair("sni", &tls["serverName"])?;
            pair("fp", &tls["fingerprint"])?;
            if !tls["allowInsecure"].is_null() {
                ensure!(
                    tls["allowInsecure"] == false,
                    "出口跳过证书验证需要手动配置"
                );
                pair("allowInsecure", &json!("false"))?;
            }
            if !tls["alpn"].is_null() {
                let alpn = tls["alpn"]
                    .as_array()
                    .context("出口 ALPN 必须是数组")?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .filter(|s| !s.is_empty() && !s.contains(','))
                            .context("出口 ALPN 无效")
                    })
                    .collect::<Result<Vec<_>>>()?;
                ensure!(!alpn.is_empty(), "出口 ALPN 为空，需要手动配置");
                pair("alpn", &json!(alpn.join(",")))?;
            }
        }
        _ => anyhow::bail!("VLESS 出口需要 TLS 或 REALITY"),
    }
    let xhttp = &stream["xhttpSettings"];
    if !empty(xhttp) {
        only(xhttp, &["path", "host", "mode", "extra"])?;
        for field in ["path", "host", "mode"] {
            pair(field, &xhttp[field])?;
        }
        if !xhttp["extra"].is_null() {
            ensure!(xhttp["extra"].is_object(), "XHTTP extra 必须是对象");
            pair("extra", &json!(serde_json::to_string(&xhttp["extra"])?))?;
        }
    }
    let link = url.to_string();
    let converted = crate::compiler::external_outbound(&link, "check")?;
    ensure!(
        empty(xhttp) || converted["streamSettings"]["network"] == "xhttp",
        "非 XHTTP 出口包含 XHTTP 设置"
    );
    Ok(link)
}

fn global(config: &Value) -> Result<Option<String>> {
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
    let first = out.first().context("缺少默认出口")?;
    let link = if first["protocol"] == "freedom" {
        only(first, &["tag", "protocol", "settings", "streamSettings"])?;
        ensure!(
            empty(&first["settings"]) && empty(&first["streamSettings"]),
            "自定义直连出口需要手动配置"
        );
        None
    } else {
        Some(outbound_link(first)?)
    };
    only(&config["routing"], &["rules", "domainStrategy"])?;
    for rule in config["routing"]["rules"].as_array().into_iter().flatten() {
        only(rule, &["type", "inboundTag", "outboundTag"]).context("自定义路由需要手动配置")?;
        ensure!(
            matches!(text(&rule["type"]), "" | "field")
                && ((rule["inboundTag"] == json!(["api"]) && rule["outboundTag"] == "api")
                    || (!text(&out[0]["tag"]).is_empty() && rule["outboundTag"] == out[0]["tag"])),
            "自定义路由需要手动配置"
        );
    }
    Ok(link)
}

fn convert(
    doc: &mut Document,
    node_id: &str,
    inbound: &Value,
    id: String,
    title: String,
    shared: bool,
    local_source: bool,
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
            "method",
            "security",
            "tcpSettings",
            "rawSettings",
            "xhttpSettings",
            "realitySettings",
            "tlsSettings",
        ],
    )?;
    let network = stream
        .get("method")
        .filter(|v| !v.is_null())
        .unwrap_or(&stream["network"]);
    access.transport.network = match text(network) {
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
    ensure!(doc.nodes.iter().any(|n| n.id == node_id), "本机节点不存在");
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
                    "limitFallbackUpload",
                    "limitFallbackDownload",
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
                local_source
                    || (host.contains('.') && !host.starts_with("127.") && host != "localhost"),
                "REALITY 容器目标需要手动配置"
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
            access.reality = Some(RealityTarget {
                target: reality.target.clone(),
                server_name: reality.server_name.clone(),
                credentials: Some(RealityCredentials {
                    private_key: reality.private_key,
                    public_key: reality.public_key,
                    short_id: reality.short_id,
                }),
                limit_fallback_upload: serde_json::from_value(r["limitFallbackUpload"].clone())?,
                limit_fallback_download: serde_json::from_value(
                    r["limitFallbackDownload"].clone(),
                )?,
            });
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
        if request.preserve_config {
            let digest = Sha256::digest(format!("{}:complete", source.id));
            let id = uuid::Uuid::from_slice(&digest[..16])?.to_string();
            let profile = LegacyConfig {
                id: id.clone(),
                node_id: request.node_id.clone(),
                name: name(&source.name),
                config: source.config.clone(),
                asset_dir: source.asset_dir.clone(),
                source_inbounds: source.source_inbounds.clone(),
                enabled: true,
            };
            let result = (|| -> Result<()> {
                ensure!(source.error.is_empty(), "{}", source.error);
                crate::legacy_config::validate(&profile)?;
                if let Some(old) = doc.legacy_configs.iter().find(|p| p.id == id) {
                    ensure!(
                        old == &profile,
                        "该旧配置已经导入且内容已改变，请在面板中编辑，避免覆盖"
                    );
                } else {
                    let mut candidate = doc.clone();
                    candidate.legacy_configs.push(profile.clone());
                    compile(&candidate)?;
                    doc.legacy_configs.push(profile.clone());
                }
                Ok(())
            })();
            match result {
                Ok(()) => {
                    for port in crate::legacy_config::ports(&profile.config)? {
                        let reality_target = profile.config["inbounds"].as_array().into_iter().flatten()
                            .find(|i| i["port"].as_u64() == Some(port as u64))
                            .and_then(|i| {
                                let r = &i["streamSettings"]["realitySettings"];
                                r.get("target").filter(|v| !v.is_null()).or_else(|| r.get("dest"))
                            }).and_then(Value::as_str);
                        imported.push(json!({"id":id,"name":profile.name,"port":port,"protocol":"原始 Xray 配置",
                        "can_share":false,"sni":"","chain":[],"preserved":true,"reality_target":reality_target}));
                    }
                }
                Err(error) => {
                    pending.push(json!({"id":id,"name":profile.name,"reason":format!("{error:#}")}))
                }
            }
            continue;
        }
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
            let title = name(&if !text(&entry["remark"]).is_empty() {
                text(&entry["remark"]).to_owned()
            } else if !text(&entry["tag"]).is_empty() {
                format!("{} / {}", source.name, text(&entry["tag"]))
            } else {
                source.name.clone()
            });
            let before = doc.clone();
            let result = (|| {
                ensure!(source.error.is_empty(), "{}", source.error);
                let outbound = global(&source.config)?;
                let mut line = convert(
                    doc,
                    &request.node_id,
                    entry,
                    id.clone(),
                    title.clone(),
                    request.shared_443,
                    source.id.starts_with("systemd:") || source.id.starts_with("openrc:"),
                )?;
                if let Some(url) = outbound {
                    let external_id =
                        if let Some(existing) = doc.externals.iter().find(|e| e.url == url) {
                            existing.id.clone()
                        } else {
                            let digest = Sha256::digest(format!("{}:outbound", source.id));
                            let id = uuid::Uuid::from_slice(&digest[..16])?.to_string();
                            ensure!(
                                !doc.externals.iter().any(|e| e.id == id),
                                "原出口已导入但参数不一致"
                            );
                            doc.externals.push(External {
                                id: id.clone(),
                                name: name(&format!("{} · 出口", source.name)),
                                url,
                            });
                            id
                        };
                    line.chain = vec![format!("external:{external_id}")];
                }
                doc.lines.push(line.clone());
                compile(doc)?;
                Ok::<_, anyhow::Error>(line)
            })();
            match result {
                Ok(line) => imported.push(json!({"id":id,"name":title,"port":line.hops[0].port,"protocol":line.access.protocol,
                    "can_share": line.access.protocol == Protocol::Vless && line.access.tls.mode != "manual",
                    "sni":line.access.server_name(doc.nodes.iter().find(|n| n.id == request.node_id).unwrap()),
                    "reality_target":line.access.reality.as_ref().map(|r| &r.target), "chain":line.chain})),
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
            preserve_config: false,
            node_id: node.id.clone(),
            preview: false,
            shared_443: false,
            sources: vec![Source {
                asset_dir: String::new(),
                source_inbounds: Vec::new(),
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
    fn complete_xui_import_preserves_users_sniffing_routes_and_null_fields() {
        let (mut doc, mut request) = fixture(vmess());
        request.preserve_config = true;
        let config = &mut request.sources[0].config;
        config["inbounds"][0]["settings"]["clients"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":id()}));
        config["inbounds"][0]["sniffing"] = json!({"enabled":true,"destOverride":["http","tls"]});
        config["routing"] = json!({"rules":[{"ip":["geoip:private"],"outboundTag":"blocked"}]});
        config["outbounds"]
            .as_array_mut()
            .unwrap()
            .push(json!({"tag":"blocked","protocol":"blackhole"}));
        config["dns"] = json!({"servers":["1.1.1.1"]});
        config["transport"] = Value::Null;
        config["reverse"] = Value::Null;
        config["fakeDns"] = Value::Null;
        let original = config.clone();
        let result = import(&mut doc, &request).unwrap();
        assert_eq!(result["pending"], json!([]));
        assert_eq!(doc.legacy_configs[0].config, original);
        assert!(doc.lines.is_empty());
        import(&mut doc, &request).unwrap();
        assert_eq!(doc.legacy_configs.len(), 1);
        let compiled = compile(&doc).unwrap();
        assert_eq!(compiled[&request.node_id]["_legacy"][0]["config"], original);
        assert!(
            crate::gateway::xray_config(&compiled[&request.node_id])
                .get("_legacy")
                .is_none()
        );
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
        let keys = doc.lines[0]
            .access
            .reality
            .as_ref()
            .unwrap()
            .credentials
            .as_ref()
            .unwrap();
        assert_eq!(keys.private_key, reality.private_key);
        assert_eq!(keys.public_key, reality.public_key);
        assert_eq!(keys.short_id, reality.short_id);
        assert!(doc.lines[0].access.shared_port);
    }

    #[test]
    fn preserves_local_reality_target_method_and_fallback_limits() {
        let reality = Reality::new("127.0.0.1:18443".into(), "cover.example.com".into());
        let uuid = id();
        let upload = json!({"afterBytes":0,"bytesPerSec":1048576,"burstBytesPerSec":2097152});
        let download = json!({"afterBytes":0,"bytesPerSec":4194304,"burstBytesPerSec":8388608});
        let input = json!({"tag":"old reality","listen":"0.0.0.0","port":443,"protocol":"vless",
            "settings":{"decryption":"none","clients":[{"id":uuid,"flow":"xtls-rprx-vision"}]},
            "streamSettings":{"method":"raw","network":"ws","security":"reality","realitySettings":{
                "target":reality.target,"serverNames":[reality.server_name],"shortIds":[reality.short_id],
                "privateKey":reality.private_key,"limitFallbackUpload":upload,"limitFallbackDownload":download}}});
        let (mut doc, mut request) = fixture(input);
        request.sources[0].config["outbounds"][0]["tag"] = json!("direct");
        request.sources[0].config["routing"] = json!({"domainStrategy":"AsIs","rules":[
            {"type":"field","inboundTag":["old reality"],"outboundTag":"direct"}]});
        let result = import(&mut doc, &request).unwrap();
        assert_eq!(result["pending"], json!([]));
        let restored: Document =
            serde_json::from_value(serde_json::to_value(&doc).unwrap()).unwrap();
        let configs = compile(&restored).unwrap();
        let inbound = &configs[&doc.nodes[0].id]["inbounds"][0];
        assert_eq!(inbound["settings"]["clients"][0]["id"], uuid);
        let settings = &inbound["streamSettings"]["realitySettings"];
        assert_eq!(settings["target"], reality.target);
        assert_eq!(settings["privateKey"], reality.private_key);
        assert_eq!(settings["shortIds"], json!([reality.short_id]));
        assert_eq!(settings["limitFallbackUpload"], upload);
        assert_eq!(settings["limitFallbackDownload"], download);
    }

    #[test]
    fn local_reality_target_requires_a_host_service() {
        for kind in ["systemd", "openrc", "docker", "unmanaged"] {
            let reality = Reality::new("127.0.0.1:18443".into(), "cover.example.com".into());
            let (mut doc, mut request) = fixture(json!({"port":443,"protocol":"vless",
                "settings":{"clients":[{"id":id(),"flow":"xtls-rprx-vision"}]},
                "streamSettings":{"security":"reality","realitySettings":{"target":reality.target,
                    "serverNames":[reality.server_name],"shortIds":[reality.short_id],
                    "privateKey":reality.private_key}}}));
            request.sources[0].id = format!("{kind}:xray:/config.json");
            let result = import(&mut doc, &request).unwrap();
            if matches!(kind, "systemd" | "openrc") {
                assert_eq!(result["pending"], json!([]));
                assert_eq!(
                    doc.lines[0].access.reality.as_ref().unwrap().target,
                    reality.target
                );
            } else {
                assert!(doc.lines.is_empty());
                assert_eq!(doc.pending_imports.len(), 1);
            }
        }
    }

    #[test]
    fn invalid_fallback_limits_remain_pending() {
        for limit in [
            json!({"bytesPerSec":-1}),
            json!({"unknown":1}),
            json!({"afterBytes":"1"}),
        ] {
            let reality = Reality::new("cover.example.com:443".into(), "cover.example.com".into());
            let (mut doc, request) = fixture(json!({"port":443,"protocol":"vless",
                "settings":{"clients":[{"id":id(),"flow":"xtls-rprx-vision"}]},
                "streamSettings":{"security":"reality","realitySettings":{"target":reality.target,
                    "serverNames":[reality.server_name],"shortIds":[reality.short_id],
                    "privateKey":reality.private_key,"limitFallbackUpload":limit}}}));
            let result = import(&mut doc, &request).unwrap();
            assert_eq!(result["imported"], json!([]));
            assert_eq!(doc.pending_imports.len(), 1);
        }
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
        assert_eq!(doc.pending_imports[0].name, "old / original");
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

    fn native_fixture() -> (Document, Import) {
        let (doc, mut request) = fixture(vmess());
        request.sources =
            serde_json::from_str(include_str!("../tests/fixtures/native-v3.json")).unwrap();
        (doc, request)
    }

    #[test]
    fn native_instances_keep_each_identity_and_external_exit() {
        let (mut doc, request) = native_fixture();
        let original_node = serde_json::to_value(&doc.nodes[0]).unwrap();
        let result = import(&mut doc, &request).unwrap();
        assert_eq!(result["pending"], json!([]));
        assert_eq!(doc.lines.len(), 4);
        assert_eq!(doc.externals.len(), 3);
        assert_eq!(serde_json::to_value(&doc.nodes[0]).unwrap(), original_node);
        let restored: Document =
            serde_json::from_value(serde_json::to_value(&doc).unwrap()).unwrap();
        let compiled = compile(&restored).unwrap();
        let config = &compiled[&request.node_id];
        for (source, line) in request.sources.iter().zip(&restored.lines) {
            let original = &source.config["inbounds"][0];
            let tag = format!("line-{}-in", line.id);
            let inbound = config["inbounds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["tag"] == tag)
                .unwrap();
            let listener = config["_chain"].as_array().unwrap().iter()
                .find(|v| v["id"] == line.id).unwrap();
            assert_eq!(listener["port"], original["port"]);
            assert_eq!(listener["backend"], inbound["port"]);
            assert_eq!(
                inbound["settings"]["clients"][0]["id"],
                original["settings"]["clients"][0]["id"]
            );
            let mut expected_reality = original["streamSettings"]["realitySettings"].clone();
            expected_reality.as_object_mut().unwrap().remove("xver");
            assert_eq!(
                inbound["streamSettings"]["realitySettings"],
                expected_reality
            );
            let keys = line
                .access
                .reality
                .as_ref()
                .unwrap()
                .credentials
                .as_ref()
                .unwrap();
            let share = url::Url::parse(&crate::compiler::share(line, &restored).unwrap()).unwrap();
            let query: std::collections::BTreeMap<_, _> =
                share.query_pairs().into_owned().collect();
            assert_eq!(query["pbk"], keys.public_key);
            assert_eq!(query["sid"], keys.short_id);
            let mihomo = crate::access::mihomo_node(line, &restored).unwrap();
            assert_eq!(mihomo["reality-opts"]["public-key"], keys.public_key);
            assert_eq!(mihomo["reality-opts"]["short-id"], keys.short_id);
            let outbound_tag = format!("line-{}-out", line.id);
            let outbound = config["outbounds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["tag"] == outbound_tag)
                .unwrap();
            let exit = &source.config["outbounds"][0];
            if exit["protocol"] == "freedom" {
                assert!(line.chain.is_empty());
                assert_eq!(outbound["protocol"], "freedom");
            } else {
                assert_eq!(line.chain.len(), 1);
                let external = restored
                    .externals
                    .iter()
                    .find(|e| line.chain[0] == format!("external:{}", e.id))
                    .unwrap();
                assert!(external.url.starts_with("vless://"));
                let server = &outbound["settings"]["vnext"][0];
                assert_eq!(server["address"], exit["settings"]["address"]);
                assert_eq!(server["port"], exit["settings"]["port"]);
                assert_eq!(server["users"][0]["id"], exit["settings"]["id"]);
                assert_eq!(server["users"][0]["flow"], exit["settings"]["flow"]);
                for field in ["network", "security"] {
                    assert_eq!(
                        outbound["streamSettings"][field],
                        exit["streamSettings"][field]
                    );
                }
                for (key, value) in exit["streamSettings"]["realitySettings"]
                    .as_object()
                    .unwrap()
                {
                    assert_eq!(&outbound["streamSettings"]["realitySettings"][key], value);
                }
            }
            assert!(
                config["routing"]["rules"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["inboundTag"] == json!([format!("line-{}-meter-in", line.id)]) && r["outboundTag"] == outbound_tag)
            );
        }
        import(&mut doc, &request).unwrap();
        assert_eq!((doc.lines.len(), doc.externals.len()), (4, 3));
        let mut invalid = restored.clone();
        invalid.lines[0]
            .access
            .reality
            .as_mut()
            .unwrap()
            .credentials
            .as_mut()
            .unwrap()
            .public_key = "invalid".into();
        assert!(compile(&invalid).is_err());
    }

    #[test]
    fn vnext_tls_xhttp_exit_preserves_transport_and_deduplicates() {
        let (mut doc, mut request) = native_fixture();
        request.sources.truncate(2);
        let outbound = json!({"tag":"upstream-vless","protocol":"vless",
            "settings":{"vnext":[{"address":"2001:db8::1","port":8443,"users":[{"id":id(),"encryption":"none"}]}]},
            "streamSettings":{"method":"xhttp","security":"tls","tlsSettings":{
                "serverName":"exit.example.com","fingerprint":"firefox","allowInsecure":false,"alpn":["h2","http/1.1"]},
                "xhttpSettings":{"path":"/a?b=c","host":"cdn.example.com","mode":"packet-up","extra":{"scMaxEachPostBytes":12345}}}});
        for source in &mut request.sources {
            source.config["outbounds"][0] = outbound.clone();
            source.config["routing"]["rules"][0]["outboundTag"] = json!("upstream-vless");
        }
        assert_eq!(import(&mut doc, &request).unwrap()["pending"], json!([]));
        assert_eq!(doc.externals.len(), 1);
        assert_eq!(doc.lines[0].chain, doc.lines[1].chain);
        let converted =
            crate::compiler::external_outbound(&doc.externals[0].url, "upstream-vless").unwrap();
        assert_eq!(converted["settings"]["vnext"][0]["address"], "2001:db8::1");
        assert_eq!(converted["streamSettings"]["network"], "xhttp");
        assert_eq!(
            converted["streamSettings"]["xhttpSettings"],
            outbound["streamSettings"]["xhttpSettings"]
        );
        for field in ["serverName", "fingerprint", "alpn"] {
            assert_eq!(
                converted["streamSettings"]["tlsSettings"][field],
                outbound["streamSettings"]["tlsSettings"][field]
            );
        }
    }

    #[test]
    fn unsupported_exit_or_failed_compile_never_leaves_an_external() {
        for case in ["mux", "routing", "multiple-users", "port-conflict"] {
            let (mut doc, mut request) = native_fixture();
            request.sources.remove(0);
            request.sources.truncate(1);
            let config = &mut request.sources[0].config;
            match case {
                "mux" => config["outbounds"][0]["mux"] = json!({"enabled":true}),
                "routing" => config["routing"]["rules"][0]["outboundTag"] = json!("direct"),
                "multiple-users" => {
                    config["outbounds"][0]["settings"] = json!({"vnext":[{"address":"exit.example.com","port":443,"users":[{"id":id()},{"id":id()}]}]})
                }
                "port-conflict" => {
                    let mut original = native_fixture().1;
                    original.sources.truncate(1);
                    original.sources[0].config["inbounds"][0]["port"] =
                        config["inbounds"][0]["port"].clone();
                    original.node_id = request.node_id.clone();
                    assert_eq!(import(&mut doc, &original).unwrap()["pending"], json!([]));
                }
                _ => unreachable!(),
            }
            let count = doc.lines.len();
            let result = import(&mut doc, &request).unwrap();
            assert_eq!(result["imported"], json!([]), "{case}");
            assert_eq!(doc.lines.len(), count);
            assert!(doc.externals.is_empty(), "{case}");
            assert_eq!(doc.pending_imports.len(), 1);
        }
    }
}
