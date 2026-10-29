use super::{
    platform,
    upgrade::{Manager, atomic, command, ordinary, output},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
#[derive(Clone, Debug)]
pub struct Directive {
    pub words: Vec<String>,
    pub start: usize,
    pub end: usize,
    pub block: bool,
    pub depth: Vec<String>,
}
pub fn parse(text: &str) -> Result<Vec<Directive>> {
    let mut result = vec![];
    let mut words = vec![];
    let mut token = String::new();
    let mut depth = vec![];
    let (mut start, mut quote, mut escape, mut comment) = (0, None, false, false);
    let mut variable = false;
    for (i, c) in text.char_indices() {
        if comment {
            if c == '\n' {
                comment = false;
            }
            continue;
        }
        if escape {
            token.push(c);
            escape = false;
            continue;
        }
        if c == '\\' {
            escape = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                token.push(c);
            }
            continue;
        }
        if c == '#' {
            comment = true;
            continue;
        }
        if c == '\'' || c == '"' {
            if token.is_empty() && words.is_empty() {
                start = i;
            }
            quote = Some(c);
            continue;
        }
        if c.is_whitespace() {
            if !token.is_empty() {
                words.push(std::mem::take(&mut token));
            }
            continue;
        }
        if variable || (c == '{' && token.ends_with('$')) {
            variable = c != '}';
            token.push(c);
            continue;
        }
        if matches!(c, ';' | '{' | '}') {
            if !token.is_empty() {
                words.push(std::mem::take(&mut token));
            }
            if c == '}' {
                ensure!(words.is_empty() && !depth.is_empty(), "Nginx 块结构无效");
                depth.pop();
            } else {
                ensure!(!words.is_empty(), "Nginx 指令为空");
                result.push(Directive {
                    words: words.clone(),
                    start,
                    end: i + 1,
                    block: c == '{',
                    depth: depth.clone(),
                });
                if c == '{' {
                    depth.push(words[0].clone());
                }
                words.clear();
            }
        } else {
            if token.is_empty() && words.is_empty() {
                start = i;
            }
            token.push(c);
        }
    }
    ensure!(
        quote.is_none() && !escape && depth.is_empty() && words.is_empty() && token.is_empty(),
        "Nginx 配置不完整"
    );
    Ok(result)
}
pub fn quote(v: &str) -> String {
    format!(
        "\"{}\"",
        v.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
    )
}
pub fn main_path(manager: &Manager) -> PathBuf {
    if cfg!(target_os = "macos") {
        manager.path("/opt/homebrew/etc/nginx/nginx.conf")
    } else {
        manager.path("/etc/nginx/nginx.conf")
    }
}
fn expand(pattern: &Path) -> Result<Vec<PathBuf>> {
    let name = pattern
        .file_name()
        .and_then(|s| s.to_str())
        .context("include 路径无效")?;
    if !name.contains('*') {
        ensure!(!name.contains(['?', '[']), "不支持的 include 模式");
        return Ok(vec![pattern.into()]);
    }
    ensure!(
        name.matches('*').count() == 1
            && !pattern.parent().unwrap().to_string_lossy().contains('*'),
        "复杂 include 模式需手动整合"
    );
    let (prefix, suffix) = name.split_once('*').unwrap();
    let mut result = vec![];
    let entries = match fs::read_dir(pattern.parent().unwrap()) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    for p in entries {
        let p = p?;
        let n = p.file_name();
        let n = n.to_string_lossy();
        if n.starts_with(prefix) && n.ends_with(suffix) {
            result.push(p.path());
        }
    }
    result.sort();
    Ok(result)
}
pub fn files(main: &Path) -> Result<BTreeMap<PathBuf, String>> {
    fn visit(
        path: &Path,
        base: &Path,
        out: &mut BTreeMap<PathBuf, String>,
        active: &mut BTreeSet<PathBuf>,
    ) -> Result<()> {
        let path = path
            .canonicalize()
            .with_context(|| format!("读取 include {}", path.display()))?;
        ensure!(active.insert(path.clone()), "Nginx include 循环");
        if out.contains_key(&path) {
            active.remove(&path);
            return Ok(());
        }
        ensure!(out.len() < 256, "Nginx include 文件过多");
        let text = fs::read_to_string(&path)?;
        ensure!(text.len() < 4 * 1024 * 1024, "Nginx 配置过大");
        let directives = parse_fragment(&text)?;
        out.insert(path.clone(), text);
        for d in directives {
            if d.words.first().is_some_and(|s| s == "include") {
                ensure!(d.words.len() == 2, "include 参数无效");
                let p = Path::new(&d.words[1]);
                let p = if p.is_absolute() {
                    p.into()
                } else {
                    base.join(p)
                };
                for child in expand(&p)? {
                    visit(&child, base, out, active)?;
                }
            }
        }
        active.remove(&path);
        Ok(())
    }
    let main = main.canonicalize()?;
    let mut out = BTreeMap::new();
    visit(
        &main,
        main.parent().unwrap(),
        &mut out,
        &mut BTreeSet::new(),
    )?;
    Ok(out)
}
// Included files may contain directives without a surrounding http block.
fn parse_fragment(text: &str) -> Result<Vec<Directive>> {
    parse(text)
}
pub fn site_directory(main: &Path) -> Result<PathBuf> {
    for d in parse(&fs::read_to_string(main)?)? {
        if d.words.first().is_some_and(|s| s == "include")
            && d.depth == ["http"]
            && d.words.len() == 2
        {
            let p = Path::new(&d.words[1]);
            if p.file_name().is_some_and(|s| s == "*.conf" || s == "*") {
                return Ok(if p.is_absolute() {
                    p.parent().unwrap().into()
                } else {
                    main.parent().unwrap().join(p.parent().unwrap())
                });
            }
        }
    }
    anyhow::bail!("Nginx http 块缺少独立站点 include")
}
pub fn domain(value: &str) -> Result<String> {
    let name = value.trim_end_matches('.').to_ascii_lowercase();
    ensure!(
        name.parse::<std::net::IpAddr>().is_err()
            && name.contains('.')
            && name.len() <= 253
            && name.split('.').all(|label| !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')),
        "域名无效"
    );
    Ok(name)
}
pub fn replace_owned(path: &Path, expected: Option<&str>, text: &str, owner: &str) -> Result<()> {
    ordinary(path)?;
    if path.exists() {
        let current = fs::read_to_string(path)?;
        ensure!(
            current.starts_with(owner) && expected.is_none_or(|v| v == current),
            "配置归属不符或已被修改：{}",
            path.display()
        );
        return super::ingress::write_existing(path, &current, text);
    } else {
        ensure!(expected.is_none(), "原配置已被删除");
    }
    atomic(path, text.as_bytes(), 0o644)
}
async fn ensure_service(manager: &Manager) -> Result<()> {
    if manager.kind() != "launchd" {
        return Ok(());
    }
    let probe = output("ps", &["-axo", "uid=,command="], 10).await?;
    ensure!(probe.status.success(), "无法检查 Nginx 主进程");
    let processes = String::from_utf8(probe.stdout)?;
    let masters = processes
        .lines()
        .filter(|s| s.contains("nginx: master process"))
        .collect::<Vec<_>>();
    ensure!(
        masters
            .iter()
            .all(|s| s.split_whitespace().next() == Some("0")),
        "请先停止普通用户的 Nginx 服务"
    );
    if !masters.is_empty() {
        ensure!(
            output("launchctl", &["print", "system/org.vless-chain.nginx"], 15)
                .await?
                .status
                .success(),
            "运行中的 Nginx 不属于本项目系统服务"
        );
    }
    let path = manager.path("/Library/LaunchDaemons/org.vless-chain.nginx.plist");
    ordinary(&path)?;
    if path.exists() {
        ensure!(
            fs::read_to_string(&path)?.contains(super::upgrade::OWNER)
                || fs::read_to_string(&path)?.contains(super::https::OWNER),
            "Nginx 系统服务归属不匹配"
        );
    }
    let binary = platform::executable("nginx").context("缺少 Nginx")?;
    let text = platform::definition(
        manager,
        "nginx",
        &[
            binary.to_string_lossy().into_owned(),
            "-g".into(),
            "daemon off;".into(),
        ],
        None,
        &Default::default(),
    )?;
    atomic(&path, text.as_bytes(), 0o644)
}
pub async fn reload(manager: &Manager) -> Result<()> {
    ensure!(manager.live(), "离线安装不会操作 Nginx");
    validate(manager).await?;
    ensure_service(manager).await?;
    let active = match manager.kind() {
        "systemd" => output("systemctl", &["is-active", "--quiet", "nginx"], 15)
            .await?
            .status
            .success(),
        "openrc" => output("rc-service", &["nginx", "status"], 15)
            .await?
            .status
            .success(),
        _ => output("launchctl", &["print", "system/org.vless-chain.nginx"], 15)
            .await?
            .status
            .success(),
    };
    platform::related(manager, if active { "reload" } else { "start" }, "nginx").await
}
pub async fn validate(manager: &Manager) -> Result<()> {
    let main = main_path(manager);
    validate_config(manager, &main).await
}
pub async fn validate_config(manager: &Manager, main: &Path) -> Result<()> {
    let files = files(&main).with_context(|| format!("读取 Nginx 配置 {}", main.display()))?;
    let work = tempfile::Builder::new()
        .prefix("chain-nginx-check-")
        .tempdir_in(manager.path("/var/tmp"))?;
    let root = work.path();
    fs::create_dir(root.join("logs"))?;
    let mut rewritten = BTreeMap::new();
    for (i, path) in files.keys().enumerate() {
        rewritten.insert(path.clone(), root.join(format!("conf-{i}.conf")));
    }
    for (path, text) in &files {
        let mut result = text.clone();
        for d in parse(text)?.into_iter().rev() {
            let key = d.words[0].as_str();
            let replacement = if key == "include" {
                let p = Path::new(&d.words[1]);
                let p = if p.is_absolute() {
                    p.into()
                } else {
                    main.parent().unwrap().join(p)
                };
                Some(
                    expand(&p)?
                        .iter()
                        .map(|p| {
                            Ok(format!(
                                "include {};",
                                quote(
                                    rewritten
                                        .get(&p.canonicalize()?)
                                        .context("include 未收录")?
                                        .to_str()
                                        .unwrap()
                                )
                            ))
                        })
                        .collect::<Result<Vec<_>>>()?
                        .join("\n"),
                )
            } else if key == "user" {
                Some("user root;".into())
            } else if ["error_log", "access_log", "pid", "lock_file"].contains(&key) {
                Some(format!(
                    "{key} {};",
                    quote(root.join(format!("{key}-{}", d.start)).to_str().unwrap())
                ))
            } else if key.ends_with("_temp_path") || key.ends_with("_cache_path") {
                let dir = root.join(format!(
                    "tmp-{}-{}",
                    rewritten[path].file_stem().unwrap().to_string_lossy(),
                    d.start
                ));
                fs::create_dir(&dir)?;
                Some(format!(
                    "{key} {} {};",
                    quote(dir.to_str().unwrap()),
                    d.words
                        .iter()
                        .skip(2)
                        .map(|s| quote(s))
                        .collect::<Vec<_>>()
                        .join(" ")
                ))
            } else {
                None
            };
            if let Some(r) = replacement {
                result.replace_range(d.start..d.end, &r);
            }
        }
        if path == &main.canonicalize()? {
            // Override compiled defaults too; all initialization output stays in this disposable tree.
            if !parse(text)?.iter().any(|d| d.words[0] == "error_log") {
                result = format!(
                    "error_log {};\n{result}",
                    quote(root.join("error.log").to_str().unwrap())
                );
            }
            if !parse(text)?.iter().any(|d| d.words[0] == "pid") {
                result = format!(
                    "pid {};\n{result}",
                    quote(root.join("nginx.pid").to_str().unwrap())
                );
            }
            let http = parse(&result)?
                .into_iter()
                .find(|d| d.words[0] == "http" && d.block)
                .context("缺少 http 块")?;
            let defaults = ["client_body", "proxy", "fastcgi", "uwsgi", "scgi"]
                .iter()
                .map(|n| {
                    let dir = root.join(n);
                    fs::create_dir_all(&dir)?;
                    Ok(format!("\n{n}_temp_path {};", quote(dir.to_str().unwrap())))
                })
                .collect::<Result<String>>()?;
            // Only inject defaults absent from the full expanded configuration.
            let defaults = defaults
                .lines()
                .filter(|line| {
                    !files
                        .values()
                        .any(|s| s.contains(line.split_whitespace().next().unwrap_or("\0")))
                })
                .collect::<Vec<_>>()
                .join("\n");
            result.insert_str(http.end, &defaults);
        }
        fs::write(&rewritten[path], result)?;
    }
    let args = vec![
        platform::executable("nginx")
            .context("缺少 Nginx")?
            .display()
            .to_string(),
        "-t".into(),
        "-p".into(),
        root.display().to_string(),
        "-c".into(),
        rewritten[&main.canonicalize()?].display().to_string(),
    ];
    fs::write(
        root.join("request.json"),
        serde_json::to_vec(
            &serde_json::json!({"nginx_check":true,"args":args,"env":{"PATH":platform::PATH,"HOME":root,"TMPDIR":root}}),
        )?,
    )?;
    // The same worker blocks writes outside the mirror and chmod/chown anywhere.
    command(
        std::env::current_exe()?.to_str().unwrap(),
        &["internal-core-worker", root.to_str().unwrap()],
        30,
    )
    .await?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn domains_reject_addresses_and_configuration_injection() {
        assert_eq!(domain("Panel.Example.com.").unwrap(), "panel.example.com");
        for value in [
            "https://example.com",
            "example.com:443",
            "example.com; include /tmp/x;",
            "1.2.3.4",
            "*.example.com",
            "-bad.example.com",
        ] {
            assert!(domain(value).is_err(), "{value}");
        }
    }
    #[test]
    fn lexer_keeps_quoted_semicolons_and_blocks() {
        let d = parse(
            "# test\nhttp { server { listen 443 ssl; set $x 'a;b'; } include conf.d/*.conf; }",
        )
        .unwrap();
        assert_eq!(
            d.iter().find(|d| d.words[0] == "set").unwrap().words[2],
            "a;b"
        );
        assert_eq!(d.last().unwrap().depth, ["http"]);
        assert!(parse("http {").is_err());
        assert!(domain("x;bad.com").is_err());
    }
}
