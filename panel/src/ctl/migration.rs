use super::{
    lifecycle,
    upgrade::{Manager, atomic, command, ordinary, output},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

fn proxy(name: &str) -> bool {
    ["xray", "v2ray", "x-ui", "3x-ui", "sing-box", "ss-server"]
        .iter()
        .any(|p| name == *p || name.starts_with(&format!("{p}-")))
}
pub fn json_config(body: &str) -> Result<Value> {
    let mut out = String::new();
    let mut chars = body.chars().peekable();
    let (mut quoted, mut escape) = (false, false);
    while let Some(c) = chars.next() {
        if quoted {
            out.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                quoted = false;
            }
            continue;
        }
        if c == '"' {
            quoted = true;
            out.push(c);
        } else if c == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for ch in chars.by_ref() {
                if ch == '\n' {
                    out.push(ch);
                    break;
                }
            }
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut previous = ' ';
            let mut ended = false;
            for ch in chars.by_ref() {
                if previous == '*' && ch == '/' {
                    ended = true;
                    break;
                }
                previous = ch;
            }
            ensure!(ended, "JSON 注释不完整");
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    Ok(serde_json::from_str(&out)?)
}
fn read(path: &Path) -> Result<Vec<u8>> {
    ensure!(
        fs::metadata(path)?.is_file() && fs::metadata(path)?.len() <= 2 * 1024 * 1024,
        "配置文件大小或类型无效"
    );
    Ok(fs::read(path)?)
}
fn config_paths(args: &[String], cwd: &Path) -> Result<Vec<(PathBuf, bool)>> {
    let mut paths = vec![];
    for (i, arg) in args.iter().enumerate() {
        let (key, value) = arg
            .split_once('=')
            .map(|(k, v)| (k, Some(v)))
            .unwrap_or((arg, None));
        if ["-config", "--config", "-c", "-confdir", "--confdir"].contains(&key) {
            let p = Path::new(
                value
                    .or_else(|| args.get(i + 1).map(String::as_str))
                    .context("缺少配置路径")?,
            );
            paths.push((
                if p.is_absolute() {
                    p.into()
                } else {
                    cwd.join(p)
                },
                key.contains("confdir"),
            ));
        }
    }
    Ok(paths)
}
fn asset_names(
    v: &Value,
    field: &str,
    names: &mut std::collections::BTreeSet<String>,
) -> Result<()> {
    match v {
        Value::Object(items) => {
            for (k, value) in items {
                if field == "hosts" {
                    asset_names(&json!(k), "domain", names)?;
                    asset_names(value, "ip", names)?;
                } else {
                    asset_names(value, k, names)?;
                }
            }
        }
        Value::Array(items) => {
            for v in items {
                asset_names(v, field, names)?;
            }
        }
        Value::String(s) if ["ip", "ips", "domain", "domains"].contains(&field) => {
            let name = if s.starts_with("geoip:") {
                Some("geoip.dat")
            } else if s.starts_with("geosite:") {
                Some("geosite.dat")
            } else if s.starts_with("ext:") || s.starts_with("ext-ip:") {
                s.split(':').nth(1)
            } else {
                None
            };
            if let Some(name) = name {
                ensure!(
                    !name.is_empty()
                        && ![".", ".."].contains(&name)
                        && name
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)),
                    "地理文件名称无效"
                );
                names.insert(name.into());
            }
        }
        _ => (),
    }
    ensure!(names.len() <= 16, "地理文件超过 16 个");
    Ok(())
}
fn xui_inbounds(path: &Path) -> Result<Vec<Value>> {
    let db = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let cols = db
        .prepare("PRAGMA table_info(inbounds)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let names = [
        "port",
        "protocol",
        "enable",
        "remark",
        "settings",
        "stream_settings",
        "sniffing",
        "listen",
        "tag",
    ];
    ensure!(
        names.iter().all(|n| cols.iter().any(|c| c == n)),
        "原面板数据库缺少入口字段"
    );
    let query = format!(
        "SELECT {} FROM inbounds {} LIMIT 1025",
        names.join(","),
        if cols.iter().any(|s| s == "node_id") {
            "WHERE node_id IS NULL OR node_id = 0"
        } else {
            ""
        }
    );
    let mut statement = db.prepare(&query)?;
    let mut rows = statement.query([])?;
    let mut result = vec![];
    while let Some(row) = rows.next()? {
        ensure!(result.len() < 1024, "旧入口超过 1024 条");
        let mut v = serde_json::Map::new();
        for (i, key) in names.iter().enumerate() {
            let value = match *key {
                "port" => json!(row.get::<_, i64>(i)?),
                "enable" => json!(row.get::<_, i64>(i)? != 0),
                "settings" | "stream_settings" | "sniffing" => {
                    let value = row.get::<_, Option<String>>(i)?.unwrap_or_default();
                    if value.trim().is_empty() {
                        Value::Null
                    } else {
                        serde_json::from_str(&value)?
                    }
                }
                _ => json!(row.get::<_, Option<String>>(i)?.unwrap_or_default()),
            };
            v.insert(
                if *key == "stream_settings" {
                    "streamSettings"
                } else {
                    key
                }
                .to_string(),
                value,
            );
        }
        result.push(Value::Object(v));
    }
    Ok(result)
}
fn stage_assets(m: &Manager, owners: &mut [Owner]) -> Result<()> {
    use sha2::{Digest, Sha256};
    for owner in owners {
        for source in &mut owner.sources {
            let Some(assets) = source["asset_files"].as_array() else {
                continue;
            };
            if assets.is_empty() {
                continue;
            }
            let mut files = BTreeMap::new();
            for asset in assets {
                let name = asset["name"].as_str().context("地理文件缺少名称")?;
                let path = Path::new(asset["path"].as_str().context("地理文件缺少路径")?);
                files.insert(
                    name.to_owned(),
                    owner.files.get(path).context("地理数据未备份")?,
                );
            }
            let mut hash = Sha256::new();
            for (name, bytes) in &files {
                hash.update(name.as_bytes());
                hash.update([0]);
                hash.update(Sha256::digest(bytes));
            }
            let digest = hash.finalize();
            let dir = m
                .path("/var/lib/chain-legacy-assets")
                .join(uuid::Uuid::from_slice(&digest[..16])?.to_string());
            for path in [dir.parent().unwrap(), dir.as_path()] {
                ordinary(path)?;
                let marker = path.join(".chain-owner");
                if path.exists() {
                    ensure!(
                        fs::read_to_string(marker)? == "chain-legacy-assets-v1",
                        "地理目录归属不匹配"
                    );
                } else {
                    fs::create_dir(path)?;
                    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
                    atomic(&marker, b"chain-legacy-assets-v1", 0o644)?;
                }
            }
            for (name, bytes) in files {
                let path = dir.join(name);
                ordinary(&path)?;
                if path.exists() {
                    ensure!(fs::read(&path)? == *bytes, "地理数据已变化");
                } else {
                    atomic(&path, bytes, 0o644)?;
                }
            }
            source["asset_dir"] = json!(dir);
        }
    }
    Ok(())
}
#[derive(Clone, serde::Serialize)]
pub struct Owner {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub pids: Vec<u32>,
    pub sources: Vec<Value>,
    #[serde(skip)]
    files: BTreeMap<PathBuf, Vec<u8>>,
    pub reason: String,
    pub container: Option<Value>,
    pub database: Option<PathBuf>,
}
fn openrc_owner(proc: &Path, process: &Path) -> Result<Option<String>> {
    let checked = || -> Result<Option<String>> {
        if fs::read_link(process.join("ns/pid"))? != fs::read_link(proc.join("1/ns/pid"))? {
            return Ok(None);
        }
        let text = fs::read_to_string(process.join("status"))?;
        let ppid = text
            .lines()
            .find_map(|s| s.strip_prefix("PPid:"))
            .context("缺少父进程")?
            .trim();
        ppid.parse::<u32>()?;
        let parent = proc.join(ppid);
        let supervisor = super::platform::executable("supervise-daemon")
            .context("缺少 OpenRC supervisor")?
            .canonicalize()?;
        if fs::read_link(parent.join("exe"))? != supervisor {
            return Ok(None);
        }
        let bytes = fs::read(parent.join("environ"))?;
        let env = bytes
            .split(|c| *c == 0)
            .filter_map(|s| std::str::from_utf8(s).ok())
            .filter_map(|s| s.split_once('='))
            .collect::<BTreeMap<_, _>>();
        let name = env.get("RC_SVCNAME").context("缺少 OpenRC 归属")?;
        ensure!(
            !name.starts_with("chain-")
                && !name.is_empty()
                && name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)),
            "OpenRC 服务名无效"
        );
        let cmd = fs::read(parent.join("cmdline"))?;
        let args = cmd
            .split(|c| *c == 0)
            .filter_map(|s| std::str::from_utf8(s).ok())
            .collect::<Vec<_>>();
        if args.get(1) != Some(name)
            || args.get(2) != Some(&"--start")
            || env.get("RC_SERVICE").copied() != Some(format!("/etc/init.d/{name}").as_str())
            || !process.join(format!("root/etc/init.d/{name}")).is_file()
        {
            return Ok(None);
        }
        Ok(Some(name.to_string()))
    };
    Ok(checked().unwrap_or(None))
}
pub async fn discover(proc: &Path) -> Result<Vec<Owner>> {
    if !proc.is_dir() {
        return Ok(vec![]);
    }
    let mut containers = vec![];
    if super::platform::executable("docker").is_some() {
        let probe = output("docker", &["ps", "-q", "--no-trunc"], 15).await?;
        if probe.status.success() {
            let ids = String::from_utf8(probe.stdout)?;
            if !ids.trim().is_empty() {
                let mut args = vec!["inspect"];
                args.extend(ids.split_whitespace());
                let result = output("docker", &args, 30).await?;
                ensure!(result.status.success(), "无法检查旧容器，未操作旧服务");
                containers = serde_json::from_slice::<Vec<Value>>(&result.stdout)?;
            }
        }
    }
    let mut owners: BTreeMap<String, Owner> = BTreeMap::new();
    for entry in fs::read_dir(proc)? {
        let path = entry?.path();
        let Some(pid) = path
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let inspected: Result<()> = async {
            let exe = fs::read_link(path.join("exe"))?;
            let name = exe
                .file_name()
                .and_then(|s| s.to_str())
                .context("程序路径无效")?
                .trim_end_matches(" (deleted)");
            if !proxy(name) {
                return Ok(());
            }
            let args = fs::read(path.join("cmdline"))?
                .split(|c| *c == 0)
                .filter(|s| !s.is_empty())
                .map(|s| Ok(std::str::from_utf8(s)?.to_owned()))
                .collect::<Result<Vec<_>>>()?;
            let cwd = fs::read_link(path.join("cwd"))?;
            let cgroup = fs::read_to_string(path.join("cgroup"))?;
            let unit = cgroup
                .split(|c| c == '/' || c == '\n')
                .filter(|s| s.ends_with(".service"))
                .next_back();
            let env = fs::read(path.join("environ")).unwrap_or_default();
            let vars = env
                .split(|c| *c == 0)
                .filter_map(|s| std::str::from_utf8(s).ok())
                .filter_map(|s| s.split_once('='))
                .collect::<BTreeMap<_, _>>();
            let namespace=fs::read_link(path.join("ns/pid")).ok();
            let host_namespace=fs::read_link(proc.join("1/ns/pid")).ok();
            let matches=containers.iter().filter(|c| c["Id"].as_str().is_some_and(|id|cgroup.contains(id)) || (namespace.is_some() && namespace!=host_namespace && fs::read_link(proc.join(c["State"]["Pid"].as_u64().unwrap_or(0).to_string()).join("ns/pid")).ok()==namespace)).collect::<Vec<_>>();
            let container=if matches.len()==1 {Some(matches[0])} else {None};
            let (kind, id) = if let Some(c)=container {
                let main=fs::read_link(proc.join(c["State"]["Pid"].as_u64().context("容器 PID 无效")?.to_string()).join("exe"))?;
                let image=c["Config"]["Image"].as_str().unwrap_or("").rsplit('/').next().unwrap_or("").split(':').next().unwrap_or("");
                ensure!(main.file_name().and_then(|n|n.to_str()).is_some_and(proxy) || proxy(image), "容器主进程不是旧代理，保留容器");
                ("docker".into(),c["Id"].as_str().context("容器 ID 缺失")?.to_owned())
            } else if let Some(unit) = unit.filter(|u| {
                !u.starts_with("chain-") && !u.starts_with("docker") && !u.starts_with("containerd")
            }) {
                let main = output(
                    "systemctl",
                    &["show", unit, "--property=MainPID", "--value"],
                    10,
                )
                .await?;
                let p = String::from_utf8(main.stdout)?.trim().to_owned();
                let main_exe = fs::read_link(proc.join(p).join("exe"))?;
                if !main_exe
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(proxy)
                {
                    ("unmanaged".into(), pid.to_string())
                } else {
                    ("systemd".into(), unit.to_string())
                }
            } else if let Some(name)=openrc_owner(proc,&path)? {
                ("openrc".into(),name)
            } else { ("unmanaged".into(),pid.to_string()) };
            let key = format!("{kind}:{id}");
            let owner = owners.entry(key.clone()).or_insert(Owner {
                id: id.clone(),
                kind,
                name: name.into(),
                pids: vec![],
                sources: vec![],
                files: BTreeMap::new(),
                reason: String::new(),
                container: container.cloned(),
                database: None,
            });
            owner.pids.push(pid);
            if let Some(c)=&owner.container {
                if c["Config"]["Labels"].as_object().is_some_and(|v|v.keys().any(|k|k.starts_with("com.docker.swarm.") || k.starts_with("io.kubernetes."))) {owner.reason="集群管理容器必须在调度器迁移，保留运行".into();}
            }
            let root = path.join("root");
            let mut paths = config_paths(&args, &cwd)?;
            if paths.is_empty() && name.contains("x-ui") { paths.push((exe.parent().unwrap().join("bin/config.json"),false)); }
            if paths.len() != 1 || paths.iter().any(|(_, directory)| *directory) {
                owner.reason = "配置未指定或由多文件组成，保留原服务，使用 Web 手动导入".into();
            }
            let mut expanded = vec![];
            for (logical, directory) in paths {
                if directory {
                    for f in fs::read_dir(root.join(logical.strip_prefix("/").unwrap_or(&logical)))? {
                        let f = f?; if f.path().extension().is_some_and(|s|s=="json") { expanded.push(logical.join(f.file_name())); }
                    }
                } else { expanded.push(logical); }
            }
            for logical in expanded {
                let file = root.join(logical.strip_prefix("/").unwrap_or(&logical));
                let bytes = read(&file)?;
                let mut config = json_config(std::str::from_utf8(&bytes)?)?;
                fn hydrate(
                    v: &mut Value,
                    root: &Path,
                    cwd: &Path,
                    files: &mut BTreeMap<PathBuf, Vec<u8>>,
                ) -> Result<()> {
                    match v {
                        Value::Object(map) => {
                            for (file_key, inline_key) in
                                [("certificateFile", "certificate"), ("keyFile", "key")]
                            {
                                if let Some(path) = map.get(file_key).and_then(Value::as_str) {
                                    let p = Path::new(path);
                                    let logical = if p.is_absolute() {
                                        p.into()
                                    } else {
                                        cwd.join(p)
                                    };
                                    let actual =
                                        root.join(logical.strip_prefix("/").unwrap_or(&logical));
                                    let bytes = read(&actual)?;
                                    let lines = std::str::from_utf8(&bytes)?
                                        .lines()
                                        .map(str::to_owned)
                                        .collect::<Vec<_>>();
                                    files.insert(actual, bytes);
                                    map.remove(file_key);
                                    map.insert(inline_key.into(), json!(lines));
                                }
                            }
                            for child in map.values_mut() {
                                hydrate(child, root, cwd, files)?;
                            }
                        }
                        Value::Array(items) => {
                            for child in items {
                                hydrate(child, root, cwd, files)?;
                            }
                        }
                        _ => (),
                    }
                    Ok(())
                }
                hydrate(&mut config, &root, &cwd, &mut owner.files)?;
                let mut names = std::collections::BTreeSet::new();
                asset_names(&config,"",&mut names)?;
                let asset_base = vars.get("XRAY_LOCATION_ASSET").map(PathBuf::from).unwrap_or_else(||exe.parent().unwrap().into());
                let asset_base = if asset_base.is_absolute() {asset_base}else{cwd.join(asset_base)};
                let mut assets = vec![];
                for name in names {
                    let path = root.join(asset_base.strip_prefix("/").unwrap_or(&asset_base)).join(&name);
                    let size = fs::metadata(&path)?.len();
                    ensure!(size<=64*1024*1024 && owner.files.values().map(Vec::len).sum::<usize>() as u64 +size<=128*1024*1024,"旧配置地理数据过大");
                    owner.files.insert(path.clone(),fs::read(&path)?);
                    assets.push(json!({"name":name,"path":path}));
                }
                let original_ports = config["inbounds"].as_array().map(|v|v.iter().map(|i|i["port"].clone()).collect::<Vec<_>>()).unwrap_or_default();
                if let Some(c)=&owner.container {
                    if c["HostConfig"]["NetworkMode"]!="host" {
                        for inbound in config["inbounds"].as_array_mut().context("配置缺少 inbounds")? {
                            let port=inbound["port"].as_u64().context("容器入口端口无效")?;
                            let mut published=std::collections::BTreeSet::new();
                            for protocol in ["tcp","udp"] {
                                if let Some(mappings)=c["NetworkSettings"]["Ports"][format!("{port}/{protocol}")].as_array() {
                                    for mapping in mappings {
                                        ensure!(matches!(mapping["HostIp"].as_str(),Some(""|"0.0.0.0"|"::")),"容器绑定特定地址，保留旧服务");
                                        published.insert(mapping["HostPort"].as_str().context("缺少宿主机端口")?.parse::<u16>()?);
                                    }
                                }
                            }
                            ensure!(published.len()==1,"容器端口未映射或 TCP/UDP 映射不同");
                            inbound["port"]=json!(published.into_iter().next().unwrap());
                        }
                    }
                }
                owner.files.insert(file, bytes);
                let source_id = format!("{key}:{}", logical.display());
                if !owner.sources.iter().any(|s| s["id"] == source_id) {
                    owner
                        .sources
                        .push(json!({"id":source_id,"name":owner.name,"original_ports":original_ports,"config":config,"asset_files":assets}));
                }
            }
            let db = root.join("etc/x-ui/x-ui.db");
            if db.is_file() {
                let originals = xui_inbounds(&db)?;
                let ports = owner.sources.iter().flat_map(|s|s["original_ports"].as_array().cloned().unwrap_or_default()).collect::<Vec<_>>();
                if originals.iter().any(|i|i["enable"]==true && !ports.contains(&i["port"])) { owner.reason="原面板包含运行配置未覆盖的启用入口，保留原服务".into(); }
                for source in &mut owner.sources {
                    if let Some(items)=source["config"]["inbounds"].as_array_mut() {
                        for i in items {if let Some(original)=originals.iter().find(|o|o["port"]==i["port"]) {i["remark"]=original["remark"].clone();}}
                    }
                    source["source_inbounds"]=json!(originals);
                }
                owner.database=Some(db);
            } else if name.contains("x-ui") { owner.reason="无法读取原面板数据库，保留原面板".into(); }
            Ok(())
        }
        .await;
        if let Err(e) = inspected {
            eprintln!("跳过无法确认归属的代理 PID {pid}：{e:#}");
            for owner in owners.values_mut().filter(|o| o.pids.contains(&pid)) {
                owner.reason = "配置或依赖检查失败，保留原服务".into();
            }
        }
    }
    for owner in owners.values_mut() {
        if owner.sources.is_empty() {
            owner.reason = "没有可读取的配置，保留原服务".into();
        }
    }
    Ok(owners.into_values().collect())
}
pub struct Api {
    client: reqwest::Client,
    base: String,
    cookie: String,
}
impl Api {
    pub async fn login(m: &Manager) -> Result<Self> {
        let mut addr: std::net::SocketAddr = m.environment("panel")?["CHAIN_LISTEN"].parse()?;
        if addr.ip().is_unspecified() {
            addr.set_ip(if addr.is_ipv6() {
                "::1".parse()?
            } else {
                "127.0.0.1".parse()?
            });
        }
        let base = format!("http://{addr}/api");
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(90))
            .build()?;
        let response=client.post(format!("{base}/login")).header("X-Chain-Request","1").json(&json!({"token":fs::read_to_string(m.path("/var/lib/chain-panel/admin-token"))?.trim()})).send().await?.error_for_status()?;
        let cookie = response
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .context("缺少会话")?
            .to_str()?
            .split(';')
            .next()
            .unwrap()
            .into();
        Ok(Self {
            client,
            base,
            cookie,
        })
    }
    pub async fn call(&self, path: &str, payload: Option<Value>) -> Result<Value> {
        let request = if payload.is_some() {
            self.client.post(format!("{}/{path}", self.base))
        } else {
            self.client.get(format!("{}/{path}", self.base))
        };
        let request = request
            .header("X-Chain-Request", "1")
            .header(reqwest::header::COOKIE, &self.cookie);
        let response = if let Some(v) = payload {
            request.json(&v)
        } else {
            request
        }
        .send()
        .await?;
        let status = response.status();
        let value: Value = response.json().await?;
        ensure!(
            status.is_success(),
            "{}",
            value["error"].as_str().unwrap_or("控制台请求失败")
        );
        Ok(value)
    }
    pub async fn publish(&self) -> Result<()> {
        let job = self.call("publish", Some(json!({}))).await?;
        let id = job["job_id"].as_str().context("缺少发布任务")?;
        for _ in 0..260 {
            let state = self.call("state", None).await?;
            let j = state["jobs"]
                .as_array()
                .context("任务列表缺失")?
                .iter()
                .find(|j| j["id"] == id)
                .context("发布任务不存在")?;
            if j["state"] == "applied" {
                return Ok(());
            }
            ensure!(
                matches!(j["state"].as_str(), Some("publishing" | "rolling_back")),
                "迁移发布失败，草稿保留"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        anyhow::bail!("迁移发布超时")
    }
}
fn unchanged(before: &Owner, current: &Owner) -> Result<()> {
    ensure!(
        before.id == current.id
            && before.kind == current.kind
            && before
                .pids
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                == current
                    .pids
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>()
            && before.files == current.files
            && current.reason.is_empty()
            && ["HostConfig", "Config", "NetworkSettings"]
                .into_iter()
                .all(|key| before.container.as_ref().map(|c| &c[key])
                    == current.container.as_ref().map(|c| &c[key]))
            && before
                .sources
                .iter()
                .map(|s| &s["source_inbounds"])
                .collect::<Vec<_>>()
                == current
                    .sources
                    .iter()
                    .map(|s| &s["source_inbounds"])
                    .collect::<Vec<_>>(),
        "旧服务进程、配置或管理状态已改变，未切换：{}",
        before.id
    );
    Ok(())
}
pub async fn handover(m: &Manager, owners: Vec<Owner>) -> Result<()> {
    ensure!(m.live(), "离线树不操作旧服务");
    let mut selected = vec![];
    let mut disabled = std::collections::BTreeSet::new();
    for owner in owners {
        println!(
            "发现旧代理：{} / {} / PID {:?}",
            owner.name, owner.kind, owner.pids
        );
        if owner.kind == "unmanaged" || !owner.reason.is_empty() {
            println!(
                "保留原服务：{}",
                if owner.reason.is_empty() {
                    "无法确认服务管理归属"
                } else {
                    &owner.reason
                }
            );
            continue;
        }
        let choice = lifecycle::prompt(
            "IMPORT：备份导入后停用旧服务；DISABLE：仅备份并停用；留空保留",
            "保留",
        )?;
        if choice == "IMPORT" || choice == "DISABLE" {
            if choice == "DISABLE" {
                disabled.insert(owner.id.clone());
            }
            selected.push(owner);
        }
    }
    if selected.is_empty() {
        return Ok(());
    }
    let api = Api::login(m).await?;
    let state = api.call("state", None).await?;
    ensure!(
        state["lines"].as_array().is_some_and(Vec::is_empty)
            && state["pending_imports"]
                .as_array()
                .is_some_and(Vec::is_empty)
            && state["nodes"].as_array().is_some_and(|v| v.len() == 1),
        "自动接管要求新面板，已有代理请在 Web 处理"
    );
    let node = fs::read_to_string(m.path("/var/lib/chain-panel/local-system-node"))?
        .trim()
        .to_owned();
    stage_assets(m, &mut selected)?;
    let sources = selected
        .iter()
        .filter(|o| !disabled.contains(&o.id))
        .flat_map(|o| o.sources.clone())
        .collect::<Vec<_>>();
    let importing = !sources.is_empty();
    let request = json!({"node_id":node,"sources":sources,"preview":true,"preserve_config":true});
    let preview = if importing {
        api.call("legacy-import", Some(request.clone())).await?
    } else {
        json!({"pending":[],"imported":[]})
    };
    let backup = m.path("/var/backups/chain-legacy").join(format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
        uuid::Uuid::new_v4()
    ));
    ordinary(&backup)?;
    fs::create_dir_all(&backup)?;
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o700))?;
    let mut records = vec![];
    for (i, owner) in selected.iter().enumerate() {
        let dir = backup.join(i.to_string());
        fs::create_dir(&dir)?;
        let mut files = vec![];
        for (j, (path, body)) in owner.files.iter().enumerate() {
            atomic(&dir.join(format!("file-{j}")), body, 0o600)?;
            files.push(path.display().to_string());
        }
        if let Some(database) = &owner.database {
            let db = rusqlite::Connection::open_with_flags(
                database,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            db.execute(
                "VACUUM INTO ?1",
                [dir.join("x-ui.db").to_str().context("备份路径无效")?],
            )?;
            fs::set_permissions(dir.join("x-ui.db"), fs::Permissions::from_mode(0o600))?;
        }
        let runlevels = if owner.kind == "openrc" {
            fs::read_dir("/etc/runlevels")?
                .filter_map(|p| p.ok())
                .filter(|p| p.path().join(&owner.id).is_symlink())
                .map(|p| p.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
        let enabled = if owner.kind == "systemd" {
            output("systemctl", &["is-enabled", "--quiet", &owner.id], 15)
                .await?
                .status
                .success()
        } else {
            Path::new("/etc/runlevels/default").join(&owner.id).exists()
        };
        let record = json!({"id":owner.id,"kind":owner.kind,"enabled":enabled,"files":files,"runlevels":runlevels,"container":owner.container});
        atomic(
            &dir.join("source.json"),
            &serde_json::to_vec(&record)?,
            0o600,
        )?;
        records.push(record);
    }
    println!("旧服务配置备份：{}", backup.display());
    if !preview["pending"].as_array().is_some_and(Vec::is_empty) {
        println!(
            "存在无法完整导入的配置，旧服务全部保持运行；继续完成面板安装：{}",
            preview["pending"]
        );
        let _ = api.call("logout", Some(json!({}))).await;
        return Ok(());
    }
    let mut changed = vec![];
    let result: Result<()> = async {
        let current = discover(Path::new("/proc")).await?;
        for owner in &selected {
            unchanged(
                owner,
                current
                    .iter()
                    .find(|o| o.id == owner.id && o.kind == owner.kind)
                    .context("旧服务已退出或归属改变，未切换")?,
            )?;
        }
        let mut request = request.clone();
        request["preview"] = json!(false);
        let committed = if importing {
            api.call("legacy-import", Some(request)).await?
        } else {
            preview.clone()
        };
        ensure!(
            committed["pending"].as_array().is_some_and(Vec::is_empty)
                && committed["imported"].as_array().map(Vec::len)
                    == preview["imported"].as_array().map(Vec::len),
            "实际导入与预检不一致，旧服务未停止"
        );
        for (i, o) in selected.iter().enumerate() {
            for (p, b) in &o.files {
                ensure!(fs::read(p)? == *b, "旧配置在安装期间被修改，未切换");
            }
            changed.push(i);
            if o.kind == "docker" {
                command("docker", &["update", "--restart=no", &o.id], 30).await?;
                command("docker", &["stop", "--time", "30", &o.id], 45).await?;
                let check = output(
                    "docker",
                    &["inspect", "--format", "{{.State.Running}}", &o.id],
                    15,
                )
                .await?;
                ensure!(
                    check.status.success() && String::from_utf8(check.stdout)?.trim() == "false",
                    "旧容器未停止"
                );
            } else if o.kind == "systemd" {
                if records[i]["enabled"] == true {
                    command("systemctl", &["disable", &o.id], 30).await?;
                }
                command("systemctl", &["stop", &o.id], 60).await?;
                ensure!(
                    !output("systemctl", &["is-active", "--quiet", &o.id], 15)
                        .await?
                        .status
                        .success(),
                    "旧服务未停止"
                );
            } else {
                for level in records[i]["runlevels"]
                    .as_array()
                    .context("旧运行级别缺失")?
                {
                    command(
                        "rc-update",
                        &["del", &o.id, level.as_str().context("运行级别无效")?],
                        30,
                    )
                    .await?;
                }
                command("rc-service", &[&o.id, "stop"], 60).await?;
                ensure!(
                    !output("rc-service", &[&o.id, "status"], 15)
                        .await?
                        .status
                        .success(),
                    "旧服务未停止"
                );
            }
        }
        if importing {
            api.publish().await?;
        }
        Ok(())
    }
    .await;
    if let Err(e) = result {
        // Stop our Agent before restoring listeners owned by the old service.
        m.service("stop", "agent").await?;
        let mut failures = vec![];
        for &i in changed.iter().rev() {
            let o = &selected[i];
            let restore: Result<()> = async {
                if o.kind == "docker" {
                    let policy = &records[i]["container"]["HostConfig"]["RestartPolicy"];
                    let mut restart = policy["Name"].as_str().unwrap_or("no").to_owned();
                    if restart.is_empty() {
                        restart = "no".into();
                    }
                    if restart == "on-failure"
                        && policy["MaximumRetryCount"].as_u64().unwrap_or(0) > 0
                    {
                        restart = format!("on-failure:{}", policy["MaximumRetryCount"]);
                    }
                    command(
                        "docker",
                        &["update", &format!("--restart={restart}"), &o.id],
                        30,
                    )
                    .await?;
                    command("docker", &["start", &o.id], 60).await?;
                } else if o.kind == "systemd" {
                    if records[i]["enabled"] == true {
                        command("systemctl", &["enable", &o.id], 30).await?;
                    }
                    command("systemctl", &["start", &o.id], 60).await?;
                } else {
                    for level in records[i]["runlevels"]
                        .as_array()
                        .context("旧运行级别缺失")?
                    {
                        command(
                            "rc-update",
                            &["add", &o.id, level.as_str().context("运行级别无效")?],
                            30,
                        )
                        .await?;
                    }
                    command("rc-service", &[&o.id, "start"], 60).await?;
                }
                Ok(())
            }
            .await;
            if let Err(e) = restore {
                failures.push(format!("{}: {e:#}", o.id));
            }
        }
        anyhow::bail!(
            "迁移失败：{e:#}；原服务恢复结果：{}；新 Agent 已停止，草稿和备份保留",
            if failures.is_empty() {
                "已恢复".into()
            } else {
                failures.join("；")
            }
        );
    }
    let _ = api.call("logout", Some(json!({}))).await;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_refuses_changed_processes_configuration_and_management() {
        let owner = Owner {
            id: "xray.service".into(),
            kind: "systemd".into(),
            name: "xray".into(),
            pids: vec![42],
            sources: vec![json!({"source_inbounds":[{"port":443}]})],
            files: BTreeMap::from([(PathBuf::from("/config"), b"original".to_vec())]),
            reason: String::new(),
            container: None,
            database: None,
        };
        unchanged(&owner, &owner).unwrap();
        let mut changed = owner.clone();
        changed.pids = vec![43];
        assert!(unchanged(&owner, &changed).is_err());
        changed = owner.clone();
        changed
            .files
            .insert(PathBuf::from("/config"), b"new".to_vec());
        assert!(unchanged(&owner, &changed).is_err());
        changed = owner.clone();
        changed.sources[0]["source_inbounds"][0]["port"] = json!(8443);
        assert!(unchanged(&owner, &changed).is_err());
        changed = owner.clone();
        changed.reason = "unknown owner".into();
        assert!(unchanged(&owner, &changed).is_err());
    }
    #[test]
    fn geodata_is_copied_without_modifying_originals_and_paths_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let m = lifecycle::open(root.path()).unwrap();
        fs::create_dir_all(m.path("/var/lib")).unwrap();
        let old = root.path().join("original.dat");
        fs::write(&old, b"geodata").unwrap();
        let before = fs::metadata(&old).unwrap();
        let mut owners = vec![Owner {
            id: "fixture".into(),
            kind: "unmanaged".into(),
            name: "fixture".into(),
            pids: vec![],
            reason: String::new(),
            container: None,
            database: None,
            files: BTreeMap::from([(old.clone(), b"geodata".to_vec())]),
            sources: vec![json!({"asset_files":[{"name":"geoip.dat","path":old}]})],
        }];
        stage_assets(&m, &mut owners).unwrap();
        stage_assets(&m, &mut owners).unwrap();
        assert_eq!(
            fs::read(
                Path::new(owners[0].sources[0]["asset_dir"].as_str().unwrap()).join("geoip.dat")
            )
            .unwrap(),
            b"geodata"
        );
        use std::os::unix::fs::MetadataExt;
        let after = fs::metadata(&old).unwrap();
        assert_eq!(
            (
                before.uid(),
                before.gid(),
                before.mode(),
                before.ctime_nsec()
            ),
            (after.uid(), after.gid(), after.mode(), after.ctime_nsec())
        );
        let mut names = std::collections::BTreeSet::new();
        asset_names(&json!({"routing":{"rules":[{"ip":["geoip:private"],"domain":["ext:custom.dat:entry","geosite:category"]}]}}),"",&mut names).unwrap();
        assert_eq!(names.len(), 3);
        assert!(asset_names(&json!({"ip":["ext:../private.dat:x"]}), "", &mut names).is_err());
    }
    #[test]
    fn xui_keeps_disabled_and_non_xray_entries_for_import_validation() {
        let root = tempfile::tempdir().unwrap();
        let p = root.path().join("panel.db");
        let db = rusqlite::Connection::open(&p).unwrap();
        db.execute_batch("CREATE TABLE inbounds(port INTEGER,protocol TEXT,enable INTEGER,remark TEXT,settings TEXT,stream_settings TEXT,sniffing TEXT,listen TEXT,tag TEXT,node_id INTEGER); INSERT INTO inbounds VALUES(443,'tuic',1,'other','{}','{}',NULL,'','',0); INSERT INTO inbounds VALUES(8443,'vless',0,'disabled','{}','{}',NULL,'','',0); INSERT INTO inbounds VALUES(9443,'vless',1,'remote','{}','{}',NULL,'','',2);").unwrap();
        let rows = xui_inbounds(&p).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["protocol"], "tuic");
        assert_eq!(rows[1]["enable"], false);
    }
    #[test]
    fn config_comments_do_not_corrupt_urls() {
        assert_eq!(
            json_config("{ /* header */ \"url\":\"https://example/a//b\", // line\n \"x\":1}")
                .unwrap()["url"],
            "https://example/a//b"
        );
        assert!(json_config("{/*").is_err());
        assert_eq!(
            config_paths(
                &["xray".into(), "-c".into(), "config.json".into()],
                Path::new("/etc/xray")
            )
            .unwrap(),
            vec![(PathBuf::from("/etc/xray/config.json"), false)]
        );
    }
}
