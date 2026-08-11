//! 独立的最近 SQLite 基础角色资料 staging 命令行入口。

#[allow(dead_code)]
#[path = "../player_staging.rs"]
mod player_staging;

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use player_staging::{PlayerStagingMetadata, stage_recent_sqlite_player_profiles};

const USAGE: &str = "用法:\n  douluo-player-stage --source <最近SQLite> --output <staging.sqlite> --protocol onebot11 --account-id <稳定账号> --namespace <命名空间>";

struct CliOptions {
    source: PathBuf,
    output: PathBuf,
    metadata: PlayerStagingMetadata,
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
                eprintln!("玩家 staging 失败：{error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("参数错误：{error}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// 执行只读角色资料 staging，并只输出不含原始资料的批次摘要。
fn run(options: CliOptions) -> Result<(), String> {
    let summary =
        stage_recent_sqlite_player_profiles(&options.source, &options.output, &options.metadata)?;
    println!("已创建玩家 staging：{}", options.output.display());
    println!(
        "总记录：{}；可确认：{}；拒绝：{}",
        summary.total_players, summary.ready_players, summary.rejected_players
    );
    if !summary.issue_counts.is_empty() {
        let issues = summary
            .issue_counts
            .iter()
            .map(|(code, count)| format!("{code}={count}"))
            .collect::<Vec<_>>()
            .join("，");
        println!("校验问题：{issues}");
    }
    Ok(())
}

/// 解析固定参数，拒绝位置参数、重复参数和未显式指定的身份作用域。
fn parse_arguments(arguments: impl Iterator<Item = String>) -> Result<Option<CliOptions>, String> {
    let mut arguments = arguments.peekable();
    if arguments.peek().is_none() {
        return Ok(None);
    }

    let mut source = None;
    let mut output = None;
    let mut protocol = None;
    let mut account_id = None;
    let mut namespace = None;
    while let Some(argument) = arguments.next() {
        if matches!(argument.as_str(), "--help" | "-h") {
            return Ok(None);
        }
        let value = match argument.as_str() {
            "--source" => next_value(&mut arguments, "--source")?,
            "--output" => next_value(&mut arguments, "--output")?,
            "--protocol" => next_value(&mut arguments, "--protocol")?,
            "--account-id" => next_value(&mut arguments, "--account-id")?,
            "--namespace" => next_value(&mut arguments, "--namespace")?,
            _ => return Err(format!("不支持的参数：{argument}")),
        };
        match argument.as_str() {
            "--source" => assign_once(&mut source, value, "--source")?,
            "--output" => assign_once(&mut output, value, "--output")?,
            "--protocol" => assign_once(&mut protocol, value, "--protocol")?,
            "--account-id" => assign_once(&mut account_id, value, "--account-id")?,
            "--namespace" => assign_once(&mut namespace, value, "--namespace")?,
            _ => unreachable!("已在参数白名单外拒绝"),
        }
    }

    Ok(Some(CliOptions {
        source: PathBuf::from(required(source, "--source")?),
        output: PathBuf::from(required(output, "--output")?),
        metadata: PlayerStagingMetadata {
            protocol: required(protocol, "--protocol")?,
            account_id: required(account_id, "--account-id")?,
            namespace: required(namespace, "--namespace")?,
        },
    }))
}

/// 读取紧随参数的值，避免下一个选项被误作路径或身份元数据。
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

/// 拒绝重复参数，避免后一个身份作用域静默覆盖前一个值。
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
