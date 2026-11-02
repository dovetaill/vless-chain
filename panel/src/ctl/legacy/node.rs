use super::*;
use percent_encoding::percent_decode_str;
pub(super) fn parse(text: &str) -> Result<Data> {
    let mut d = UP
        .split_whitespace()
        .map(|k| (k.into(), String::new()))
        .collect::<Data>();
    for (k, v) in [
        ("FINGERPRINT", "chrome"),
        ("ALLOW_INSECURE", "false"),
        ("XHTTP_MODE", "auto"),
        ("XHTTP_EXTRA", "{}"),
    ] {
        d.insert(format!("UPSTREAM_{k}"), v.into());
    }
    if text.starts_with("vless://") {
        let n = url::Url::parse(text).map_err(|_| anyhow::anyhow!("VLESS 地址格式不正确"))?;
        ensure!(n.password().is_none(), "VLESS 链接不能包含密码段");
        for (k, v) in [
            (
                "UUID",
                percent_decode_str(n.username()).decode_utf8()?.into_owned(),
            ),
            (
                "ADDRESS",
                n.host_str().unwrap_or("").trim_matches(['[', ']']).into(),
            ),
            ("PORT", n.port().map_or(String::new(), |v| v.to_string())),
            (
                "NAME",
                percent_decode_str(n.fragment().unwrap_or(""))
                    .decode_utf8()?
                    .into_owned(),
            ),
            ("TRANSPORT", "raw".into()),
        ] {
            d.insert(format!("UPSTREAM_{k}"), v);
        }
        let mut q = BTreeMap::<String, Vec<String>>::new();
        for (k, v) in n.query_pairs() {
            q.entry(k.into_owned()).or_default().push(v.into_owned());
        }
        for (k, s) in [
            ("security", "SECURITY"),
            ("type", "TRANSPORT"),
            ("sni", "SNI"),
            ("flow", "FLOW"),
            ("fp", "FINGERPRINT"),
            ("pbk", "REALITY_PUBLIC_KEY"),
            ("sid", "REALITY_SHORT_ID"),
            ("pqv", "REALITY_MLDSA65_VERIFY"),
            ("path", "XHTTP_PATH"),
            ("host", "XHTTP_HOST"),
            ("mode", "XHTTP_MODE"),
            ("extra", "XHTTP_EXTRA"),
            ("alpn", "ALPN"),
        ] {
            if let Some(v) = q.get(k) {
                ensure!(v.len() == 1, "链接包含重复参数：{k}");
                d.insert(format!("UPSTREAM_{s}"), v[0].clone());
            }
        }
        ensure!(
            q.get("encryption")
                .is_none_or(|a| matches!(a[0].as_str(), "" | "none")),
            "当前仅支持 encryption=none 的落地"
        );
        let insecure = q
            .get("allowInsecure")
            .or_else(|| q.get("insecure"))
            .map(|v| json!(v[0]))
            .unwrap_or(json!(false));
        d.insert(
            "UPSTREAM_ALLOW_INSECURE".into(),
            boolean(&insecure)?.to_string(),
        );
    } else {
        let mut n: Value = serde_yaml_ng::from_str(text)
            .map_err(|_| anyhow::anyhow!("YAML/JSON 格式不正确；请粘贴完整的单个节点"))?;
        if let Some(p) = n.get_mut("proxies") {
            n = p.take();
        }
        if let Some(a) = n.as_array_mut() {
            ensure!(a.len() == 1, "一次只能导入一个节点，请粘贴目标节点");
            n = a.remove(0);
        }
        ensure!(n.is_object(), "请输入 vless:// 链接或单个 YAML/JSON 节点");
        ensure!(
            n.get("type").is_none_or(|v| v == "vless"),
            "只支持 type: vless 的节点"
        );
        ensure!(
            n.get("encryption")
                .is_none_or(|v| v.is_null() || v == "" || v == "none"),
            "当前仅支持 encryption=none 的落地"
        );
        for option in [
            "shadow-tls-opts",
            "restls-opts",
            "jls-opts",
            "ech-opts",
            "certificate",
            "private-key",
            "name-cert-verify",
        ] {
            ensure!(
                n.get(option)
                    .is_none_or(|v| v.is_null() || v == false || v == "" || v == &json!({})),
                "当前不能转换节点参数：{option}"
            );
        }
        ensure!(
            n["smux"]["enabled"] != true,
            "当前不能转换 smux，请使用不依赖 smux 的落地配置"
        );
        let empty = json!({});
        let r = n
            .get("reality-opts")
            .filter(|v| !v.is_null())
            .unwrap_or(&empty);
        ensure!(r.is_object(), "reality-opts 必须是对象");
        for (k, v) in [
            ("NAME", &n["name"]),
            ("ADDRESS", &n["server"]),
            ("PORT", &n["port"]),
            ("UUID", &n["uuid"]),
            ("TRANSPORT", &n["network"]),
            ("FLOW", &n["flow"]),
            ("SNI", n.get("servername").unwrap_or(&n["sni"])),
            ("REALITY_PUBLIC_KEY", &r["public-key"]),
            ("REALITY_SHORT_ID", &r["short-id"]),
            ("REALITY_MLDSA65_VERIFY", &r["mldsa65-verify"]),
        ] {
            put(&mut d, &format!("UPSTREAM_{k}"), v)?;
        }
        d.insert(
            "UPSTREAM_SECURITY".into(),
            if !r.as_object().unwrap().is_empty() {
                "reality"
            } else if boolean(&n["tls"])? {
                "tls"
            } else {
                ""
            }
            .into(),
        );
        d.insert(
            "UPSTREAM_FINGERPRINT".into(),
            n["client-fingerprint"]
                .as_str()
                .filter(|v| !v.is_empty())
                .unwrap_or("chrome")
                .into(),
        );
        d.insert(
            "UPSTREAM_ALLOW_INSECURE".into(),
            boolean(&n["skip-cert-verify"])?.to_string(),
        );
        let alpn = if let Some(a) = n["alpn"].as_array() {
            a.iter().map(clean).collect::<Result<Vec<_>>>()?.join(",")
        } else {
            clean(&n["alpn"])?
        };
        d.insert("UPSTREAM_ALPN".into(), alpn);
        let x = n
            .get("xhttp-opts")
            .filter(|v| !v.is_null())
            .unwrap_or(&empty);
        let options = x.as_object().context("xhttp-opts 必须是对象")?;
        if !options.is_empty() {
            d.insert(
                "UPSTREAM_TRANSPORT".into(),
                n["network"].as_str().unwrap_or("xhttp").into(),
            );
            for (k, v) in [
                ("PATH", &x["path"]),
                ("HOST", &x["host"]),
                ("MODE", x.get("mode").unwrap_or(&json!("auto"))),
            ] {
                put(&mut d, &format!("UPSTREAM_XHTTP_{k}"), v)?;
            }
            let mut extra = json!({});
            let translated = BTreeMap::from([
                ("headers", "headers"),
                ("no-grpc-header", "noGRPCHeader"),
                ("no-sse-header", "noSSEHeader"),
                ("x-padding-bytes", "xPaddingBytes"),
                ("sc-max-each-post-bytes", "scMaxEachPostBytes"),
                ("sc-min-posts-interval-ms", "scMinPostsIntervalMs"),
            ]);
            for (k, v) in options {
                if ["path", "host", "mode"].contains(&k.as_str()) {
                    continue;
                }
                if let Some(key) = translated.get(k.as_str()) {
                    extra[*key] = v.clone();
                } else if k == "reuse-settings" {
                    let translated = BTreeMap::from([
                        ("max-concurrency", "maxConcurrency"),
                        ("max-connections", "maxConnections"),
                        ("c-max-reuse-times", "cMaxReuseTimes"),
                        ("h-max-request-times", "hMaxRequestTimes"),
                        ("h-max-reusable-secs", "hMaxReusableSecs"),
                        ("h-keep-alive-period", "hKeepAlivePeriod"),
                    ]);
                    let mut xmux = json!({});
                    for (k, v) in v.as_object().context("reuse-settings 必须是对象")? {
                        let key = translated
                            .get(k.as_str())
                            .context("存在不能转换的 reuse-settings 参数")?;
                        xmux[*key] = v.clone();
                    }
                    extra["xmux"] = xmux;
                } else if k == "extra" && v.is_object() {
                    extra
                        .as_object_mut()
                        .unwrap()
                        .extend(v.as_object().unwrap().clone());
                } else {
                    anyhow::bail!(
                        "当前不能转换 xhttp-opts 参数：{k}；可改用带原生 extra 的分享链接"
                    );
                }
            }
            d.insert(
                "UPSTREAM_XHTTP_EXTRA".into(),
                serde_json::to_string(&extra)?,
            );
        }
    }
    for v in d.values() {
        clean(&json!(v))?;
    }
    if field(&d, "UPSTREAM_TRANSPORT") == "tcp" {
        d.insert("UPSTREAM_TRANSPORT".into(), "raw".into());
    }
    ensure!(
        matches!(field(&d, "UPSTREAM_TRANSPORT"), "" | "raw" | "xhttp"),
        "仅支持 TCP/raw 和 XHTTP 落地"
    );
    ensure!(
        matches!(field(&d, "UPSTREAM_SECURITY"), "" | "tls" | "reality"),
        "仅支持 TLS / REALITY 落地"
    );
    Ok(d)
}
pub(super) fn validate(d: &Data) -> Result<()> {
    for v in d.values() {
        clean(&json!(v))?;
    }
    ensure!(
        crate::compiler::host_valid(field(d, "UPSTREAM_ADDRESS")),
        "落地地址格式不正确"
    );
    ensure!(
        field(d, "UPSTREAM_PORT")
            .parse::<u16>()
            .is_ok_and(|v| v > 0),
        "落地端口必须为 1–65535"
    );
    let id = field(d, "UPSTREAM_UUID");
    ensure!(
        uuid::Uuid::parse_str(id).is_ok_and(|v| v.to_string().eq_ignore_ascii_case(id)),
        "落地 UUID 格式不正确"
    );
    ensure!(
        matches!(field(d, "UPSTREAM_TRANSPORT"), "raw" | "xhttp"),
        "请选择落地传输方式 raw 或 xhttp"
    );
    ensure!(
        matches!(field(d, "UPSTREAM_SECURITY"), "tls" | "reality"),
        "请选择落地安全方式 tls 或 reality"
    );
    let flow = field(d, "UPSTREAM_FLOW");
    ensure!(
        matches!(flow, "" | "xtls-rprx-vision" | "xtls-rprx-vision-udp443"),
        "不支持该落地 flow"
    );
    ensure!(
        field(d, "UPSTREAM_TRANSPORT") != "xhttp" || flow.is_empty(),
        "XHTTP + encryption=none 不能设置 Vision flow，请核对落地配置"
    );
    boolean(&json!(field(d, "UPSTREAM_ALLOW_INSECURE")))?;
    if field(d, "UPSTREAM_SECURITY") == "reality" {
        let key = field(d, "UPSTREAM_REALITY_PUBLIC_KEY");
        ensure!(
            key.len() == 43
                && key
                    .bytes()
                    .all(|v| v.is_ascii_alphanumeric() || b"_-".contains(&v)),
            "REALITY 落地需要有效的 public-key/pbk（43 位）"
        );
        let sid = field(d, "UPSTREAM_REALITY_SHORT_ID");
        ensure!(
            sid.len() <= 16 && sid.len() % 2 == 0 && sid.bytes().all(|v| v.is_ascii_hexdigit()),
            "REALITY short-id/sid 必须是最多 16 位、偶数长度的十六进制"
        );
    }
    if field(d, "UPSTREAM_TRANSPORT") == "xhttp" {
        ensure!(
            field(d, "UPSTREAM_XHTTP_PATH").starts_with('/'),
            "XHTTP path 必须以 / 开头"
        );
        ensure!(
            matches!(
                field(d, "UPSTREAM_XHTTP_MODE"),
                "auto" | "stream-one" | "stream-up" | "packet-up"
            ),
            "XHTTP mode 不正确"
        );
        ensure!(
            serde_json::from_str::<Value>(field(d, "UPSTREAM_XHTTP_EXTRA"))?.is_object(),
            "XHTTP extra 必须是 JSON 对象"
        );
    }
    Ok(())
}
pub(super) fn outbound(d: &Data) -> Result<Value> {
    validate(d)?;
    let get = |k| field(d, k);
    let mut settings = json!({"address":get("UPSTREAM_ADDRESS"),"port":get("UPSTREAM_PORT").parse::<u16>()?,"id":get("UPSTREAM_UUID"),"encryption":"none"});
    if !get("UPSTREAM_FLOW").is_empty() {
        settings["flow"] = json!(get("UPSTREAM_FLOW"));
    }
    let security = get("UPSTREAM_SECURITY");
    let mut stream = json!({"network":get("UPSTREAM_TRANSPORT"),"security":security});
    let sni = if get("UPSTREAM_SNI").is_empty() {
        get("UPSTREAM_ADDRESS")
    } else {
        get("UPSTREAM_SNI")
    };
    let fp = if get("UPSTREAM_FINGERPRINT").is_empty() {
        "chrome"
    } else {
        get("UPSTREAM_FINGERPRINT")
    };
    if security == "tls" {
        stream["tlsSettings"] = json!({"serverName":sni,"allowInsecure":boolean(&json!(get("UPSTREAM_ALLOW_INSECURE")))?,"fingerprint":fp});
        if !get("UPSTREAM_ALPN").is_empty() {
            stream["tlsSettings"]["alpn"] =
                json!(get("UPSTREAM_ALPN").split(',').collect::<Vec<_>>());
        }
    } else {
        stream["realitySettings"] = json!({"serverName":sni,"fingerprint":fp,"publicKey":get("UPSTREAM_REALITY_PUBLIC_KEY"),"shortId":get("UPSTREAM_REALITY_SHORT_ID")});
        if !get("UPSTREAM_REALITY_MLDSA65_VERIFY").is_empty() {
            stream["realitySettings"]["mldsa65Verify"] =
                json!(get("UPSTREAM_REALITY_MLDSA65_VERIFY"));
        }
    }
    if get("UPSTREAM_TRANSPORT") == "xhttp" {
        stream["xhttpSettings"] = json!({"path":get("UPSTREAM_XHTTP_PATH"),"host":get("UPSTREAM_XHTTP_HOST"),"mode":get("UPSTREAM_XHTTP_MODE"),"extra":serde_json::from_str::<Value>(get("UPSTREAM_XHTTP_EXTRA"))?});
    }
    Ok(
        json!({"tag":"upstream-vless","protocol":"vless","settings":settings,"streamSettings":stream}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_and_unsafe() {
        let d=parse("name: reality vision\ntype: vless\nserver: exit.example\nport: 443\nuuid: 00000000-0000-4000-8000-000000000001").unwrap();
        assert_eq!(field(&d, "UPSTREAM_SECURITY"), "");
        assert_eq!(field(&d, "UPSTREAM_TRANSPORT"), "");
        assert!(validate(&d).is_err());
        for text in [
            "!!python/object/apply:os.system ['touch /tmp/never']",
            "proxies: [{type: vless}, {type: vless}]",
            "{\"type\":\"socks5\"}",
            "{\"type\":\"vless\",\"smux\":{\"enabled\":true}}",
            "vless://a:password@exit.example:443?security=tls",
            "vless://a@exit.example:443?security=tls&security=reality",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
    }
    #[test]
    fn xhttp_uri_and_yaml() {
        let mut d=parse("vless://00000000-0000-4000-8000-000000000001@exit.example:443?security=tls&type=xhttp&sni=target.example&path=%2Fchannel%3Fa%3D1&extra=%7B%22headers%22%3A%7B%22X-Test%22%3A%22a%26b%22%7D%7D#%E8%8A%82%E7%82%B9").unwrap();
        validate(&d).unwrap();
        assert_eq!(field(&d, "UPSTREAM_NAME"), "节点");
        assert_eq!(
            outbound(&d).unwrap()["streamSettings"]["xhttpSettings"]["extra"]["headers"]["X-Test"],
            "a&b"
        );
        d.insert("UPSTREAM_FLOW".into(), "xtls-rprx-vision".into());
        assert!(validate(&d).unwrap_err().to_string().contains("XHTTP"));
        let d=parse("{\"type\":\"vless\",\"server\":\"exit.example\",\"port\":443,\"uuid\":\"00000000-0000-4000-8000-000000000001\",\"tls\":true,\"network\":\"xhttp\",\"xhttp-opts\":{\"path\":\"/path\",\"reuse-settings\":{\"max-concurrency\":8},\"no-grpc-header\":true},\"reality-opts\":{\"public-key\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"short-id\":\"001122\"}}").unwrap();
        validate(&d).unwrap();
        assert_eq!(
            outbound(&d).unwrap()["streamSettings"]["xhttpSettings"]["extra"]["xmux"]["maxConcurrency"],
            8
        );
    }
}
