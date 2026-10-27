use anyhow::{Context, Result, ensure};
use futures_util::{StreamExt, stream};
use ipnet::IpNet;
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{net::SocketAddr, time::Duration};

#[derive(Clone, Deserialize, Serialize)]
struct Candidate {
    name: String,
    host: String,
}

#[derive(Deserialize)]
struct Catalog {
    source: String,
    revision: String,
    candidates: Vec<Candidate>,
}

fn allowed_host(host: &str) -> bool {
    crate::compiler::host_valid(host)
        && host.ends_with(".edu")
        && !["apple", "icloud", "microsoft"]
            .iter()
            .any(|word| host.contains(word))
}

fn catalog() -> Result<Catalog> {
    let mut catalog: Catalog = serde_json::from_str(include_str!(
        "../reality-candidates/reality-us-universities.json"
    ))?;
    catalog.candidates.retain(|c| allowed_host(&c.host));
    Ok(catalog)
}

fn parse_ranges(body: &str, ipv4: bool) -> Result<Vec<IpNet>> {
    let ranges = body
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::parse::<IpNet>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        !ranges.is_empty()
            && ranges
                .iter()
                .all(|net| net.addr().is_ipv4() == ipv4 && net.prefix_len() > 0),
        "Cloudflare 网段列表为空或地址族无效，未放行候选"
    );
    Ok(ranges)
}

async fn fetch_ranges(client: &reqwest::Client, ipv4: bool) -> Result<Vec<IpNet>> {
    let url = if ipv4 {
        "https://www.cloudflare.com/ips-v4"
    } else {
        "https://www.cloudflare.com/ips-v6"
    };
    let mut response = client.get(url).send().await?.error_for_status()?;
    ensure!(
        !response.status().is_redirection(),
        "Cloudflare 网段源发生重定向，未放行候选"
    );
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            body.len() + chunk.len() <= 16_384,
            "Cloudflare 网段列表过大"
        );
        body.extend_from_slice(&chunk);
    }
    parse_ranges(std::str::from_utf8(&body)?, ipv4)
}

fn addresses_allowed(addresses: &[SocketAddr], ranges: &[IpNet]) -> bool {
    !addresses.is_empty()
        && !ranges.is_empty()
        && addresses.iter().all(|address| {
            let ip = address.ip();
            let ip = match ip {
                std::net::IpAddr::V6(v) => v.to_ipv4_mapped().map(Into::into).unwrap_or(ip),
                _ => ip,
            };
            crate::rules::public_ip(ip) && !ranges.iter().any(|net| net.contains(&ip))
        })
}

fn cloudflare_headers(headers: &reqwest::header::HeaderMap) -> bool {
    headers.contains_key("cf-ray")
        || headers
            .get(reqwest::header::SERVER)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().contains("cloudflare"))
}

async fn probe(candidate: &Candidate, ranges: &[IpNet]) -> Result<Value> {
    let input = crate::model::RealityTarget {
        target: format!("{}:443", candidate.host), server_name: candidate.host.clone(),
        credentials: None, limit_fallback_upload: None, limit_fallback_download: None,
    };
    let (addresses, address) = check_addresses(&input, ranges).await?;
    let check = crate::runtime::check_reality(&crate::model::RealityTarget { target: address.to_string(), ..input }).await?;
    ensure!(check["ok"] == true, "TLS 1.3、H2 或证书校验未通过");
    Ok(json!({"name":candidate.name,"target":format!("{}:443",candidate.host),"server_name":candidate.host,
        "tls13":true,"h2":true,"certificate_valid":true,
        "addresses":addresses.iter().map(|a|a.ip().to_string()).collect::<Vec<_>>(),
        "detail":"TLS 1.3、H2、证书/SNI 检查通过；当前解析地址及响应未发现 Cloudflare，发布时重新检查"}))
}

async fn check_addresses(input: &crate::model::RealityTarget, ranges: &[IpNet]) -> Result<(Vec<SocketAddr>, SocketAddr)> {
    let (host, port) = crate::compiler::target(&input.target)?;
    let name = input.server_name.to_ascii_lowercase();
    ensure!(crate::compiler::host_valid(&name), "SNI 无效");
    ensure!(!["apple", "icloud", "microsoft"].iter().any(|word| name.contains(word))
        && ![".cn", ".ru", ".ir"].iter().any(|suffix| name.ends_with(suffix)), "目标属于上游告警域名，请重新选择");
    let addresses: Vec<_> = tokio::net::lookup_host((host.as_str(), port))
        .await?
        .collect();
    ensure!(
        addresses_allowed(&addresses, ranges),
        "DNS 地址为空、非公网或命中 Cloudflare"
    );
    // Pin the HTTP and TLS checks to the addresses that passed the range filter.
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(4))
        .resolve_to_addrs(&name, &addresses)
        .build()?;
    let response = client
        .head(format!("https://{}/", crate::compiler::address(&name, port)))
        .send()
        .await?;
    ensure!(
        !cloudflare_headers(response.headers()),
        "响应包含 Cloudflare 特征"
    );
    ensure!(
        !response.status().is_redirection(),
        "官网发生重定向，不推荐当前主机名"
    );
    let address = response.remote_addr().context("无法确认实际连接地址")?;
    ensure!(
        addresses.contains(&address),
        "实际连接地址不在已检测 DNS 结果中"
    );
    Ok((addresses, address))
}

pub async fn preflight(binary: &std::path::Path, input: &crate::model::RealityTarget) -> Result<Value> {
    let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8)).build()?;
    let (v4, v6) = tokio::try_join!(fetch_ranges(&client, true), fetch_ranges(&client, false))
        .context("无法确认 Cloudflare 官方网段，已阻止发布")?;
    let ranges = [v4, v6].concat();
    check_addresses(input, &ranges).await?;
    let mut result = crate::diagnostics::reality(binary, input).await?;
    if result["ok"] == true {
        // Recheck after the handshake too: the core resolves a domain independently.
        check_addresses(input, &ranges).await?;
        result["cloudflare_checked"] = json!(true);
    }
    Ok(result)
}

/// Only a small random sample of known websites is contacted; never scan IP ranges.
pub async fn recommend() -> Result<Value> {
    let catalog = catalog()?;
    let candidates: Vec<_> = catalog
        .candidates
        .choose_multiple(&mut rand::thread_rng(), 12)
        .cloned()
        .collect();
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .build()?;
    let (v4, v6) = tokio::try_join!(fetch_ranges(&client, true), fetch_ranges(&client, false))
        .context("无法确认 Cloudflare 官方 IPv4/IPv6 网段，本次停止推荐，请稍后重试")?;
    let ranges = [v4, v6].concat();
    let checked = candidates.len();
    let results: Vec<_> =
        stream::iter(
            candidates.into_iter().map(|candidate| {
                let ranges = &ranges;
                async move {
                    tokio::time::timeout(Duration::from_secs(8), probe(&candidate, ranges)).await
                }
            }),
        )
        .buffer_unordered(3)
        .collect()
        .await;
    let mut candidates: Vec<Value> = results
        .into_iter()
        .filter_map(|r| r.ok().and_then(Result::ok))
        .collect();
    candidates.shuffle(&mut rand::thread_rng());
    candidates.truncate(5);
    Ok(
        json!({"candidates":candidates,"checked":checked,"source":catalog.source,"revision":catalog.revision,
        "detail":if candidates.is_empty() {"本批没有通过全部检查的目标；可换一批或手动输入。未填入默认域名。"}
            else {"仅表示该节点当前的目标兼容性检测通过，不保证不会被封锁；选中后在发布前验证完整 REALITY 握手。"}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_catalog_is_unique_and_excludes_old_defaults() {
        let catalog = catalog().unwrap();
        assert!(catalog.candidates.len() > 100);
        assert_eq!(catalog.revision.len(), 40);
        let mut hosts = std::collections::BTreeSet::new();
        for candidate in catalog.candidates {
            assert!(allowed_host(&candidate.host));
            assert!(hosts.insert(candidate.host));
            assert!(!candidate.name.is_empty());
        }
        for host in [
            "www.apple.com",
            "www.microsoft.com",
            "www.icloud.com",
            "apple.example.edu",
            "127.0.0.1",
        ] {
            assert!(!allowed_host(host));
        }
    }

    #[test]
    fn candidate_cloudflare_filter_fails_closed_for_mixed_addresses() {
        let ranges = [
            parse_ranges("104.16.0.0/13\n", true).unwrap(),
            parse_ranges("2606:4700::/32", false).unwrap(),
        ]
        .concat();
        let clean = "8.8.8.8:443".parse().unwrap();
        assert!(addresses_allowed(&[clean], &ranges));
        for blocked in [
            "104.16.0.1:443",
            "[2606:4700::1]:443",
            "[::ffff:104.16.0.1]:443",
            "127.0.0.1:443",
            "10.0.0.1:443",
            "[::1]:443",
        ] {
            assert!(
                !addresses_allowed(&[clean, blocked.parse().unwrap()], &ranges),
                "{blocked}"
            );
        }
        assert!(!addresses_allowed(&[], &ranges));
        assert!(!addresses_allowed(&[clean], &[]));
        for invalid in [
            "",
            "<html>unavailable</html>",
            "0.0.0.0/0",
            "2606:4700::/32",
            "104.16.0.0/13\nbad",
        ] {
            assert!(parse_ranges(invalid, true).is_err());
        }
        assert!(parse_ranges("104.16.0.0/13", false).is_err());
    }

    #[test]
    fn candidate_cloudflare_headers_are_rejected_even_outside_published_ranges() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert!(!cloudflare_headers(&headers));
        headers.insert("server", "Cloudflare".parse().unwrap());
        assert!(cloudflare_headers(&headers));
        headers.remove("server");
        headers.insert("cf-ray", "example".parse().unwrap());
        assert!(cloudflare_headers(&headers));
    }

    #[test]
    fn candidate_endpoint_keeps_existing_diagnostic_permissions() {
        use crate::security::Role;
        let id = "00000000-0000-4000-8000-000000000001";
        let path = format!("/api/diagnostics/{id}/reality-candidates");
        assert!(crate::management::machine_route("POST", &path));
        assert!(Role::Ops.allows("POST", &path));
        assert!(!Role::Viewer.allows("POST", &path));
        assert!(!Role::Finance.allows("POST", &path));
        assert!(!crate::management::machine_route("GET", &path));
        assert!(!crate::management::machine_route(
            "POST",
            "/api/diagnostics/invalid/reality-candidates"
        ));
    }

    #[test]
    fn candidate_recommendation_can_run_in_the_agent_task() {
        fn is_send<T: Send>(_: T) {}
        is_send(recommend());
    }
}
