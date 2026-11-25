//! Keep each imported Xray configuration isolated: global DNS and routing must not leak between services.
use crate::model::*;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub fn port_ranges(config: &Value) -> Result<Vec<(u16, u16)>> {
    let mut ranges = Vec::new();
    for inbound in config["inbounds"]
        .as_array()
        .context("旧配置 inbounds 必须是数组")?
    {
        ensure!(inbound.is_object(), "旧配置入口必须是对象");
        let value = &inbound["port"];
        if let Some(port) = value.as_u64() {
            ensure!(port > 0 && port <= 65535, "旧配置端口无效");
            let p = port as u16;
            ranges.push((p, p));
        } else if let Some(value) = value.as_str() {
            for range in value.split(',') {
                let (first, last) = range.split_once('-').unwrap_or((range, range));
                let first: u16 = first.trim().parse().context("旧配置端口范围无效")?;
                let last: u16 = last.trim().parse().context("旧配置端口范围无效")?;
                ensure!(first > 0 && first <= last, "旧配置端口范围无效");
                ranges.push((first, last));
            }
        } else {
            anyhow::bail!("旧配置入口缺少端口；Unix socket / TUN 入口需要原运行环境，不能自动接管");
        }
    }
    ranges.sort_unstable();
    let mut merged: Vec<(u16, u16)> = Vec::with_capacity(ranges.len());
    for (first, last) in ranges {
        if let Some(prev) = merged.last_mut()
            && first <= prev.1.saturating_add(1)
        {
            prev.1 = prev.1.max(last);
        } else {
            merged.push((first, last));
        }
    }
    Ok(merged)
}

pub fn port_in_ranges(ranges: &[(u16, u16)], port: u16) -> bool {
    ranges.iter().any(|&(first, last)| (first..=last).contains(&port))
}

pub fn ranges_overlap(a: &[(u16, u16)], b: &[(u16, u16)]) -> Option<u16> {
    for &(fa, la) in a {
        for &(fb, lb) in b {
            if fa <= lb && fb <= la {
                return Some(fa.max(fb));
            }
        }
    }
    None
}

pub fn ports(config: &Value) -> Result<Vec<Value>> {
    Ok(port_ranges(config)?
        .into_iter()
        .map(|(first, last)| {
            if first == last {
                json!(first)
            } else {
                json!(format!("{first}-{last}"))
            }
        })
        .collect())
}

pub fn node_ranges(doc: &Document) -> Result<BTreeMap<String, Vec<(u16, u16)>>> {
    let mut map: BTreeMap<String, Vec<(u16, u16)>> = BTreeMap::new();
    for profile in doc.legacy_configs.iter().filter(|p| p.enabled) {
        map.entry(profile.node_id.clone())
            .or_default()
            .extend(port_ranges(&profile.config)?);
    }
    Ok(map)
}

fn validate_with_ranges(profile: &LegacyConfig) -> Result<Vec<(u16, u16)>> {
    uuid::Uuid::parse_str(&profile.id).context("旧配置 ID 无效")?;
    if !profile.asset_dir.is_empty() {
        let path = std::path::Path::new(&profile.asset_dir);
        ensure!(
            path.parent() == Some(std::path::Path::new("/var/lib/chain-legacy-assets"))
                && path
                    .file_name()
                    .and_then(|v| v.to_str())
                    .is_some_and(|v| uuid::Uuid::parse_str(v).is_ok()),
            "地理数据目录必须由安装器迁移"
        );
    }
    ensure!(
        !profile.name.trim().is_empty() && profile.name.len() <= 240,
        "旧配置名称无效"
    );
    let object = profile
        .config
        .as_object()
        .context("旧配置必须是 JSON 对象")?;
    ensure!(
        !object.keys().any(|k| k.starts_with('_')),
        "旧配置不能包含平台内部字段"
    );
    ensure!(
        serde_json::to_vec(&profile.config)?.len() <= 2 * 1024 * 1024,
        "旧配置超过 2 MiB"
    );
    ensure!(
        profile.config["outbounds"].is_array(),
        "旧配置 outbounds 必须是数组"
    );
    let ranges = port_ranges(&profile.config)?;
    fn files(value: &Value) -> Result<()> {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    if matches!(key.as_str(), "certificateFile" | "keyFile" | "keyLog") {
                        ensure!(
                            value.is_null() || value == "",
                            "旧配置的 {key} 必须先读取为内联内容，不能依赖旧服务的文件权限"
                        );
                    }
                    files(value)?;
                }
            }
            Value::Array(values) => {
                for value in values {
                    files(value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    files(&profile.config)?;
    Ok(ranges)
}

pub fn validate(profile: &LegacyConfig) -> Result<()> {
    validate_with_ranges(profile).map(|_| ())
}

pub fn attach(doc: &Document, configs: &mut BTreeMap<String, Value>) -> Result<()> {
    let mut ids = BTreeSet::new();
    let mut reserved_ports: BTreeMap<String, BTreeSet<u16>> = BTreeMap::new();
    for line in &doc.lines {
        for hop in &line.hops {
            reserved_ports
                .entry(hop.node_id.clone())
                .or_default()
                .insert(hop.port);
        }
    }
    let mut reserved_ranges: BTreeMap<String, Vec<(u16, u16)>> = BTreeMap::new();
    for profile in &doc.legacy_configs {
        let ranges = validate_with_ranges(profile)?;
        ensure!(ids.insert(&profile.id), "旧配置 ID 重复");
        let config = configs
            .get_mut(&profile.node_id)
            .context("旧配置引用的节点不存在")?;
        if !profile.enabled {
            continue;
        }
        if let Some(ports) = reserved_ports.get(&profile.node_id) {
            for &port in ports {
                ensure!(
                    !port_in_ranges(&ranges, port),
                    "旧配置 {} 的端口 {port} 与其他代理冲突",
                    profile.name
                );
            }
        }
        let existing = reserved_ranges.entry(profile.node_id.clone()).or_default();
        if let Some(conflict) = ranges_overlap(existing, &ranges) {
            anyhow::bail!("旧配置 {} 的端口 {conflict} 与其他代理冲突", profile.name);
        }
        existing.extend_from_slice(&ranges);
        for inbound in config["inbounds"].as_array().into_iter().flatten() {
            if let Some(port) = inbound["port"].as_u64().and_then(|p| u16::try_from(p).ok()) {
                ensure!(
                    !port_in_ranges(&ranges, port),
                    "旧配置端口 {port} 与平台内部入口冲突"
                );
            }
        }
        for spec in config["_chain"].as_array().into_iter().flatten() {
            for field in ["port", "backend"] {
                if let Some(port) = spec[field].as_u64().and_then(|p| u16::try_from(p).ok()) {
                    ensure!(
                        !port_in_ranges(&ranges, port),
                        "旧配置端口 {port} 与平台入口冲突"
                    );
                }
            }
        }
        if config.get("_legacy").is_none() {
            config["_legacy"] = json!([]);
        }
        config["_legacy"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::to_value(profile)?);
        ensure!(
            config["_legacy"].as_array().unwrap().len() <= 64,
            "同一节点最多运行 64 份旧配置"
        );
    }
    Ok(())
}

pub fn profiles(config: &Value) -> Result<Vec<LegacyConfig>> {
    let profiles: Vec<LegacyConfig> =
        serde_json::from_value(config.get("_legacy").cloned().unwrap_or(json!([])))?;
    ensure!(profiles.len() <= 64, "最多运行 64 份旧配置");
    for profile in &profiles {
        validate(profile)?;
    }
    Ok(profiles)
}

pub fn runtime_config(profile: &LegacyConfig) -> Value {
    let mut config = profile.config.clone();
    // Logs belong to the Agent; never write to the old service's log directory.
    if config["log"].is_object() {
        for key in ["access", "error"] {
            if config["log"][key]
                .as_str()
                .is_some_and(|v| !v.is_empty() && v != "none")
            {
                config["log"][key] = json!("");
            }
        }
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> LegacyConfig {
        LegacyConfig {
            id: id(),
            node_id: "node".into(),
            name: "3x-ui".into(),
            enabled: true,
            asset_dir: String::new(),
            source_inbounds: Vec::new(),
            config: json!({"inbounds":[{"port":443,"protocol":"vless","settings":{"clients":[{"id":"one"},{"id":"two"}]},
                "sniffing":{"enabled":true,"destOverride":["http","tls"]},"streamSettings":{"network":"ws","wsSettings":{"path":"/old"}}}],
                "outbounds":[{"protocol":"freedom"},{"tag":"blocked","protocol":"blackhole"}],
                "dns":{"servers":["1.1.1.1"]},"routing":{"rules":[{"ip":["geoip:private"],"outboundTag":"blocked"}]},
                "transport":null,"reverse":null,"fakeDns":null}),
        }
    }
    #[test]
    fn preserves_complete_configuration_and_credentials() {
        let profile = profile();
        validate(&profile).unwrap();
        assert_eq!(runtime_config(&profile), profile.config);
        let decoded = profiles(&json!({"_legacy":[profile.clone()]})).unwrap();
        assert!(decoded == vec![profile]);
    }
    #[test]
    fn validates_ranges_without_expanding_large_port_spans_and_rejects_external_paths() {
        let mut profile = profile();
        profile.config["inbounds"][0]["port"] = json!("10000-60000,60001,60005");
        assert_eq!(
            port_ranges(&profile.config).unwrap(),
            vec![(10000, 60001), (60005, 60005)]
        );
        assert_eq!(
            ports(&profile.config).unwrap(),
            vec![json!("10000-60001"), json!(60005)]
        );
        profile.config["inbounds"][0]["streamSettings"]["tlsSettings"] =
            json!({"certificates":[{"keyFile":"/root/key.pem"}]});
        assert!(
            validate(&profile)
                .unwrap_err()
                .to_string()
                .contains("keyFile")
        );
    }
    #[test]
    fn refuses_node_port_conflicts() {
        let profile = profile();
        let mut configs =
            BTreeMap::from([("node".into(), json!({"inbounds":[{"port":443}]}))]);
        let doc = Document {
            legacy_configs: vec![profile],
            ..Default::default()
        };
        assert!(attach(&doc, &mut configs).is_err());
    }
}
