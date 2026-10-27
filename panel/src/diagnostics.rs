//! Bounded, private application logs. System journal retention is deliberately separate.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io::{Read, Seek, SeekFrom, Write}, os::unix::fs::OpenOptionsExt, path::{Path, PathBuf}, sync::Mutex, time::{Duration, SystemTime}};
use tokio::io::AsyncReadExt;

const SEGMENT: u64 = 1024 * 1024;
// Panel and Agent each own half: no shared writable directory or cross-service permissions.
const BUDGET: u64 = 50 * 1024 * 1024;
const AGE: Duration = Duration::from_secs(3600);
static WRITE: Mutex<()> = Mutex::new(());

#[derive(Serialize, Deserialize)]
struct LogPolicy {
    retention_hours: u64,
    max_mib: u64,
}
impl LogPolicy {
    fn validate(&self) -> Result<()> {
        ensure!((1..=720).contains(&self.retention_hours), "日志保留时长须为 1 小时至 30 天");
        ensure!((1..=1024).contains(&self.max_mib), "日志大小上限须为 1 至 1024 MiB");
        Ok(())
    }
    fn age(&self) -> Duration { Duration::from_secs(self.retention_hours * 3600) }
    fn budget(&self) -> u64 { self.max_mib * 1024 * 1024 }
}
fn log_policy(dir: &Path) -> LogPolicy {
    std::fs::read(dir.join("log-policy.json")).ok()
        .and_then(|bytes| serde_json::from_slice::<LogPolicy>(&bytes).ok())
        .filter(|policy| policy.validate().is_ok())
        .unwrap_or(LogPolicy {retention_hours:AGE.as_secs()/3600, max_mib:BUDGET/1024/1024})
}

pub fn logging_enabled(dir: &Path) -> bool {
    !dir.join("logging-disabled").exists()
}

fn set_logging(dir: &Path, enabled: bool) -> Result<()> {
    let _guard = WRITE.lock().unwrap();
    let marker = dir.join("logging-disabled");
    if enabled { if marker.exists() { std::fs::remove_file(marker)?; } }
    else {
        crate::runtime::atomic_write(&marker, b"disabled\n")?;
        for (_, path) in sources(dir) {
            for file in segments(&path).into_iter().filter(|p| regular(p)) { std::fs::remove_file(file)?; }
        }
    }
    Ok(())
}

pub fn redact(text: &str) -> String {
    text.lines().map(|line| {
        let lower = line.to_ascii_lowercase();
        // Keep only a fixed error category: authentication errors can include credentials anywhere in the line.
        for (patterns, category) in [
            (&["invalid password", "incorrect password", "wrong password"][..], "密码认证失败 (invalid password)"),
            (&["password is not set", "missing password", "empty password"][..], "缺少密码 (missing password)"),
            (&["invalid user", "unknown user", "user not found"][..], "用户认证失败 (invalid user)"),
            (&["invalid shortid", "invalid short-id", "invalid short id"][..], "REALITY shortId 无效 (invalid shortid)"),
        ] {
            if patterns.iter().any(|pattern| lower.contains(pattern)) {
                return format!("[认证错误类别：{category}；其余字段已隐藏]");
            }
        }
        if (line.contains("://") && line.contains('@')) || ["privatekey", "private_key", "password", "authorization", "bearer ", "token=", "token:", "token\"", "secret", "private-key", "shortid", "short-id", "\"id\":", "\"uuid\"", "vless://", "vmess://", "trojan://", "ss://"].iter().any(|key| lower.contains(key)) {
            return "[已隐藏含认证信息的日志行]".to_string();
        }
        line.chars().filter(|c| !c.is_control() || *c == '\t').take(8192).collect()
    }).collect::<Vec<String>>().join("\n")
}
fn root(path: &Path) -> &Path {
    let parent = path.parent().unwrap();
    let name = parent.file_name().unwrap_or_default().to_string_lossy();
    if name.starts_with("legacy-") || name == "mihomo" { parent.parent().unwrap() } else { parent }
}
fn sources(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut result = vec![("agent".into(), dir.join("diagnostics.log")), ("xray".into(), dir.join("xray.log")), ("mihomo".into(), dir.join("mihomo/mihomo.log"))];
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.strip_prefix("legacy-").is_some_and(|id| uuid::Uuid::parse_str(id).is_ok()) && entry.file_type().is_ok_and(|t| t.is_dir()) {
                result.push((name, entry.path().join("xray.log")));
            }
        }
    }
    result
}
fn regular(path: &Path) -> bool { !path.parent().is_some_and(|p| p.is_symlink()) && std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file()) }
fn segments(path: &Path) -> Vec<PathBuf> {
    let mut result = vec![];
    let prefix = format!("{}.", path.file_name().unwrap().to_string_lossy());
    if let Ok(entries) = std::fs::read_dir(path.parent().unwrap()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let archive = name.strip_prefix(&prefix).and_then(|s| s.strip_suffix(".gz")).is_some_and(|stamp| {
                stamp.parse::<u128>().is_ok() || stamp.split_once('.').is_some_and(|(date, number)|
                    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok() && number.parse::<u128>().is_ok())
            });
            if (entry.path() == path || archive) && regular(&entry.path()) { result.push(entry.path()); }
        }
    }
    result
}
fn segment_time(path: &Path, modified: SystemTime) -> SystemTime {
    if path.extension().is_some_and(|v| v == "gz") { return modified; }
    let mut prefix = [0u8; 32];
    if let Ok(mut file) = std::fs::File::open(path) {
        if let Ok(n) = file.read(&mut prefix) {
            if let Some(at) = String::from_utf8_lossy(&prefix[..n]).split_whitespace().next().and_then(|v|v.parse::<u64>().ok()).filter(|at| *at > 1_000_000_000 && *at <= crate::model::now()) {
                return (SystemTime::UNIX_EPOCH+Duration::from_secs(at)).min(modified);
            }
        }
    }
    modified
}
fn prune(dir: &Path, budget: u64) -> Result<()> {
    let age = log_policy(dir).age();
    let mut files: Vec<_> = sources(dir).into_iter().flat_map(|(_,p)| segments(&p)).filter_map(|p| {
        let m = std::fs::metadata(&p).ok()?; Some((segment_time(&p,m.modified().ok()?), p, m.len()))
    }).collect();
    files.sort_by_key(|f| f.0);
    let mut total: u64 = files.iter().map(|f| f.2).sum();
    for (at, path, size) in files {
        if total > budget || at.elapsed().unwrap_or_default() > age {
            std::fs::remove_file(path)?; total = total.saturating_sub(size);
        }
    }
    Ok(())
}
pub fn maintain(dir: &Path) -> Result<()> {
    let _guard = WRITE.lock().unwrap();
    let policy = log_policy(dir);
    prune(dir, policy.budget())?;
    for (_, path) in sources(dir) {
        if regular(&path) {
            let since = segment_time(&path, std::fs::metadata(&path)?.modified()?);
            if log_day(since) != log_day(SystemTime::now()) { compress(&path, since)?; }
        }
    }
    prune(dir, policy.budget())
}
fn log_day(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time).format("%Y-%m-%d").to_string()
}
fn compress(path: &Path, since: SystemTime) -> Result<()> {
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_nanos();
    let archive = path.with_file_name(format!("{}.{}.{stamp}.gz", path.file_name().unwrap().to_string_lossy(), log_day(since)));
    let output = std::fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(&archive)?;
    let status = std::process::Command::new("gzip").args(["-c", "--"]).arg(path).stdout(output).stderr(std::process::Stdio::null()).status();
    if !status.is_ok_and(|s| s.success()) {
        let _ = std::fs::remove_file(archive);
        anyhow::bail!("日志压缩失败，原日志已保留");
    }
    std::fs::File::open(&archive)?.set_times(std::fs::FileTimes::new().set_modified(since))?;
    std::fs::remove_file(path)?;
    Ok(())
}
pub fn append(path: &Path, text: &str) -> Result<()> {
    let _guard = WRITE.lock().unwrap();
    if !logging_enabled(root(path)) { return Ok(()); }
    let policy = log_policy(root(path));
    if path.symlink_metadata().is_ok() { ensure!(regular(path), "拒绝写入日志符号链接"); }
    if std::fs::metadata(path).is_ok_and(|m| segment_time(path,m.modified().unwrap_or(SystemTime::now())).elapsed().unwrap_or_default() > policy.age()) {
        std::fs::remove_file(path)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() >= SEGMENT || log_day(segment_time(path,m.modified().unwrap_or(SystemTime::now()))) != log_day(SystemTime::now())) {
        let since = segment_time(path,std::fs::metadata(path)?.modified()?);
        compress(path, since)?;
    }
    ensure!(!path.parent().unwrap().is_symlink(), "拒绝日志目录符号链接");
    let mut file = std::fs::OpenOptions::new().create(true).append(true).custom_flags(libc::O_NOFOLLOW).mode(0o600).open(path)?;
    writeln!(file, "{} {}", crate::model::now(), redact(text))?;
    drop(file);
    prune(root(path), policy.budget())
}
pub fn capture<R: tokio::io::AsyncRead + Unpin + Send + 'static>(mut reader: R, path: PathBuf, secrets: Vec<String>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut chunk = [0u8; 4096]; let mut line = Vec::new(); let mut truncated = false;
        while let Ok(n) = reader.read(&mut chunk).await {
            if n == 0 { break; }
            if !logging_enabled(root(&path)) { line.clear(); truncated = false; continue; }
            for byte in &chunk[..n] {
                if *byte == b'\n' {
                    let mut text = String::from_utf8_lossy(&line).to_string();
                    for secret in &secrets { text = text.replace(secret, "[已隐藏]"); }
                    let _ = append(&path, &format!("{}{}", if truncated { "[日志行已截断] " } else { "" }, text));
                    line.clear(); truncated = false;
                } else if line.len() < 8192 { line.push(*byte); } else { truncated = true; }
            }
        }
        if !line.is_empty() {
            let mut text = String::from_utf8_lossy(&line).to_string();
            for secret in &secrets { text = text.replace(secret, "[已隐藏]"); }
            let _ = append(&path, &text);
        }
    })
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LogRequest {
    pub source: String,
    #[serde(default)] pub line_id: String,
    #[serde(default)] pub archive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub retention_hours: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub max_mib: Option<u64>,
}
impl LogRequest {
    pub fn validate(&self) -> Result<()> {
        ensure!(["agent", "panel", "xray", "mihomo"].contains(&self.source.as_str()) || self.source.strip_prefix("legacy-").is_some_and(|v| uuid::Uuid::parse_str(v).is_ok()), "日志来源无效");
        ensure!(self.line_id.is_empty() || uuid::Uuid::parse_str(&self.line_id).is_ok(), "代理标识无效");
        LogPolicy {retention_hours:self.retention_hours.unwrap_or(1), max_mib:self.max_mib.unwrap_or(50)}.validate()
    }
}
pub fn read(dir: &Path, request: &LogRequest) -> Result<Value> {
    request.validate()?;
    let mut available = sources(dir);
    if request.source == "panel" { available = vec![("panel".into(), dir.join("diagnostics.log"))]; }
    let path = available.iter().find(|(s,_)| s == &request.source).context("日志来源尚未创建")?.1.clone();
    if let Some(enabled) = request.enabled { set_logging(dir, enabled)?; }
    let _guard = WRITE.lock().unwrap();
    let mut policy = log_policy(dir);
    if request.retention_hours.is_some() || request.max_mib.is_some() {
        if let Some(hours) = request.retention_hours { policy.retention_hours = hours; }
        if let Some(mib) = request.max_mib { policy.max_mib = mib; }
        crate::runtime::atomic_write(&dir.join("log-policy.json"), &serde_json::to_vec(&policy)?)?;
    }
    prune(dir, policy.budget())?;
    let mut files = if request.archive { segments(&path) } else { vec![path] };
    files.retain(|p| regular(p));
    files.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH));
    let mut text = String::new();
    for path in files.iter().rev().take(8).rev() {
        let mut buf = Vec::new();
        if path.extension().is_some_and(|e| e == "gz") {
            let mut child = std::process::Command::new("gzip").args(["-dc", "--"]).arg(path).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn()?;
            let result = child.stdout.take().unwrap().take(2 * SEGMENT).read_to_end(&mut buf);
            let _ = child.kill(); let _ = child.wait(); result?;
        } else {
            let mut file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)?;
            let size = file.metadata()?.len(); file.seek(SeekFrom::Start(size.saturating_sub(128 * 1024)))?;
            file.take(128 * 1024).read_to_end(&mut buf)?;
        }
        text.push_str(&String::from_utf8_lossy(&buf));
    }
    let filtered: Vec<_> = text.lines().filter(|line| request.line_id.is_empty() || line.contains(&format!("line-{}", request.line_id))).collect();
    let mut kept: Vec<_> = filtered.iter().rev().take(500).copied().collect(); kept.reverse();
    let mut text = kept.join("\n");
    for name in ["xray.json", "active.json", "config.json"] {
        let file = dir.join(name);
        if regular(&file) {
            if let Ok(bytes) = std::fs::read(file) {
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) { for secret in secrets(&value) { text = text.replace(&secret, "[已隐藏]"); } }
            }
        }
    }
    let text = redact(&text);
    let text: String = text.chars().rev().take(64 * 1024).collect::<String>().chars().rev().collect();
    Ok(json!({"text":text,"logging_enabled":logging_enabled(dir),"retention_hours":policy.retention_hours,"max_mib":policy.max_mib,"sampled_at":crate::model::now(),"sources":available.iter().map(|s| &s.0).collect::<Vec<_>>(),"detail":format!("当前组件日志总计最多 {} MiB，保留 {} 小时；按 UTC 日期轮转，历史日志 gzip 压缩，定期清理。系统 journal 单独管理。",policy.max_mib,policy.retention_hours)}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_logging_persists_clears_archives_and_preserves_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("xray.log");
        append(&log, "before").unwrap();
        let archive = dir.path().join("xray.log.123.gz");
        std::fs::write(&archive, b"archive").unwrap();
        let data = dir.path().join("panel.db");
        std::fs::write(&data, b"keep").unwrap();
        let request = LogRequest {source:"xray".into(),line_id:String::new(),archive:false,enabled:Some(false),retention_hours:None,max_mib:None};
        assert_eq!(read(dir.path(), &request).unwrap()["logging_enabled"], false);
        assert!(!log.exists() && !archive.exists());
        append(&log, "after").unwrap();
        assert!(!log.exists());
        assert_eq!(std::fs::read(&data).unwrap(), b"keep");
        assert!(!logging_enabled(dir.path()));
        read(dir.path(), &LogRequest {enabled:Some(true), ..request}).unwrap();
        append(&log, "enabled again").unwrap();
        assert!(std::fs::read_to_string(log).unwrap().contains("enabled again"));
    }
    #[test]
    fn reading_logs_remains_compatible_with_old_agents() {
        let request: LogRequest = serde_json::from_value(json!({"source":"agent"})).unwrap();
        let value = serde_json::to_value(request).unwrap();
        for key in ["enabled", "retention_hours", "max_mib"] { assert!(value.get(key).is_none()); }
    }
    #[test]
    fn log_policy_rotates_days_compresses_and_enforces_time_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xray.log");
        let request = |value| serde_json::from_value::<LogRequest>(value).unwrap();
        let get = request(json!({"source":"xray"}));
        let defaults = read(dir.path(), &get).unwrap();
        assert_eq!(defaults["retention_hours"], 1);
        assert_eq!(defaults["max_mib"], 50);
        read(dir.path(), &request(json!({"source":"xray","retention_hours":48,"max_mib":1}))).unwrap();
        let yesterday = SystemTime::now()-Duration::from_secs(86400);
        let body = format!("{} yesterday\n", crate::model::now()-86400);
        std::fs::write(&path, &body).unwrap();
        maintain(dir.path()).unwrap();
        assert!(!path.exists());
        let archives = segments(&path);
        assert_eq!(archives.len(), 1);
        assert!(archives[0].file_name().unwrap().to_str().unwrap().starts_with(&format!("xray.log.{}.", log_day(yesterday))));
        let output = std::process::Command::new("gzip").arg("-dc").arg(&archives[0]).output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, body.as_bytes());
        append(&path, "fresh").unwrap();
        read(dir.path(), &request(json!({"source":"xray","retention_hours":1}))).unwrap();
        assert!(!archives[0].exists());
        assert!(std::fs::read_to_string(&path).unwrap().contains("fresh"));
        let oversized = dir.path().join("xray.log.123.gz");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len(1024*1024+1).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(SystemTime::now()-Duration::from_secs(10))).unwrap();
        drop(file);
        maintain(dir.path()).unwrap();
        assert!(!oversized.exists());
        assert!(path.exists());
        let before = std::fs::read(dir.path().join("log-policy.json")).unwrap();
        for invalid in [json!({"source":"xray","retention_hours":0}),json!({"source":"xray","max_mib":1025}),json!({"source":"xray","retention_hours":721,"enabled":false})] {
            assert!(read(dir.path(), &request(invalid)).is_err());
        }
        assert_eq!(std::fs::read(dir.path().join("log-policy.json")).unwrap(), before);
        assert!(logging_enabled(dir.path()));
    }

    #[tokio::test]
    async fn diagnostics_capture_redacts_secrets_and_bounds_long_lines() {
        use tokio::io::AsyncWriteExt;
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("xray.log");
        let (mut writer,reader) = tokio::io::duplex(64);
        let credentials = secrets(&json!({"settings":{"clients":[{"id":"test-user-credential"}]}}));
        let task = capture(reader,path.clone(),credentials);
        writer.write_all(format!("authenticated test-user-credential\n{}\n", "x".repeat(20_000)).as_bytes()).await.unwrap(); drop(writer); task.await.unwrap();
        let text = std::fs::read_to_string(path).unwrap(); assert!(!text.contains("test-user-credential"));
        assert!(text.contains("[已隐藏]")); assert!(text.contains("日志行已截断")); assert!(text.len()<9000);
    }
    #[test]
    fn authentication_errors_keep_only_safe_categories() {
        for (message, category) in [
            ("proxy/trojan: invalid password: hunter2", "密码认证失败"),
            ("PASSWORD is not set: token=hidden-token", "缺少密码"),
            ("proxy/vless: invalid user: f43628ed-1183-4f0a-b960-a33b774275df", "用户认证失败"),
            ("REALITY: invalid shortId: deadbeef password=hunter2", "REALITY shortId 无效"),
        ] {
            let result = redact(message);
            assert!(result.contains(category), "{result}");
            for secret in ["hunter2", "hidden-token", "f43628ed", "deadbeef"] {
                assert!(!result.contains(secret), "{result}");
            }
            assert_eq!(redact(&result), result, "Stored logs are redacted again on read");
        }
        assert_eq!(redact("password: hunter2"), "[已隐藏含认证信息的日志行]");
        assert_eq!(redact("Bearer hidden-token"), "[已隐藏含认证信息的日志行]");
        assert_eq!(redact("connection refused"), "connection refused");
        assert_eq!(redact("invalid password: hunter2\nconnection refused").lines().count(), 2);
    }
    #[test]
    fn archives_are_readable_and_private() {
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("xray.log");
        let mut output = std::fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(&path).unwrap();
        output.write_all(format!("marker-before-rotation\n{}\n", "x".repeat(SEGMENT as usize)).as_bytes()).unwrap(); drop(output);
        append(&path,"marker-after-rotation").unwrap();
        let files = segments(&path);
        assert!(files.iter().any(|p|p.extension().is_some_and(|e|e=="gz")));
        use std::os::unix::fs::PermissionsExt;
        for file in &files { assert_eq!(std::fs::metadata(file).unwrap().permissions().mode() & 0o077,0); }
        let value = read(dir.path(),&LogRequest {source:"xray".into(),line_id:String::new(),archive:true,enabled:None,retention_hours:None,max_mib:None}).unwrap();
        assert!(value["text"].as_str().unwrap().contains("marker-after-rotation"));
        let file = std::fs::File::open(&path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(SystemTime::now()-AGE-Duration::from_secs(1))).unwrap();
        maintain(dir.path()).unwrap(); assert!(!path.exists());
        std::fs::write(&path, format!("{} old record\n{} recent record\n",crate::model::now()-AGE.as_secs()-1,crate::model::now())).unwrap();
        maintain(dir.path()).unwrap(); assert!(!path.exists(),"不断追加的低流量日志也必须按最早记录过期");
    }
    #[tokio::test]
    #[ignore = "requires isolated CHAIN_TEST_XRAY and trusted local TLS target"]
    async fn reality_uses_real_handshake_and_releases_processes() {
        let binary = PathBuf::from(std::env::var("CHAIN_TEST_XRAY").unwrap());
        let input = crate::model::RealityTarget {target:std::env::var("CHAIN_TEST_REALITY_TARGET").unwrap(),server_name:"localhost".into(),credentials:None,limit_fallback_upload:None,limit_fallback_download:None};
        let result = reality(&binary,&input).await.unwrap();
        assert_eq!(result["ok"],true,"{result}");
        assert_eq!(result["fallback"]["status"], "verified", "{result}");
        let result = reality(&binary,&crate::model::RealityTarget {server_name:"wrong.example".into(),..input}).await.unwrap();
        assert_eq!(result["ok"],false); assert_eq!(result["status"],"failed");
        use tokio::io::AsyncWriteExt;
        let dir = tempfile::tempdir().unwrap(); let line = crate::model::id();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(); let address = listener.local_addr().unwrap();
        std::fs::write(dir.path().join("xray.json"),serde_json::to_vec(&json!({"outbounds":[{"tag":format!("line-{line}-out"),"protocol":"freedom","settings":{"finalRules":[{"action":"allow","ip":["127.0.0.1"],"port":address.port().to_string()}]}}]})).unwrap()).unwrap();
        let server = tokio::spawn(async move { let (mut stream,_) = listener.accept().await.unwrap(); let mut bytes=[0;4096]; let _=stream.read(&mut bytes).await; stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await.unwrap(); });
        let result = crate::runtime::diagnose_path(&binary,dir.path(),&line,&format!("http://{address}/")).await.unwrap();
        server.await.unwrap(); assert_eq!(result["result"]["ok"],true,"{result}");
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }
    #[test]
    fn cdn_detection_does_not_treat_unknown_as_safe() {
        assert_eq!(cdn_provider("a.edgekey.net. CNAME b.akamaiedge.net."), Some("Akamai"));
        assert_eq!(cdn_provider("Server: cloudflare\nCF-Ray: test"), Some("Cloudflare"));
        assert_eq!(cdn_provider("x-amz-cf-id: test"), Some("CloudFront"));
        assert_eq!(cdn_provider("Server: nginx"), None);
    }
    #[test]
    fn logs_keep_history_bound_storage_and_reject_paths() {
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("xray.log");
        append(&path, "first run").unwrap(); append(&path, "second run").unwrap();
        let request = LogRequest { source:"xray".into(), line_id:String::new(), archive:false, enabled:None, retention_hours:None, max_mib:None };
        let value = read(dir.path(), &request).unwrap(); assert!(value["text"].as_str().unwrap().contains("first run"));
        assert!(!redact("password: secret-value").contains("secret-value"));
        assert!(LogRequest {source:"../../admin-token".into(), ..request.clone()}.validate().is_err());
        prune(dir.path(), 1).unwrap(); assert!(!path.exists());
        std::os::unix::fs::symlink(dir.path().join("secret"), &path).unwrap();
        assert!(append(&path, "x").is_err());
    }
}

/// Two independently authenticated HTTPS clocks; unavailable references never imply clock drift.
pub async fn clock_status() -> Value {
    async fn reference(host: &str) -> Result<i64> {
        let start = std::time::Instant::now();
        let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).redirect(reqwest::redirect::Policy::none()).build()?;
        let response = client.head(format!("https://{host}/?chain-clock={}", uuid::Uuid::new_v4())).header("Cache-Control", "no-cache").send().await?;
        ensure!(start.elapsed() < Duration::from_secs(5), "时间参考延迟过高");
        ensure!(response.headers().get("age").and_then(|v| v.to_str().ok()).unwrap_or("0") == "0", "时间参考被缓存");
        let date = response.headers().get("date").context("时间参考没有 Date")?.to_str()?;
        Ok(chrono::DateTime::parse_from_rfc2822(date)?.timestamp())
    }
    let mut command = tokio::process::Command::new("timedatectl");
    command.args(["show", "--property=NTP", "--property=NTPSynchronized", "--property=Timezone"]).kill_on_drop(true);
    let system = tokio::time::timeout(Duration::from_secs(3), command.output()).await;
    let system = match system { Ok(Ok(v)) if v.status.success() => String::from_utf8_lossy(&v.stdout).to_string(), _ => String::new() };
    let (a,b) = tokio::join!(reference("www.cloudflare.com"), reference("www.google.com"));
    let mut value = json!({"checked_at":crate::model::now(),"verified":false,"detail":"无法取得两个一致的可信时间参考；时钟偏差未验证，不判定时间错误"});
    for line in system.lines() {
        if let Some(v) = line.strip_prefix("NTP=") { value["ntp_enabled"] = json!(v == "yes"); }
        if let Some(v) = line.strip_prefix("NTPSynchronized=") { value["synchronized"] = json!(v == "yes"); }
        if let Some(v) = line.strip_prefix("Timezone=") { value["timezone"] = json!(v); }
    }
    if let (Ok(a),Ok(b)) = (a,b) {
        if a.abs_diff(b) <= 10 {
            let offset = (a+b)/2 - crate::model::now() as i64;
            value["verified"] = json!(true); value["offset_seconds"] = json!(offset);
            value["detail"] = json!(format!("两个 HTTPS 时间参考一致；本机与参考相差 {offset} 秒（正数为本机落后）"));
        }
    }
    value
}

fn cdn_provider(evidence: &str) -> Option<&'static str> {
    let evidence = evidence.to_ascii_lowercase();
    if evidence.contains("cf-ray:") || evidence.contains("server: cloudflare") { Some("Cloudflare") }
    else if evidence.contains(".akamaiedge.net") || evidence.contains(".edgekey.net") || evidence.contains(".edgesuite.net") || evidence.contains("server: akamaighost") { Some("Akamai") }
    else if evidence.contains(".cloudfront.net") || evidence.contains("x-amz-cf-id:") { Some("CloudFront") }
    else { None }
}

async fn reality_cdn(input: &crate::model::RealityTarget) -> Value {
    let check = async {
        let (host, port) = crate::compiler::target(&input.target)?;
        let addresses: Vec<_> = tokio::net::lookup_host((host.as_str(), port)).await?.collect();
        ensure!(!addresses.is_empty(), "目标 DNS 未返回地址");
        let mut evidence = String::new();
        if host.parse::<std::net::IpAddr>().is_err() {
            let mut command = tokio::process::Command::new("dig");
            command.args(["+time=1", "+tries=1", "+noall", "+answer", &host, "A"]).kill_on_drop(true);
            if let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(2), command.output()).await {
                if output.status.success() { evidence = String::from_utf8_lossy(&output.stdout).into_owned(); }
            }
        }
        let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2)).resolve_to_addrs(&input.server_name, &addresses).build()?;
        if let Ok(response) = client.head(format!("https://{}/", crate::compiler::address(&input.server_name, port))).send().await {
            for name in ["server", "cf-ray", "x-amz-cf-id"] {
                if let Some(value) = response.headers().get(name).and_then(|v|v.to_str().ok()) {
                    evidence.push_str(&format!("\n{name}: {value}"));
                }
            }
        }
        let provider = cdn_provider(&evidence);
        Ok::<_,anyhow::Error>(json!({"status":if provider.is_some() {"warning"} else {"unknown"},"provider":provider,
            "addresses":addresses.iter().map(|a|a.ip().to_string()).collect::<Vec<_>>(),
            "detail":provider.map(|p|format!("发现 {p} CDN 特征；存在共享目标风险，不代表已证实可盗流"))
                .unwrap_or_else(|| "未识别到已知 CDN 特征，不代表无 CDN 或无回落风险".into())}))
    };
    match tokio::time::timeout(Duration::from_secs(5), check).await {
        Ok(Ok(result)) => result,
        _ => json!({"status":"unverified","detail":"CDN 特征检测未完成，不能判断回落风险"}),
    }
}

async fn fallback_tls(endpoint: &str, sni: Option<&str>) -> &'static str {
    let mut command = tokio::process::Command::new("openssl");
    command.args(["s_client", "-connect", endpoint, "-tls1_3", "-brief"]);
    if let Some(sni) = sni { command.args(["-servername", sni]); } else { command.arg("-noservername"); }
    command.stdin(std::process::Stdio::null()).kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(3), command.output()).await {
        Ok(Ok(output)) => {
            let text = String::from_utf8_lossy(&output.stderr);
            if text.contains("CONNECTION ESTABLISHED") { "accepted" }
            else if !output.status.success() && (text.contains("unexpected eof") || text.contains("errno=104") || text.contains("Connection reset by peer")) { "rejected" }
            else { "unverified" }
        },
        _ => "unverified",
    }
}

/// An isolated server/client pair verifies REALITY itself, without restarting the live core.
pub async fn reality(binary: &Path, input: &crate::model::RealityTarget) -> Result<Value> {
    use crate::{model::{Reality, id}, runtime::{atomic_write, spawn_xray, stop, validate}};
    crate::compiler::target(&input.target)?;
    ensure!(crate::compiler::host_valid(&input.server_name), "SNI 域名无效");
    let start = std::time::Instant::now();
    let (basic, cdn) = tokio::join!(crate::runtime::check_reality(input), reality_cdn(input));
    if basic.as_ref().is_ok_and(|v|v["sni_mismatch"] == true) {
        return Ok(json!({"ok":false,"status":"failed","cdn":cdn,"fallback":{"status":"unverified","detail":"握手失败，未验证过滤"},"detail":"SNI 与目标证书不匹配，请修正当前跳的目标与 SNI；各跳无需使用相同 SNI","latency_ms":start.elapsed().as_millis()}));
    }
    if !basic.as_ref().is_ok_and(|v| v["ok"] == true) {
        return Ok(json!({"ok":false,"status":"unverified","cdn":cdn,"fallback":{"status":"unverified","detail":"握手未通过，未验证过滤"},"detail":"目标 TLS / SNI 验证未通过；可能是网络、目标证书或本机时间异常。尚未确认 REALITY 可用，未更换目标。","latency_ms":start.elapsed().as_millis()}));
    }
    let temp = tempfile::tempdir()?;
    let reserve = || -> Result<u16> { Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port()) };
    let server_port = reserve()?; let socks_port = reserve()?;
    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let echo_port = echo.local_addr()?.port();
    let marker = id(); let response_marker = marker.clone();
    let echo_task = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        if let Ok((mut socket,_)) = echo.accept().await {
            let mut buf = [0;4096]; let _ = socket.read(&mut buf).await;
            let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response_marker.len(), response_marker);
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    struct Abort(tokio::task::AbortHandle);
    impl Drop for Abort { fn drop(&mut self) { self.0.abort(); } }
    let _echo_cleanup = Abort(echo_task.abort_handle());
    let identity = Reality::new(input.target.clone(), input.server_name.clone());
    let user = id();
    // Newer cores block private destinations; allow only this disposable echo endpoint.
    let mut server = json!({"log":{"loglevel":"debug"},"inbounds":[],"outbounds":[{"tag":"direct","protocol":"freedom","settings":{"finalRules":[{"action":"allow","ip":["127.0.0.1"],"port":echo_port.to_string()}]}},{"tag":"blocked","protocol":"blackhole"}],"routing":{"rules":[]}});
    let mut inbound = json!({"tag":"diagnostic","listen":"127.0.0.1","port":server_port,"protocol":"vless","settings":{"clients":[{"id":user,"flow":"xtls-rprx-vision"}],"decryption":"none"},"streamSettings":{"network":"tcp","security":"reality","realitySettings":{"target":input.target,"serverNames":[input.server_name],"privateKey":identity.private_key,"shortIds":[identity.short_id]}}});
    crate::compiler::guard_reality(&mut server, &mut inbound, reserve()?)?;
    server["inbounds"].as_array_mut().unwrap().push(inbound);
    let client = json!({"log":{"loglevel":"debug"},"inbounds":[{"listen":"127.0.0.1","port":socks_port,"protocol":"socks","settings":{"auth":"noauth"}}],"outbounds":[{"protocol":"vless","settings":{"vnext":[{"address":"127.0.0.1","port":server_port,"users":[{"id":user,"encryption":"none","flow":"xtls-rprx-vision"}]}]},"streamSettings":{"network":"tcp","security":"reality","realitySettings":{"serverName":input.server_name,"publicKey":identity.public_key,"shortId":identity.short_id,"fingerprint":"chrome"}}}]});
    let mut server_child = None; let mut client_child = None;
    let mut fallback = json!({"status":"unverified","detail":"尚未完成 SNI 过滤验证"});
    let outcome = async {
        let server_file = temp.path().join("server.json"); let client_file = temp.path().join("client.json");
        atomic_write(&server_file, &serde_json::to_vec(&server)?)?; atomic_write(&client_file, &serde_json::to_vec(&client)?)?;
        validate(binary, &server_file).await?; validate(binary, &client_file).await?;
        server_child = Some(spawn_xray(binary, &server_file, &temp.path().join("server.log"))?);
        client_child = Some(spawn_xray(binary, &client_file, &temp.path().join("client.log"))?);
        tokio::time::sleep(Duration::from_secs(6)).await;
        let response = reqwest::Client::builder().no_proxy().proxy(reqwest::Proxy::all(format!("socks5h://127.0.0.1:{socks_port}"))?).timeout(Duration::from_secs(10)).build()?.get(format!("http://127.0.0.1:{echo_port}/")).send().await?;
        ensure!(response.status().is_success() && response.text().await? == marker, "REALITY 数据验证失败");
        let endpoint = format!("127.0.0.1:{server_port}");
        let wrong = format!("{}.invalid", id());
        let (allowed, other, missing) = tokio::join!(fallback_tls(&endpoint, Some(&input.server_name)), fallback_tls(&endpoint, Some(&wrong)), fallback_tls(&endpoint, None));
        let verified = allowed == "accepted" && other == "rejected" && missing == "rejected";
        fallback = json!({"status":if verified {"verified"} else {"unverified"},"allowed_sni":allowed,"wrong_sni":other,"missing_sni":missing,
            "detail":if verified {"临时实例：正确 SNI 可达，错误及无 SNI 已拒绝；允许目标仍可消耗流量，线上入口未验证"} else {"临时实例的 SNI 过滤未全部验证通过；线上入口未验证"}});
        Ok::<_,anyhow::Error>(())
    }.await;
    stop(&mut client_child).await; stop(&mut server_child).await; echo_task.abort(); let _ = echo_task.await;
    let ok = outcome.is_ok();
    let timeout = outcome.as_ref().err().and_then(|e|e.downcast_ref::<reqwest::Error>()).is_some_and(|e|e.is_timeout());
    let logs = ["server.log","client.log"].iter().map(|name|std::fs::read_to_string(temp.path().join(name)).unwrap_or_default()).collect::<Vec<_>>().join("\n");
    let logs: String = logs.chars().rev().take(4000).collect::<String>().chars().rev().collect();
    Ok(json!({"ok":ok,"status":if ok {"verified"} else if timeout {"unverified"} else {"failed"},"cdn":cdn,"fallback":fallback,"detail":if ok {"该 Agent 上真实 REALITY 握手与数据交换通过；此结果不代表无盗流风险".to_string()} else {format!("普通 TLS 检查通过，但该 Agent 的真实 REALITY 握手未通过：{}。检查目标/SNI与网络；未修改运行中代理。\n{}", redact(&format!("{:#}",outcome.unwrap_err())),redact(&logs))},"latency_ms":start.elapsed().as_millis()}))
}


pub fn secrets(value: &Value) -> Vec<String> {
    fn visit(value:&Value, output:&mut Vec<String>) {
        match value {
            Value::Object(map) => for (key,value) in map {
                if ["id","uuid","password","pass","privatekey","private_key","token","shortid","shortids"].contains(&key.to_ascii_lowercase().as_str()) {
                    if let Some(text)=value.as_str().filter(|v| !v.is_empty()) { output.push(text.to_string()); }
                    if let Some(items)=value.as_array() { for text in items.iter().filter_map(Value::as_str).filter(|s|!s.is_empty()) { output.push(text.to_string()); } }
                }
                visit(value,output);
            },
            Value::Array(values) => for value in values { visit(value,output); },
            _ => (),
        }
    }
    let mut output=vec![];visit(value,&mut output);output.sort_by_key(|v|std::cmp::Reverse(v.len()));output.dedup();output
}
