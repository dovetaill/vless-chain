use super::upgrade::{command, output};
use anyhow::{Result, bail, ensure};
use std::{collections::BTreeMap, time::Duration};

const UNITS: [&str; 6] = [
    "chrony.service",
    "chronyd.service",
    "systemd-timesyncd.service",
    "ntp.service",
    "ntpd.service",
    "ntpsec.service",
];

async fn status() -> Result<BTreeMap<String, String>> {
    let result = output(
        "timedatectl",
        &[
            "show",
            "--property=NTP",
            "--property=NTPSynchronized",
            "--property=Timezone",
        ],
        10,
    )
    .await?;
    ensure!(result.status.success(), "无法查询系统时间状态");
    Ok(String::from_utf8(result.stdout)?
        .lines()
        .filter_map(|s| s.split_once('='))
        .map(|(k, v)| (k.into(), v.into()))
        .collect())
}

pub async fn prepare(manager: &super::upgrade::Manager) {
    if !manager.live() || manager.kind() != "systemd" {
        return;
    }
    let result: Result<()> = async {
        let clock = status().await?;
        if clock.get("NTP").map(String::as_str) != Some("yes")
            || clock.get("NTPSynchronized").map(String::as_str) != Some("yes")
        {
            println!("自动检查并启用时间同步，保持现有时区。");
            println!("{}", synchronize().await?);
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        eprintln!("时间同步未完成：{error:#}；继续组件操作。");
    }
}

fn installed(text: &str) -> Vec<&'static str> {
    UNITS
        .into_iter()
        .filter(|unit| {
            text.lines().any(|line| {
                let parts = line.split_whitespace().collect::<Vec<_>>();
                parts.len() >= 2 && parts[0] == *unit && parts[1] != "alias"
            })
        })
        .collect()
}

pub async fn synchronize() -> Result<String> {
    let before = status().await?;
    ensure!(before.contains_key("Timezone"), "无法确认现有时区");
    let mut args = vec!["list-unit-files"];
    args.extend(UNITS);
    args.extend(["--no-legend", "--no-pager"]);
    let inventory = output("systemctl", &args, 10).await?;
    ensure!(
        inventory.status.success()
            || (inventory.status.code() == Some(1)
                && inventory.stdout.is_empty()
                && inventory.stderr.is_empty()),
        "无法查询已安装的时间服务"
    );
    let mut units = installed(&String::from_utf8(inventory.stdout)?);
    let mut active = Vec::new();
    for unit in &units {
        if output("systemctl", &["is-active", "--quiet", unit], 10)
            .await?
            .status
            .success()
        {
            active.push(*unit);
        }
    }
    ensure!(
        active.len() <= 1,
        "检测到多个运行中的时间服务，请先解决冲突"
    );
    if units.is_empty() {
        if available("apt-get") {
            command(
                "env",
                &["DEBIAN_FRONTEND=noninteractive", "apt-get", "update"],
                300,
            )
            .await?;
            command(
                "env",
                &[
                    "DEBIAN_FRONTEND=noninteractive",
                    "apt-get",
                    "install",
                    "-y",
                    "chrony",
                ],
                300,
            )
            .await?;
            units.push("chrony.service");
        } else if available("dnf") {
            command("dnf", &["install", "-y", "chrony"], 300).await?;
            units.push("chronyd.service");
        } else {
            bail!("未找到时间服务及支持的包管理器，请先安装系统 NTP 服务");
        }
    }
    let unit = active.first().or(units.first()).unwrap();
    command("systemctl", &["enable", "--now", unit], 30).await?;
    if matches!(*unit, "chrony.service" | "chronyd.service") {
        let source = output("chronyc", &["waitsync", "30", "0", "0", "2"], 70).await?;
        ensure!(
            source.status.success(),
            "chrony 尚未取得有效同步时间源；未执行时间跳变"
        );
        command("chronyc", &["makestep"], 10).await?;
        command("chronyc", &["waitsync", "30", "0.1", "0", "2"], 70).await?;
    } else {
        let mut synced = false;
        for _ in 0..30 {
            let clock = status().await?;
            ensure!(
                clock.get("Timezone") == before.get("Timezone"),
                "时区发生并发变化；本操作未修改时区"
            );
            if clock.get("NTPSynchronized").is_some_and(|v| v == "yes") {
                synced = true;
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        ensure!(synced, "时间服务已启用，但 60 秒内尚未确认同步");
    }
    ensure!(
        status().await?.get("Timezone") == before.get("Timezone"),
        "时区发生并发变化；本操作未修改时区"
    );
    Ok(format!("时间同步已确认，时区保持 {}", before["Timezone"]))
}

fn available(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            dir.join(name)
                .metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_and_unrelated_units_are_not_time_daemons() {
        assert_eq!(
            installed(
                "chrony.service enabled\nchronyd.service alias\nother.service enabled\nntp.service disabled"
            ),
            vec!["chrony.service", "ntp.service"]
        );
        assert!(installed("").is_empty());
    }
}
