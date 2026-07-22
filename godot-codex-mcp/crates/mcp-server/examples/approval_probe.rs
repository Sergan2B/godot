#[allow(dead_code)]
#[path = "../tests/support/mod.rs"]
mod support;

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use rmcp::ServiceExt;
use support::ApprovalProbeServer;

const DEFAULT_ELICITATION_TIMEOUT: Duration = Duration::from_secs(120);

fn parse_args(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<(PathBuf, Duration), String> {
    let mut arguments = arguments.into_iter();
    let mut output = None;
    let mut timeout = DEFAULT_ELICITATION_TIMEOUT;
    while let Some(argument) = arguments.next() {
        if argument == "--output" {
            if output.is_some() {
                return Err("--output may be specified only once".to_owned());
            }
            output = Some(PathBuf::from(
                arguments
                    .next()
                    .ok_or_else(|| "--output requires a path".to_owned())?,
            ));
        } else if argument == "--timeout" {
            let value = arguments
                .next()
                .ok_or_else(|| "--timeout requires seconds".to_owned())?;
            let seconds = value
                .to_string_lossy()
                .parse::<u64>()
                .map_err(|_| "--timeout must be an integer number of seconds".to_owned())?;
            if !(1..=120).contains(&seconds) {
                return Err("--timeout must be between 1 and 120 seconds".to_owned());
            }
            timeout = Duration::from_secs(seconds);
        } else {
            return Err(format!("unknown argument: {}", argument.to_string_lossy()));
        }
    }
    let output = output.ok_or_else(|| {
        "usage: approval_probe --output <new-json-path> [--timeout <1..120>]".to_owned()
    })?;
    if !output.is_absolute() {
        return Err("--output must be an absolute path".to_owned());
    }
    Ok((output, timeout))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (output, timeout) =
        parse_args(std::env::args_os().skip(1)).map_err(std::io::Error::other)?;
    ApprovalProbeServer::new(timeout, Some(output))
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_probe_requires_absolute_new_output() {
        assert!(parse_args(Vec::new()).is_err());
        assert!(
            parse_args([OsString::from("--output"), OsString::from("relative.json"),]).is_err()
        );
        assert!(
            parse_args([
                OsString::from("--output"),
                OsString::from("/tmp/s9-approval-probe.json"),
                OsString::from("--timeout"),
                OsString::from("121"),
            ])
            .is_err()
        );
    }
}
