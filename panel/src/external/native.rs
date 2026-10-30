//! Field schema follows Mihomo v1.19.33 adapter/outbound structs.
use super::*;
use std::sync::LazyLock;
static SCHEMA: LazyLock<Value> = LazyLock::new(|| serde_json::from_str(include_str!("mihomo-schema.json")).unwrap());

fn typed(value: &Value, ty: &str, path: &str) -> Result<()> {
    let ty = ty.trim_start_matches('*');
    if let Some(item) = ty.strip_prefix("[]") {
        for v in value.as_array().with_context(|| format!("{path} 必须是数组"))? { typed(v, item, path)?; }
    } else if let Some(item) = ty.strip_prefix("map[string]") {
        for (k,v) in value.as_object().with_context(|| format!("{path} 必须是对象"))? { typed(v,item,&format!("{path}.{k}"))?; }
    } else {
        let valid = match ty {
            "string" | "C.DNSPrefer" => value.is_string(),
            "bool" => value.is_boolean(),
            "int" | "int64" | "uint64" | "uint32" | "uint16" | "uint8" => value.as_u64().is_some(),
            "float64" => value.as_f64().is_some_and(f64::is_finite),
            "any" | "interface{}" => true,
            _ => { fields(value, ty, path)?; true },
        };
        ensure!(valid, "{path} 类型错误，需要 {ty}");
    }
    Ok(())
}
fn fields(value: &Value, ty: &str, path: &str) -> Result<()> {
    let schema = SCHEMA["types"][ty].as_object().with_context(|| format!("暂不支持 {path} ({ty})"))?;
    for (key, value) in value.as_object().with_context(|| format!("{path} 必须是对象"))? {
        if key == "type" && path == "node" { continue; }
        if key == "udp" && path == "node" { ensure!(value.is_boolean(), "udp 必须是布尔值"); continue; }
        ensure!(!matches!(key.as_str(), "dialer-proxy" | "interface-name" | "routing-mark" | "certificate" | "private-key") || key == "private-key" && ty == "WireGuardOption", "不允许节点参数 {path}.{key}");
        let kind = schema.get(key).and_then(Value::as_str).with_context(|| format!("不支持参数 {path}.{key}"))?;
        typed(value, kind, &format!("{path}.{key}"))?;
        if key == "skip-cert-verify" { ensure!(value == false, "不支持跳过证书验证"); }
    }
    Ok(())
}
pub(super) fn validate(node: &Value) -> Result<()> {
    let kind = text(node,"type","")?;
    let ty = SCHEMA["protocols"][kind].as_str().with_context(|| format!("不支持 Mihomo 协议 {kind}"))?;
    fields(node,ty,"node")?;
    if node.get("peers").is_none() {
        ensure!(host_valid(text(node,"server","")?), "代理地址无效");
        ensure!(node["port"].as_u64().is_some_and(|p| p>0 && p<=65535), "端口必须为 1–65535");
    }
    for key in match kind {
        "ss" | "ssr" => &["cipher","password"][..], "snell" => &["psk"],
        "wireguard" => &["private-key"], "vmess" | "vless" => &["uuid"],
        "trojan" | "hysteria2" | "anytls" => &["password"],
        "shadowquic" => &["username","password"], _ => &[],
    } { ensure!(!text(node,key,"")?.is_empty(), "{kind} 缺少 {key}"); }
    if matches!(kind,"vmess"|"vless") || kind == "tuic" && node.get("uuid").is_some() { uuid::Uuid::parse_str(text(node,"uuid","")?)?; }
    if kind == "vless" { ensure!(flag(node,"tls",false)?, "VLESS 落地需要 TLS"); }
    if kind == "hysteria2" && node.get("obfs").is_some() {
        ensure!(text(node,"obfs","")? == "salamander", "不支持 Hysteria2 obfs");
        ensure!(!text(node,"obfs-password","")?.is_empty(), "缺少 obfs-password");
    }
    if kind == "hysteria2" {
        if let Some(interval) = node.get("hop-interval") {
            for seconds in interval.as_str().unwrap_or("").split('-') {
                ensure!(seconds.parse::<u64>().is_ok_and(|n| n >= 5), "hop-interval 必须为不小于 5 的秒数或范围，例如 30 或 30-60");
            }
        }
    }
    ensure!(node.get("plugin-opts").is_none() || node.get("plugin").is_some(), "plugin-opts 缺少 plugin");
    if let Some(plugin) = node.get("plugin") {
        let plugin = plugin.as_str().context("plugin 必须是文本")?;
        let ty = match plugin {
            "obfs" => "simpleObfsOption", "v2ray-plugin" => "v2rayObfsOption", "gost-plugin" => "gostObfsOption", "shadow-tls" => "shadowTLSOption", "restls" => "restlsOption", "jls" => "jlsOption", "kcptun" => "kcpTunOption",
            _ => bail!("暂不支持 SS 插件 {plugin}"),
        };
        fields(node.get("plugin-opts").unwrap_or(&json!({})),ty,"plugin-opts")?;
    }
    Ok(())
}
pub(super) fn encode(node: &Value) -> Result<String> {
    validate(node)?;
    Ok(format!("mihomo://{}",URL_SAFE_NO_PAD.encode(serde_json::to_vec(node)?)))
}
pub(super) fn decode(link: &str) -> Result<Value> {
    let encoded = link.strip_prefix("mihomo://").context("节点格式无效")?;
    let node = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded)?)?;
    validate(&node)?;
    Ok(node)
}

pub(super) fn share_node(link: &str) -> Result<Option<Value>> {
    if let Some(encoded) = link.strip_prefix("vmess://") {
        let mut v: Value = serde_json::from_str(&crate::access::b64(encoded)?)?;
        for key in v.as_object().context("VMess 节点必须是对象")?.keys() {
            ensure!("v ps add port id aid scy net type tls sni fp alpn host path tfo udp allowInsecure".split_whitespace().any(|k|k==key), "VMess 不支持参数 {key}");
        }
        let net = v["net"].as_str().unwrap_or("tcp").to_owned();
        if !matches!(net.as_str(),"ws"|"grpc"|"h2"|"http") { return Ok(None); }
        let path = v["path"].as_str().unwrap_or("/").to_owned();
        let host = v["host"].as_str().unwrap_or("").to_owned();
        v["net"] = json!("tcp");
        let clean = format!("vmess://{}",URL_SAFE_NO_PAD.encode(serde_json::to_vec(&v)?));
        let mut node = super::mihomo(&clean, v["ps"].as_str().unwrap_or(""))?;
        transport(&mut node,&net,&path,&host)?;
        validate(&node)?;
        return Ok(Some(node));
    }
    let Ok(mut url) = Url::parse(link) else { return Ok(None); };
    let scheme = url.scheme().to_owned();
    let q: std::collections::BTreeMap<String,String> = url.query_pairs().into_owned().collect();
    let allowed = match scheme.as_str() {
        "ss" => "type tfo udp plugin",
        "hy2" | "hysteria2" => "sni alpn insecure allowInsecure udp obfs obfs-password mport ports hop-interval up down udp-mtu handshake-timeout",
        "tuic" => "sni alpn insecure allowInsecure udp congestion_control udp_relay_mode",
        "trojan" | "vless" => "security type encryption flow sni fp alpn insecure allowInsecure pbk sid pqv spx path host mode extra serviceName tfo udp",
        "https" => "sni fp alpn insecure allowInsecure tfo udp",
        "http" | "socks" | "socks5" => "tfo udp",
        _ => return Ok(None),
    };
    ensure!(url.query_pairs().count() == q.len(), "{scheme} 存在重复参数");
    for key in q.keys() { ensure!(allowed.split_whitespace().any(|k|k==key), "{scheme} 不支持参数 {key}"); }
    let network = q.get("type").map(String::as_str).unwrap_or("tcp");
    let extended = scheme == "ss" && q.contains_key("plugin")
        || matches!(scheme.as_str(),"hy2"|"hysteria2") && q.keys().any(|k| ["obfs","obfs-password","mport","ports","hop-interval","up","down","udp-mtu","handshake-timeout"].contains(&k.as_str()))
        || matches!(scheme.as_str(),"trojan"|"vless") && matches!(network,"ws"|"grpc"|"h2"|"http");
    if !extended { return Ok(None); }
    let extensions = ["plugin","obfs","obfs-password","mport","ports","hop-interval","up","down","udp-mtu","handshake-timeout","path","host","serviceName"];
    url.set_query(None);
    for (key,value) in &q {
        if extensions.contains(&key.as_str()) || key == "type" { continue; }
        url.query_pairs_mut().append_pair(key,value);
    }
    let name = percent_decode_str(url.fragment().unwrap_or("")).decode_utf8()?.into_owned();
    let mut node = super::mihomo(url.as_str(), &name)?;
    if matches!(scheme.as_str(),"trojan"|"vless") {
        transport(&mut node,network,q.get("serviceName").or_else(||q.get("path")).map(String::as_str).unwrap_or("/"),q.get("host").map(String::as_str).unwrap_or(""))?;
        if scheme == "trojan" { node.as_object_mut().unwrap().remove("tls"); }
    } else if scheme == "ss" {
        let mut parts = q["plugin"].split(';');
        let plugin = parts.next().unwrap_or_default();
        node["plugin"] = json!(if matches!(plugin,"obfs-local"|"simple-obfs") { "obfs" } else { plugin });
        let mut opts = json!({});
        for part in parts {
            let (key,value) = part.split_once('=').unwrap_or((part,"true"));
            let key = match key { "obfs"=>"mode", "obfs-host"=>"host", other=>other };
            ensure!(opts.get(key).is_none(),"重复 SS 插件参数 {key}");
            opts[key] = if matches!(key,"tls"|"mux"|"skip-cert-verify") {
                ensure!(matches!(value,"true"|"false"|"1"|"0"),"SS 插件 {key} 必须是布尔值"); json!(matches!(value,"true"|"1"))
            } else { json!(value) };
        }
        if node["plugin"] == "v2ray-plugin" && opts.get("mode").is_none() { opts["mode"] = json!("websocket"); }
        node["plugin-opts"] = opts;
    } else {
        for key in extensions {
            if let Some(value) = q.get(key) {
                let field = if key == "mport" {"ports"} else {key};
                node[field] = if matches!(key,"udp-mtu"|"handshake-timeout") { json!(value.parse::<u64>().with_context(||format!("{key} 必须为整数"))?) } else {json!(value)};
            }
        }
    }
    validate(&node)?;
    Ok(Some(node))
}
fn transport(node: &mut Value, network: &str, path: &str, host: &str) -> Result<()> {
    node["network"] = json!(network);
    match network {
        "ws" => node["ws-opts"] = json!({"path":path,"headers":{"Host":host}}),
        "grpc" => node["grpc-opts"] = json!({"grpc-service-name":path}),
        "h2" => node["h2-opts"] = json!({"path":path,"host":host.split(',').collect::<Vec<_>>()}),
        "http" => node["http-opts"] = json!({"path":[path],"headers":{"Host":[host]}}),
        _ => bail!("不支持传输 {network}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocols_preserve_fields_and_reject_unknown_options() {
        let fixtures: Vec<Value> = serde_json::from_str(include_str!("fixtures.json")).unwrap();
        for node in fixtures {
            let (link,name) = super::super::import(&node.to_string()).unwrap_or_else(|e| panic!("{}: {e:#}",node["name"]));
            assert_eq!(super::super::mihomo(&link,&name).unwrap(),node);
            assert_eq!(super::super::import(&link).unwrap().0,link);
            assert!(crate::compiler::needs_mihomo(&external_outbound(&link,"test").unwrap()));
            let mut invalid=node.clone(); invalid["unknown-option"]=json!(true);
            assert!(super::super::import(&invalid.to_string()).is_err());
            invalid=node.clone(); invalid["skip-cert-verify"]=json!(true);
            assert!(super::super::import(&invalid.to_string()).is_err());
        }
    }
    #[test]
    fn extended_links_round_trip() {
        for link in [
            "ss://YWVzLTEyOC1nY206dGVzdA@proxy.example:443?plugin=obfs-local%3Bobfs%3Dhttp%3Bobfs-host%3Dexample.com",
            "hy2://test@proxy.example:443?obfs=salamander&obfs-password=test&mport=443,8443&up=10%20Mbps&udp-mtu=1200",
            "vless://00000000-0000-4000-8000-000000000001@proxy.example:443?security=tls&type=ws&host=example.com&path=%2Fproxy",
            "trojan://test@proxy.example:443?type=grpc&serviceName=proxy",
        ] {
            let (stored,name)=super::super::import(link).unwrap();
            assert_eq!(super::super::mihomo(link,&name).unwrap(),super::super::mihomo(&stored,&name).unwrap());
            assert!(crate::compiler::needs_mihomo(&external_outbound(link,"test").unwrap()));
        }
    }
}
