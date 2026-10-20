//! Server-side destination allowlists. Client process names and loose keyword rules are not identities.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    net::{IpAddr, SocketAddr},
};

#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Debug)]
#[serde(default)]
pub struct Policy {
    pub restricted: bool,
    pub sources: Vec<String>,
    pub domains: Vec<String>,
    pub denied_ports: Vec<u16>,
    pub udp: String,
    pub udp_ports: Vec<u16>,
    pub max_connections: Option<usize>,
}
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Debug)]
#[serde(default)]
pub struct CompiledPolicy {
    pub policy: Policy,
    pub exact: BTreeSet<String>,
    pub suffix: BTreeSet<String>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Rules {
    pub exact: BTreeSet<String>,
    pub suffix: BTreeSet<String>,
    pub excluded: Vec<String>,
    pub excluded_count: usize,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Source {
    pub id: String,
    pub name: String,
    pub category: String,
    pub url: String,
    pub auto_apply: bool,
    pub checked_at: u64,
    pub applied_at: u64,
    pub error: String,
    pub active: Option<Rules>,
    pub pending: Option<Rules>,
}
/// First publication must not silently drop restrictions on an older Agent.
pub fn requires_capability(policy: &serde_json::Value) -> bool {
    policy["restricted"] == true
        || policy["max_connections"].is_number()
        || policy["denied_ports"].as_array().is_some_and(|ports| !ports.is_empty())
        || matches!(policy["udp"].as_str(), Some("deny" | "ports"))
}

pub fn domain(text: &str) -> Result<String> {
    let text = text.trim().trim_end_matches('.');
    ensure!(
        !text.is_empty() && text.len() <= 253 && !text.contains(['/', ':', '@', '*', '\\']),
        "域名格式无效"
    );
    let value = match url::Host::parse(text)? {
        url::Host::Domain(d) => d,
        _ => anyhow::bail!("此处需要域名，不能填写 IP"),
    };
    ensure!(
        value.contains('.')
            && value.split('.').all(|s| !s.is_empty()
                && s.len() <= 63
                && !s.starts_with('-')
                && !s.ends_with('-')
                && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')),
        "域名格式无效"
    );
    Ok(value)
}
pub fn parse(text: &str) -> Result<Rules> {
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(text)?;
    let payload = value
        .get("payload")
        .and_then(|v| v.as_sequence())
        .context("规则文件需要 payload 列表")?;
    ensure!(
        !payload.is_empty() && payload.len() <= 100_000,
        "规则为空或超过 100000 项"
    );
    let mut result = Rules::default();
    for value in payload {
        let value = value.as_str().context("规则条目必须是字符串")?.trim();
        let (kind, body) = value.split_once(',').unwrap_or(("DOMAIN-LIST", value));
        let entry = match kind {
            "DOMAIN" => domain(body).map(|d| (false, d)),
            "DOMAIN-SUFFIX" => domain(body).map(|d| (true, d)),
            "DOMAIN-LIST" => {
                domain(body.strip_prefix("+.").unwrap_or(body)).map(|d| (body.starts_with("+."), d))
            }
            _ => Err(anyhow::anyhow!("严格模式不采用此规则类型")),
        };
        match entry {
            Ok((true, d)) => {
                result.suffix.insert(d);
            }
            Ok((false, d)) => {
                result.exact.insert(d);
            }
            Err(_) => {
                result.excluded_count += 1;
                if result.excluded.len() < 100 {
                    result.excluded.push(value.chars().take(300).collect());
                }
            }
        }
    }
    ensure!(
        !result.exact.is_empty() || !result.suffix.is_empty(),
        "没有可供严格白名单使用的域名条目"
    );
    Ok(result)
}
pub fn compile(doc: &crate::model::Document, id: &str) -> Result<CompiledPolicy> {
    let policy = doc.access_policies.get(id).cloned().unwrap_or_default();
    ensure!(
        matches!(policy.udp.as_str(), "" | "allow" | "deny" | "ports"),
        "UDP 策略无效"
    );
    ensure!(
        policy.max_connections.is_none_or(|n| n > 0 && n <= 1024),
        "连接上限须为 1–1024"
    );
    ensure!(
        policy
            .denied_ports
            .iter()
            .chain(&policy.udp_ports)
            .all(|p| *p > 0),
        "端口无效"
    );
    let mut result = CompiledPolicy {
        policy: policy.clone(),
        ..Default::default()
    };
    for d in &policy.domains {
        result.suffix.insert(domain(d)?);
    }
    for id in &policy.sources {
        let source = doc
            .rule_sources
            .iter()
            .find(|s| s.id == *id)
            .context("规则源不存在")?;
        if let Some(active) = &source.active {
            result.exact.extend(active.exact.clone());
            result.suffix.extend(active.suffix.clone());
        }
        // No active version means no permission; never fall back to unrestricted access.
    }
    Ok(result)
}
impl CompiledPolicy {
    pub fn allows_domain(&self, d: &str) -> bool {
        self.exact.contains(d)
            || d.split('.').enumerate().any(|(i, _)| {
                self.suffix
                    .contains(&d.split('.').skip(i).collect::<Vec<_>>().join("."))
            })
    }
    pub async fn destination(&self, kind: u8, address: &[u8], udp: bool) -> Result<(u8, Vec<u8>)> {
        ensure!(address.len() >= 2, "目标地址无效");
        let port = u16::from_be_bytes(address[address.len() - 2..].try_into()?);
        ensure!(
            port > 0 && !self.policy.denied_ports.contains(&port),
            "目标端口被禁止"
        );
        if udp {
            ensure!(
                self.policy.udp != "deny"
                    && (self.policy.udp != "ports" || self.policy.udp_ports.contains(&port)),
                "UDP 目标端口未放行"
            );
        }
        if !self.policy.restricted {
            return Ok((kind, address.to_vec()));
        }
        ensure!(
            kind == 3 && address.len() >= 3 && address[0] as usize + 3 == address.len(),
            "严格白名单拒绝无法确认域名的 IP 目标"
        );
        let d = domain(std::str::from_utf8(&address[1..address.len() - 2])?)?;
        ensure!(self.allows_domain(&d), "目标不在允许的网站中");
        // Pass the resolved numeric address through every proxy hop. A client-supplied IP
        // and a spoofed SNI can never select an unrelated destination through this path.
        let addresses: Vec<_> = tokio::net::lookup_host((d.as_str(), port)).await?.collect();
        ensure!(
            !addresses.is_empty() && addresses.iter().all(|a| public_ip(a.ip())),
            "白名单目标未解析到公网地址"
        );
        let ip = addresses[0].ip();
        let (kind, mut bytes) = match ip {
            IpAddr::V4(ip) => (1, ip.octets().to_vec()),
            IpAddr::V6(ip) => (4, ip.octets().to_vec()),
        };
        bytes.extend(port.to_be_bytes());
        Ok((kind, bytes))
    }
}
pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let a = v.octets();
            !v.is_private()
                && !v.is_loopback()
                && !v.is_link_local()
                && !v.is_broadcast()
                && !v.is_documentation()
                && !v.is_unspecified()
                && !v.is_multicast()
                && a[0] != 0
                && a[0] < 240
                && !(a[0] == 100 && (64..=127).contains(&a[1]))
                && !(a[0] == 198 && (a[1] == 18 || a[1] == 19))
        }
        IpAddr::V6(v) => v.to_ipv4_mapped().map(public_ip_v4).unwrap_or_else(|| {
            let s = v.segments();
            (s[0] & 0xe000) == 0x2000 && !(s[0] == 0x2001 && (s[1] == 0xdb8 || s[1] < 0x200))
        }),
    }
}
fn public_ip_v4(ip: std::net::Ipv4Addr) -> bool {
    public_ip(ip.into())
}
pub async fn fetch(url: &str) -> Result<Rules> {
    let url = url::Url::parse(url)?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "规则源须为不含凭据的 HTTPS 地址"
    );
    let host = url.host_str().context("规则源缺少主机")?;
    let addresses: Vec<SocketAddr> =
        tokio::net::lookup_host((host, url.port_or_known_default().unwrap()))
            .await?
            .collect();
    ensure!(
        !addresses.is_empty() && addresses.iter().all(|a| public_ip(a.ip())),
        "规则源必须使用公网地址"
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, &addresses)
        .build()?;
    let mut response = client.get(url).send().await?.error_for_status()?;
    ensure!(
        !response.status().is_redirection(),
        "规则源重定向未被允许，请填写最终地址"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 4 * 1024 * 1024,
            "规则文件超过 4MiB"
        );
        bytes.extend_from_slice(&chunk);
    }
    parse(std::str::from_utf8(&bytes)?)
}

// Small, explicit site lists where the upstream catalog has no independent strict rules.
// Keep shared suffixes (dyndns.org, docomo.ne.jp, google.com) out of these allowlists.
fn builtin_rules(key: &str) -> Option<Rules> {
    let (suffix, exact) = match key {
        "DAnime" => ("animestore.docomo.ne.jp", "id.smt.docomo.ne.jp"),
        "Getchu" => ("getchu.com", ""),
        "ErogameScape" => ("erogamescape.dyndns.org", ""),
        "MGStage" => ("mgstage.com", ""),
        "Mora" => ("mora.jp", ""),
        "MusicJP" => ("music.jp music-book.jp", ""),
        "DAM" => ("clubdam.com dam-karaoke.com", ""),
        "JoySound" => ("joysound.com joysound.cloud", ""),
        "GoogleSearch" => ("", "www.google.com www.google.co.jp www.google.com.hk www.google.co.uk www.google.de www.google.fr www.google.ca www.google.com.au www.google.com.tw www.google.com.sg encrypted.google.com accounts.google.com ssl.gstatic.com www.gstatic.com"),
        _ => return None,
    };
    Some(Rules {
        suffix: suffix.split_whitespace().map(str::to_owned).collect(),
        exact: exact.split_whitespace().map(str::to_owned).collect(),
        ..Default::default()
    })
}

pub fn seed_presets(sources: &mut Vec<Source>) {
    for preset in presets() {
        if let Some(source) = sources.iter_mut().find(|s| s.id == preset.id) {
            let obsolete_google = source.id == "preset-googlesearch"
                && source.url == "https://raw.githubusercontent.com/blackmatrix7/ios_rule_script/master/rule/Clash/GoogleSearch/GoogleSearch.yaml";
            if preset.pending.is_some() && (source.url.is_empty() || obsolete_google)
                && source.active.is_none() && source.pending.is_none() {
                source.url = preset.url;
                source.pending = preset.pending;
                source.error.clear();
                source.checked_at = 0;
            }
        } else {
            sources.push(preset);
        }
    }
}

pub fn presets() -> Vec<Source> {
    let groups = [
        (
            "流媒体",
            "Netflix:Netflix|Disney+:Disney|Hotstar:Hotstar|YouTube:YouTube|Prime Video:AmazonPrimeVideo|HBO Max:HBO|Hulu:Hulu|Paramount+:ParamountPlus|Discovery+:DiscoveryPlus|DAZN:DAZN|iQyi Oversea:iQIYIIntl|Bilibili 国际版:BiliBiliIntl|Spotify:Spotify|Twitch:Twitch|巴哈姆特动画疯:Bahamut|Viu / ViuTV:ViuTV|myTV SUPER:MyTVSuper|TVB Anywhere+:TVB|KKTV:KKTV|LINE TV:LineTV|ABEMA:Abema|U-NEXT:U-NEXT|TVer:TVer|niconico:Niconico|Hulu Japan:HuluJP|NHK+:NHK|dアニメストア:DAnime|radiko:Radiko",
        ),
        (
            "社交 / 社区",
            "TikTok:TikTok|Instagram:Instagram|Facebook / Messenger:Facebook|WhatsApp:Whatsapp|X（Twitter）:Twitter|Reddit:Reddit|Discord:Discord|Telegram:Telegram",
        ),
        (
            "其他",
            "Google 搜索:GoogleSearch|Google Play:GooglePlay|Wikipedia:Wikipedia|Steam:Steam|DMM / FANZA:DMM|Getchu:Getchu|批评空间:ErogameScape|MGStage:MGStage|DLsite:DLsite|Pixiv / FANBOX:Pixiv|mora:Mora|music.jp:MusicJP|カラオケ DAM:DAM|JOYSOUND:JoySound",
        ),
    ];
    let mut result = vec![];
    for (category, items) in groups {
        for item in items.split('|') {
            let (name, key) = item.split_once(':').unwrap();
            let url = match key {
                "Hotstar" => format!(
                    "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/meta/geo/geosite/classical/hotstar.yaml"
                ),
                "MyTVSuper" => format!(
                    "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/meta/geo/geosite/classical/mytvsuper.yaml"
                ),
                "U-NEXT" => format!(
                    "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/meta/geo/geosite/classical/unext.yaml"
                ),
                "NHK" => format!(
                    "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/meta/geo/geosite/classical/nhk.yaml"
                ),
                "DAnime" => String::new(),
                "Radiko" => format!(
                    "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/meta/geo/geosite/classical/radiko.yaml"
                ),
                "GooglePlay" => format!(
                    "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/meta/geo/geosite/classical/google-play.yaml"
                ),
                "Getchu" => String::new(),
                "ErogameScape" => String::new(),
                "MGStage" => String::new(),
                "DLsite" => format!(
                    "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/meta/geo/geosite/classical/dlsite.yaml"
                ),
                "Mora" => String::new(),
                "MusicJP" => String::new(),
                "DAM" => String::new(),
                "JoySound" => String::new(),
                _ => format!(
                    "https://raw.githubusercontent.com/blackmatrix7/ios_rule_script/master/rule/Clash/{key}/{key}.yaml"
                ),
            };
            let builtin = builtin_rules(key);
            let url = if builtin.is_some() { String::new() } else { url };
            result.push(Source {
                id: format!("preset-{}", key.to_ascii_lowercase()),
                name: name.into(),
                category: category.into(),
                url,
                pending: builtin,
                ..Default::default()
            });
        }
    }
    for (name, key) in [
        ("ChatGPT / OpenAI", "openai"),
        ("Claude / Anthropic", "anthropic"),
        ("Gemini / Google AI", "google-ai"),
        ("Grok / xAI", "x-ai"),
    ] {
        result.push(Source {id:format!("ai-{key}"),name:name.into(),category:"AI".into(),url:format!("https://raw.githubusercontent.com/VPSDance/ai-proxy-rules/main/rules/clash/{key}.yaml"),..Default::default()});
    }
    result.push(Source {id:"ai-microsoft-copilot".into(),name:"Microsoft Copilot".into(),category:"AI".into(),url:"https://raw.githubusercontent.com/blackmatrix7/ios_rule_script/master/rule/Clash/Copilot/Copilot.yaml".into(),..Default::default()});
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_rules_do_not_accept_keywords_or_processes() {
        let r=parse("payload:\n - DOMAIN-SUFFIX,example.com\n - DOMAIN,login.example.net\n - DOMAIN-KEYWORD,openai\n - PROCESS-NAME,ChatGPT\n - IP-CIDR,1.1.1.0/24\n").unwrap();
        assert_eq!(r.excluded_count, 3);
        let p = CompiledPolicy {
            exact: r.exact,
            suffix: r.suffix,
            ..Default::default()
        };
        assert!(p.allows_domain("a.example.com"));
        assert!(!p.allows_domain("notexample.com"));
        assert!(!p.allows_domain("example.com.attacker.net"));
        assert!(!public_ip("::ffff:127.0.0.1".parse().unwrap()));
    }
    #[test]
    fn every_enforced_policy_requires_a_capable_agent() {
        use serde_json::json;
        for policy in [json!({"restricted":true}), json!({"max_connections":2}),
            json!({"denied_ports":[25]}), json!({"udp":"deny"}),
            json!({"udp":"ports","udp_ports":[53]}), json!({"udp":"ports"})] {
            assert!(requires_capability(&policy), "{policy}");
        }
        assert!(!requires_capability(&json!(Policy::default())));
        assert!(!requires_capability(&json!({"udp":"allow","udp_ports":[53]})));
    }
    #[test]
    fn complete_preset_catalog() {
        let p = presets();
        assert_eq!(p.len(), 55);
        assert_eq!(p.iter().map(|s| &s.id).collect::<BTreeSet<_>>().len(), 55);
        assert_eq!(p.iter().filter(|s| s.url.is_empty()).count(), 9);
        for source in p.iter().filter(|s| s.url.is_empty()) {
            let rules = source.pending.as_ref().unwrap();
            assert!(!rules.exact.is_empty() || !rules.suffix.is_empty());
            for d in rules.exact.iter().chain(&rules.suffix) { assert_eq!(domain(d).unwrap(), *d); }
            for broad in ["google.com", "docomo.ne.jp", "dyndns.org"] {
                assert!(!rules.suffix.contains(broad));
            }
        }

    }
    #[test]
    fn preset_upgrade_repairs_only_unconfigured_empty_sources() {
        let mut sources = presets();
        for s in &mut sources {
            if s.url.is_empty() { s.pending = None; s.error = "旧版本无规则".into(); }
            if s.id == "preset-googlesearch" { s.url = "https://raw.githubusercontent.com/blackmatrix7/ios_rule_script/master/rule/Clash/GoogleSearch/GoogleSearch.yaml".into(); }
        }
        sources.iter_mut().find(|s| s.id == "preset-getchu").unwrap().url = "https://custom.example/rules.yaml".into();
        sources.iter_mut().find(|s| s.id == "preset-mora").unwrap().active = Some(parse("payload: [DOMAIN,mora.jp]").unwrap());
        seed_presets(&mut sources);
        assert_eq!(sources.len(), 55);
        assert!(sources.iter().find(|s| s.id == "preset-googlesearch").unwrap().pending.is_some());
        assert!(sources.iter().find(|s| s.id == "preset-danime").unwrap().pending.is_some());
        let custom = sources.iter().find(|s| s.id == "preset-getchu").unwrap();
        assert_eq!(custom.url, "https://custom.example/rules.yaml");
        assert!(custom.pending.is_none());
        let active = sources.iter().find(|s| s.id == "preset-mora").unwrap();
        assert!(active.active.as_ref().unwrap().exact.contains("mora.jp"));
        assert!(active.pending.is_none());
        let before = serde_json::to_string(&sources).unwrap();
        seed_presets(&mut sources);
        assert_eq!(serde_json::to_string(&sources).unwrap(), before);
    }
    #[tokio::test]
    async fn forged_ip_and_ports_are_rejected_before_resolution() {
        let mut p = CompiledPolicy::default();
        p.policy.restricted = true;
        p.suffix.insert("example.com".into());
        assert!(
            p.destination(1, &[1, 1, 1, 1, 1, 187], false)
                .await
                .is_err()
        );
        let name = b"example.com.attacker.net";
        let mut addr = vec![name.len() as u8];
        addr.extend(name);
        addr.extend([1, 187]);
        assert!(p.destination(3, &addr, false).await.is_err());
        p.policy.restricted = false;
        p.policy.denied_ports = vec![25];
        assert!(p.destination(1, &[1, 1, 1, 1, 0, 25], false).await.is_err());
        p.policy.udp = "ports".into();
        p.policy.udp_ports = vec![53];
        assert!(p.destination(1, &[1, 1, 1, 1, 0, 53], true).await.is_ok());
        assert!(p.destination(1, &[1, 1, 1, 1, 1, 187], true).await.is_err());
    }
}
