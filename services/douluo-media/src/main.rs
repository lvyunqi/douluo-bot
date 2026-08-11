use std::{env, ffi::OsString, path::PathBuf};

#[derive(Debug, PartialEq, Eq)]
enum CatalogCommand {
    Publish { root: PathBuf, catalog: PathBuf },
    Verify { root: PathBuf, catalog: PathBuf },
}

#[tokio::main]
async fn main() {
    let result = match parse_catalog_command() {
        Ok(Some(CatalogCommand::Publish { root, catalog })) => {
            douluo_media::publish_catalog(root, catalog)
        }
        Ok(Some(CatalogCommand::Verify { root, catalog })) => {
            douluo_media::verify_catalog(root, catalog).map(|asset_count| {
                println!("douluo-media: catalog verified: {asset_count} assets");
            })
        }
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

/// 解析离线 catalog 命令；没有参数时保持原有服务启动方式。
fn parse_catalog_command() -> Result<Option<CatalogCommand>, String> {
    parse_catalog_command_from(env::args_os().skip(1))
}

fn parse_catalog_command_from(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<Option<CatalogCommand>, String> {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        return Ok(None);
    };
    let Some(subcommand) = arguments.next() else {
        return Err(usage());
    };
    if command.to_str() != Some("catalog") {
        return Err(usage());
    }
    let is_publish = match subcommand.to_str() {
        Some("publish") => true,
        Some("verify") => false,
        _ => return Err(usage()),
    };

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
    let command = if is_publish {
        CatalogCommand::Publish { root, catalog }
    } else {
        CatalogCommand::Verify { root, catalog }
    };
    Ok(Some(command))
}

fn usage() -> String {
    "使用方式: douluo-media catalog <publish|verify> --root <发布目录> [--catalog <catalog.sqlite>]"
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Option<CatalogCommand>, String> {
        parse_catalog_command_from(arguments.iter().map(|value| OsString::from(*value)))
    }

    #[test]
    fn parses_publish_and_verify_commands_without_writing() {
        assert_eq!(
            parse(&["catalog", "publish", "--root", "published"]),
            Ok(Some(CatalogCommand::Publish {
                root: PathBuf::from("published"),
                catalog: PathBuf::from("published").join("catalog.sqlite"),
            }))
        );
        assert_eq!(
            parse(&[
                "catalog",
                "verify",
                "--root",
                "published",
                "--catalog",
                "check.sqlite",
            ]),
            Ok(Some(CatalogCommand::Verify {
                root: PathBuf::from("published"),
                catalog: PathBuf::from("check.sqlite"),
            }))
        );
    }

    #[test]
    fn rejects_incomplete_or_duplicate_catalog_arguments() {
        assert!(parse(&["catalog", "verify"]).is_err());
        assert!(parse(&["catalog", "verify", "--root", "one", "--root", "two",]).is_err());
        assert!(parse(&["catalog", "delete", "--root", "published"]).is_err());
    }
}
