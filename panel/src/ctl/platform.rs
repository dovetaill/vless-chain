use super::upgrade::{Manager, OWNER, command, output};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeMap, path::Path};

pub const PATH: &str = "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin";
pub fn shell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
pub fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
pub fn executable(name: &str) -> Option<std::path::PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_else(|| PATH.into()))
        .map(|p| p.join(name))
        .find(|p| p.is_file())
}
pub fn definition(
    manager: &Manager,
    name: &str,
    argv: &[String],
    user: Option<&str>,
    env: &BTreeMap<String, String>,
) -> Result<String> {
    ensure!(
        !argv.is_empty()
            && argv
                .iter()
                .chain(env.values())
                .all(|v| !v.contains(['\n', '\r', '\0'])),
        "服务参数无效"
    );
    let log = if user.is_some() {
        manager.path(&format!("/var/lib/{name}/service.log"))
    } else {
        manager.path(&format!("/var/log/{name}.log"))
    };
    if manager.kind() == "openrc" {
        let args = argv[1..]
            .iter()
            .map(|v| shell(v))
            .collect::<Vec<_>>()
            .join(" ");
        let mut text = format!(
            "#!/sbin/openrc-run\n# {OWNER}\nsupervisor=supervise-daemon\ncommand={}\ncommand_args={}\nrespawn_delay=5\nrespawn_max=0\nretry=\"TERM/50/KILL/5\"\nstopgroup=yes\numask=0077\noutput_log={}\nerror_log={}\nexport PATH={}:\"$PATH\"\n",
            shell(&argv[0]),
            shell(&args),
            shell(&log.to_string_lossy()),
            shell(&log.to_string_lossy()),
            shell(PATH)
        );
        if let Some(user) = user {
            text.push_str(&format!(
                "command_user={}\nno_new_privs=yes\n",
                shell(&format!("{user}:{user}"))
            ));
        }
        if name == "chain-agent" {
            text.push_str("capabilities=\"^cap_net_bind_service\"\nrc_ulimit=\"-n 65536\"\n");
        }
        for (k, v) in env {
            ensure!(
                k.bytes().all(|c| c.is_ascii_uppercase() || c == b'_'),
                "环境变量名无效"
            );
            text.push_str(&format!("export {k}={}\n", shell(v)));
        }
        return Ok(text + "depend() { need net; }\n");
    }
    ensure!(manager.kind() == "launchd", "需要非 systemd 服务定义");
    let mut text = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- {OWNER} -->\n<plist version=\"1.0\"><dict><key>Label</key><string>org.vless-chain.{name}</string><key>ProgramArguments</key><array>{}</array><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>Umask</key><integer>63</integer><key>StandardOutPath</key><string>{}</string><key>StandardErrorPath</key><string>{}</string>",
        argv.iter()
            .map(|v| format!("<string>{}</string>", xml(v)))
            .collect::<String>(),
        xml(&log.to_string_lossy()),
        xml(&log.to_string_lossy())
    );
    if let Some(user) = user {
        text.push_str(&format!(
            "<key>UserName</key><string>{}</string><key>GroupName</key><string>{}</string>",
            xml(user),
            xml(user)
        ));
    }
    text.push_str("<key>EnvironmentVariables</key><dict>");
    text.push_str(&format!("<key>PATH</key><string>{PATH}</string>"));
    for (k, v) in env {
        text.push_str(&format!("<key>{}</key><string>{}</string>", xml(k), xml(v)));
    }
    text.push_str("</dict></dict></plist>\n");
    Ok(text)
}
pub async fn enable(manager: &Manager, name: &str) -> Result<()> {
    if !manager.live() {
        return Ok(());
    }
    match manager.kind() {
        "systemd" => {
            command("systemctl", &["daemon-reload"], 30).await?;
            command("systemctl", &["enable", &format!("{name}.service")], 30).await?;
        }
        "openrc" => {
            command("rc-update", &["add", name, "default"], 30).await?;
        }
        _ => {
            command(
                "launchctl",
                &["enable", &format!("system/org.vless-chain.{name}")],
                30,
            )
            .await?;
        }
    }
    Ok(())
}
pub async fn create_account(name: &str) -> Result<()> {
    let home = format!("/var/lib/{name}");
    if cfg!(target_os = "macos") {
        let mut free = None;
        for id in (200..500).rev() {
            if unsafe { libc::getpwuid(id) }.is_null() && unsafe { libc::getgrgid(id) }.is_null() {
                free = Some(id);
                break;
            }
        }
        let id = free.context("没有可用的服务账户 ID")?.to_string();
        for (record, attrs) in [
            (
                format!("/Groups/{name}"),
                vec![("PrimaryGroupID", id.as_str())],
            ),
            (
                format!("/Users/{name}"),
                vec![
                    ("UniqueID", id.as_str()),
                    ("PrimaryGroupID", id.as_str()),
                    ("NFSHomeDirectory", &home),
                    ("UserShell", "/usr/bin/false"),
                    ("IsHidden", "1"),
                    ("Password", "*"),
                ],
            ),
        ] {
            command("dscl", &[".", "-create", &record], 30).await?;
            for (key, value) in attrs {
                command("dscl", &[".", "-create", &record, key, value], 30).await?;
            }
        }
    } else if executable("useradd").is_some() {
        let shell = executable("nologin").unwrap_or_else(|| Path::new("/bin/false").into());
        command(
            "useradd",
            &[
                "--system",
                "--user-group",
                "--no-create-home",
                "--home-dir",
                &home,
                "--shell",
                shell.to_str().unwrap(),
                name,
            ],
            30,
        )
        .await?;
    } else {
        command("addgroup", &["-S", name], 30).await?;
        command(
            "adduser",
            &[
                "-S",
                "-D",
                "-H",
                "-h",
                &home,
                "-s",
                "/sbin/nologin",
                "-G",
                name,
                name,
            ],
            30,
        )
        .await?;
    }
    Ok(())
}
pub async fn remove_account(name: &str) -> Result<()> {
    use std::ffi::{CStr, CString};
    ensure!(
        matches!(name, "chain-panel" | "chain-agent"),
        "账户名称无效"
    );
    let c = CString::new(name)?;
    let user = unsafe { libc::getpwnam(c.as_ptr()) };
    if user.is_null() {
        return Ok(());
    }
    let u = unsafe { &*user };
    let uid = u.pw_uid;
    let gid = u.pw_gid;
    ensure!(
        uid != 0
            && unsafe { CStr::from_ptr(u.pw_dir) }.to_str()? == format!("/var/lib/{name}")
            && ["nologin", "false"]
                .iter()
                .any(|s| unsafe { CStr::from_ptr(u.pw_shell) }
                    .to_string_lossy()
                    .ends_with(s)),
        "账户归属不匹配"
    );
    let g = unsafe { libc::getgrgid(gid) };
    ensure!(!g.is_null(), "服务组不存在");
    ensure!(
        unsafe { CStr::from_ptr((*g).gr_name) }.to_str()? == name,
        "服务组归属不匹配"
    );
    let mut member = unsafe { (*g).gr_mem };
    while !member.is_null() && !unsafe { *member }.is_null() {
        ensure!(
            unsafe { CStr::from_ptr(*member) }.to_str()? == name,
            "服务组被其他账户使用"
        );
        member = unsafe { member.add(1) };
    }
    unsafe { libc::setpwent() };
    let mut foreign = false;
    loop {
        let p = unsafe { libc::getpwent() };
        if p.is_null() {
            break;
        }
        if unsafe { (*p).pw_gid == gid && (*p).pw_uid != uid } {
            foreign = true;
        }
    }
    unsafe { libc::endpwent() };
    ensure!(!foreign, "服务组被其他账户使用");
    if cfg!(target_os = "macos") {
        command("dscl", &[".", "-delete", &format!("/Users/{name}")], 30).await?;
        command("dscl", &[".", "-delete", &format!("/Groups/{name}")], 30).await?;
    } else {
        command(
            if executable("userdel").is_some() {
                "userdel"
            } else {
                "deluser"
            },
            &[name],
            30,
        )
        .await?;
        if !unsafe { libc::getgrnam(c.as_ptr()) }.is_null() {
            command(
                if executable("groupdel").is_some() {
                    "groupdel"
                } else {
                    "delgroup"
                },
                &[name],
                30,
            )
            .await?;
        }
    }
    Ok(())
}
pub async fn stop_openrc_residue(manager: &Manager, name: &str) -> Result<()> {
    use std::{fs, os::unix::fs::MetadataExt};
    ensure!(
        matches!(name, "chain-panel" | "chain-agent"),
        "服务名称无效"
    );
    let pidfile = manager.path(&format!("/run/supervise-{name}.pid"));
    super::upgrade::ordinary(&pidfile)?;
    if !pidfile.exists() {
        return Ok(());
    }
    let info = fs::metadata(&pidfile)?;
    ensure!(
        info.is_file() && info.uid() == 0 && info.mode() & 0o022 == 0,
        "supervisor PID 文件归属无效"
    );
    let pid = fs::read_to_string(&pidfile)?.trim().parse::<u32>()?;
    ensure!(pid > 1, "supervisor PID 无效");
    let process = Path::new("/proc").join(pid.to_string());
    if !process.exists() {
        fs::remove_file(pidfile)?;
        return Ok(());
    }
    let binary = executable("supervise-daemon")
        .context("缺少 supervisor")?
        .canonicalize()?;
    let args = fs::read(process.join("cmdline"))?
        .split(|c| *c == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8(s.to_vec()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let bytes = fs::read(process.join("environ"))?;
    let vars = bytes
        .split(|c| *c == 0)
        .filter_map(|s| std::str::from_utf8(s).ok())
        .filter_map(|s| s.split_once('='))
        .collect::<BTreeMap<_, _>>();
    let role = name.strip_prefix("chain-").unwrap();
    ensure!(
        fs::metadata(&process)?.uid() == 0
            && fs::read_link(process.join("exe"))? == binary
            && args.get(1).map(String::as_str) == Some(name)
            && args.get(2).map(String::as_str) == Some("--start")
            && args.ends_with(&[format!("/opt/chain/current-{role}/bin/{name}"), "--".into()])
            && vars.get("RC_SVCNAME") == Some(&name)
            && vars.get("RC_SERVICE").copied() == Some(format!("/etc/init.d/{name}").as_str()),
        "supervisor 身份不匹配，拒绝清理"
    );
    command(
        "env",
        &[
            &format!("RC_SVCNAME={name}"),
            binary.to_str().context("程序路径无效")?,
            name,
            "--stop",
        ],
        65,
    )
    .await
}
pub async fn related(manager: &Manager, action: &str, name: &str) -> Result<()> {
    ensure!(
        matches!(action, "start" | "stop" | "restart" | "disable" | "reload"),
        "服务操作无效"
    );
    if !manager.live() {
        return Ok(());
    }
    match manager.kind() {
        "systemd" => {
            command("systemctl", &[action, &format!("{name}.service")], 70).await?;
        }
        "openrc" => {
            if action == "disable" {
                command("rc-update", &["del", name, "default"], 30).await?;
            } else {
                command("rc-service", &[name, action], 70).await?;
            }
        }
        _ => {
            let label = format!("system/org.vless-chain.{name}");
            if action == "disable" {
                command("launchctl", &["disable", &label], 30).await?;
            } else if action == "reload" {
                command("launchctl", &["kill", "SIGHUP", &label], 30).await?;
            } else {
                if matches!(action, "stop" | "restart")
                    && output("launchctl", &["print", &label], 15)
                        .await?
                        .status
                        .success()
                {
                    command("launchctl", &["bootout", &label], 70).await?;
                }
                if matches!(action, "start" | "restart") {
                    command(
                        "launchctl",
                        &[
                            "bootstrap",
                            "system",
                            manager
                                .path(&format!(
                                    "/Library/LaunchDaemons/org.vless-chain.{name}.plist"
                                ))
                                .to_str()
                                .unwrap(),
                        ],
                        70,
                    )
                    .await?;
                }
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_values_are_not_shell_code() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("run/openrc")).unwrap();
        let m = Manager {
            root: root.path().into(),
            base: root.path().join("opt/chain"),
        };
        let body = definition(
            &m,
            "chain-agent",
            &["/tmp/my binary".into()],
            Some("chain-agent"),
            &BTreeMap::from([("CHAIN_VALUE".into(), "$(id);'quoted'".into())]),
        )
        .unwrap();
        assert!(body.contains("export CHAIN_VALUE='$(id);'\\''quoted'\\'''"));
        assert!(body.contains("cap_net_bind_service"));
        assert!(!body.contains("python"));
    }
}
