use std::{env, fs, io::ErrorKind, path::Path, process::Command};

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("Cargo 应提供项目根目录");
    let web_dir = Path::new(&manifest_dir).join("web");

    for path in [
        "web/components.json",
        "web/index.html",
        "web/package.json",
        "web/pnpm-lock.yaml",
        "web/tsconfig.app.json",
        "web/tsconfig.json",
        "web/tsconfig.node.json",
        "web/vite.config.ts",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    emit_source_changes(&web_dir.join("src"));
    emit_source_changes(&web_dir.join("public"));

    if !web_dir.join("node_modules").is_dir() {
        panic!(
            "构建内置管理端需要 web/node_modules；请先执行 `pnpm --dir web install --frozen-lockfile`"
        );
    }

    let pnpm = if cfg!(windows) { "pnpm.cmd" } else { "pnpm" };
    let status = match Command::new(pnpm)
        .args(["--dir", "web", "run", "build"])
        .current_dir(&manifest_dir)
        .status()
    {
        Ok(status) => status,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            panic!("构建内置管理端需要 pnpm；请安装 pnpm 后重试")
        }
        Err(error) => panic!("无法启动管理端构建命令 {pnpm}：{error}"),
    };
    if !status.success() {
        panic!("管理端构建失败，无法把静态资源编入动态插件")
    }
}

fn emit_source_changes(path: &Path) {
    if !path.exists() {
        return;
    }
    if path.is_file() {
        println!("cargo:rerun-if-changed={}", path.display());
        return;
    }
    let entries = fs::read_dir(path)
        .unwrap_or_else(|error| panic!("读取管理端源目录 {} 失败：{error}", path.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("读取管理端源文件失败：{error}"));
        emit_source_changes(&entry.path());
    }
}
