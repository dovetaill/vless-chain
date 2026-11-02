use super::{release, workspace};
use crate::ctl::{cores, package};
use serde_json::json;
use std::{collections::BTreeMap, fs, path::Path};

pub(super) fn manifests(root: &Path) {
    fs::create_dir_all(root.join("panel/frontend")).unwrap();
    fs::write(
        root.join("panel/Cargo.toml"),
        "[package]\nname = \"chain-panel\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        root.join("panel/Cargo.lock"),
        "[[package]]\nname = \"chain-panel\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        root.join("panel/frontend/package.json"),
        r#"{"version":"0.1.0"}"#,
    )
    .unwrap();
    fs::write(
        root.join("panel/frontend/package-lock.json"),
        r#"{"version":"0.1.0","packages":{"":{"version":"0.1.0"}}}"#,
    )
    .unwrap();
}
#[test]
fn version_updates_four_manifests_and_rejects_invalid_input_before_writing() {
    let root = tempfile::tempdir().unwrap();
    manifests(root.path());
    release::set_version(root.path(), "0.3.7").unwrap();
    assert_eq!(release::version(root.path()).unwrap(), "0.3.7");
    let before = release::MANIFESTS
        .iter()
        .map(|p| fs::read(root.path().join(p)).unwrap())
        .collect::<Vec<_>>();
    assert!(release::set_version(root.path(), "bad; version").is_err());
    assert_eq!(
        before,
        release::MANIFESTS
            .iter()
            .map(|p| fs::read(root.path().join(p)).unwrap())
            .collect::<Vec<_>>()
    );
    assert!(super::delivery::validate_tag(root.path(), "main").is_err());
    assert!(super::delivery::validate_tag(root.path(), "v0.3.6").is_err());
    super::delivery::validate_tag(root.path(), "v0.3.7").unwrap();
}
#[test]
fn github_token_is_exclusive_to_the_exact_api_origin() {
    let client = reqwest::Client::new();
    for token in [None, Some(""), Some("fixture-token")] {
        for url in [
            "https://api.github.com/repos/XTLS/Xray-core/releases/tags/v26.10.21",
            "https://github.com/XTLS/Xray-core/releases/download/v26.10.21/Xray-linux-64.zip",
            "https://raw.githubusercontent.com/MetaCubeX/mihomo/v1.19.33/LICENSE",
            "https://api.github.com.example.test/file",
            "https://api.github.com@example.test/file",
            "http://api.github.com/file",
        ] {
            let request = package::download_request(&client, url, token)
                .build()
                .unwrap();
            assert_eq!(
                request
                    .headers()
                    .get("authorization")
                    .map(|v| v.to_str().unwrap()),
                if token == Some("fixture-token") && url.starts_with("https://api.github.com/") {
                    Some("Bearer fixture-token")
                } else if url == "https://api.github.com@example.test/file" {
                    Some("Basic YXBpLmdpdGh1Yi5jb206")
                } else {
                    None
                },
                "{url}"
            );
        }
    }
}
#[tokio::test]
async fn default_download_redirect_does_not_forward_credentials_to_another_host() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let mut headers = vec![];
        for index in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![];
            loop {
                let byte = stream.read_u8().await.unwrap();
                bytes.push(byte);
                if bytes.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            headers.push(String::from_utf8(bytes).unwrap().to_lowercase());
            let response = if index == 0 {
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://localhost:{port}/asset\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
            } else {
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".into()
            };
            stream.write_all(response.as_bytes()).await.unwrap();
        }
        headers
    });
    let result = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap()
        .get(format!("http://127.0.0.1:{port}/api"))
        .bearer_auth("fixture-token")
        .send()
        .await;
    if result.is_err() {
        server.abort();
        let _ = server.await;
        panic!("本机重定向夹具失败：{result:?}");
    }
    assert_eq!(result.unwrap().text().await.unwrap(), "ok");
    let headers = server.await.unwrap();
    assert!(headers[0].contains("authorization: bearer fixture-token"));
    assert!(!headers[1].contains("authorization:"));
    assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
}
#[test]
fn official_assets_require_version_digest_unique_attachment_and_stable_release() {
    let name = format!(
        "Xray-{}-{}.zip",
        if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        },
        if cfg!(target_arch = "aarch64") {
            "arm64-v8a"
        } else {
            "64"
        }
    );
    let item = json!({"name":name,"browser_download_url":format!("https://github.com/XTLS/Xray-core/releases/download/v26.10.21/{name}"),"digest":format!("sha256:{}","a".repeat(64))});
    let release =
        json!({"tag_name":"v26.10.21","draft":false,"prerelease":false,"assets":[item.clone()]});
    assert_eq!(
        cores::asset("xray", "26.10.21", &release, false).unwrap().1,
        "a".repeat(64)
    );
    for (key, value) in [
        ("draft", json!(true)),
        ("tag_name", json!("v26.10.22")),
        ("prerelease", json!(true)),
        ("assets", json!([item.clone(), item.clone()])),
    ] {
        let mut altered = release.clone();
        altered[key] = value;
        assert!(
            cores::asset("xray", "26.10.21", &altered, false).is_err(),
            "{key}"
        );
    }
    for (key, value) in [
        ("browser_download_url", "https://example.test/xray.zip"),
        ("digest", "md5:bad"),
    ] {
        let mut altered = release.clone();
        altered["assets"][0][key] = json!(value);
        assert!(
            cores::asset("xray", "26.10.21", &altered, false).is_err(),
            "{key}"
        );
    }
}
#[test]
fn build_cache_needs_both_ci_flags_and_canonical_runner_directory() {
    let root = tempfile::tempdir().unwrap();
    let original = BTreeMap::from([
        ("CARGO_TARGET_DIR".into(), "owned-target".into()),
        ("CARGO_HOME".into(), "owned-cargo".into()),
        ("npm_config_cache".into(), "owned-npm".into()),
    ]);
    for (github, enabled) in [("", ""), ("true", ""), ("", "1")] {
        let mut env = original.clone();
        workspace::configure_cache(&mut env, github, enabled, None).unwrap();
        assert_eq!(env, original);
    }
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(root.path(), &alias).unwrap();
    for runner in [root.path(), alias.as_path()] {
        let mut env = original.clone();
        workspace::configure_cache(&mut env, "true", "1", Some(runner)).unwrap();
        let cache = root
            .path()
            .canonicalize()
            .unwrap()
            .join("chain-build-cache");
        assert_eq!(
            env["CARGO_TARGET_DIR"],
            cache.join("target").to_str().unwrap()
        );
        assert_eq!(env["CARGO_HOME"], cache.join("cargo").to_str().unwrap());
        assert_eq!(env["npm_config_cache"], cache.join("npm").to_str().unwrap());
        assert!(!cache.exists());
    }
}
#[test]
fn release_archive_is_verified_and_checksum_tampering_rejected() {
    let root = tempfile::tempdir().unwrap();
    let stage = root.path().join("stage");
    fs::create_dir_all(stage.join("bin")).unwrap();
    let mut files = serde_json::Map::new();
    for name in ["chain-panel", "chain-agent", "chainctl", "xray"] {
        let path = stage.join("bin").join(name);
        fs::write(&path, name).unwrap();
        files.insert(
            format!("bin/{name}"),
            json!({"sha256":package::digest(&path).unwrap(),"mode":0o755}),
        );
    }
    fs::write(stage.join("release.json"),json!({"format":1,"version":"0.1.0","os":release::system(),"arch":std::env::consts::ARCH,"files":files}).to_string()).unwrap();
    let archive = root.path().join("release.tar.gz");
    release::archive(&stage, &archive).unwrap();
    fs::write(
        root.path().join("SHA256SUMS"),
        format!("{}  release.tar.gz\n", package::digest(&archive).unwrap()),
    )
    .unwrap();
    package::checksum(&archive).unwrap();
    let extracted = root.path().join("extract");
    package::extract(&archive, &extracted).unwrap();
    let original = fs::read(&archive).unwrap();
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(&archive)
        .unwrap()
        .write_all(b"tampered")
        .unwrap();
    assert!(package::checksum(&archive).is_err());
    fs::write(&archive, original).unwrap();
    fs::write(stage.join("bin/chain-panel"), "tampered").unwrap();
    assert!(package::verify(&stage, true).is_err());
}
#[test]
fn original_load_profile_and_stage_arguments_are_preserved() {
    let options = super::Options::parse(
        [
            "integration",
            "--case",
            "load-protocols",
            "--profiles",
            "socks5",
            "tls-vision",
            "--stages",
            "basic",
            "udp-rights",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
    )
    .unwrap();
    assert_eq!(options.values["--profiles"], "socks5,tls-vision");
    assert_eq!(options.values["--stages"], "basic,udp-rights");
    assert!(super::Options::parse(vec!["integration".into(), "--profiles".into()]).is_err());
}
