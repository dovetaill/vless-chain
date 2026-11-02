use super::*;
use std::{collections::BTreeSet, path::Path};
pub(super) fn access(config: &Value, mut d: Data) -> Result<Data> {
    let mut ids = BTreeSet::new();
    let mut modes = BTreeSet::new();
    d.insert("TLS_PORT".into(), String::new());
    d.insert("REALITY_PORT".into(), String::new());
    for inbound in config["inbounds"].as_array().context("缺少入站")? {
        if inbound["protocol"] != "vless" {
            continue;
        }
        for client in inbound["settings"]["clients"]
            .as_array()
            .context("缺少客户端")?
        {
            ids.insert(client["id"].as_str().context("缺少 UUID")?.to_string());
        }
        let mode = inbound["streamSettings"]["security"].as_str().unwrap_or("");
        ensure!(
            matches!(mode, "tls" | "reality") && modes.insert(mode),
            "链接导出要求每种安全模式最多一个 TLS / REALITY 入站"
        );
        put(
            &mut d,
            &format!("{}_PORT", mode.to_uppercase()),
            &inbound["port"],
        )?;
        if mode == "reality" {
            let r = &inbound["streamSettings"]["realitySettings"];
            for (k, v) in [
                ("REALITY_PRIVATE_KEY", &r["privateKey"]),
                ("REALITY_SERVER_NAME", &r["serverNames"][0]),
                ("REALITY_SHORT_ID", &r["shortIds"][0]),
            ] {
                put(&mut d, k, v)?;
            }
            d.insert("REALITY_PUBLIC_KEY".into(), String::new());
        }
    }
    ensure!(
        ids.len() == 1 && !modes.is_empty(),
        "实例必须只有一个 UUID 才能导出或修改接入信息"
    );
    d.insert("UUID".into(), ids.into_iter().next().unwrap());
    d.insert(
        "SECURITY_MODE".into(),
        if modes.len() == 2 {
            "dual"
        } else {
            modes.into_iter().next().unwrap()
        }
        .into(),
    );
    Ok(d)
}
fn records() -> Result<Vec<Data>> {
    let root = Path::new(&std::env::var("MANAGER_DIR")?).join("instances");
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut entries = fs::read_dir(root)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    let mut records = vec![];
    for e in entries {
        let path = e.path().join("state.json");
        if !path.exists() {
            continue;
        }
        match read(path.to_str().context("路径无效")?).and_then(|v| data(&v)) {
            Ok(d) if field(&d, "INSTANCE_ID") == e.file_name().to_string_lossy() => records.push(d),
            _ => eprintln!("无法读取实例登记：{}", path.display()),
        }
    }
    Ok(records)
}
pub(super) fn records_action(action: &str, env: &Data) -> Result<()> {
    for d in records()? {
        let get = |k| field(&d, k);
        match action {
            "ports" => {
                for key in [
                    "TLS_PORT",
                    "REALITY_PORT",
                    "INTERNAL_HTTP_PORT",
                    "INTERNAL_HTTPS_PORT",
                ] {
                    if !get(key).is_empty() {
                        println!("{}", get(key));
                    }
                }
            }
            "list" => {
                let destination = if get("NODE_MODE") == "direct" {
                    "Internet".into()
                } else {
                    format!("{}:{}", get("UPSTREAM_ADDRESS"), get("UPSTREAM_PORT"))
                };
                let ports = [get("REALITY_PORT"), get("TLS_PORT")]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("/");
                println!(
                    "{}",
                    [
                        get("INSTANCE_ID"),
                        get("INSTANCE_NAME"),
                        get("NODE_MODE"),
                        &ports,
                        &destination,
                        get("SERVICE_NAME")
                    ]
                    .into_iter()
                    .map(|s| clean(&json!(s)))
                    .collect::<Result<Vec<_>>>()?
                    .join("\t")
                );
            }
            _ => {
                if action != "uuid-unique" || get("INSTANCE_ID") != field(env, "INSTANCE_ID") {
                    ensure!(
                        !get("UUID").eq_ignore_ascii_case(field(env, "UUID")),
                        "该 UUID 已被实例 {} 使用",
                        get("INSTANCE_ID")
                    );
                }
                if action == "unique" {
                    ensure!(
                        get("APP_DIR") != field(env, "APP_DIR")
                            && get("SERVICE_NAME") != field(env, "SERVICE_NAME"),
                        "该目录或服务已登记为实例 {}",
                        get("INSTANCE_ID")
                    );
                }
            }
        }
    }
    Ok(())
}
pub(super) fn clients(action: &str, d: &Data) -> Result<()> {
    let get = |k| field(d, k);
    if action == "client-yaml" {
        println!("proxies:");
    }
    for mode in ["reality", "tls"] {
        let port = get(if mode == "reality" {
            "REALITY_PORT"
        } else {
            "TLS_PORT"
        });
        if port.is_empty() || ![mode, "dual"].contains(&get("SECURITY_MODE")) {
            continue;
        }
        let name = format!(
            "{}-{mode}",
            if get("INSTANCE_NAME").is_empty() {
                get("DOMAIN")
            } else {
                get("INSTANCE_NAME")
            }
        );
        let sni = if mode == "reality" {
            get("REALITY_SERVER_NAME")
        } else {
            get("DOMAIN")
        };
        let fp = if get("REALITY_FINGERPRINT").is_empty() {
            "chrome"
        } else {
            get("REALITY_FINGERPRINT")
        };
        let mut n = json!({"name":name,"type":"vless","server":get("DOMAIN"),"port":port.parse::<u16>()?,"uuid":get("UUID"),"network":"tcp","tls":true,"flow":"xtls-rprx-vision","skip-cert-verify":false,"client-fingerprint":fp,"servername":sni});
        if mode == "reality" {
            n["reality-opts"] =
                json!({"public-key":get("REALITY_PUBLIC_KEY"),"short-id":get("REALITY_SHORT_ID")});
        }
        if action == "client-yaml" {
            println!("  - {}", serde_json::to_string(&n)?);
        } else {
            let mut url =
                url::Url::parse(&format!("vless://{}@{}:{port}", get("UUID"), get("DOMAIN")))?;
            {
                let mut q = url.query_pairs_mut();
                q.extend_pairs([
                    ("encryption", "none"),
                    ("type", "tcp"),
                    ("flow", "xtls-rprx-vision"),
                    ("security", mode),
                    ("fp", fp),
                    ("sni", sni),
                ]);
                if mode == "reality" {
                    q.extend_pairs([
                        ("pbk", get("REALITY_PUBLIC_KEY")),
                        ("sid", get("REALITY_SHORT_ID")),
                    ]);
                }
            }
            url.set_fragment(Some(&name));
            println!("{url}");
        }
    }
    Ok(())
}
pub(super) fn legacy(app: &str) -> Result<Data> {
    let root = Path::new(app).canonicalize()?;
    let config = read(root.join("config.json").to_str().context("路径无效")?)?;
    let mut d = environment(&format!("{STATE} {UP}"));
    for (k, v) in [
        ("APP_DIR", root.display().to_string()),
        ("INSTANCE_IMPORTED", "true".into()),
        ("NODE_MODE", "direct".into()),
        ("CERT_MODE", "existing".into()),
    ] {
        d.insert(k.into(), v);
    }
    for filename in ["install.env", "reality.env"] {
        let p = root.join(filename);
        if p.exists() {
            for l in fs::read_to_string(p)?.lines() {
                if let Some((k, v)) = l.split_once('=') {
                    if STATE.split_whitespace().any(|s| s == k)
                        && ![
                            "INSTANCE_ID",
                            "INSTANCE_NAME",
                            "INSTANCE_IMPORTED",
                            "APP_DIR",
                            "SERVICE_NAME",
                            "XRAY_BIN",
                            "INIT_SYSTEM",
                        ]
                        .contains(&k)
                    {
                        d.insert(k.into(), clean(&json!(v))?);
                    }
                }
            }
        }
    }
    for inbound in config["inbounds"].as_array().context("缺少入站")? {
        ensure!(
            inbound["protocol"] == "vless",
            "旧配置含非 VLESS 入站，暂不能导入"
        );
        let stream = &inbound["streamSettings"];
        ensure!(
            matches!(
                stream
                    .get("network")
                    .or_else(|| stream.get("method"))
                    .and_then(Value::as_str)
                    .unwrap_or("raw"),
                "raw" | "tcp"
            ),
            "旧配置仅能导入 TCP/raw 入站"
        );
        if stream["security"] == "tls" {
            put(
                &mut d,
                "CERT_FILE",
                &stream["tlsSettings"]["certificates"][0]["certificateFile"],
            )?;
            put(
                &mut d,
                "KEY_FILE",
                &stream["tlsSettings"]["certificates"][0]["keyFile"],
            )?;
        } else if stream["security"] == "reality" {
            put(
                &mut d,
                "REALITY_TARGET",
                stream["realitySettings"]
                    .get("target")
                    .unwrap_or(&stream["realitySettings"]["dest"]),
            )?;
        }
    }
    d = access(&config, d)?;
    let routes = config["routing"]["rules"]
        .as_array()
        .context("缺少路由")?
        .iter()
        .filter(|r| r["inboundTag"].as_array().is_some_and(|a| !a.is_empty()))
        .map(|r| r["outboundTag"].as_str().unwrap_or(""))
        .collect::<BTreeSet<_>>();
    ensure!(
        routes.len() == 1
            && routes
                .iter()
                .all(|r| matches!(*r, "direct" | "upstream-vless")),
        "旧配置路由结构不能安全导入"
    );
    if routes.contains("upstream-vless") {
        let o = config["outbounds"]
            .as_array()
            .context("缺少出站")?
            .iter()
            .find(|o| o["tag"] == "upstream-vless")
            .context("缺少落地出站")?;
        let mut settings = o["settings"].clone();
        if let Some(vnext) = settings.get("vnext") {
            let server = vnext[0].clone();
            settings = server["users"][0].clone();
            settings["address"] = server["address"].clone();
            settings["port"] = server["port"].clone();
        }
        let s = &o["streamSettings"];
        let r = &s["realitySettings"];
        let t = &s["tlsSettings"];
        let x = &s["xhttpSettings"];
        d.insert("NODE_MODE".into(), "relay".into());
        for (k, v) in [
            ("ADDRESS", &settings["address"]),
            ("PORT", &settings["port"]),
            ("UUID", &settings["id"]),
            ("FLOW", &settings["flow"]),
            ("SECURITY", &s["security"]),
            (
                "TRANSPORT",
                s.get("network")
                    .or_else(|| s.get("method"))
                    .unwrap_or(&json!("raw")),
            ),
            ("SNI", r.get("serverName").unwrap_or(&t["serverName"])),
            (
                "FINGERPRINT",
                r.get("fingerprint")
                    .or_else(|| t.get("fingerprint"))
                    .unwrap_or(&json!("chrome")),
            ),
            (
                "REALITY_PUBLIC_KEY",
                r.get("publicKey").unwrap_or(&r["password"]),
            ),
            ("REALITY_SHORT_ID", &r["shortId"]),
            ("REALITY_MLDSA65_VERIFY", &r["mldsa65Verify"]),
            ("XHTTP_PATH", &x["path"]),
            ("XHTTP_HOST", &x["host"]),
            ("XHTTP_MODE", x.get("mode").unwrap_or(&json!("auto"))),
        ] {
            put(&mut d, &format!("UPSTREAM_{k}"), v)?;
        }
        d.insert(
            "UPSTREAM_ALLOW_INSECURE".into(),
            boolean(&t["allowInsecure"])?.to_string(),
        );
        d.insert(
            "UPSTREAM_XHTTP_EXTRA".into(),
            serde_json::to_string(x.get("extra").unwrap_or(&json!({})))?,
        );
        d.insert(
            "UPSTREAM_ALPN".into(),
            t["alpn"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default(),
        );
        node::validate(&d)?;
    }
    Ok(d)
}
