use super::{
    package,
    upgrade::{Manager, OWNER, atomic, ordinary, output},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
const LIMIT: u64 = 128 * 1024 * 1024;
fn valid(core: &str) -> Result<&'static str> {
    match core {
        "xray" => Ok("XTLS/Xray-core"),
        "mihomo" => Ok("MetaCubeX/mihomo"),
        _ => anyhow::bail!("请选择 Xray 或 Mihomo"),
    }
}
pub fn validate(v: &Value) -> Result<()> {
    valid(v["core"].as_str().unwrap_or_default())?;
    let has = |keys: &[&str]| keys.iter().any(|k| v.get(*k).is_some_and(|v| !v.is_null()));
    match v["action"].as_str() {
        Some("core-upgrade") => {
            package::version(v["version"].as_str().context("缺少内核版本")?)?;
            ensure!(!has(&["task_id"]), "升级不能指定原任务 ID");
            for k in ["security_update", "rollback_safe", "prerelease"] {
                ensure!(
                    v.get(k).is_none_or(|v| v.is_null() || v.is_boolean()),
                    "内核选项无效"
                );
            }
            ensure!(
                v["rollback_safe"] != true || v["security_update"] == true,
                "仅安全升级可确认回退安全"
            );
        }
        Some("core-confirm" | "core-reject") => {
            ensure!(
                !has(&["version", "security_update", "rollback_safe", "prerelease"]),
                "确认不能修改升级参数"
            );
            let id = v["task_id"].as_str().context("缺少原任务 ID")?;
            ensure!(
                uuid::Uuid::parse_str(id)?.to_string() == id,
                "原任务 ID 无效"
            );
        }
        Some("core-rollback") => ensure!(
            !has(&[
                "version",
                "security_update",
                "rollback_safe",
                "prerelease",
                "task_id"
            ]),
            "回退使用保存的安全版本"
        ),
        _ => anyhow::bail!("内核操作无效"),
    };
    Ok(())
}
pub async fn download(core: &str, version: &str, directory: &Path, allow: bool) -> Result<Value> {
    let repo = valid(core)?;
    package::version(version)?;
    let client = reqwest::Client::builder()
        .https_only(true)
        .user_agent("chainctl")
        .timeout(Duration::from_secs(60))
        .build()?;
    let meta = directory.join("metadata.json");
    package::fetch(
        &client,
        &format!("https://api.github.com/repos/{repo}/releases/tags/v{version}"),
        &meta,
        2 * 1024 * 1024,
    )
    .await?;
    let release: Value = serde_json::from_slice(&fs::read(meta)?)?;
    ensure!(
        release["tag_name"] == format!("v{version}")
            && release["draft"] == false
            && (release["prerelease"] != true || allow),
        "官方版本信息无效或未允许预发布"
    );
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let arm = std::env::consts::ARCH == "aarch64";
    let name = if core == "xray" {
        format!(
            "Xray-{}-{}.zip",
            if os == "darwin" { "macos" } else { os },
            if arm { "arm64-v8a" } else { "64" }
        )
    } else {
        format!(
            "mihomo-{os}-{}-v{version}.gz",
            if arm { "arm64" } else { "amd64-compatible" }
        )
    };
    let assets = release["assets"]
        .as_array()
        .context("缺少附件")?
        .iter()
        .filter(|v| v["name"] == name)
        .collect::<Vec<_>>();
    let url = format!("https://github.com/{repo}/releases/download/v{version}/{name}");
    ensure!(
        assets.len() == 1 && assets[0]["browser_download_url"] == url,
        "附件来源或架构不匹配"
    );
    let digest = assets[0]["digest"]
        .as_str()
        .and_then(|s| s.strip_prefix("sha256:"))
        .context("附件缺少摘要")?;
    ensure!(
        digest.len() == 64 && digest.bytes().all(|c| c.is_ascii_hexdigit()),
        "附件摘要无效"
    );
    let archive = directory.join(&name);
    package::fetch(&client, &url, &archive, LIMIT).await?;
    ensure!(package::digest(&archive)? == digest, "附件摘要不匹配");
    fs::create_dir_all(directory.join("bin"))?;
    fs::create_dir_all(directory.join("share"))?;
    if core == "xray" {
        let (binary, license) = package::xray_files(&archive)?;
        fs::write(directory.join("bin/xray"), binary)?;
        fs::write(directory.join("share/Xray-LICENSE"), license)?;
    } else {
        let mut bytes = vec![];
        flate2::read::GzDecoder::new(fs::File::open(archive)?)
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            !bytes.is_empty() && bytes.len() as u64 <= LIMIT,
            "内核解压大小无效"
        );
        fs::write(directory.join("bin/mihomo"), bytes)?;
        package::fetch(
            &client,
            &format!("https://raw.githubusercontent.com/{repo}/v{version}/LICENSE"),
            &directory.join("share/Mihomo-LICENSE"),
            65536,
        )
        .await?;
    }
    fs::set_permissions(
        directory.join(format!("bin/{core}")),
        fs::Permissions::from_mode(0o755),
    )?;
    Ok(
        json!({"version":version,"source":url,"archive_sha256":digest,"prerelease":release["prerelease"]==true}),
    )
}
pub fn replace(stage: &Path, core: &str, source: &Path, meta: &Value) -> Result<()> {
    valid(core)?;
    let mut manifest: Value = serde_json::from_slice(&fs::read(stage.join("release.json"))?)?;
    let license = if core == "xray" {
        "Xray-LICENSE"
    } else {
        "Mihomo-LICENSE"
    };
    for (name, mode) in [
        (format!("bin/{core}"), 0o755),
        (format!("share/{license}"), 0o644),
    ] {
        fs::create_dir_all(stage.join(&name).parent().unwrap())?;
        fs::copy(source.join(&name), stage.join(&name))?;
        fs::set_permissions(stage.join(&name), fs::Permissions::from_mode(mode))?;
        manifest["files"][&name] =
            json!({"sha256":package::digest(&stage.join(&name))?,"mode":mode});
    }
    if core == "xray" {
        let old = stage.join("share/Xray-PATCHES.txt");
        if old.exists() {
            fs::remove_file(old)?;
        }
        manifest["files"]
            .as_object_mut()
            .unwrap()
            .remove("share/Xray-PATCHES.txt");
    }
    manifest[core] = meta["version"].clone();
    manifest[format!("{core}_selected")] = json!(true);
    for key in ["source", "archive_sha256", "prerelease"] {
        manifest[format!("{core}_{key}")] = meta[key].clone();
    }
    atomic(
        &stage.join("release.json"),
        &serde_json::to_vec_pretty(&manifest)?,
        0o644,
    )?;
    package::verify(stage, true)?;
    Ok(())
}
pub fn preserve(manager: &Manager, stage: &Path) -> Result<()> {
    let state = manager.state()?;
    if let Some(id) = state["roles"]["agent"]["current"].as_str() {
        super::upgrade::release_name(id)?;
        let source = manager.base.join("releases").join(id);
        package::verify(&source, false)?;
        let old: Value = serde_json::from_slice(&fs::read(source.join("release.json"))?)?;
        for core in ["xray", "mihomo"] {
            if old[format!("{core}_selected")] == true {
                replace(
                    stage,
                    core,
                    &source,
                    &json!({"version":old[core],"source":old[format!("{core}_source")],"archive_sha256":old[format!("{core}_archive_sha256")],"prerelease":old[format!("{core}_prerelease")]}),
                )?;
            }
        }
    }
    Ok(())
}
fn configs(env: &BTreeMap<String, String>, core: &str) -> Result<Vec<(PathBuf, String)>> {
    let data = Path::new(env.get("CHAIN_AGENT_DIR").context("缺少 Agent 数据目录")?);
    let active = data.join("active.json");
    if !active.exists() {
        return Ok(vec![]);
    }
    let active: Value = serde_json::from_slice(&fs::read(active)?)?;
    let config = &active["config"];
    if core == "mihomo" {
        return Ok(if config.get("_mihomo").is_some_and(|v| !v.is_null()) {
            vec![(data.join("mihomo/config.json"), String::new())]
        } else {
            vec![]
        });
    }
    let mut result = vec![(data.join("xray.json"), String::new())];
    if let Some(profiles) = config["_legacy"].as_array() {
        for p in profiles {
            let id = p["id"].as_str().context("旧实例缺少 ID")?;
            ensure!(
                uuid::Uuid::parse_str(id)?.to_string() == id,
                "旧实例 ID 无效"
            );
            result.push((
                data.join(format!("legacy-{id}/config.json")),
                p["asset_dir"].as_str().unwrap_or_default().into(),
            ));
        }
    }
    Ok(result)
}
fn snapshot(env: &BTreeMap<String, String>, core: &str) -> Result<Value> {
    let mut paths = configs(env, core)?
        .into_iter()
        .map(|p| p.0)
        .collect::<Vec<_>>();
    paths.push(Path::new(&env["CHAIN_AGENT_DIR"]).join("active.json"));
    let mut result = serde_json::Map::new();
    for path in paths {
        ordinary(&path)?;
        result.insert(
            path.to_string_lossy().into(),
            if path.exists() {
                json!(package::digest(&path)?)
            } else {
                Value::Null
            },
        );
    }
    Ok(Value::Object(result))
}
pub fn status(manager: &Manager) -> Result<Value> {
    let state = manager.state()?;
    let Some(id) = state["roles"]["agent"]["current"].as_str() else {
        return Ok(json!({"supported":false}));
    };
    let m: Value = serde_json::from_slice(&fs::read(
        manager.base.join("releases").join(id).join("release.json"),
    )?)?;
    let p = manager.base.join(".core-pending.json");
    ordinary(&p)?;
    let pending = if p.exists() {
        serde_json::from_slice::<Value>(&fs::read(p)?)?["task_id"].clone()
    } else {
        Value::Null
    };
    Ok(
        json!({"supported":true,"versions":{"xray":m["xray"],"mihomo":m["mihomo"]},"recommended":serde_json::from_str::<Value>(include_str!("../core-versions.json"))?,"history":state["core_history"],"blocked":state["core_blocked"],"pending":pending}),
    )
}
async fn block(
    manager: &Manager,
    state: &mut Value,
    core: &str,
    id: &str,
    env: &BTreeMap<String, String>,
) -> Result<()> {
    let path = Path::new(&env["CHAIN_AGENT_DIR"]).join(".core-upgrade-blocked");
    ordinary(&path)?;
    if path.exists() {
        ensure!(
            serde_json::from_slice::<Value>(&fs::read(&path)?)?["owner"] == "chain-core-upgrade-v1",
            "停用标记归属不匹配"
        );
    }
    atomic(
        &path,
        serde_json::to_string(&json!({"owner":"chain-core-upgrade-v1","core":core,"task_id":id}))?
            .as_bytes(),
        0o600,
    )?;
    state["core_blocked"] = json!({"core":core,"task_id":id});
    save(manager, state)?;
    manager.service("stop", "agent").await
}
fn save(manager: &Manager, state: &Value) -> Result<()> {
    atomic(
        &manager.base.join(".chain-install.json"),
        &serde_json::to_vec_pretty(state)?,
        0o600,
    )
}
async fn recover(manager: &Manager, j: &Value) -> Result<String> {
    let mut state = manager.state()?;
    let env: BTreeMap<String, String> = serde_json::from_value(j["env"].clone())?;
    let core = j["core"].as_str().context("缺少内核名")?;
    manager.service("stop", "agent").await?;
    let detail = if j["rollback_allowed"] == true && snapshot(&env, core)? == j["config_snapshot"] {
        let old = &j["before"];
        let release = old["current"].as_str().context("缺少原版本")?;
        super::upgrade::release_name(release)?;
        package::verify(&manager.base.join("releases").join(release), false)?;
        manager.link("agent", release)?;
        state["roles"]["agent"] = old.clone();
        save(manager, &state)?;
        if j["was_running"] == true {
            manager.service("start", "agent").await?;
            manager.healthy("agent".into()).await?;
        }
        "已恢复原内核；配置和节点身份保持原样"
    } else {
        block(
            manager,
            &mut state,
            core,
            j["task_id"].as_str().context("缺少任务 ID")?,
            &env,
        )
        .await?;
        "已停止本机代理；未确认安全回退版本，请安装安全内核"
    };
    fs::remove_file(manager.base.join(".core-pending.json"))?;
    Ok(detail.into())
}
fn account_groups(account: &std::ffi::CStr, gid: libc::gid_t) -> Result<Vec<libc::gid_t>> {
    // Darwin's getgrouplist uses signed int IDs; setgroups still takes gid_t IDs.
    let base_gid = gid.try_into().context("服务主组超出系统接口范围")?;
    let mut count = 0;
    unsafe { libc::getgrouplist(account.as_ptr(), base_gid, std::ptr::null_mut(), &mut count) };
    ensure!(count > 0 && count < 1024, "服务组数量无效");
    let mut groups = vec![0; count as usize];
    ensure!(
        unsafe { libc::getgrouplist(account.as_ptr(), base_gid, groups.as_mut_ptr(), &mut count) }
            >= 0,
        "服务组解析失败"
    );
    ensure!(
        count >= 0 && count as usize <= groups.len(),
        "服务组数量已改变"
    );
    groups.truncate(count as usize);
    groups
        .into_iter()
        .map(|id| libc::gid_t::try_from(id).context("服务组 ID 无效"))
        .collect()
}
async fn identity(
    manager: &Manager,
    env: &BTreeMap<String, String>,
) -> Result<(u32, u32, Vec<u32>)> {
    let unit = manager.unit_path("agent");
    ordinary(&unit)?;
    ensure!(
        fs::read_to_string(&unit)?.contains(OWNER),
        "Agent 服务归属不匹配"
    );
    if !manager.live() {
        return Ok((
            unsafe { libc::geteuid() },
            unsafe { libc::getegid() },
            vec![],
        ));
    }
    let definition = fs::read_to_string(&unit)?;
    let (account, group, supplements) = if manager.kind() == "systemd" {
        let result = output(
            "systemctl",
            &[
                "show",
                "chain-agent.service",
                "--property=User,Group,SupplementaryGroups,MainPID",
            ],
            15,
        )
        .await?;
        ensure!(result.status.success(), "无法读取实际 Agent 服务身份");
        let text = String::from_utf8(result.stdout)?;
        let fields = text
            .lines()
            .filter_map(|s| s.split_once('='))
            .collect::<BTreeMap<_, _>>();
        (
            fields.get("User").context("服务缺少 User")?.to_string(),
            fields.get("Group").copied().unwrap_or("").to_owned(),
            fields
                .get("SupplementaryGroups")
                .copied()
                .unwrap_or("")
                .to_owned(),
        )
    } else {
        ensure!(manager.kind() == "openrc", "此平台不支持内核隔离升级");
        let value = definition
            .lines()
            .find_map(|s| s.strip_prefix("command_user="))
            .context("服务缺少 command_user")?
            .trim()
            .trim_matches(['\'', '"']);
        let (user, group) = value.split_once(':').context("服务身份格式无效")?;
        (user.to_owned(), group.to_owned(), String::new())
    };
    let account = std::ffi::CString::new(account)?;
    let user = unsafe { libc::getpwnam(account.as_ptr()) };
    ensure!(!user.is_null(), "服务账户不存在");
    let (uid, default_gid) = unsafe { ((*user).pw_uid, (*user).pw_gid) };
    let group_id = |name: &str| -> Result<u32> {
        if let Ok(id) = name.parse::<u32>() {
            return Ok(id);
        }
        let name = std::ffi::CString::new(name)?;
        let entry = unsafe { libc::getgrnam(name.as_ptr()) };
        ensure!(!entry.is_null(), "服务组不存在");
        Ok(unsafe { (*entry).gr_gid })
    };
    let gid = if group.is_empty() {
        default_gid
    } else {
        group_id(&group)?
    };
    let data = fs::metadata(&env["CHAIN_AGENT_DIR"])?;
    ensure!(
        uid != 0 && (data.uid(), data.gid()) == (uid, gid),
        "Agent 数据归属与服务身份不匹配"
    );
    let mut groups = account_groups(&account, gid)?;
    for name in supplements.split_whitespace() {
        groups.push(group_id(name)?);
    }
    groups.sort_unstable();
    groups.dedup();
    let binary = manager
        .base
        .join("current-agent/bin/chain-agent")
        .canonicalize()?;
    for p in fs::read_dir("/proc")? {
        let p = p?.path();
        if !p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .bytes()
            .all(|c| c.is_ascii_digit())
        {
            continue;
        }
        if fs::read_link(p.join("exe")).ok().as_ref() != Some(&binary) {
            continue;
        }
        let body = match fs::read_to_string(p.join("status")) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        let fields = body
            .lines()
            .filter_map(|s| s.split_once(':'))
            .collect::<BTreeMap<_, _>>();
        ensure!(
            fields
                .get("Uid")
                .and_then(|v| v.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u32>().ok())
                == Some(uid)
                && fields
                    .get("Gid")
                    .and_then(|v| v.split_whitespace().nth(1))
                    .and_then(|v| v.parse::<u32>().ok())
                    == Some(gid),
            "Agent 实际身份不匹配"
        );
        let mut actual = fields
            .get("Groups")
            .context("缺少进程组")?
            .split_whitespace()
            .map(str::parse::<u32>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        actual.sort_unstable();
        actual.dedup();
        ensure!(actual == groups, "Agent 实际补充组不匹配");
    }
    Ok((uid, gid, groups))
}
#[cfg(target_os = "linux")]
fn sandbox(directory: &Path, nginx_check: bool) -> Result<()> {
    use std::os::fd::AsRawFd;
    #[repr(C)]
    struct Rules {
        access: u64,
    }
    #[repr(C, packed)]
    struct Rule {
        access: u64,
        parent: i32,
    }
    let abi = unsafe { libc::syscall(444, 0, 0, 1) };
    ensure!(abi >= 3, "预检需要 Landlock ABI 3+，未执行候选内核");
    let access = (1u64 << 1) | (4..=14).map(|b| 1u64 << b).sum::<u64>();
    let attr = Rules { access };
    let fd = unsafe { libc::syscall(444, &attr, std::mem::size_of::<Rules>(), 0) } as i32;
    ensure!(fd >= 0, "无法创建隔离规则");
    let parent = fs::File::open(directory)?;
    let rule = Rule {
        access,
        parent: parent.as_raw_fd(),
    };
    let result = unsafe {
        libc::syscall(445, fd, 1, &rule, 0) == 0
            && libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0
            && libc::syscall(446, fd, 0) == 0
    };
    unsafe { libc::close(fd) };
    ensure!(result, "无法启用写入隔离");
    let (arch, denied): (u32, &[u32]) = if cfg!(target_arch = "x86_64") {
        (
            0xc000003e,
            &[
                42, 43, 44, 46, 49, 50, 53, 90, 91, 92, 93, 94, 132, 188, 189, 190, 197, 198, 199,
                235, 260, 261, 268, 280, 288, 307, 437, 452,
            ],
        )
    } else {
        (
            0xc00000b7,
            &[
                5, 6, 7, 14, 15, 16, 52, 53, 54, 55, 88, 199, 200, 201, 202, 203, 206, 211, 242,
                269, 437, 452,
            ],
        )
    };
    let f = |code, jt, jf, k| libc::sock_filter { code, jt, jf, k };
    let mut code = vec![
        f(0x20, 0, 0, 4),
        f(0x15, 1, 0, arch),
        f(0x06, 0, 0, 0x80000000),
        f(0x20, 0, 0, 0),
    ];
    if cfg!(target_arch = "x86_64") {
        code.extend([f(0x45, 0, 1, 0x40000000), f(0x06, 0, 0, 0x80000000)]);
    }
    // Nginx -t initializes listeners; acknowledge bind/listen without opening host ports.
    if nginx_check {
        for nr in if cfg!(target_arch = "x86_64") {
            [49, 50]
        } else {
            [200, 201]
        } {
            code.extend([f(0x15, 0, 1, nr), f(0x06, 0, 0, 0x50000)]);
        }
    }
    for &nr in denied {
        code.extend([
            f(0x15, 0, 1, nr),
            f(0x06, 0, 0, 0x50000 | libc::EPERM as u32),
        ]);
    }
    code.push(f(0x06, 0, 0, 0x7fff0000));
    let program = libc::sock_fprog {
        len: code.len() as u16,
        filter: code.as_mut_ptr(),
    };
    ensure!(
        unsafe { libc::prctl(libc::PR_SET_SECCOMP, 2, &program) } == 0,
        "无法禁止预检网络和元数据修改"
    );
    Ok(())
}
#[cfg(target_os = "macos")]
fn sandbox(directory: &Path, nginx_check: bool) -> Result<()> {
    #[link(name = "sandbox")]
    unsafe extern "C" {
        fn sandbox_init(
            profile: *const libc::c_char,
            flags: u64,
            error: *mut *mut libc::c_char,
        ) -> libc::c_int;
        fn sandbox_free_error(error: *mut libc::c_char);
    }
    let directory = serde_json::to_string(directory.to_str().context("隔离路径无效")?)?;
    let listeners = if nginx_check {
        "(allow network-bind)(allow network-inbound)"
    } else {
        ""
    };
    let profile = std::ffi::CString::new(format!(
        "(version 1)(allow default)(deny network*){listeners}(deny file-write*)(allow file-write* (subpath {directory}))"
    ))?;
    let mut error = std::ptr::null_mut();
    let result = unsafe { sandbox_init(profile.as_ptr(), 0, &mut error) };
    let detail = if error.is_null() {
        String::new()
    } else {
        let text = unsafe { std::ffi::CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned();
        unsafe { sandbox_free_error(error) };
        text
    };
    ensure!(result == 0, "无法隔离预检：{detail}");
    Ok(())
}
pub fn worker(directory: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let directory = directory.canonicalize()?;
    let request: Value = serde_json::from_slice(&fs::read(directory.join("request.json"))?)?;
    sandbox(&directory, request["nginx_check"] == true)?;
    let limit = libc::rlimit {
        rlim_cur: 8 * 1024 * 1024,
        rlim_max: 8 * 1024 * 1024,
    };
    ensure!(
        unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &limit) } == 0,
        "无法限制预检输出"
    );
    let args = request["args"]
        .as_array()
        .context("参数无效")?
        .iter()
        .map(|v| v.as_str().context("参数无效"))
        .collect::<Result<Vec<_>>>()?;
    ensure!(!args.is_empty(), "缺少程序");
    let env: BTreeMap<String, String> = serde_json::from_value(request["env"].clone())?;
    Err(std::process::Command::new(args[0])
        .args(&args[1..])
        .env_clear()
        .envs(env)
        .current_dir(directory)
        .exec()
        .into())
}
async fn preflight(
    manager: &Manager,
    env: &BTreeMap<String, String>,
    core: &str,
    binary: &Path,
    version: &str,
) -> Result<()> {
    let (uid, gid, groups) = identity(manager, env).await?;
    let work = tempfile::Builder::new()
        .prefix("chain-core-check-")
        .tempdir_in(manager.path("/var/tmp"))?;
    let root = work.path();
    let target = root.join(core);
    fs::copy(binary, &target)?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755))?;
    if core == "mihomo" {
        for name in [
            "GeoSite.dat",
            "GeoIP.dat",
            "Country.mmdb",
            "ASN.mmdb",
            "geoip.metadb",
        ] {
            let path = Path::new(&env["CHAIN_AGENT_DIR"]).join("mihomo").join(name);
            if path.is_file() {
                ordinary(&path)?;
                fs::copy(path, root.join(name))?;
            }
        }
    }
    let mut checks = vec![(
        vec![
            target.to_string_lossy().into_owned(),
            if core == "xray" {
                "version".into()
            } else {
                "-v".into()
            },
        ],
        String::new(),
    )];
    for (i, (path, assets)) in configs(env, core)?.iter().enumerate() {
        ordinary(path)?;
        let mut config: Value = serde_json::from_slice(&fs::read(path)?)?;
        if core == "xray" {
            config["log"] = json!({"loglevel":"none"});
        }
        let candidate = root.join(format!("config-{i}.json"));
        fs::write(&candidate, serde_json::to_vec(&config)?)?;
        let args = if core == "xray" {
            vec![
                target.display().to_string(),
                "run".into(),
                "-test".into(),
                "-config".into(),
                candidate.display().to_string(),
            ]
        } else {
            vec![
                target.display().to_string(),
                "-t".into(),
                "-f".into(),
                candidate.display().to_string(),
                "-d".into(),
                root.display().to_string(),
            ]
        };
        checks.push((args, assets.clone()));
    }
    if unsafe { libc::geteuid() } == 0 {
        use std::os::fd::AsRawFd;
        for path in std::iter::once(root.to_path_buf())
            .chain(fs::read_dir(root)?.map(|p| p.unwrap().path()))
        {
            let file = fs::File::open(path)?;
            ensure!(
                unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } == 0,
                "无法设置临时预检目录身份"
            );
        }
    }
    for (i, (args, assets)) in checks.into_iter().enumerate() {
        let child_env = json!({"PATH":super::platform::PATH,"HOME":root,"TMPDIR":root,"SAFE_PATHS":Path::new(&env["CHAIN_AGENT_DIR"]).join("acme/config"),"XRAY_LOCATION_ASSET":if assets.is_empty(){root.to_string_lossy().into_owned()}else{assets}});
        let req = serde_json::to_vec(&json!({"args":args,"env":child_env}))?;
        super::install::owned_write(&root.join("request.json"), &req, 0o600, uid, gid)?;
        let log = fs::File::create(root.join("check.log"))?;
        let mut cmd = tokio::process::Command::new(std::env::current_exe()?);
        cmd.args(["internal-core-worker", root.to_str().unwrap()])
            .env_clear()
            .env("PATH", super::platform::PATH)
            .current_dir(root)
            .stdout(log.try_clone()?)
            .stderr(log)
            .kill_on_drop(true)
            .process_group(0);
        if unsafe { libc::geteuid() } == 0 {
            let groups = groups.clone();
            cmd.uid(uid).gid(gid);
            unsafe {
                cmd.pre_exec(move || {
                    let count = groups.len().try_into().map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "服务组数量超出系统接口范围",
                        )
                    })?;
                    if libc::setgroups(count, groups.as_ptr()) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let mut child = cmd.spawn()?;
        let pid = child.id().context("预检进程未启动")?;
        let result = tokio::time::timeout(Duration::from_secs(30), child.wait()).await;
        unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        let _ = child.wait().await;
        ensure!(
            result.context("内核预检超时")??.success(),
            "{core} 隔离版本/配置预检失败，未替换现用配置"
        );
        if i == 0 {
            let text = fs::read_to_string(root.join("check.log"))?;
            ensure!(
                text.split(|c: char| !c.is_ascii_digit() && c != '.')
                    .any(|s| s == version),
                "候选内核实际版本不匹配"
            );
        }
    }
    Ok(())
}
pub async fn operate(manager: &Manager, job: &Value, directory: &Path) -> Result<Value> {
    validate(job)?;
    let core = job["core"].as_str().unwrap();
    let action = job["action"].as_str().unwrap();
    let journal = manager.base.join(".core-pending.json");
    ordinary(&journal)?;
    let _lock = manager.lock()?;
    if matches!(action, "core-confirm" | "core-reject") {
        let j: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        ensure!(
            j["task_id"] == job["task_id"] && j["core"] == core,
            "待确认任务不匹配"
        );
        let mut state = manager.state()?;
        let current = &state["roles"]["agent"]["current"];
        ensure!(
            current == &j["target"]
                || (action == "core-reject" && current == &j["before"]["current"]),
            "当前版本已变化"
        );
        let release = current.as_str().context("Agent 版本缺失")?;
        super::upgrade::release_name(release)?;
        ensure!(
            manager.base.join("current-agent").canonicalize()?
                == manager.base.join("releases").join(release).canonicalize()?,
            "Agent 链接与状态不一致"
        );
        let env = serde_json::from_value(j["env"].clone())?;
        let detail = if action == "core-reject" {
            recover(manager, &j).await?
        } else {
            ensure!(
                snapshot(&env, core)? == j["config_snapshot"],
                "配置已变化，未确认旧检查结果"
            );
            manager.healthy("agent".into()).await?;
            state["core_history"][core] = json!({"release":j["before"]["current"],"version":j["previous_version"],"safe":j["rollback_allowed"]});
            state.as_object_mut().unwrap().remove("core_blocked");
            save(manager, &state)?;
            fs::remove_file(&journal)?;
            "内核及实际连接已确认；原配置保持不变".into()
        };
        let receipt = directory.join(format!("{}.json", job["task_id"].as_str().unwrap()));
        ordinary(&receipt)?;
        let mut original: Value = serde_json::from_slice(&fs::read(&receipt)?)?;
        original["state"] = json!(if action == "core-confirm" {
            "succeeded"
        } else {
            "failed"
        });
        original["detail"] = json!(detail);
        atomic(&receipt, &serde_json::to_vec(&original)?, 0o600)?;
        return Ok(json!({"state":"succeeded","detail":detail}));
    }
    ensure!(!journal.exists(), "请先确认或恢复上次内核升级");
    let mut state = manager.state()?;
    let before = state["roles"]["agent"].clone();
    let id = before["current"].as_str().context("本机尚未安装 Agent")?;
    super::upgrade::release_name(id)?;
    let source = manager.base.join("releases").join(id);
    ensure!(
        manager.base.join("current-agent").canonicalize()? == source.canonicalize()?,
        "Agent 链接与状态不一致"
    );
    let manifest = package::verify(&source, true)?;
    let old: Value = serde_json::from_slice(&fs::read(source.join("release.json"))?)?;
    let env = manager.environment("agent")?;
    ensure!(
        Path::new(&env["CHAIN_XRAY_BIN"]).canonicalize()?
            == source.join("bin/xray").canonicalize()?
            && !env.contains_key("CHAIN_MIHOMO_BIN"),
        "自定义内核路径不支持替换"
    );
    ensure!(
        fs::read_to_string(Path::new(&env["CHAIN_AGENT_DIR"]).join(".chain-owner"))?
            == "chain-agent",
        "Agent 数据归属不匹配"
    );
    let baseline = snapshot(&env, core)?;
    let security = job["security_update"] == true;
    let allowed = !security || job["rollback_safe"] == true;
    let work = tempfile::Builder::new()
        .prefix(&format!(
            "work-{}-",
            job["id"].as_str().context("缺少任务 ID")?
        ))
        .tempdir_in(directory)?;
    let stage = work.path().join("stage");
    package::copy(&source, &stage, &manifest)?;
    let preparation:Result<Value>=async{
  let (download,meta)=if action=="core-rollback"{ensure!(state["core_blocked"].is_null(),"停用后必须明确选择安全版本");let history=&state["core_history"][core];ensure!(history["safe"]==true,"没有确认安全的上一版");let id=history["release"].as_str().context("缺少回退记录")?;super::upgrade::release_name(id)?;let path=manager.base.join("releases").join(id);package::verify(&path,true)?;let m:Value=serde_json::from_slice(&fs::read(path.join("release.json"))?)?;(path,json!({"version":m[core],"source":m[format!("{core}_source")],"archive_sha256":m[format!("{core}_archive_sha256")],"prerelease":m[format!("{core}_prerelease")]}))}else{let path=work.path().join("download");fs::create_dir(&path)?;let meta=download(core,job["version"].as_str().unwrap(),&path,job["prerelease"]==true).await?;(path,meta)};
  preflight(manager,&env,core,&download.join(format!("bin/{core}")),meta["version"].as_str().context("缺少候选版本")?).await?;ensure!(snapshot(&env,core)?==baseline,"预检期间配置已变化");replace(&stage,core,&download,&meta)?;Ok(meta)
 }.await;
    let meta = match preparation {
        Ok(v) => v,
        Err(e) => {
            if security && !allowed {
                block(manager, &mut state, core, job["id"].as_str().unwrap(), &env).await?;
            }
            return Err(e);
        }
    };
    let release = format!(
        "{}-{}-{}-{}",
        manifest.version,
        manifest.os,
        manifest.arch,
        &package::digest(&stage.join("release.json"))?[..12]
    );
    let destination = manager.base.join("releases").join(&release);
    if destination.exists() {
        ensure!(
            fs::read(destination.join("release.json"))? == fs::read(stage.join("release.json"))?,
            "目标发行目录冲突"
        );
        package::verify(&destination, true)?;
    } else {
        package::copy(&stage, &destination, &package::verify(&stage, true)?)?;
    }
    let j = json!({"task_id":job["id"],"core":core,"before":before,"target":release,"env":env,"config_snapshot":baseline,"previous_version":old[core],"rollback_allowed":allowed&&state["core_blocked"].is_null(),"was_running":manager.active("agent").await?});
    atomic(&journal, &serde_json::to_vec(&j)?, 0o600)?;
    let result: Result<()> = async {
        manager.service("stop", "agent").await?;
        manager.link("agent", &release)?;
        state["roles"]["agent"]["current"] = json!(release);
        state.as_object_mut().unwrap().remove("core_blocked");
        save(manager, &state)?;
        let marker = Path::new(&env["CHAIN_AGENT_DIR"]).join(".core-upgrade-blocked");
        ordinary(&marker)?;
        if marker.exists() {
            ensure!(
                serde_json::from_slice::<Value>(&fs::read(&marker)?)?["owner"]
                    == "chain-core-upgrade-v1",
                "停用标记不匹配"
            );
            fs::remove_file(marker)?;
        }
        manager.service("start", "agent").await?;
        manager.healthy("agent".into()).await?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        let detail = recover(manager, &j).await?;
        anyhow::bail!("内核切换失败：{e:#}；{detail}");
    }
    Ok(
        json!({"state":"awaiting_verification","core_version":meta["version"],"detail":"内核版本和配置预检通过；请用原链接验证实际传输，确认前不继续下一台"}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_groups_preserve_native_gid_type_and_primary_group() {
        let entry = unsafe { libc::getpwuid(libc::geteuid()) };
        assert!(!entry.is_null());
        let account = unsafe { std::ffi::CStr::from_ptr((*entry).pw_name) }.to_owned();
        let gid = unsafe { (*entry).pw_gid };
        let groups: Vec<libc::gid_t> = account_groups(&account, gid).unwrap();
        assert!(groups.contains(&gid));
        assert!(!groups.is_empty() && groups.len() < 1024);
    }
    fn installed(root: &Path) -> (Manager, BTreeMap<String, String>) {
        let m = super::super::lifecycle::open(root).unwrap();
        let release = m.base.join("releases/old");
        package::tests::fixture(&release);
        let data = m.path("/var/lib/chain-agent");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join(".chain-owner"), "chain-agent").unwrap();
        fs::write(data.join("active.json"), r#"{"config":{"inbounds":[]}}"#).unwrap();
        fs::write(data.join("xray.json"), r#"{"inbounds":[]}"#).unwrap();
        m.link("agent", "old").unwrap();
        save(
            &m,
            &json!({"owner":OWNER,"roles":{"agent":{"current":"old","version":"0.3.0"}}}),
        )
        .unwrap();
        let env = BTreeMap::from([(
            "CHAIN_AGENT_DIR".into(),
            data.to_string_lossy().into_owned(),
        )]);
        (m, env)
    }
    #[tokio::test]
    async fn recovery_restores_safe_binary_but_never_changed_customer_config() {
        for (safe, changed) in [(true, false), (false, false), (true, true)] {
            let root = tempfile::tempdir().unwrap();
            let (m, env) = installed(root.path());
            let baseline = snapshot(&env, "xray").unwrap();
            let old = m.state().unwrap()["roles"]["agent"].clone();
            package::tests::fixture(&m.base.join("releases/new"));
            m.link("agent", "new").unwrap();
            let mut state = m.state().unwrap();
            state["roles"]["agent"]["current"] = json!("new");
            save(&m, &state).unwrap();
            let config = Path::new(&env["CHAIN_AGENT_DIR"]).join("xray.json");
            if changed {
                fs::write(&config, "customer changed config").unwrap();
            }
            let before = fs::read(&config).unwrap();
            let metadata = fs::metadata(&config).unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            let j = json!({"task_id":id,"core":"xray","before":old,"env":env,"config_snapshot":baseline,"rollback_allowed":safe,"was_running":false});
            fs::write(
                m.base.join(".core-pending.json"),
                serde_json::to_vec(&j).unwrap(),
            )
            .unwrap();
            recover(&m, &j).await.unwrap();
            assert_eq!(fs::read(&config).unwrap(), before);
            assert_eq!(
                fs::metadata(&config).unwrap().ctime_nsec(),
                metadata.ctime_nsec()
            );
            let state = m.state().unwrap();
            if safe && !changed {
                assert_eq!(state["roles"]["agent"]["current"], "old");
            } else {
                assert_eq!(state["core_blocked"]["task_id"], id);
                assert!(
                    Path::new(&env["CHAIN_AGENT_DIR"])
                        .join(".core-upgrade-blocked")
                        .exists()
                );
            }
            assert!(!m.base.join(".core-pending.json").exists());
        }
    }
    #[test]
    fn selected_kernel_survives_application_update() {
        let root = tempfile::tempdir().unwrap();
        let (m, _) = installed(root.path());
        let old = m.base.join("releases/old");
        fs::create_dir(old.join("share")).unwrap();
        fs::write(old.join("share/Xray-LICENSE"), "license").unwrap();
        let mut v: Value =
            serde_json::from_slice(&fs::read(old.join("release.json")).unwrap()).unwrap();
        v["xray_selected"] = json!(true);
        v["xray"] = json!("26.10.21");
        v["files"]["share/Xray-LICENSE"] =
            json!({"sha256":package::digest(&old.join("share/Xray-LICENSE")).unwrap(),"mode":420});
        fs::write(old.join("release.json"), serde_json::to_vec(&v).unwrap()).unwrap();
        let stage = root.path().join("stage");
        package::tests::fixture(&stage);
        preserve(&m, &stage).unwrap();
        let updated: Value =
            serde_json::from_slice(&fs::read(stage.join("release.json")).unwrap()).unwrap();
        assert_eq!(updated["xray"], "26.10.21");
        assert_eq!(updated["xray_selected"], true);
        assert_eq!(
            fs::read(stage.join("bin/xray")).unwrap(),
            fs::read(old.join("bin/xray")).unwrap()
        );
        package::verify(&stage, true).unwrap();
    }
    #[test]
    fn rejects_unsafe_core_operations() {
        let id = uuid::Uuid::new_v4();
        assert!(
            validate(&json!({"action":"core-upgrade","core":"xray","version":"26.10.21"})).is_ok()
        );
        for v in [
            json!({"action":"core-upgrade","core":"xray","version":"latest"}),
            json!({"action":"core-upgrade","core":"xray","version":"26.10.21","rollback_safe":true}),
            json!({"action":"core-confirm","core":"xray","task_id":id,"version":"26.10.21"}),
        ] {
            assert!(validate(&v).is_err());
        }
    }
}
