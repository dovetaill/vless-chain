//! Native HTTP-01 and Cloudflare DNS-01 issuance; existing certificate files are only read.
use anyhow::{Context, Result, ensure};
use instant_acme::{
    Account, AccountBuilder, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt,
    NewAccount, NewOrder, OrderStatus, RetryPolicy,
};
use openssl::{
    asn1::Asn1Time,
    ec::{EcGroup, EcKey},
    hash::MessageDigest,
    nid::Nid,
    pkey::PKey,
    x509::{X509, X509NameBuilder, X509Req, extension::SubjectAlternativeName},
};
use std::{
    fs,
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, OpenOptionsExt, symlink},
    },
    path::{Path, PathBuf},
    time::Duration,
};
mod cloudflare;
#[cfg(test)]
#[path = "acme_tests.rs"]
mod tests;

pub fn paths(config: &Path, name: &str) -> PathBuf {
    let native = config.join("rust/live").join(name);
    if native.exists() {
        native
    } else {
        config.join("live").join(name)
    }
}

fn identifier(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 253
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c))
            && !name.starts_with('.')
            && !name.ends_with('.')
            && !name.contains(".."),
        "证书名称或域名无效"
    );
    Ok(())
}

fn ordinary(path: &Path) -> Result<()> {
    for p in path.ancestors() {
        match fs::symlink_metadata(p) {
            Ok(m) => ensure!(
                !m.file_type().is_symlink(),
                "ACME 写入路径包含链接：{}",
                p.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn private_dir(path: &Path) -> Result<()> {
    ordinary(path)?;
    if !path.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    ensure!(fs::metadata(path)?.is_dir(), "ACME 路径不是目录");
    Ok(())
}
pub(crate) fn challenge_directory(path: &Path) -> Result<()> {
    ordinary(path)?;
    let missing = path
        .ancestors()
        .take_while(|p| !p.exists())
        .collect::<Vec<_>>();
    for directory in missing.into_iter().rev() {
        fs::DirBuilder::new().mode(0o755).create(directory)?;
        fs::set_permissions(
            directory,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )?;
    }
    ensure!(fs::metadata(path)?.is_dir(), "HTTP-01 路径不是目录");
    Ok(())
}
fn save(path: &Path, bytes: &[u8]) -> Result<()> {
    ordinary(path)?;
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("缺少父目录")?)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

pub fn remaining(config: &Path, name: &str, domain: &str) -> Result<Option<i64>> {
    identifier(name)?;
    identifier(domain)?;
    let directory = paths(config, name);
    let cert = directory.join("fullchain.pem");
    let key = directory.join("privkey.pem");
    if !cert.exists() && !key.exists() {
        return Ok(None);
    }
    ensure!(
        cert.is_file() && key.is_file(),
        "已有证书或私钥缺失，拒绝覆盖"
    );
    let chain = X509::stack_from_pem(&fs::read(&cert)?)?;
    let leaf = chain.first().context("证书链为空")?;
    let key = PKey::private_key_from_pem(&fs::read(key)?)?;
    ensure!(leaf.public_key()?.public_eq(&key), "已有证书与私钥不匹配");
    let remaining = validate_leaf(leaf, domain)?;
    Ok(Some(remaining))
}
fn validate_leaf(leaf: &openssl::x509::X509Ref, domain: &str) -> Result<i64> {
    let names = leaf.subject_alt_names().context("已有证书缺少域名")?;
    ensure!(
        names
            .iter()
            .filter_map(|n| n.dnsname())
            .any(|n| n.eq_ignore_ascii_case(domain)
                || n.strip_prefix("*.").is_some_and(|suffix| domain
                    .split_once('.')
                    .is_some_and(|(_, rest)| rest.eq_ignore_ascii_case(suffix)))),
        "已有证书域名不匹配"
    );
    let now = Asn1Time::days_from_now(0)?;
    ensure!(leaf.not_before() <= now.as_ref(), "已有证书尚未生效");
    let diff = now.diff(leaf.not_after())?;
    Ok(i64::from(diff.days) * 86400 + i64::from(diff.secs))
}

pub async fn obtain(config: &Path, name: &str, domain: &str, webroot: &Path) -> Result<bool> {
    if remaining(config, name, domain)?.is_some_and(|seconds| seconds > 30 * 86400) {
        return Ok(false);
    }
    tokio::time::timeout(
        Duration::from_secs(180),
        issue(
            config,
            name,
            domain,
            webroot,
            Account::builder()?,
            LetsEncrypt::Production.url(),
        ),
    )
    .await
    .context("证书申请超时")?
}

async fn issue(
    config: &Path,
    name: &str,
    domain: &str,
    webroot: &Path,
    builder: AccountBuilder,
    directory: &str,
) -> Result<bool> {
    issue_with(
        config,
        name,
        domain,
        Some(webroot),
        None,
        builder,
        directory,
    )
    .await
}
pub async fn obtain_dns(
    config: &Path,
    name: &str,
    domain: &str,
    credentials: &Path,
) -> Result<bool> {
    if remaining(config, name, domain)?.is_some_and(|s| s > 30 * 86400) {
        return Ok(false);
    }
    tokio::time::timeout(
        Duration::from_secs(240),
        issue_with(
            config,
            name,
            domain,
            None,
            Some(credentials),
            Account::builder()?,
            LetsEncrypt::Production.url(),
        ),
    )
    .await
    .context("DNS 证书申请超时")?
}
async fn issue_with(
    config: &Path,
    name: &str,
    domain: &str,
    webroot: Option<&Path>,
    credentials: Option<&Path>,
    builder: AccountBuilder,
    directory: &str,
) -> Result<bool> {
    identifier(name)?;
    identifier(domain)?;
    private_dir(config)?;
    let config = config.canonicalize()?;
    let root = config.join("rust");
    private_dir(&root)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join(".lock"))?;
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "已有证书任务运行，请稍后重试"
    );
    let account_file = root.join("account.json");
    ordinary(&account_file)?;
    let account = if account_file.exists() {
        builder
            .from_credentials(serde_json::from_slice(&fs::read(&account_file)?)?)
            .await?
    } else {
        let (account, credentials) = builder
            .create(
                &NewAccount {
                    contact: &[],
                    terms_of_service_agreed: true,
                    only_return_existing: false,
                },
                directory.to_string(),
                None,
            )
            .await?;
        save(&account_file, &serde_json::to_vec(&credentials)?)?;
        account
    };
    let identifiers = [Identifier::Dns(domain.into())];
    let mut order = account.new_order(&NewOrder::new(&identifiers)).await?;
    let challenge_dir = webroot.map(|root| root.join(".well-known/acme-challenge"));
    if let Some(path) = &challenge_dir {
        challenge_directory(path)?;
    }
    let mut dns = if let Some(path) = credentials {
        Some(cloudflare::Challenges::new(path)?)
    } else {
        None
    };
    struct Challenges(Vec<PathBuf>);
    impl Drop for Challenges {
        fn drop(&mut self) {
            for path in &self.0 {
                if let Err(e) = fs::remove_file(path) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        eprintln!("无法清理挑战文件 {}：{e}", path.display());
                    }
                }
            }
        }
    }
    let mut challenges = Challenges(vec![]);
    {
        let mut authorizations = order.authorizations();
        while let Some(result) = authorizations.next().await {
            let mut authorization = result?;
            match authorization.status {
                AuthorizationStatus::Valid => continue,
                AuthorizationStatus::Pending => (),
                ref other => anyhow::bail!("ACME 授权状态无效：{other:?}"),
            }
            let kind = if dns.is_some() {
                ChallengeType::Dns01
            } else {
                ChallengeType::Http01
            };
            let mut challenge = authorization
                .challenge(kind)
                .context("CA 不支持所需验证方式")?;
            if let Some(dns) = dns.as_mut() {
                dns.create(domain, &challenge.key_authorization().dns_value())
                    .await?;
                tokio::time::sleep(Duration::from_secs(60)).await;
                challenge.set_ready().await?;
                continue;
            }
            let token = &challenge.token;
            ensure!(
                !token.is_empty()
                    && token.len() <= 256
                    && token
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
                "CA 挑战 token 无效"
            );
            let path = challenge_dir
                .as_ref()
                .context("缺少 HTTP-01 目录")?
                .join(token);
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o644)
                .open(&path)?;
            challenges.0.push(path);
            file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o644))?;
            file.write_all(challenge.key_authorization().as_str().as_bytes())?;
            file.sync_all()?;
            challenge.set_ready().await?;
        }
    }
    ensure!(
        order.poll_ready(&RetryPolicy::default()).await? == OrderStatus::Ready,
        "ACME 订单未通过验证"
    );
    if let Some(dns) = dns.as_mut() {
        dns.clear().await?;
    }
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1)?;
    let key = PKey::from_ec_key(EcKey::generate(&group)?)?;
    let mut request = X509Req::builder()?;
    request.set_pubkey(&key)?;
    let mut subject = X509NameBuilder::new()?;
    subject.append_entry_by_nid(Nid::COMMONNAME, domain)?;
    request.set_subject_name(&subject.build())?;
    let mut extensions = openssl::stack::Stack::new()?;
    extensions.push(
        SubjectAlternativeName::new()
            .dns(domain)
            .build(&request.x509v3_context(None))?,
    )?;
    request.add_extensions(&extensions)?;
    request.sign(&key, MessageDigest::sha256())?;
    order.finalize_csr(&request.build().to_der()?).await?;
    let chain = order.poll_certificate(&RetryPolicy::default()).await?;
    let leaf = X509::stack_from_pem(chain.as_bytes())?
        .into_iter()
        .next()
        .context("CA 返回空证书链")?;
    ensure!(
        leaf.public_key()?.public_eq(&key),
        "CA 返回证书的公钥不匹配"
    );
    ensure!(validate_leaf(&leaf, domain)? > 0, "CA 返回过期证书");
    let archive = root.join("archive").join(name);
    private_dir(&archive)?;
    let generation = tempfile::Builder::new()
        .prefix("cert-")
        .tempdir_in(&archive)?;
    save(&generation.path().join("fullchain.pem"), chain.as_bytes())?;
    save(
        &generation.path().join("privkey.pem"),
        &key.private_key_to_pem_pkcs8()?,
    )?;
    let live = root.join("live");
    private_dir(&live)?;
    let link = live.join(name);
    if let Ok(info) = fs::symlink_metadata(&link) {
        ensure!(info.file_type().is_symlink(), "原生证书目录被外部文件占用");
        ensure!(
            link.canonicalize()?.starts_with(archive.canonicalize()?),
            "证书链接指向其他目录"
        );
    }
    let temporary = live.join(format!(".{}", uuid::Uuid::new_v4()));
    let relative = Path::new("../archive")
        .join(name)
        .join(generation.path().file_name().context("缺少证书版本名")?);
    symlink(&relative, &temporary)?;
    if let Err(e) = fs::rename(&temporary, &link) {
        let _ = fs::remove_file(&temporary);
        return Err(e.into());
    }
    let _ = generation.keep();
    fs::File::open(&live)?.sync_all()?;
    // Keep historical certificate generations for rollback; no existing Certbot paths are rewritten.
    Ok(true)
}
