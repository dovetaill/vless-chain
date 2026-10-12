use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=CHAIN_WEB_ASSETS");
    let assets = env::var_os("CHAIN_WEB_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("frontend/dist")
        });
    println!("cargo:rerun-if-changed={}", assets.display());
    // Agent-only builds need no frontend. The panel checks for index.html on startup.
    let assets = if assets.join("index.html").is_file() {
        assets.canonicalize().expect("无法读取前端构建目录")
    } else {
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("empty-web")
    };
    println!("cargo:rerun-if-changed={}", assets.display());
    println!("cargo:rustc-env=CHAIN_EMBED_WEB={}", assets.display());
}
