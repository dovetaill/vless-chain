mod delivery;
mod integration;
mod release;
#[cfg(test)]
mod tests;
pub(crate) mod workspace;

use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub fn repository() -> PathBuf {
    std::env::var_os("CHAIN_TASK_REPO")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_path_buf()
        })
}

#[derive(Debug, Default)]
pub struct Options {
    pub action: String,
    pub values: BTreeMap<String, String>,
    pub flags: std::collections::BTreeSet<String>,
    pub trailing: Vec<String>,
}

impl Options {
    pub fn parse(args: Vec<String>) -> Result<Self> {
        let mut result = Self {
            action: "menu".into(),
            ..Default::default()
        };
        let mut args = args.into_iter().peekable();
        if let Some(action) = args.next() {
            result.action = action;
        }
        if result.action == "ops" {
            result.trailing.extend(args);
            if result.trailing.first().map(String::as_str) == Some("--") {
                result.trailing.remove(0);
            }
            return Ok(result);
        }
        while let Some(key) = args.next() {
            if ["--profiles", "--stages"].contains(&key.as_str()) {
                let mut values = vec![];
                while args.peek().is_some_and(|v| !v.starts_with("--")) {
                    values.push(args.next().unwrap());
                }
                ensure!(
                    !values.is_empty() && result.values.insert(key, values.join(",")).is_none(),
                    "选择不能为空或重复"
                );
                continue;
            }
            if key == "--" {
                result.trailing.extend(args);
                break;
            }
            if ["--full", "--browser"].contains(&key.as_str()) {
                ensure!(result.flags.insert(key), "重复选项");
            } else {
                ensure!(
                    [
                        "--work-parent",
                        "--output",
                        "--input",
                        "--directory",
                        "--repo",
                        "--set",
                        "--tag",
                        "--commit",
                        "--case",
                        "--xray"
                    ]
                    .contains(&key.as_str()),
                    "未知选项：{key}"
                );
                let value = args.next().with_context(|| format!("{key} 缺少值"))?;
                ensure!(result.values.insert(key, value).is_none(), "重复选项");
            }
        }
        Ok(result)
    }
    pub fn value(&self, key: &str) -> Result<&str> {
        self.values
            .get(key)
            .map(String::as_str)
            .with_context(|| format!("缺少 {key}"))
    }
    pub fn parent(&self) -> PathBuf {
        self.values
            .get("--work-parent")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("RUNNER_TEMP").map(PathBuf::from))
            .unwrap_or_else(|| repository().parent().unwrap().to_path_buf())
    }
}

pub fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|v| v.to_string()).collect()
}
pub fn fixture(args: &[String]) -> Result<bool> {
    integration::fixture(&Options::parse(args.to_vec())?)
}
pub fn command_fixture() -> Result<Option<i32>> {
    integration::command_fixture()
}

pub async fn main(args: Vec<String>) -> Result<()> {
    let args = Options::parse(args)?;
    match args.action.as_str() {
        "menu" => release::menu().await,
        "version" => {
            if let Some(value) = args.values.get("--set") {
                release::set_version(&repository(), value)?;
            } else {
                println!("{}", release::version(&repository())?);
            }
            Ok(())
        }
        "status" => release::status(&args).await,
        "build" => release::build(&args).await,
        "cores" => release::build_cores(&args).await,
        "check" => delivery::check(&args).await,
        "validate" => delivery::validate(&args),
        "assemble" => delivery::assemble(&args),
        "publish-ci" => delivery::publish(&args).await,
        "smoke" => integration::smoke(&args).await,
        "verify" => integration::verify(&args).await,
        "integration" => integration::run(&args).await,
        "fixture-xray" => integration::fixture_xray(&args),
        "openrc-test" => integration::openrc(&args).await,
        "release" => release::wizard(false).await,
        "push" => release::wizard(true).await,
        "draft" | "publish" => release::github(&args).await,
        "ops" => {
            let work = workspace::Workspace::new(&args.parent())?;
            let mut command = strings(&["sh"]);
            command.push(
                repository()
                    .join("scripts/install-server.sh")
                    .display()
                    .to_string(),
            );
            command.extend(args.trailing);
            work.run(&repository(), &command).await
        }
        "--help" | "-h" | "help" => {
            println!(
                "Chain Rust 构建发布工具\nversion [--set X.Y.Z] | status | build/cores [--work-parent DIR --output DIR]\ncheck [--full --browser] | verify --xray PATH --work-parent DIR [--browser]\nsmoke --input DIR | integration --case CASE [--profiles NAME ... --stages NAME ...]\nvalidate --tag vX.Y.Z | assemble --input DIR --output DIR --tag TAG --commit SHA\npublish-ci --input DIR --repo OWNER/REPO --tag TAG --commit SHA\ndraft/publish --repo OWNER/REPO [--directory DIR] | release | push | ops -- ARGS"
            );
            Ok(())
        }
        _ => anyhow::bail!("未知操作：{}", args.action),
    }
}
