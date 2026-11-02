//! Data-only helpers used by the standalone SSH installer.
mod node;
mod state;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, io::Write};
const STATE: &str = "INSTANCE_ID INSTANCE_NAME INSTANCE_IMPORTED APP_DIR WEB_ROOT SERVICE_NAME INIT_SYSTEM XRAY_BIN DOMAIN UUID NODE_MODE SECURITY_MODE CERT_MODE CERT_FILE KEY_FILE TLS_PORT REALITY_PORT REALITY_TARGET_MODE REALITY_TARGET REALITY_SERVER_NAME REALITY_PRIVATE_KEY REALITY_PUBLIC_KEY REALITY_SHORT_ID REALITY_FINGERPRINT INTERNAL_HTTP_PORT INTERNAL_HTTPS_PORT";
const UP: &str = "UPSTREAM_ADDRESS UPSTREAM_PORT UPSTREAM_UUID UPSTREAM_SECURITY UPSTREAM_SNI UPSTREAM_FLOW UPSTREAM_FINGERPRINT UPSTREAM_ALLOW_INSECURE UPSTREAM_REALITY_PUBLIC_KEY UPSTREAM_REALITY_SHORT_ID UPSTREAM_REALITY_MLDSA65_VERIFY UPSTREAM_TRANSPORT UPSTREAM_NAME UPSTREAM_XHTTP_PATH UPSTREAM_XHTTP_HOST UPSTREAM_XHTTP_MODE UPSTREAM_XHTTP_EXTRA UPSTREAM_ALPN";
type Data = BTreeMap<String, String>;
fn clean(v: &Value) -> Result<String> {
    let s = match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        _ => v.to_string(),
    };
    ensure!(
        !s.chars().any(|c| (c as u32) < 32),
        "节点字段不能包含换行或控制字符"
    );
    Ok(s)
}
fn boolean(v: &Value) -> Result<bool> {
    match clean(v)?.to_ascii_lowercase().as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" | "" => Ok(false),
        _ => anyhow::bail!("布尔字段必须为 true/false"),
    }
}
fn environment(fields: &str) -> Data {
    fields
        .split_whitespace()
        .map(|k| (k.into(), std::env::var(k).unwrap_or_default()))
        .collect()
}
fn field<'a>(data: &'a Data, key: &str) -> &'a str {
    data.get(key).map(String::as_str).unwrap_or("")
}
fn put(data: &mut Data, key: &str, value: &Value) -> Result<()> {
    data.insert(key.into(), clean(value)?);
    Ok(())
}
fn read(path: &str) -> Result<Value> {
    serde_json::from_slice(&fs::read(path)?).context("配置格式或登记文件不正确，请检查字段")
}
fn data(value: &Value) -> Result<Data> {
    value
        .as_object()
        .context("配置必须是对象")?
        .iter()
        .map(|(k, v)| Ok((k.clone(), clean(v)?)))
        .collect()
}
fn dump(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
fn values(data: &Data, fields: &str) -> Result<()> {
    let mut out = std::io::stdout().lock();
    for key in fields.split_whitespace() {
        let value = clean(&json!(field(data, key)))?;
        write!(out, "{key}\0{value}\0")?;
    }
    Ok(())
}

pub fn run(args: &[String]) -> Result<()> {
    let action = args.first().context("缺少内部操作")?.as_str();
    let path = || args.get(1).map(String::as_str).context("缺少配置路径");
    let fields = format!("{STATE} {UP}");
    match action {
        "protocol" => {
            println!("1");
            Ok(())
        }
        "parse" => dump(&node::parse(fs::read_to_string(path()?)?.trim())?),
        "validate" => node::validate(&environment(UP)),
        "outbound" => dump(&node::outbound(&environment(UP))?),
        "state-save" => super::upgrade::atomic(
            std::path::Path::new(path()?),
            format!("{}\n", serde_json::to_string_pretty(&environment(&fields))?).as_bytes(),
            0o600,
        ),
        "upstream-values" | "state-values" => values(
            &data(&read(path()?)?)?,
            if action == "upstream-values" {
                UP
            } else {
                &fields
            },
        ),
        "access-values" => values(
            &state::access(&read(path()?)?, environment(&fields))?,
            &fields,
        ),
        "legacy" => dump(&state::legacy(path()?)?),
        "list" | "ports" | "unique" | "uuid-unique" => {
            state::records_action(action, &environment(&fields))
        }
        "edit-access" => {
            let mut config = read(path()?)?;
            state::access(&config, environment(&fields))?;
            for inbound in config["inbounds"].as_array_mut().context("缺少入站")? {
                if inbound["protocol"] != "vless" {
                    continue;
                }
                for client in inbound["settings"]["clients"]
                    .as_array_mut()
                    .context("缺少客户端")?
                {
                    client["id"] = json!(std::env::var("UUID")?);
                }
                let mode = inbound["streamSettings"]["security"]
                    .as_str()
                    .context("缺少安全方式")?
                    .to_string();
                inbound["port"] =
                    json!(std::env::var(format!("{}_PORT", mode.to_uppercase()))?.parse::<u16>()?);
                if mode == "reality" {
                    inbound["streamSettings"]["realitySettings"]["privateKey"] =
                        json!(std::env::var("REALITY_PRIVATE_KEY")?);
                    inbound["streamSettings"]["realitySettings"]["shortIds"] =
                        json!([std::env::var("REALITY_SHORT_ID")?]);
                }
            }
            dump(&config)
        }
        "replace-upstream" => {
            let mut config = read(path()?)?;
            let mut out = vec![node::outbound(&environment(UP))?];
            out.extend(
                config["outbounds"]
                    .as_array()
                    .context("缺少出站")?
                    .iter()
                    .filter(|v| v["tag"] != "upstream-vless")
                    .cloned(),
            );
            config["outbounds"] = json!(out);
            dump(&config)
        }
        "client" | "client-yaml" => state::clients(action, &environment(STATE)),
        "ipv6" => {
            path()?
                .parse::<std::net::Ipv6Addr>()
                .context("IPv6 地址无效")?;
            Ok(())
        }
        "release-tag" => {
            let v = read(path()?)?;
            let tag = v["tag_name"].as_str().context("正式版本信息无效")?;
            super::package::version(tag.strip_prefix('v').context("正式版本信息无效")?)?;
            println!("{tag}");
            Ok(())
        }
        "selinux-policy" => {
            let d = environment(&format!("{STATE} MANAGER_WEB_ROOT"));
            print!("{}", selinux_policy(&d)?);
            Ok(())
        }
        _ => anyhow::bail!("未知内部操作"),
    }
}

fn selinux_policy(d: &Data) -> Result<String> {
    let mut body = String::new();
    for key in ["INTERNAL_HTTP_PORT", "INTERNAL_HTTPS_PORT"] {
        let port = field(d, key).parse::<u16>()?;
        ensure!(port > 0, "SELinux 端口无效");
        body.push_str(&format!(
            "(portcon tcp {port} (system_u object_r http_port_t ((s0) (s0))))\n"
        ));
    }
    for (key, suffix, kind) in [
        ("MANAGER_WEB_ROOT", "", "httpd_sys_content_t"),
        ("APP_DIR", "/certs", "httpd_config_t"),
    ] {
        let path = format!("{}{suffix}", field(d, key));
        ensure!(
            path.starts_with('/')
                && path.len() > 1
                && path
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_./-".contains(&b))
                && !path.split('/').any(|s| s == ".."),
            "SELinux 路径无效"
        );
        let pattern = format!("{}(/.*)?", path.replace('.', "\\."));
        body.push_str(&format!(
            "(filecon {} any (system_u object_r {kind} ((s0) (s0))))\n",
            serde_json::to_string(&pattern)?
        ));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selinux_policy_scopes_paths_and_refuses_source_injection() {
        let mut d = Data::from([
            ("INTERNAL_HTTP_PORT".into(), "18080".into()),
            ("INTERNAL_HTTPS_PORT".into(), "18443".into()),
            ("MANAGER_WEB_ROOT".into(), "/srv/chain.web".into()),
            ("APP_DIR".into(), "/opt/chain/instances/a".into()),
        ]);
        let body = selinux_policy(&d).unwrap();
        assert!(body.contains("(portcon tcp 18080 "));
        assert!(body.contains(r#""/srv/chain\\.web(/.*)?""#));
        assert!(body.contains("/opt/chain/instances/a/certs"));
        d.insert("APP_DIR".into(), "/opt/../foreign".into());
        assert!(selinux_policy(&d).is_err());
        d.insert("APP_DIR".into(), "/tmp/x\") (allow all)".into());
        assert!(selinux_policy(&d).is_err());
    }
}
