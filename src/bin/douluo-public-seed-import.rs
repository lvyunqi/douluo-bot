//! 独立的公共目录 SQLite 导入命令行入口。

#[allow(dead_code)]
#[path = "../content.rs"]
mod content;
#[path = "../public_seed.rs"]
mod public_seed;

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use public_seed::{
    PublicSeedPackageMetadata, import_public_seed_sqlite, write_public_seed_package_json,
};

const USAGE: &str = "用法:\n  douluo-public-seed-import --source <源SQLite> --output <内容包.json> --package-key <键> --revision <正整数> --author <作者> --minimum-runtime <版本>";

struct CliOptions {
    source: PathBuf,
    output: PathBuf,
    metadata: PublicSeedPackageMetadata,
}

fn main() -> ExitCode {
    match parse_arguments(env::args().skip(1)) {
        Ok(None) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Some(options)) => match run(options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("公共种子导入失败：{error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("参数错误：{error}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// 执行只读目录导出，生成后续内容发布可直接暂存的 JSON 文件。
fn run(options: CliOptions) -> Result<(), String> {
    let loaded = import_public_seed_sqlite(&options.source, &options.metadata)?;
    write_public_seed_package_json(&options.output, &loaded)?;
    println!("已生成公共种子内容包：{}", options.output.display());
    Ok(())
}

/// 解析固定参数，拒绝位置参数、重复参数和不完整的发布元数据。
fn parse_arguments(arguments: impl Iterator<Item = String>) -> Result<Option<CliOptions>, String> {
    let mut arguments = arguments.peekable();
    if arguments.peek().is_none() {
        return Ok(None);
    }

    let mut source = None;
    let mut output = None;
    let mut package_key = None;
    let mut revision = None;
    let mut author = None;
    let mut minimum_runtime = None;
    while let Some(argument) = arguments.next() {
        if matches!(argument.as_str(), "--help" | "-h") {
            return Ok(None);
        }
        let value = match argument.as_str() {
            "--source" => next_value(&mut arguments, "--source")?,
            "--output" => next_value(&mut arguments, "--output")?,
            "--package-key" => next_value(&mut arguments, "--package-key")?,
            "--revision" => next_value(&mut arguments, "--revision")?,
            "--author" => next_value(&mut arguments, "--author")?,
            "--minimum-runtime" => next_value(&mut arguments, "--minimum-runtime")?,
            _ => return Err(format!("不支持的参数：{argument}")),
        };
        match argument.as_str() {
            "--source" => assign_once(&mut source, value, "--source")?,
            "--output" => assign_once(&mut output, value, "--output")?,
            "--package-key" => assign_once(&mut package_key, value, "--package-key")?,
            "--revision" => assign_once(&mut revision, value, "--revision")?,
            "--author" => assign_once(&mut author, value, "--author")?,
            "--minimum-runtime" => assign_once(&mut minimum_runtime, value, "--minimum-runtime")?,
            _ => unreachable!("已在参数白名单外拒绝"),
        }
    }

    let revision = required(revision, "--revision")?
        .parse::<i64>()
        .map_err(|error| format!("--revision 必须是正整数：{error}"))?;
    if revision <= 0 {
        return Err("--revision 必须是正整数".to_string());
    }
    Ok(Some(CliOptions {
        source: PathBuf::from(required(source, "--source")?),
        output: PathBuf::from(required(output, "--output")?),
        metadata: PublicSeedPackageMetadata {
            package_key: required(package_key, "--package-key")?,
            revision,
            author: required(author, "--author")?,
            minimum_runtime: required(minimum_runtime, "--minimum-runtime")?,
        },
    }))
}

/// 读取紧随参数的值，避免把下一个参数静默当作路径或元数据。
fn next_value(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<String, String> {
    let value = arguments.next().ok_or_else(|| format!("{option} 缺少值"))?;
    if value.starts_with('-') {
        return Err(format!("{option} 缺少值"));
    }
    Ok(value)
}

/// 拒绝重复参数，避免调用者误以为后一个值会安全覆盖前一个值。
fn assign_once(slot: &mut Option<String>, value: String, option: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err(format!("{option} 不能重复提供"));
    }
    Ok(())
}

/// 读取必填参数并在缺失时给出固定错误。
fn required(value: Option<String>, option: &str) -> Result<String, String> {
    value.ok_or_else(|| format!("缺少必填参数：{option}"))
}
