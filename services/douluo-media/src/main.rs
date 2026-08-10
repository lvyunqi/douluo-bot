use std::{env, path::PathBuf};

#[tokio::main]
async fn main() {
    let result = match parse_catalog_publish_command() {
        Ok(Some((root, catalog))) => douluo_media::publish_catalog(root, catalog),
        Ok(None) => douluo_media::run_from_env().await,
        Err(message) => {
            eprintln!("douluo-media: {message}");
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        eprintln!("douluo-media: {error}");
        std::process::exit(1);
    }
}

/// 解析发布阶段的 catalog 命令；没有参数时保持原有服务启动方式。
fn parse_catalog_publish_command() -> Result<Option<(PathBuf, PathBuf)>, String> {
    let mut arguments = env::args_os().skip(1);
    let Some(command) = arguments.next() else {
        return Ok(None);
    };
    let Some(subcommand) = arguments.next() else {
        return Err(usage());
    };
    if command.to_str() != Some("catalog") || subcommand.to_str() != Some("publish") {
        return Err(usage());
    }

    let mut root = None;
    let mut catalog = None;
    while let Some(flag) = arguments.next() {
        match flag.to_str() {
            Some("--root") => {
                let value = arguments.next().ok_or_else(usage)?;
                if root.replace(PathBuf::from(value)).is_some() {
                    return Err(usage());
                }
            }
            Some("--catalog") => {
                let value = arguments.next().ok_or_else(usage)?;
                if catalog.replace(PathBuf::from(value)).is_some() {
                    return Err(usage());
                }
            }
            _ => return Err(usage()),
        }
    }
    let root = root.ok_or_else(usage)?;
    let catalog = catalog.unwrap_or_else(|| root.join("catalog.sqlite"));
    Ok(Some((root, catalog)))
}

fn usage() -> String {
    "使用方式: douluo-media catalog publish --root <发布目录> [--catalog <catalog.sqlite>]"
        .to_owned()
}
