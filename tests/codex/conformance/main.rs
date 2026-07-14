use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use codex_bridge_conformance::{RunOptions, run};

fn parse_options() -> Result<RunOptions, String> {
    let mut project_root = None;
    let mut trace_path = None;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--project-root" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--project-root requires a path".to_owned())?;
                project_root = Some(PathBuf::from(value));
            }
            "--trace" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--trace requires a path".to_owned())?;
                trace_path = Some(PathBuf::from(value));
            }
            "--help" | "-h" => {
                return Err(
                    "usage: codex-bridge-conformance --project-root PATH --trace PATH".to_owned(),
                );
            }
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }
    Ok(RunOptions {
        project_root: project_root.ok_or_else(|| "--project-root is required".to_owned())?,
        trace_path: trace_path.ok_or_else(|| "--trace is required".to_owned())?,
    })
}

fn main() -> ExitCode {
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    match run(&options) {
        Ok(summary) => {
            println!(
                "Sprint 1 conformance passed: {} cases; trace={}",
                summary.passed_cases,
                options.trace_path.display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Sprint 1 conformance failed: {error}");
            ExitCode::FAILURE
        }
    }
}
