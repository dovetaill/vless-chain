use super::upgrade::{command, output};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Seek, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
const OWNER: &str = "# chain-panel network defaults v1\n";
const KEYS: [&str; 2] = ["net.ipv4.tcp_congestion_control", "net.core.default_qdisc"];
const QUEUES: [&str; 5] = ["fq", "fq_codel", "fq_pie", "cake", "pfifo_fast"];
pub fn validate(cc: &str, qdisc: &str) -> Result<()> {
    ensure!(
        !cc.is_empty()
            && cc.len() <= 32
            && cc.as_bytes()[0].is_ascii_lowercase()
            && cc
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
            && QUEUES.contains(&qdisc),
        "拥塞算法或队列无效"
    );
    Ok(())
}
fn read(key: &str) -> Result<String> {
    Ok(
        fs::read_to_string(Path::new("/proc/sys").join(key.replace('.', "/")))?
            .trim()
            .into(),
    )
}
async fn modules() -> Result<(BTreeSet<String>, String)> {
    let kernel = String::from_utf8(output("uname", &["-r"], 5).await?.stdout)?
        .trim()
        .to_owned();
    let mut names = BTreeSet::new();
    for name in ["modules.builtin", "modules.dep"] {
        let body = fs::read_to_string(Path::new("/lib/modules").join(&kernel).join(name))
            .unwrap_or_default();
        for word in body.split_whitespace() {
            if let Some(file) = word.rsplit('/').next() {
                if let Some((stem, _)) = file.split_once(".ko") {
                    if stem.starts_with("tcp_") || stem.starts_with("sch_") {
                        names.insert(stem.to_owned());
                    }
                }
            }
        }
    }
    for item in fs::read_dir("/sys/module")? {
        let name = item?.file_name().to_string_lossy().into_owned();
        if name.starts_with("tcp_") || name.starts_with("sch_") {
            names.insert(name);
        }
    }
    Ok((names, kernel))
}
async fn queues() -> Result<Vec<Value>> {
    let out = output("tc", &["-j", "qdisc", "show"], 3).await?;
    ensure!(
        out.status.success(),
        "无法读取实际队列：{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(serde_json::from_slice(&out.stdout)?)
}
pub async fn status() -> Result<Value> {
    let cc = read(KEYS[0])?;
    let qdisc = read(KEYS[1])?;
    let (present, kernel) = modules().await?;
    let mut algorithms = read("net.ipv4.tcp_available_congestion_control")?
        .split_whitespace()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    algorithms.insert(cc.clone());
    for name in ["bbr", "bbrplus", "cubic"] {
        if present.contains(&format!("tcp_{name}")) {
            algorithms.insert(name.into());
        }
    }
    let (actual, warning) = match queues().await {
        Ok(v) => (v, String::new()),
        Err(e) => (vec![], format!("未能读取网卡实际队列：{e:#}")),
    };
    let queues = QUEUES
        .into_iter()
        .filter(|n| {
            *n == qdisc
                || present.contains(&format!("sch_{n}"))
                || actual.iter().any(|q| q["kind"] == *n)
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"congestion_control":cc,"qdisc":qdisc,"algorithms":algorithms,"queues":queues,"interfaces":actual.iter().map(|q|json!({"dev":q["dev"],"kind":q["kind"],"parent":q.get("parent").cloned().unwrap_or(json!("root"))})).collect::<Vec<_>>(),"warning":warning,"kernel":kernel}),
    )
}
fn owned(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(m) => {
            ensure!(
                m.is_file()
                    && m.uid() == unsafe { libc::geteuid() }
                    && m.nlink() == 1
                    && m.mode() & 0o022 == 0,
                "配置归属、类型或权限不匹配：{}",
                path.display()
            );
            let bytes = fs::read(path)?;
            ensure!(bytes.starts_with(OWNER.as_bytes()), "配置不属于本项目");
            Ok(Some(bytes))
        }
    }
}
fn replace(path: &Path, expected: Option<&[u8]>, content: Option<&[u8]>) -> Result<()> {
    ensure!(
        owned(path)?.as_deref() == expected,
        "配置被其他操作修改，未覆盖"
    );
    let Some(content) = content else {
        fs::remove_file(path)?;
        return Ok(());
    };
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o644);
    if expected.is_none() {
        options.create_new(true);
    }
    let mut file = options.open(path)?;
    let info = file.metadata()?;
    ensure!(
        info.is_file() && info.uid() == unsafe { libc::geteuid() } && info.nlink() == 1,
        "配置归属已改变"
    );
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    ensure!(bytes == expected.unwrap_or_default(), "配置内容已改变");
    let result = (|| -> Result<()> {
        file.rewind()?;
        file.write_all(content)?;
        file.set_len(content.len() as u64)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = result {
        file.rewind()?;
        file.write_all(&bytes)?;
        file.set_len(bytes.len() as u64)?;
        file.sync_all()?;
        if expected.is_none() {
            fs::remove_file(path)?;
        }
        return Err(e);
    }
    Ok(())
}
fn automatic(queue: &Value, all: &[Value]) -> bool {
    let kind = queue["kind"].as_str().unwrap_or_default();
    ["fq", "fq_codel", "fq_pie", "pfifo_fast"].contains(&kind)
        && queue["handle"] == "0:"
        && (queue["root"] == true
            || all.iter().any(|root| {
                root["dev"] == queue["dev"] && root["root"] == true && root["kind"] == "mq"
            }))
}
async fn apply_queues(qdisc: &str) -> Vec<String> {
    let all = match queues().await {
        Ok(q) => q,
        Err(e) => return vec![format!("默认设置已保存，网卡队列未切换：{e:#}")],
    };
    let mut warnings = vec![];
    for q in &all {
        let kind = q["kind"].as_str().unwrap_or_default();
        if ["noqueue", "mq", "ingress", "clsact", qdisc].contains(&kind) {
            continue;
        }
        let dev = q["dev"].as_str().unwrap_or_default();
        let parent = q["parent"].as_str().unwrap_or("root");
        if !automatic(q, &all) {
            warnings.push(format!("{dev}/{parent} 保留现有 {kind} 自定义队列"));
            continue;
        }
        let args = if q["root"] == true {
            vec!["qdisc", "replace", "dev", dev, "root", qdisc]
        } else {
            vec!["qdisc", "replace", "dev", dev, "parent", parent, qdisc]
        };
        let result: Result<()> = async {
            let filters = output("tc", &["-j", "filter", "show", "dev", dev], 3).await?;
            ensure!(filters.status.success(), "无法检查现有流量过滤器，保留队列");
            ensure!(
                serde_json::from_slice::<Vec<Value>>(&filters.stdout)?.is_empty(),
                "存在流量过滤器，保留队列"
            );
            ensure!(queues().await?.contains(q), "队列已被其他操作修改，未覆盖");
            command("tc", &args, 3).await?;
            ensure!(
                queues().await?.iter().any(|v| v["dev"] == dev
                    && v["kind"] == qdisc
                    && v["parent"].as_str().unwrap_or("root") == parent),
                "切换后实际队列不匹配"
            );
            Ok(())
        }
        .await;
        if let Err(e) = result {
            warnings.push(format!("{dev}/{parent} 切换失败：{e:#}"));
        }
    }
    warnings
}
pub async fn apply(cc: &str, qdisc: &str) -> Result<Value> {
    validate(cc, qdisc)?;
    ensure!(unsafe { libc::geteuid() } == 0, "网络设置需要管理员权限");
    let before = status().await?;
    ensure!(
        before["algorithms"]
            .as_array()
            .is_some_and(|v| v.contains(&json!(cc)))
            && before["queues"]
                .as_array()
                .is_some_and(|v| v.contains(&json!(qdisc))),
        "当前内核不支持所选算法或队列"
    );
    let paths = [
        PathBuf::from("/etc/sysctl.d/99-chain-network.conf"),
        PathBuf::from("/etc/modules-load.d/chain-network.conf"),
    ];
    let previous = paths.iter().map(|p| owned(p)).collect::<Result<Vec<_>>>()?;
    let (present, _) = modules().await?;
    let required = [format!("tcp_{cc}"), format!("sch_{qdisc}")]
        .into_iter()
        .filter(|m| present.contains(m))
        .collect::<Vec<_>>();
    let desired = [cc, qdisc];
    let original = [
        before["congestion_control"]
            .as_str()
            .context("缺少原算法")?,
        before["qdisc"].as_str().context("缺少原队列")?,
    ];
    let content = [
        format!("{OWNER}{} = {cc}\n{} = {qdisc}\n", KEYS[0], KEYS[1]),
        format!(
            "{OWNER}{}",
            required
                .iter()
                .map(|m| format!("{m}\n"))
                .collect::<String>()
        ),
    ];
    let (mut loaded, mut applied, mut written) = (vec![], vec![], vec![]);
    let result: Result<()> = async {
        for m in &required {
            if !Path::new("/sys/module").join(m).exists() {
                command("modprobe", &[m], 15).await?;
                loaded.push(m.clone());
            }
        }
        for (i, key) in KEYS.iter().enumerate() {
            applied.push(i);
            command("sysctl", &["-w", &format!("{key}={}", desired[i])], 15).await?;
            ensure!(read(key)? == desired[i], "{key} 未生效");
        }
        for i in 0..2 {
            replace(
                &paths[i],
                previous[i].as_deref(),
                Some(content[i].as_bytes()),
            )?;
            written.push(i);
        }
        for i in 0..2 {
            ensure!(read(KEYS[i])? == desired[i], "默认参数被其他操作修改");
        }
        Ok(())
    }
    .await;
    if let Err(e) = result {
        let mut failures = vec![];
        for &i in written.iter().rev() {
            if let Err(e) = replace(
                &paths[i],
                Some(content[i].as_bytes()),
                previous[i].as_deref(),
            ) {
                failures.push(e.to_string());
            }
        }
        for &i in applied.iter().rev() {
            let restore: Result<()> = async {
                let current = read(KEYS[i])?;
                if current == original[i] {
                    return Ok(());
                }
                ensure!(
                    current == desired[i],
                    "{} 已被其他操作修改，未覆盖",
                    KEYS[i]
                );
                command(
                    "sysctl",
                    &["-w", &format!("{}={}", KEYS[i], original[i])],
                    15,
                )
                .await?;
                ensure!(read(KEYS[i])? == original[i], "恢复校验失败");
                Ok(())
            }
            .await;
            if let Err(e) = restore {
                failures.push(e.to_string());
            }
        }
        bail!(
            "{e:#}；恢复结果：{}；本次加载模块仍保留：{}",
            if failures.is_empty() {
                "已恢复本次默认参数和配置".into()
            } else {
                failures.join("；")
            },
            loaded.join(", ")
        );
    }
    let warnings = apply_queues(qdisc).await;
    let mut after = status().await?;
    let mut warning = after["warning"].as_str().unwrap_or_default().to_string();
    for w in warnings {
        warning.push_str(&format!("；{w}"));
    }
    after["warning"] = json!(warning);
    Ok(
        json!({"before":before,"after":after,"loaded_modules":loaded,"detail":"TCP 默认算法及默认队列已保存，已尝试在线应用网卡队列；请核对实际队列及跳过项。已有连接和监听器不会自动切换 TCP 算法。"}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protects_custom_queues() {
        let mq = json!({"kind":"mq","dev":"eth0","root":true});
        let q = json!({"kind":"fq_codel","dev":"eth0","handle":"0:","parent":"1:1"});
        assert!(automatic(&q, &[mq]));
        assert!(!automatic(&q, &[]));
        assert!(!automatic(
            &json!({"kind":"cake","handle":"0:","root":true}),
            &[]
        ));
        assert!(validate("bbr;id", "fq").is_err());
    }
    #[test]
    fn config_preserves_metadata_and_rejects_foreign_or_concurrent_changes() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config");
        let body = format!("{OWNER}old\n");
        replace(&p, None, Some(body.as_bytes())).unwrap();
        let old = fs::metadata(&p).unwrap();
        let next = format!("{OWNER}new\n");
        replace(&p, Some(body.as_bytes()), Some(next.as_bytes())).unwrap();
        let now = fs::metadata(&p).unwrap();
        assert_eq!(
            (old.ino(), old.uid(), old.gid(), old.mode()),
            (now.ino(), now.uid(), now.gid(), now.mode())
        );
        assert!(replace(&p, Some(body.as_bytes()), None).is_err());
        replace(&p, Some(next.as_bytes()), None).unwrap();
        fs::write(&p, "foreign").unwrap();
        assert!(owned(&p).is_err());
    }
}
