use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;

use godot_codex_operations::{
    DoctorOptions, GuidanceMode, PRODUCT_VERSION, PendingSetup, RemoveOptions, RepairOptions,
    SetupOptions, SetupPreview, SetupProfile, SurfaceSelection, apply_setup_plan, prepare_setup,
    prepare_setup_for_consent, prepare_setup_remove, prepare_setup_remove_for_consent,
    prepare_setup_repair, prepare_setup_repair_for_consent, run_doctor,
};

const USAGE: &str = "\
usage:
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" version
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" doctor --project-root <path> [--surface auto|app|cli|ide] [--require-editor] [--json] [--show-paths]
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" setup --project-root <path> --profile read-only|full-beta [--guidance none|agents|skill|all] [--dry-run] [--json]
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" setup --repair --project-root <path> [--dry-run] [--json]
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" setup --apply-plan sha256:<digest> [--json]
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" setup --remove --project-root <path> [--dry-run] [--json]";

const DOCTOR_USAGE: &str = "\
usage:
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" doctor --project-root <path> [--surface auto|app|cli|ide] [--require-editor] [--json] [--show-paths]

`project_config_invalid` with remediation `repair_project_config` can be
previewed using setup --repair. Doctor itself is read-only.";

const SETUP_USAGE: &str = "\
usage:
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" setup --project-root <path> --profile read-only|full-beta [--guidance none|agents|skill|all] [--dry-run] [--json]
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" setup --repair --project-root <path> [--dry-run] [--json]
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" setup --apply-plan sha256:<digest> [--json]
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" setup --remove --project-root <path> [--dry-run] [--json]

Setup, repair, and removal first emit an immutable, expiring, digest-bound preview.
Without --dry-run they still require an interactive yes; the default is no.
For non-interactive use, inspect --dry-run --json and apply only its exact
digest with --apply-plan.

Repair derives profile and guidance from a valid private setup receipt. It may
replace only a missing or drifted receipt-owned godot_editor stanza in an
otherwise valid TOML file. It preserves unrelated TOML/comments and file mode;
malformed, unowned, unsafe, or oversized inputs fail closed.";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help(&'static str),
    Version,
    Doctor {
        options: DoctorOptions,
        json: bool,
    },
    SetupPreview {
        options: SetupOptions,
        dry_run: bool,
        json: bool,
    },
    SetupRepairPreview {
        options: RepairOptions,
        dry_run: bool,
        json: bool,
    },
    SetupApply {
        digest: String,
        json: bool,
    },
    SetupRemovePreview {
        options: RemoveOptions,
        dry_run: bool,
        json: bool,
    },
}

fn parse_command(arguments: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut arguments = arguments.into_iter().collect::<VecDeque<_>>();
    let Some(command) = arguments.pop_front() else {
        return Err(USAGE.to_owned());
    };
    match command.to_str() {
        Some("help" | "--help" | "-h") => {
            if arguments.is_empty() {
                Ok(Command::Help(USAGE))
            } else {
                Err("help cannot be combined with other arguments".to_owned())
            }
        }
        Some("version" | "--version" | "-V") => {
            if arguments.is_empty() {
                Ok(Command::Version)
            } else {
                Err("version cannot be combined with other arguments".to_owned())
            }
        }
        Some("doctor") => parse_doctor(arguments),
        Some("setup") => parse_setup(arguments),
        _ => Err(USAGE.to_owned()),
    }
}

fn parse_doctor(mut arguments: VecDeque<OsString>) -> Result<Command, String> {
    if matches!(
        arguments.front().and_then(|argument| argument.to_str()),
        Some("help" | "--help" | "-h")
    ) {
        return if arguments.len() == 1 {
            Ok(Command::Help(DOCTOR_USAGE))
        } else {
            Err("doctor help cannot be combined with other arguments".to_owned())
        };
    }
    let mut project_root = None;
    let mut surface = SurfaceSelection::Auto;
    let mut surface_seen = false;
    let mut require_editor = false;
    let mut json = false;
    let mut show_paths = false;
    while let Some(argument) = arguments.pop_front() {
        match argument.to_str() {
            Some("--project-root") => {
                set_once(
                    &mut project_root,
                    PathBuf::from(required_value(&mut arguments, "--project-root")?),
                    "--project-root",
                )?;
            }
            Some("--surface") if !surface_seen => {
                surface_seen = true;
                let value = required_utf8(&mut arguments, "--surface")?;
                surface = match value.as_str() {
                    "auto" => SurfaceSelection::Auto,
                    "app" => SurfaceSelection::App,
                    "cli" => SurfaceSelection::Cli,
                    "ide" => SurfaceSelection::Ide,
                    _ => return Err("--surface must be auto, app, cli, or ide".to_owned()),
                };
            }
            Some("--require-editor") if !require_editor => require_editor = true,
            Some("--json") if !json => json = true,
            Some("--show-paths") if !show_paths => show_paths = true,
            _ => return Err("unknown or duplicate doctor argument".to_owned()),
        }
    }
    let mut options =
        DoctorOptions::new(project_root.ok_or_else(|| "--project-root is required".to_owned())?);
    options.surface = surface;
    options.require_editor = require_editor;
    options.show_paths = show_paths;
    Ok(Command::Doctor { options, json })
}

fn parse_setup(mut arguments: VecDeque<OsString>) -> Result<Command, String> {
    if matches!(
        arguments.front().and_then(|argument| argument.to_str()),
        Some("help" | "--help" | "-h")
    ) {
        return if arguments.len() == 1 {
            Ok(Command::Help(SETUP_USAGE))
        } else {
            Err("setup help cannot be combined with other arguments".to_owned())
        };
    }
    let mut project_root = None;
    let mut profile = None;
    let mut guidance = GuidanceMode::None;
    let mut guidance_seen = false;
    let mut dry_run = false;
    let mut json = false;
    let mut apply_plan = None;
    let mut remove = false;
    let mut repair = false;
    while let Some(argument) = arguments.pop_front() {
        match argument.to_str() {
            Some("--project-root") => {
                set_once(
                    &mut project_root,
                    PathBuf::from(required_value(&mut arguments, "--project-root")?),
                    "--project-root",
                )?;
            }
            Some("--profile") => {
                let parsed = match required_utf8(&mut arguments, "--profile")?.as_str() {
                    "read-only" => SetupProfile::ReadOnly,
                    "full-beta" => SetupProfile::FullBeta,
                    _ => return Err("--profile must be read-only or full-beta".to_owned()),
                };
                set_once(&mut profile, parsed, "--profile")?;
            }
            Some("--guidance") if !guidance_seen => {
                guidance_seen = true;
                guidance = match required_utf8(&mut arguments, "--guidance")?.as_str() {
                    "none" => GuidanceMode::None,
                    "agents" => GuidanceMode::Agents,
                    "skill" => GuidanceMode::Skill,
                    "all" => GuidanceMode::All,
                    _ => {
                        return Err("--guidance must be none, agents, skill, or all".to_owned());
                    }
                };
            }
            Some("--dry-run") if !dry_run => dry_run = true,
            Some("--json") if !json => json = true,
            Some("--apply-plan") => {
                set_once(
                    &mut apply_plan,
                    required_utf8(&mut arguments, "--apply-plan")?,
                    "--apply-plan",
                )?;
            }
            Some("--remove") if !remove => remove = true,
            Some("--repair") if !repair => repair = true,
            _ => return Err("unknown or duplicate setup argument".to_owned()),
        }
    }

    if let Some(digest) = apply_plan {
        if project_root.is_some()
            || profile.is_some()
            || guidance_seen
            || dry_run
            || remove
            || repair
        {
            return Err("--apply-plan cannot be combined with planning/removal options".to_owned());
        }
        return Ok(Command::SetupApply { digest, json });
    }
    if remove {
        if profile.is_some() || guidance_seen || repair {
            return Err("--remove cannot be combined with profile/guidance/repair".to_owned());
        }
        return Ok(Command::SetupRemovePreview {
            options: RemoveOptions::new(
                project_root
                    .ok_or_else(|| "--project-root is required with --remove".to_owned())?,
            ),
            dry_run,
            json,
        });
    }
    if repair {
        if profile.is_some() || guidance_seen {
            return Err("--repair derives profile/guidance from the setup receipt".to_owned());
        }
        return Ok(Command::SetupRepairPreview {
            options: RepairOptions::new(
                project_root
                    .ok_or_else(|| "--project-root is required with --repair".to_owned())?,
            ),
            dry_run,
            json,
        });
    }
    let mut options = SetupOptions::new(
        project_root.ok_or_else(|| "--project-root is required".to_owned())?,
        profile.ok_or_else(|| "--profile is required".to_owned())?,
    );
    options.guidance = guidance;
    Ok(Command::SetupPreview {
        options,
        dry_run,
        json,
    })
}

fn set_once<T>(slot: &mut Option<T>, value: T, name: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        Err(format!("{name} may be specified only once"))
    } else {
        Ok(())
    }
}

fn required_value(arguments: &mut VecDeque<OsString>, name: &str) -> Result<OsString, String> {
    arguments
        .pop_front()
        .ok_or_else(|| format!("{name} requires a value"))
}

fn required_utf8(arguments: &mut VecDeque<OsString>, name: &str) -> Result<String, String> {
    arguments
        .pop_front()
        .ok_or_else(|| format!("{name} requires a value"))?
        .into_string()
        .map_err(|_| format!("{name} requires a UTF-8 value"))
}

fn run(
    command: Command,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    error: &mut dyn Write,
    interactive: bool,
) -> i32 {
    match command {
        Command::Help(usage) => {
            let _ = writeln!(output, "{usage}");
            0
        }
        Command::Version => {
            let _ = writeln!(output, "godot-codex {PRODUCT_VERSION}");
            0
        }
        Command::Doctor { options, json } => {
            let report = run_doctor(&options);
            if json {
                let _ = serde_json::to_writer_pretty(&mut *output, &report);
                let _ = writeln!(output);
            } else {
                let _ = writeln!(
                    output,
                    "Godot Codex doctor: {:?} (exit {})",
                    report.status, report.exit_code
                );
                for check in &report.checks {
                    let _ = writeln!(
                        output,
                        "- {:?} {}: {}",
                        check.status, check.code, check.summary
                    );
                    if let Some(remediation_id) = check
                        .remediation_id
                        .and_then(|id| serde_json::to_value(id).ok())
                        .and_then(|value| value.as_str().map(str::to_owned))
                    {
                        let _ = writeln!(
                            output,
                            "  remediation: {remediation_id}; retryable: {}",
                            check.retryable
                        );
                        if remediation_id == "repair_project_config" {
                            let _ = writeln!(
                                output,
                                "  preview: \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" setup --repair --project-root <path> --dry-run"
                            );
                        }
                    }
                }
                if let Some(path) = &report.project_root {
                    let _ = writeln!(output, "Project: {path}");
                }
            }
            i32::from(report.exit_code)
        }
        Command::SetupPreview {
            options,
            dry_run,
            json,
        } => {
            if dry_run {
                run_dry_run_preview(prepare_setup(&options), json, output, error)
            } else {
                run_setup_consent(
                    prepare_setup_for_consent(&options),
                    json,
                    input,
                    output,
                    error,
                    interactive,
                )
            }
        }
        Command::SetupRepairPreview {
            options,
            dry_run,
            json,
        } => {
            if dry_run {
                run_dry_run_preview(prepare_setup_repair(&options), json, output, error)
            } else {
                run_setup_consent(
                    prepare_setup_repair_for_consent(&options),
                    json,
                    input,
                    output,
                    error,
                    interactive,
                )
            }
        }
        Command::SetupApply { digest, json } => match apply_setup_plan(&digest, None) {
            Ok(report) => {
                if json {
                    let _ = serde_json::to_writer_pretty(&mut *output, &report);
                    let _ = writeln!(output);
                } else {
                    let _ = writeln!(
                        output,
                        "Applied {} to {} project files; restart the Codex surface.",
                        report.plan_digest,
                        report.changed_paths.len()
                    );
                }
                0
            }
            Err(setup_error) => {
                render_error(error, json, &setup_error.to_string());
                2
            }
        },
        Command::SetupRemovePreview {
            options,
            dry_run,
            json,
        } => {
            if dry_run {
                run_dry_run_preview(prepare_setup_remove(&options), json, output, error)
            } else {
                run_setup_consent(
                    prepare_setup_remove_for_consent(&options),
                    json,
                    input,
                    output,
                    error,
                    interactive,
                )
            }
        }
    }
}

fn run_dry_run_preview(
    preview: Result<SetupPreview, godot_codex_operations::SetupError>,
    json: bool,
    output: &mut dyn Write,
    error: &mut dyn Write,
) -> i32 {
    let preview = match preview {
        Ok(preview) => preview,
        Err(setup_error) => {
            render_error(error, json, &setup_error.to_string());
            return 2;
        }
    };
    render_preview(output, json, &preview);
    0
}

#[allow(clippy::too_many_arguments)]
fn run_setup_consent(
    pending: Result<PendingSetup, godot_codex_operations::SetupError>,
    json: bool,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    error: &mut dyn Write,
    interactive: bool,
) -> i32 {
    let pending = match pending {
        Ok(pending) => pending,
        Err(setup_error) => {
            render_error(error, json, &setup_error.to_string());
            return 2;
        }
    };
    render_preview(output, json, pending.preview());
    if json || !interactive {
        render_error(
            error,
            json,
            "consent_required: rerun with --dry-run, then use --apply-plan with the exact digest",
        );
        return 2;
    }
    let _ = write!(
        error,
        "Apply this exact {} plan? [y/N] ",
        pending.preview().mode
    );
    let _ = error.flush();
    if !read_confirmation(input) {
        let _ = writeln!(
            error,
            "{} declined; no project files were changed.",
            pending.preview().mode
        );
        return 2;
    }
    match pending.apply() {
        Ok(report) => {
            if json {
                let _ = serde_json::to_writer_pretty(&mut *output, &report);
                let _ = writeln!(output);
            } else {
                let _ = writeln!(
                    output,
                    "{} {} changed {} project files; restart the Codex surface.",
                    report.mode,
                    report.plan_digest,
                    report.changed_paths.len()
                );
            }
            0
        }
        Err(setup_error) => {
            render_error(error, json, &setup_error.to_string());
            2
        }
    }
}

fn read_confirmation(input: &mut dyn BufRead) -> bool {
    let mut line = String::new();
    match (&mut *input).take(32).read_line(&mut line) {
        Ok(_) => matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
        Err(_) => false,
    }
}

fn render_preview(output: &mut dyn Write, json: bool, preview: &SetupPreview) {
    if json {
        let _ = serde_json::to_writer_pretty(&mut *output, preview);
        let _ = writeln!(output);
        return;
    }
    let _ = writeln!(
        output,
        "{} plan {} for package {} (expires in {} seconds)",
        preview.mode, preview.plan_digest, preview.package_version, preview.expires_in_seconds
    );
    let _ = writeln!(
        output,
        "Profile: {}; guidance: {:?}; project trust is unchanged.",
        preview.profile, preview.guidance
    );
    for change in &preview.changes {
        let _ = writeln!(output, "\n{:?} {}", change.action, change.path);
        let _ = write!(output, "{}", change.diff);
    }
}

fn render_error(error: &mut dyn Write, json: bool, message: &str) {
    if json {
        let value = serde_json::json!({
            "schema_version": "godot-codex-operation-error/1.0",
            "status": "error",
            "code": message.split(':').next().unwrap_or("operation_failed"),
        });
        let _ = serde_json::to_writer(&mut *error, &value);
        let _ = writeln!(error);
    } else {
        let _ = writeln!(error, "godot-codex: {message}");
    }
}

fn main() {
    let command = match parse_command(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(64);
        }
    };
    let interactive = io::stdin().is_terminal();
    let exit_code = run(
        command,
        &mut io::stdin().lock(),
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        interactive,
    );
    std::process::exit(exit_code);
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::Path;

    use tempfile::TempDir;

    use super::*;

    fn rendered_plan_digest(output: &str) -> String {
        output
            .split_whitespace()
            .find(|value| {
                value.len() == 71
                    && value.starts_with("sha256:")
                    && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            .unwrap()
            .to_owned()
    }

    fn setup_options(project: &TempDir, plans: &TempDir, package: &TempDir) -> SetupOptions {
        std::fs::create_dir(package.path().join("bin")).unwrap();
        std::fs::write(
            package.path().join("package-manifest.json"),
            format!(r#"{{"package_version":"{PRODUCT_VERSION}"}}"#),
        )
        .unwrap();
        let sidecar = package.path().join("bin/godot-codex-mcp");
        std::fs::write(&sidecar, b"test-sidecar").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut options = SetupOptions::new(project.path(), SetupProfile::ReadOnly);
        options.plan_store = Some(plans.path().join("plans"));
        options.package_directory = Some(package.path().to_path_buf());
        options
    }

    fn remove_options(setup: &SetupOptions) -> RemoveOptions {
        RemoveOptions {
            project_root: setup.project_root.clone(),
            plan_store: setup.plan_store.clone(),
            package_directory: setup.package_directory.clone(),
        }
    }

    fn install_setup(project: &TempDir, plans: &TempDir, package: &TempDir) -> SetupOptions {
        std::fs::write(project.path().join("project.godot"), "[application]\n").unwrap();
        let options = setup_options(project, plans, package);
        let preview = prepare_setup(&options).unwrap();
        apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()).unwrap();
        options
    }

    fn managed_bytes(project: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        [
            ".codex/config.toml",
            "AGENTS.md",
            ".agents/skills/godot-editor/SKILL.md",
            ".godot/codex/setup-receipt-v1.json",
        ]
        .into_iter()
        .map(|relative| {
            let path = project.join(relative);
            let bytes = std::fs::read(&path).ok();
            (path, bytes)
        })
        .collect()
    }

    #[test]
    fn command_surface_is_closed_and_required_values_are_explicit() {
        assert_eq!(
            parse_command([OsString::from("--help")]),
            Ok(Command::Help(USAGE))
        );
        assert_eq!(
            parse_command([OsString::from("doctor"), OsString::from("--help")]),
            Ok(Command::Help(DOCTOR_USAGE))
        );
        assert_eq!(
            parse_command([OsString::from("setup"), OsString::from("help")]),
            Ok(Command::Help(SETUP_USAGE))
        );
        assert_eq!(
            parse_command([OsString::from("version")]),
            Ok(Command::Version)
        );
        assert!(parse_command([OsString::from("doctor")]).is_err());
        assert!(
            parse_command([
                OsString::from("setup"),
                OsString::from("--project-root"),
                OsString::from("."),
            ])
            .is_err()
        );
        assert!(matches!(
            parse_command([
                OsString::from("setup"),
                OsString::from("--repair"),
                OsString::from("--project-root"),
                OsString::from("."),
                OsString::from("--dry-run"),
                OsString::from("--json"),
            ]),
            Ok(Command::SetupRepairPreview {
                dry_run: true,
                json: true,
                ..
            })
        ));
        assert!(
            parse_command([
                OsString::from("setup"),
                OsString::from("--repair"),
                OsString::from("--project-root"),
                OsString::from("."),
                OsString::from("--profile"),
                OsString::from("read-only"),
            ])
            .is_err()
        );
        assert!(parse_command([OsString::from("setup"), OsString::from("--repair")]).is_err());
        assert!(matches!(
            parse_command([
                OsString::from("setup"),
                OsString::from("--remove"),
                OsString::from("--project-root"),
                OsString::from("."),
                OsString::from("--dry-run"),
                OsString::from("--json"),
            ]),
            Ok(Command::SetupRemovePreview {
                dry_run: true,
                json: true,
                ..
            })
        ));
        assert!(
            parse_command([
                OsString::from("setup"),
                OsString::from("--apply-plan"),
                OsString::from(format!("sha256:{}", "a".repeat(64))),
                OsString::from("--profile"),
                OsString::from("read-only"),
            ])
            .is_err()
        );
    }

    #[test]
    fn doctor_parser_accepts_every_frozen_surface() {
        for surface in ["auto", "app", "cli", "ide"] {
            let command = parse_command([
                OsString::from("doctor"),
                OsString::from("--project-root"),
                OsString::from("."),
                OsString::from("--surface"),
                OsString::from(surface),
                OsString::from("--require-editor"),
                OsString::from("--json"),
                OsString::from("--show-paths"),
            ])
            .unwrap();
            assert!(matches!(command, Command::Doctor { .. }));
        }
    }

    #[test]
    fn confirmation_is_default_no_for_decline_eof_and_long_input() {
        assert!(!read_confirmation(&mut Cursor::new(b"\n")));
        assert!(!read_confirmation(&mut Cursor::new(b"n\n")));
        assert!(!read_confirmation(&mut Cursor::new(Vec::<u8>::new())));
        assert!(!read_confirmation(&mut Cursor::new(vec![b'y'; 64])));
        assert!(read_confirmation(&mut Cursor::new(b"yes\n")));
    }

    #[test]
    fn non_tty_setup_prints_plan_but_never_mutates_project() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("project.godot"), "[application]\n").unwrap();
        let plans = TempDir::new().unwrap();
        let package = TempDir::new().unwrap();
        std::fs::create_dir(package.path().join("bin")).unwrap();
        std::fs::write(
            package.path().join("package-manifest.json"),
            format!(r#"{{"package_version":"{PRODUCT_VERSION}"}}"#),
        )
        .unwrap();
        let sidecar = package.path().join("bin/godot-codex-mcp");
        std::fs::write(&sidecar, b"test-sidecar").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let plan_store = plans.path().join("plans");
        let mut options = SetupOptions::new(project.path(), SetupProfile::ReadOnly);
        options.plan_store = Some(plan_store.clone());
        options.package_directory = Some(package.path().to_path_buf());
        let command = Command::SetupPreview {
            options,
            dry_run: false,
            json: false,
        };
        let mut output = Vec::new();
        let mut error = Vec::new();
        let exit = run(
            command,
            &mut Cursor::new(Vec::<u8>::new()),
            &mut output,
            &mut error,
            false,
        );
        assert_eq!(exit, 2);
        let output = String::from_utf8(output).unwrap();
        let error = String::from_utf8(error).unwrap();
        assert!(
            output.contains("configure plan sha256:"),
            "setup preview was not rendered; stdout={output:?}, stderr={error:?}"
        );
        assert!(error.contains("consent_required"));
        let digest = rendered_plan_digest(&output);
        assert_eq!(
            apply_setup_plan(&digest, Some(&plan_store)),
            Err(godot_codex_operations::SetupError::PlanNotFound)
        );
        assert!(!project.path().join(".codex/config.toml").exists());
        assert!(
            !project
                .path()
                .join(".godot/codex/setup-receipt-v1.json")
                .exists()
        );
    }

    #[test]
    fn declined_or_eof_interactive_preview_never_publishes_apply_authority() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("project.godot"), "[application]\n").unwrap();
        let plans = TempDir::new().unwrap();
        let package = TempDir::new().unwrap();
        std::fs::create_dir(package.path().join("bin")).unwrap();
        std::fs::write(
            package.path().join("package-manifest.json"),
            format!(r#"{{"package_version":"{PRODUCT_VERSION}"}}"#),
        )
        .unwrap();
        let sidecar = package.path().join("bin/godot-codex-mcp");
        std::fs::write(&sidecar, b"test-sidecar").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let plan_store = plans.path().join("plans");
        let mut options = SetupOptions::new(project.path(), SetupProfile::ReadOnly);
        options.plan_store = Some(plan_store.clone());
        options.package_directory = Some(package.path().to_path_buf());

        for answer in [b"n\n".as_slice(), b"".as_slice()] {
            let mut output = Vec::new();
            let mut error = Vec::new();
            let exit = run(
                Command::SetupPreview {
                    options: options.clone(),
                    dry_run: false,
                    json: false,
                },
                &mut Cursor::new(answer),
                &mut output,
                &mut error,
                true,
            );
            assert_eq!(exit, 2);
            let output = String::from_utf8(output).unwrap();
            let digest = rendered_plan_digest(&output);
            assert_eq!(
                apply_setup_plan(&digest, Some(&plan_store)),
                Err(godot_codex_operations::SetupError::PlanNotFound)
            );
            assert!(!project.path().join(".codex/config.toml").exists());
            assert!(
                !project
                    .path()
                    .join(".godot/codex/setup-receipt-v1.json")
                    .exists()
            );
        }
    }

    #[test]
    fn direct_remove_on_non_tty_previews_but_never_mutates_or_publishes_authority() {
        let project = TempDir::new().unwrap();
        let plans = TempDir::new().unwrap();
        let package = TempDir::new().unwrap();
        let setup = install_setup(&project, &plans, &package);
        let before = managed_bytes(project.path());
        let plan_store = setup.plan_store.clone().unwrap();
        let mut output = Vec::new();
        let mut error = Vec::new();
        let exit = run(
            Command::SetupRemovePreview {
                options: remove_options(&setup),
                dry_run: false,
                json: false,
            },
            &mut Cursor::new(Vec::<u8>::new()),
            &mut output,
            &mut error,
            false,
        );
        assert_eq!(exit, 2);
        let output = String::from_utf8(output).unwrap();
        let error = String::from_utf8(error).unwrap();
        assert!(output.contains("remove plan sha256:"));
        assert!(error.contains("consent_required"));
        let digest = rendered_plan_digest(&output);
        assert_eq!(
            apply_setup_plan(&digest, Some(&plan_store)),
            Err(godot_codex_operations::SetupError::PlanNotFound)
        );
        for (path, expected) in before {
            assert_eq!(std::fs::read(path).ok(), expected);
        }
    }

    #[test]
    fn declined_or_eof_remove_never_mutates_or_publishes_authority() {
        for answer in [b"n\n".as_slice(), b"".as_slice()] {
            let project = TempDir::new().unwrap();
            let plans = TempDir::new().unwrap();
            let package = TempDir::new().unwrap();
            let setup = install_setup(&project, &plans, &package);
            let before = managed_bytes(project.path());
            let plan_store = setup.plan_store.clone().unwrap();
            let mut output = Vec::new();
            let mut error = Vec::new();
            let exit = run(
                Command::SetupRemovePreview {
                    options: remove_options(&setup),
                    dry_run: false,
                    json: false,
                },
                &mut Cursor::new(answer),
                &mut output,
                &mut error,
                true,
            );
            assert_eq!(exit, 2);
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains("remove plan sha256:"));
            let digest = rendered_plan_digest(&output);
            assert_eq!(
                apply_setup_plan(&digest, Some(&plan_store)),
                Err(godot_codex_operations::SetupError::PlanNotFound)
            );
            for (path, expected) in before {
                assert_eq!(std::fs::read(path).ok(), expected);
            }
        }
    }

    #[test]
    fn non_tty_repair_prints_redacted_plan_but_never_mutates_project() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("project.godot"), "[application]\n").unwrap();
        let plans = TempDir::new().unwrap();
        let package = TempDir::new().unwrap();
        std::fs::create_dir(package.path().join("bin")).unwrap();
        std::fs::write(
            package.path().join("package-manifest.json"),
            format!(r#"{{"package_version":"{PRODUCT_VERSION}"}}"#),
        )
        .unwrap();
        let sidecar = package.path().join("bin/godot-codex-mcp");
        std::fs::write(&sidecar, b"test-sidecar").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let plan_store = plans.path().join("plans");
        let mut setup = SetupOptions::new(project.path(), SetupProfile::ReadOnly);
        setup.plan_store = Some(plan_store.clone());
        setup.package_directory = Some(package.path().to_path_buf());
        let setup_preview = prepare_setup(&setup).unwrap();
        apply_setup_plan(&setup_preview.plan_digest, Some(&plan_store)).unwrap();

        let config_path = project.path().join(".codex/config.toml");
        let config = std::fs::read_to_string(&config_path).unwrap();
        std::fs::write(
            &config_path,
            config.replacen("startup_timeout_sec = 10", "startup_timeout_sec = 99", 1),
        )
        .unwrap();
        let receipt_path = project.path().join(".godot/codex/setup-receipt-v1.json");
        let config_before = std::fs::read(&config_path).unwrap();
        let receipt_before = std::fs::read(&receipt_path).unwrap();
        let mut repair = RepairOptions::new(project.path());
        repair.plan_store = Some(plan_store.clone());
        repair.package_directory = Some(package.path().to_path_buf());
        let command = Command::SetupRepairPreview {
            options: repair,
            dry_run: false,
            json: false,
        };
        let mut output = Vec::new();
        let mut error = Vec::new();
        let exit = run(
            command,
            &mut Cursor::new(Vec::<u8>::new()),
            &mut output,
            &mut error,
            false,
        );
        assert_eq!(exit, 2);
        let output = String::from_utf8(output).unwrap();
        let error = String::from_utf8(error).unwrap();
        assert!(output.contains("repair plan sha256:"));
        assert!(output.contains("<drifted receipt-owned stanza; current-file-digest=sha256:"));
        assert!(!output.contains("startup_timeout_sec = 99"));
        assert!(!output.contains(project.path().to_str().unwrap()));
        assert!(error.contains("consent_required"));
        let digest = rendered_plan_digest(&output);
        assert_eq!(
            apply_setup_plan(&digest, Some(&plan_store)),
            Err(godot_codex_operations::SetupError::PlanNotFound)
        );
        assert_eq!(std::fs::read(config_path).unwrap(), config_before);
        assert_eq!(std::fs::read(receipt_path).unwrap(), receipt_before);
    }
}
