use std::path::{Path, PathBuf};

use codex_storage_spike::runner::{
    RunConfig, merge_platform_evidence, run, worker_fault, worker_open,
};
use codex_storage_spike::{BackendKind, FaultPoint};

fn main() {
    if let Err(error) = execute() {
        eprintln!("storage spike failed: {error}");
        std::process::exit(1);
    }
}

fn execute() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("run") => {
            validate_run_arguments(&arguments)?;
            for (name, value) in [
                ("--backend", option(&arguments, "--backend")),
                ("--dataset", option(&arguments, "--dataset")),
            ] {
                if value.is_some_and(|value| value != "all") {
                    return Err(format!("{name} only accepts 'all' for D-05 evidence").into());
                }
            }
            let repo_root = option(&arguments, "--repo-root")
                .map_or_else(std::env::current_dir, |value| Ok(PathBuf::from(value)))?;
            let output = option(&arguments, "--output")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    repo_root.join("tests/codex/evidence/sprint-3-storage-spike.json")
                });
            let quick = arguments.iter().any(|argument| argument == "--quick");
            let config = if quick {
                RunConfig::quick(repo_root, output)
            } else {
                RunConfig::decision(repo_root, output)
            };
            let evidence = run(&config)?;
            println!(
                "D-05={} backends={} output={}",
                evidence.chosen_backend,
                evidence.backends.len(),
                config.output.display()
            );
            Ok(())
        }
        Some("worker-fault") if arguments.len() == 4 => {
            worker_fault(
                backend(&arguments[1])?,
                Path::new(&arguments[2]),
                fault(&arguments[3])?,
            )?;
            Ok(())
        }
        Some("worker-open") if arguments.len() == 4 => {
            std::process::exit(worker_open(
                backend(&arguments[1])?,
                Path::new(&arguments[2]),
                &arguments[3],
            ));
        }
        Some("merge") if arguments.len() >= 3 => {
            let output = PathBuf::from(&arguments[1]);
            let inputs: Vec<_> = arguments[2..].iter().map(PathBuf::from).collect();
            let evidence = merge_platform_evidence(&output, &inputs)?;
            println!(
                "D-05={} cross_platform_complete={} output={}",
                evidence.chosen_backend,
                evidence.cross_platform_complete,
                output.display()
            );
            Ok(())
        }
        _ => Err(
            "usage: codex-storage-spike run --backend all --dataset all --repo-root <path> --output <json> [--quick]".into(),
        ),
    }
}

fn validate_run_arguments(arguments: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--quick" => index += 1,
            "--backend" | "--dataset" | "--repo-root" | "--output" => {
                if index + 1 >= arguments.len() {
                    return Err(format!("{} requires a value", arguments[index]).into());
                }
                index += 2;
            }
            unknown => return Err(format!("unknown run argument {unknown}").into()),
        }
    }
    Ok(())
}

fn option<'a>(arguments: &'a [String], name: &str) -> Option<&'a str> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
}

fn backend(value: &str) -> Result<BackendKind, Box<dyn std::error::Error>> {
    match value {
        "sqlite" => Ok(BackendKind::Sqlite),
        "segment" => Ok(BackendKind::Segment),
        _ => Err(format!("unknown backend {value}").into()),
    }
}

fn fault(value: &str) -> Result<FaultPoint, Box<dyn std::error::Error>> {
    match value {
        "capture" => Ok(FaultPoint::Capture),
        "staging" => Ok(FaultPoint::Staging),
        "pre_commit" => Ok(FaultPoint::PreCommit),
        "post_commit" => Ok(FaultPoint::PostCommit),
        _ => Err(format!("unknown fault point {value}").into()),
    }
}
