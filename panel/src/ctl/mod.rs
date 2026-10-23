// shortcut: 安装入口尚未接入 CLI；接入时移除此测试构建限制。
#[cfg(test)]
pub mod install;
pub mod maintenance;
pub mod operations;
pub mod package;
pub mod time;
pub mod upgrade;
