use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;

use godot_codex_operations::{
    DoctorOptions, GuidanceMode, PRODUCT_VERSION, PendingSetup, RemoveOptions, RepairOptions,
    SetupOptions, SetupPreview, SetupProfile, SurfaceCaptureArmOptions, SurfaceCaptureArmReport,
    SurfaceCaptureError, SurfaceCaptureSurface, SurfaceSelection, abandon_surface_capture,
    apply_setup_plan, arm_surface_capture, cancel_surface_capture, consume_surface_capture,
    prepare_setup, prepare_setup_for_consent, prepare_setup_remove,
    prepare_setup_remove_for_consent, prepare_setup_repair, prepare_setup_repair_for_consent,
    run_doctor, surface_capture_status,
};

const USAGE: &str = "\
usage:
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" version
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" doctor --project-root <path> [--surface auto|app|cli|ide] [--require-editor] [--json] [--show-paths]
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" setup --project-root <path> --profile read-only|full-beta [--guidance none|agents|skill|all] [--dry-run] [--json]
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" setup --repair --project-root <path> [--dry-run] [--json]
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" setup --apply-plan sha256:<digest> [--json]
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" setup --remove --project-root <path> [--dry-run] [--json]
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" surface-capture arm --surface app|cli|ide --project-root <path> --metadata <path> --ttl-seconds <1..1800> --json
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" surface-capture status --run-id <64-lowercase-hex> --json
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" surface-capture cancel --run-id <64-lowercase-hex> --json
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" surface-capture consume --run-id <64-lowercase-hex> --capture-sha256 sha256:<64-lowercase-hex> --json
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" surface-capture abandon --run-id <64-lowercase-hex> --metadata-sha256 sha256:<64-lowercase-hex> --json";

const SURFACE_CAPTURE_OUTPUT_ERROR_EXIT_CODE: i32 = 74;

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

const SURFACE_CAPTURE_USAGE: &str = "\
usage:
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" surface-capture arm --surface app|cli|ide --project-root <path> --metadata <path> --ttl-seconds <1..1800> --json
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" surface-capture status --run-id <64-lowercase-hex> --json
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" surface-capture cancel --run-id <64-lowercase-hex> --json
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" surface-capture consume --run-id <64-lowercase-hex> --capture-sha256 sha256:<64-lowercase-hex> --json
  \"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex\" surface-capture abandon --run-id <64-lowercase-hex> --metadata-sha256 sha256:<64-lowercase-hex> --json

Capture state is private to the exact installed data root. Arm verifies the
installed current package plus the setup-owned project config and receipt.
Abandon is explicit digest-bound recovery for an inactive bare Claimed run or
an inactive malformed partial finalization. A live claim is never removed;
valid recoverable partials require recovery and healthy finalized runs require
consume.
These commands never change project files, host trust, or Codex config.";

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
    SurfaceCaptureArm {
        options: SurfaceCaptureArmOptions,
    },
    SurfaceCaptureStatus {
        run_id: String,
    },
    SurfaceCaptureCancel {
        run_id: String,
    },
    SurfaceCaptureConsume {
        run_id: String,
        capture_sha256: String,
    },
    SurfaceCaptureAbandon {
        run_id: String,
        metadata_sha256: String,
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
        Some("surface-capture") => parse_surface_capture(arguments),
        _ => Err(USAGE.to_owned()),
    }
}

fn parse_command_for_execution(
    arguments: Vec<OsString>,
    output: &mut dyn Write,
    error: &mut dyn Write,
) -> Result<Command, i32> {
    let surface_capture =
        arguments.first().and_then(|argument| argument.to_str()) == Some("surface-capture");
    match parse_command(arguments) {
        Ok(command) => Ok(command),
        Err(_) if surface_capture => {
            let value = serde_json::json!({
                "schema_version": "godot-codex-operation-error/1.0",
                "status": "error",
                "code": "surface_capture_arguments_invalid",
            });
            match write_surface_capture_json(error, &value) {
                Ok(()) => Err(64),
                Err(_) => Err(render_surface_capture_output_failure(
                    output,
                    SurfaceCaptureOutputRecovery::Parse,
                )),
            }
        }
        Err(message) => {
            let _ = writeln!(error, "{message}");
            Err(64)
        }
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

fn parse_surface_capture(mut arguments: VecDeque<OsString>) -> Result<Command, String> {
    if matches!(
        arguments.front().and_then(|argument| argument.to_str()),
        Some("help" | "--help" | "-h")
    ) {
        return if arguments.len() == 1 {
            Ok(Command::Help(SURFACE_CAPTURE_USAGE))
        } else {
            Err("surface-capture help cannot be combined with other arguments".to_owned())
        };
    }
    let action = arguments
        .pop_front()
        .and_then(|argument| argument.into_string().ok())
        .ok_or_else(|| SURFACE_CAPTURE_USAGE.to_owned())?;
    match action.as_str() {
        "arm" => parse_surface_capture_arm(arguments),
        "status" => parse_surface_capture_run(arguments, false),
        "cancel" => parse_surface_capture_run(arguments, true),
        "consume" => parse_surface_capture_consume(arguments),
        "abandon" => parse_surface_capture_abandon(arguments),
        _ => Err(SURFACE_CAPTURE_USAGE.to_owned()),
    }
}

fn parse_surface_capture_arm(mut arguments: VecDeque<OsString>) -> Result<Command, String> {
    let mut project_root = None;
    let mut surface = None;
    let mut metadata = None;
    let mut ttl_seconds = None;
    let mut json = false;
    while let Some(argument) = arguments.pop_front() {
        match argument.to_str() {
            Some("--project-root") => set_once(
                &mut project_root,
                PathBuf::from(required_value(&mut arguments, "--project-root")?),
                "--project-root",
            )?,
            Some("--metadata") => set_once(
                &mut metadata,
                PathBuf::from(required_value(&mut arguments, "--metadata")?),
                "--metadata",
            )?,
            Some("--surface") => {
                let value = match required_utf8(&mut arguments, "--surface")?.as_str() {
                    "app" => SurfaceCaptureSurface::App,
                    "cli" => SurfaceCaptureSurface::Cli,
                    "ide" => SurfaceCaptureSurface::Ide,
                    _ => return Err("--surface must be app, cli, or ide".to_owned()),
                };
                set_once(&mut surface, value, "--surface")?;
            }
            Some("--ttl-seconds") => {
                let raw = required_utf8(&mut arguments, "--ttl-seconds")?;
                let value = raw
                    .parse::<u64>()
                    .ok()
                    .filter(|value| raw == value.to_string() && (1..=1800).contains(value))
                    .ok_or_else(|| "--ttl-seconds must be an integer from 1 to 1800".to_owned())?;
                set_once(&mut ttl_seconds, value, "--ttl-seconds")?;
            }
            Some("--json") if !json => json = true,
            _ => return Err("unknown or duplicate surface-capture arm argument".to_owned()),
        }
    }
    if !json {
        return Err("--json is required for surface-capture".to_owned());
    }
    Ok(Command::SurfaceCaptureArm {
        options: SurfaceCaptureArmOptions::new(
            project_root.ok_or_else(|| "--project-root is required".to_owned())?,
            surface.ok_or_else(|| "--surface is required".to_owned())?,
            metadata.ok_or_else(|| "--metadata is required".to_owned())?,
            ttl_seconds.ok_or_else(|| "--ttl-seconds is required".to_owned())?,
        ),
    })
}

fn parse_surface_capture_run(
    mut arguments: VecDeque<OsString>,
    cancel: bool,
) -> Result<Command, String> {
    let mut run_id = None;
    let mut json = false;
    while let Some(argument) = arguments.pop_front() {
        match argument.to_str() {
            Some("--run-id") => {
                let value = required_utf8(&mut arguments, "--run-id")?;
                if !valid_surface_capture_run_id(&value) {
                    return Err("--run-id must be exactly 64 lowercase hex characters".to_owned());
                }
                set_once(&mut run_id, value, "--run-id")?;
            }
            Some("--json") if !json => json = true,
            _ => return Err("unknown or duplicate surface-capture argument".to_owned()),
        }
    }
    if !json {
        return Err("--json is required for surface-capture".to_owned());
    }
    let run_id = run_id.ok_or_else(|| "--run-id is required".to_owned())?;
    Ok(if cancel {
        Command::SurfaceCaptureCancel { run_id }
    } else {
        Command::SurfaceCaptureStatus { run_id }
    })
}

fn parse_surface_capture_consume(mut arguments: VecDeque<OsString>) -> Result<Command, String> {
    let mut run_id = None;
    let mut capture_sha256 = None;
    let mut json = false;
    while let Some(argument) = arguments.pop_front() {
        match argument.to_str() {
            Some("--run-id") => {
                let value = required_utf8(&mut arguments, "--run-id")?;
                if !valid_surface_capture_run_id(&value) {
                    return Err("--run-id must be exactly 64 lowercase hex characters".to_owned());
                }
                set_once(&mut run_id, value, "--run-id")?;
            }
            Some("--capture-sha256") => {
                let value = required_utf8(&mut arguments, "--capture-sha256")?;
                if !valid_surface_capture_digest(&value) {
                    return Err(
                        "--capture-sha256 must be sha256: followed by 64 lowercase hex characters"
                            .to_owned(),
                    );
                }
                set_once(&mut capture_sha256, value, "--capture-sha256")?;
            }
            Some("--json") if !json => json = true,
            _ => return Err("unknown or duplicate surface-capture consume argument".to_owned()),
        }
    }
    if !json {
        return Err("--json is required for surface-capture".to_owned());
    }
    Ok(Command::SurfaceCaptureConsume {
        run_id: run_id.ok_or_else(|| "--run-id is required".to_owned())?,
        capture_sha256: capture_sha256.ok_or_else(|| "--capture-sha256 is required".to_owned())?,
    })
}

fn parse_surface_capture_abandon(mut arguments: VecDeque<OsString>) -> Result<Command, String> {
    let mut run_id = None;
    let mut metadata_sha256 = None;
    let mut json = false;
    while let Some(argument) = arguments.pop_front() {
        match argument.to_str() {
            Some("--run-id") => {
                let value = required_utf8(&mut arguments, "--run-id")?;
                if !valid_surface_capture_run_id(&value) {
                    return Err("--run-id must be exactly 64 lowercase hex characters".to_owned());
                }
                set_once(&mut run_id, value, "--run-id")?;
            }
            Some("--metadata-sha256") => {
                let value = required_utf8(&mut arguments, "--metadata-sha256")?;
                if !valid_surface_capture_digest(&value) {
                    return Err(
                        "--metadata-sha256 must be sha256: followed by 64 lowercase hex characters"
                            .to_owned(),
                    );
                }
                set_once(&mut metadata_sha256, value, "--metadata-sha256")?;
            }
            Some("--json") if !json => json = true,
            _ => return Err("unknown or duplicate surface-capture abandon argument".to_owned()),
        }
    }
    if !json {
        return Err("--json is required for surface-capture".to_owned());
    }
    Ok(Command::SurfaceCaptureAbandon {
        run_id: run_id.ok_or_else(|| "--run-id is required".to_owned())?,
        metadata_sha256: metadata_sha256
            .ok_or_else(|| "--metadata-sha256 is required".to_owned())?,
    })
}

fn valid_surface_capture_run_id(run_id: &str) -> bool {
    run_id.len() == 64
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_surface_capture_digest(digest: &str) -> bool {
    digest.len() == 71
        && digest.starts_with("sha256:")
        && digest[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
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
        Command::SurfaceCaptureArm { options } => {
            run_surface_capture_arm_result(arm_surface_capture(&options), output, error)
        }
        Command::SurfaceCaptureStatus { run_id } => run_surface_capture_result(
            surface_capture_status(&run_id),
            SurfaceCaptureOutputRecovery::Status,
            output,
            error,
        ),
        Command::SurfaceCaptureCancel { run_id } => run_surface_capture_result(
            cancel_surface_capture(&run_id),
            SurfaceCaptureOutputRecovery::Mutation,
            output,
            error,
        ),
        Command::SurfaceCaptureConsume {
            run_id,
            capture_sha256,
        } => run_surface_capture_result(
            consume_surface_capture(&run_id, &capture_sha256),
            SurfaceCaptureOutputRecovery::Mutation,
            output,
            error,
        ),
        Command::SurfaceCaptureAbandon {
            run_id,
            metadata_sha256,
        } => run_surface_capture_result(
            abandon_surface_capture(&run_id, &metadata_sha256),
            SurfaceCaptureOutputRecovery::Mutation,
            output,
            error,
        ),
    }
}

fn run_surface_capture_arm_result(
    result: Result<SurfaceCaptureArmReport, SurfaceCaptureError>,
    output: &mut dyn Write,
    error: &mut dyn Write,
) -> i32 {
    match result {
        Ok(report) if report.authorizes_host_start() => {
            match write_surface_capture_json(output, &report) {
                Ok(()) => 0,
                Err(_) => {
                    render_surface_capture_output_failure(error, SurfaceCaptureOutputRecovery::Arm)
                }
            }
        }
        Ok(report) => match write_surface_capture_json(error, &report) {
            Ok(()) => 2,
            Err(_) => {
                render_surface_capture_output_failure(output, SurfaceCaptureOutputRecovery::Arm)
            }
        },
        Err(capture_error) => match write_surface_capture_operation_error(error, &capture_error) {
            Ok(()) => 2,
            Err(_) => {
                render_surface_capture_output_failure(output, SurfaceCaptureOutputRecovery::Arm)
            }
        },
    }
}

#[derive(Clone, Copy)]
enum SurfaceCaptureOutputRecovery {
    Parse,
    Arm,
    Status,
    Mutation,
}

fn run_surface_capture_result<T: serde::Serialize>(
    result: Result<T, SurfaceCaptureError>,
    recovery: SurfaceCaptureOutputRecovery,
    output: &mut dyn Write,
    error: &mut dyn Write,
) -> i32 {
    match result {
        Ok(report) => match write_surface_capture_json(output, &report) {
            Ok(()) => 0,
            Err(_) => render_surface_capture_output_failure(error, recovery),
        },
        Err(capture_error) => match write_surface_capture_operation_error(error, &capture_error) {
            Ok(()) => 2,
            Err(_) => render_surface_capture_output_failure(output, recovery),
        },
    }
}

fn write_surface_capture_json<T: serde::Serialize>(
    writer: &mut dyn Write,
    value: &T,
) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *writer, value).map_err(io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn write_surface_capture_operation_error(
    writer: &mut dyn Write,
    error: &SurfaceCaptureError,
) -> io::Result<()> {
    let value = serde_json::json!({
        "schema_version": "godot-codex-operation-error/1.0",
        "status": "error",
        "code": error.to_string(),
    });
    write_surface_capture_json(writer, &value)
}

fn render_surface_capture_output_failure(
    writer: &mut dyn Write,
    recovery: SurfaceCaptureOutputRecovery,
) -> i32 {
    let (operation, outcome, retry_safe, next_action) = match recovery {
        SurfaceCaptureOutputRecovery::Parse => (
            "parse",
            "error_response_lost",
            true,
            "correct_arguments_and_retry",
        ),
        SurfaceCaptureOutputRecovery::Arm => (
            "arm",
            "receipt_lost",
            true,
            "retry_exact_arm_before_starting_host",
        ),
        SurfaceCaptureOutputRecovery::Status => {
            ("status", "response_lost", true, "retry_exact_status")
        }
        SurfaceCaptureOutputRecovery::Mutation => (
            "mutation",
            "operation_result_ambiguous",
            false,
            "query_exact_run_status_before_retry",
        ),
    };
    let value = serde_json::json!({
        "schema_version": "godot-codex-operation-error/1.0",
        "status": "error",
        "code": "surface_capture_result_output_failed",
        "operation": operation,
        "outcome": outcome,
        "retry_safe": retry_safe,
        "next_action": next_action,
    });
    let _ = write_surface_capture_json(writer, &value);
    SURFACE_CAPTURE_OUTPUT_ERROR_EXIT_CODE
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
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let command = match parse_command_for_execution(
        arguments,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    ) {
        Ok(command) => command,
        Err(exit_code) => std::process::exit(exit_code),
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

    struct FailingWriter {
        bytes: Vec<u8>,
        remaining_bytes: Option<usize>,
        fail_flush: bool,
    }

    impl FailingWriter {
        fn after_bytes(remaining_bytes: usize) -> Self {
            Self {
                bytes: Vec::new(),
                remaining_bytes: Some(remaining_bytes),
                fail_flush: false,
            }
        }

        fn on_flush() -> Self {
            Self {
                bytes: Vec::new(),
                remaining_bytes: None,
                fail_flush: true,
            }
        }
    }

    impl Write for FailingWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let accepted = self
                .remaining_bytes
                .map_or(buffer.len(), |remaining| remaining.min(buffer.len()));
            if accepted == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "intentional writer failure",
                ));
            }
            self.bytes.extend_from_slice(&buffer[..accepted]);
            if let Some(remaining) = &mut self.remaining_bytes {
                *remaining -= accepted;
            }
            Ok(accepted)
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "intentional flush failure",
                ))
            } else {
                Ok(())
            }
        }
    }

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
            parse_command([OsString::from("surface-capture"), OsString::from("help"),]),
            Ok(Command::Help(SURFACE_CAPTURE_USAGE))
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
    fn surface_capture_parser_is_closed_and_digest_bound() {
        assert!(matches!(
            parse_command([
                OsString::from("surface-capture"),
                OsString::from("arm"),
                OsString::from("--surface"),
                OsString::from("cli"),
                OsString::from("--project-root"),
                OsString::from("/private/project"),
                OsString::from("--metadata"),
                OsString::from("/private/metadata.json"),
                OsString::from("--ttl-seconds"),
                OsString::from("1800"),
                OsString::from("--json"),
            ]),
            Ok(Command::SurfaceCaptureArm { .. })
        ));
        let run_id = "a".repeat(64);
        assert_eq!(
            parse_command([
                OsString::from("surface-capture"),
                OsString::from("status"),
                OsString::from("--run-id"),
                OsString::from(&run_id),
                OsString::from("--json"),
            ]),
            Ok(Command::SurfaceCaptureStatus {
                run_id: run_id.clone(),
            })
        );
        assert_eq!(
            parse_command([
                OsString::from("surface-capture"),
                OsString::from("cancel"),
                OsString::from("--run-id"),
                OsString::from(&run_id),
                OsString::from("--json"),
            ]),
            Ok(Command::SurfaceCaptureCancel {
                run_id: run_id.clone()
            })
        );
        let capture_sha256 = format!("sha256:{}", "b".repeat(64));
        assert_eq!(
            parse_command([
                OsString::from("surface-capture"),
                OsString::from("consume"),
                OsString::from("--run-id"),
                OsString::from(&run_id),
                OsString::from("--capture-sha256"),
                OsString::from(&capture_sha256),
                OsString::from("--json"),
            ]),
            Ok(Command::SurfaceCaptureConsume {
                run_id: run_id.clone(),
                capture_sha256: capture_sha256.clone(),
            })
        );
        let metadata_sha256 = format!("sha256:{}", "c".repeat(64));
        assert_eq!(
            parse_command([
                OsString::from("surface-capture"),
                OsString::from("abandon"),
                OsString::from("--run-id"),
                OsString::from(&run_id),
                OsString::from("--metadata-sha256"),
                OsString::from(&metadata_sha256),
                OsString::from("--json"),
            ]),
            Ok(Command::SurfaceCaptureAbandon {
                run_id: run_id.clone(),
                metadata_sha256: metadata_sha256.clone(),
            })
        );
        for ttl in ["0", "01", "1801", "-1"] {
            assert!(
                parse_command([
                    OsString::from("surface-capture"),
                    OsString::from("arm"),
                    OsString::from("--surface"),
                    OsString::from("app"),
                    OsString::from("--project-root"),
                    OsString::from("/private/project"),
                    OsString::from("--metadata"),
                    OsString::from("/private/metadata.json"),
                    OsString::from("--ttl-seconds"),
                    OsString::from(ttl),
                    OsString::from("--json"),
                ])
                .is_err()
            );
        }
        assert!(
            parse_command([
                OsString::from("surface-capture"),
                OsString::from("status"),
                OsString::from("--run-id"),
                OsString::from("A".repeat(64)),
                OsString::from("--json"),
            ])
            .is_err()
        );
        for invalid_digest in [
            "b".repeat(64),
            format!("sha256:{}", "B".repeat(64)),
            format!("sha256:{}", "b".repeat(63)),
        ] {
            assert!(
                parse_command([
                    OsString::from("surface-capture"),
                    OsString::from("consume"),
                    OsString::from("--run-id"),
                    OsString::from(&run_id),
                    OsString::from("--capture-sha256"),
                    OsString::from(invalid_digest),
                    OsString::from("--json"),
                ])
                .is_err()
            );
        }
        assert!(
            parse_command([
                OsString::from("surface-capture"),
                OsString::from("consume"),
                OsString::from("--run-id"),
                OsString::from(&run_id),
                OsString::from("--capture-sha256"),
                OsString::from(&capture_sha256),
            ])
            .is_err()
        );
        assert!(SURFACE_CAPTURE_USAGE.contains("surface-capture consume"));
        assert!(SURFACE_CAPTURE_USAGE.contains("surface-capture abandon"));
        assert!(SURFACE_CAPTURE_USAGE.contains("A live claim is never removed"));
        assert!(SURFACE_CAPTURE_USAGE.contains("valid recoverable partials require recovery"));
        assert!(
            parse_command([
                OsString::from("surface-capture"),
                OsString::from("abandon"),
                OsString::from("--run-id"),
                OsString::from(&run_id),
                OsString::from("--metadata-sha256"),
                OsString::from(format!("sha256:{}", "C".repeat(64))),
                OsString::from("--json"),
            ])
            .is_err()
        );
        assert!(
            parse_command([
                OsString::from("surface-capture"),
                OsString::from("status"),
                OsString::from("--run-id"),
                OsString::from("a".repeat(64)),
            ])
            .is_err()
        );
    }

    #[test]
    fn surface_capture_parse_errors_are_stable_json_without_input_echo() {
        let private_input = "path=/Users/alice/S11_SECRET_DO_NOT_ECHO";
        for arguments in [
            vec![
                OsString::from("surface-capture"),
                OsString::from(private_input),
            ],
            vec![
                OsString::from("surface-capture"),
                OsString::from("status"),
                OsString::from("--run-id"),
                OsString::from(private_input),
                OsString::from("--json"),
            ],
            vec![
                OsString::from("surface-capture"),
                OsString::from("cancel"),
                OsString::from("--run-id"),
                OsString::from("a".repeat(64)),
            ],
            vec![
                OsString::from("surface-capture"),
                OsString::from("status"),
                OsString::from("--json"),
            ],
        ] {
            let mut output = Vec::new();
            let mut error = Vec::new();
            assert_eq!(
                parse_command_for_execution(arguments, &mut output, &mut error).unwrap_err(),
                64
            );
            assert!(output.is_empty());
            let rendered: serde_json::Value = serde_json::from_slice(&error).unwrap();
            assert_eq!(
                rendered,
                serde_json::json!({
                    "schema_version": "godot-codex-operation-error/1.0",
                    "status": "error",
                    "code": "surface_capture_arguments_invalid",
                })
            );
            assert!(!String::from_utf8(error).unwrap().contains(private_input));
        }
    }

    #[cfg(unix)]
    #[test]
    fn surface_capture_non_utf8_parse_error_is_safe_json() {
        use std::os::unix::ffi::OsStringExt as _;

        let arguments = vec![
            OsString::from("surface-capture"),
            OsString::from_vec(vec![0xff, 0xfe]),
            OsString::from("--json"),
        ];
        let mut output = Vec::new();
        let mut error = Vec::new();
        assert_eq!(
            parse_command_for_execution(arguments, &mut output, &mut error).unwrap_err(),
            64
        );
        assert!(output.is_empty());
        let rendered: serde_json::Value = serde_json::from_slice(&error).unwrap();
        assert_eq!(rendered["code"], "surface_capture_arguments_invalid");
        assert_eq!(rendered.as_object().unwrap().len(), 3);
    }

    #[test]
    fn surface_capture_parse_error_write_failure_uses_exit_74_fallback() {
        let private_input = "S11_SECRET_DO_NOT_ECHO";
        let arguments = vec![
            OsString::from("surface-capture"),
            OsString::from("status"),
            OsString::from("--run-id"),
            OsString::from(private_input),
            OsString::from("--json"),
        ];
        let mut output = Vec::new();
        let mut failed_error = FailingWriter::after_bytes(0);
        assert_eq!(
            parse_command_for_execution(arguments, &mut output, &mut failed_error).unwrap_err(),
            SURFACE_CAPTURE_OUTPUT_ERROR_EXIT_CODE
        );
        let rendered: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(rendered["code"], "surface_capture_result_output_failed");
        assert_eq!(rendered["operation"], "parse");
        assert_eq!(rendered["outcome"], "error_response_lost");
        assert_eq!(rendered["retry_safe"], true);
        assert_eq!(rendered["next_action"], "correct_arguments_and_retry");
        assert!(!String::from_utf8(output).unwrap().contains(private_input));
    }

    #[test]
    fn surface_capture_renderer_emits_json_only_and_stable_error_codes() {
        #[derive(serde::Serialize)]
        struct SafeReport<'a> {
            run_id: &'a str,
            state: &'a str,
            digest: &'a str,
        }

        let mut output = Vec::new();
        let mut error = Vec::new();
        assert_eq!(
            run_surface_capture_result(
                Ok(SafeReport {
                    run_id: "a",
                    state: "armed",
                    digest: "sha256:fixture",
                }),
                SurfaceCaptureOutputRecovery::Status,
                &mut output,
                &mut error,
            ),
            0
        );
        let rendered: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(rendered["state"], "armed");
        assert!(error.is_empty());

        output.clear();
        assert_eq!(
            run_surface_capture_result::<SafeReport<'_>>(
                Err(SurfaceCaptureError::RunIdInvalid),
                SurfaceCaptureOutputRecovery::Status,
                &mut output,
                &mut error,
            ),
            2
        );
        let rendered: serde_json::Value = serde_json::from_slice(&error).unwrap();
        assert_eq!(rendered["code"], "surface_capture_run_id_invalid");
        assert!(output.is_empty());

        error.clear();
        assert_eq!(
            run_surface_capture_result::<SafeReport<'_>>(
                Err(SurfaceCaptureError::DigestMismatch),
                SurfaceCaptureOutputRecovery::Status,
                &mut output,
                &mut error,
            ),
            2
        );
        let rendered: serde_json::Value = serde_json::from_slice(&error).unwrap();
        assert_eq!(rendered["code"], "surface_capture_digest_mismatch");
        assert!(output.is_empty());

        error.clear();
        assert_eq!(
            run_surface_capture_result::<SafeReport<'_>>(
                Err(SurfaceCaptureError::MetadataDigestMismatch),
                SurfaceCaptureOutputRecovery::Status,
                &mut output,
                &mut error,
            ),
            2
        );
        let rendered: serde_json::Value = serde_json::from_slice(&error).unwrap();
        assert_eq!(rendered["code"], "surface_capture_metadata_digest_mismatch");
        assert!(output.is_empty());

        for (failure, code) in [
            (
                SurfaceCaptureError::ClaimActive,
                "surface_capture_claim_active",
            ),
            (
                SurfaceCaptureError::RecoveryRequired,
                "surface_capture_recovery_required",
            ),
            (
                SurfaceCaptureError::ConsumeRequired,
                "surface_capture_consume_required",
            ),
            (
                SurfaceCaptureError::Unsupported,
                "surface_capture_unsupported",
            ),
        ] {
            error.clear();
            assert_eq!(
                run_surface_capture_result::<SafeReport<'_>>(
                    Err(failure),
                    SurfaceCaptureOutputRecovery::Status,
                    &mut output,
                    &mut error,
                ),
                2
            );
            let rendered: serde_json::Value = serde_json::from_slice(&error).unwrap();
            assert_eq!(rendered["code"], code);
            assert!(output.is_empty());
        }
    }

    #[test]
    fn surface_capture_json_writer_propagates_value_newline_and_flush_failures() {
        let mut value_failure = FailingWriter::after_bytes(0);
        assert!(write_surface_capture_json(&mut value_failure, &()).is_err());

        let mut newline_failure = FailingWriter::after_bytes(b"null".len());
        assert!(write_surface_capture_json(&mut newline_failure, &()).is_err());
        assert_eq!(newline_failure.bytes, b"null");

        let mut flush_failure = FailingWriter::on_flush();
        assert!(write_surface_capture_json(&mut flush_failure, &()).is_err());
        assert_eq!(flush_failure.bytes, b"null\n");
    }

    #[test]
    fn surface_capture_renderer_reports_lost_and_ambiguous_receipts() {
        let mut failed_output = FailingWriter::after_bytes(0);
        let mut error = Vec::new();
        assert_eq!(
            run_surface_capture_result(
                Ok(serde_json::json!({"state": "armed"})),
                SurfaceCaptureOutputRecovery::Status,
                &mut failed_output,
                &mut error,
            ),
            SURFACE_CAPTURE_OUTPUT_ERROR_EXIT_CODE
        );
        let rendered: serde_json::Value = serde_json::from_slice(&error).unwrap();
        assert_eq!(rendered["code"], "surface_capture_result_output_failed");
        assert_eq!(rendered["operation"], "status");
        assert_eq!(rendered["outcome"], "response_lost");
        assert_eq!(rendered["retry_safe"], true);
        assert_eq!(rendered["next_action"], "retry_exact_status");

        let mut failed_output = FailingWriter::after_bytes(0);
        error.clear();
        assert_eq!(
            run_surface_capture_result(
                Ok(serde_json::json!({"state": "cancelled"})),
                SurfaceCaptureOutputRecovery::Mutation,
                &mut failed_output,
                &mut error,
            ),
            SURFACE_CAPTURE_OUTPUT_ERROR_EXIT_CODE
        );
        let rendered: serde_json::Value = serde_json::from_slice(&error).unwrap();
        assert_eq!(rendered["outcome"], "operation_result_ambiguous");
        assert_eq!(rendered["retry_safe"], false);
        assert_eq!(
            rendered["next_action"],
            "query_exact_run_status_before_retry"
        );

        let mut failed_error = FailingWriter::after_bytes(0);
        let mut output = Vec::new();
        assert_eq!(
            run_surface_capture_result::<serde_json::Value>(
                Err(SurfaceCaptureError::RunIdInvalid),
                SurfaceCaptureOutputRecovery::Status,
                &mut output,
                &mut failed_error,
            ),
            SURFACE_CAPTURE_OUTPUT_ERROR_EXIT_CODE
        );
        let rendered: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(rendered["outcome"], "response_lost");
        assert_eq!(rendered["retry_safe"], true);
    }

    #[test]
    fn arm_renderer_routes_created_and_recovered_authority_but_rejects_conflict_and_expiry() {
        use godot_codex_operations::{ArmDisposition, LeaseState};

        let report = |disposition, state| SurfaceCaptureArmReport {
            schema_version: "godot-codex-surface-capture-result/1.1".to_owned(),
            action: "arm".to_owned(),
            run_id: "a".repeat(64),
            state,
            disposition,
            surface: SurfaceCaptureSurface::Cli,
            project_id: "project".to_owned(),
            project_identity: format!("sha256:{}", "1".repeat(64)),
            metadata_sha256: format!("sha256:{}", "2".repeat(64)),
            package_launcher_sha256: format!("sha256:{}", "3".repeat(64)),
            project_config_sha256: format!("sha256:{}", "4".repeat(64)),
            setup_receipt_sha256: format!("sha256:{}", "5".repeat(64)),
            internal_manifest_sha256: format!("sha256:{}", "6".repeat(64)),
        };
        for disposition in [ArmDisposition::Created, ArmDisposition::Recovered] {
            let mut output = Vec::new();
            let mut error = Vec::new();
            assert_eq!(
                run_surface_capture_arm_result(
                    Ok(report(disposition, LeaseState::Armed)),
                    &mut output,
                    &mut error,
                ),
                0
            );
            assert!(!output.is_empty());
            assert!(error.is_empty());
        }
        for (disposition, state) in [
            (ArmDisposition::Conflict, LeaseState::Armed),
            (ArmDisposition::Recovered, LeaseState::Expired),
            (ArmDisposition::Conflict, LeaseState::Claimed),
        ] {
            let mut output = Vec::new();
            let mut error = Vec::new();
            assert_eq!(
                run_surface_capture_arm_result(
                    Ok(report(disposition, state)),
                    &mut output,
                    &mut error,
                ),
                2
            );
            assert!(output.is_empty());
            let rendered: serde_json::Value = serde_json::from_slice(&error).unwrap();
            assert_eq!(rendered["run_id"], "a".repeat(64));
            assert_eq!(
                rendered["disposition"],
                serde_json::to_value(disposition).unwrap()
            );
            assert_eq!(rendered["state"], serde_json::to_value(state).unwrap());
        }

        let mut failed_output = FailingWriter::after_bytes(0);
        let mut error = Vec::new();
        assert_eq!(
            run_surface_capture_arm_result(
                Ok(report(ArmDisposition::Recovered, LeaseState::Armed)),
                &mut failed_output,
                &mut error,
            ),
            SURFACE_CAPTURE_OUTPUT_ERROR_EXIT_CODE
        );
        let rendered: serde_json::Value = serde_json::from_slice(&error).unwrap();
        assert_eq!(rendered["operation"], "arm");
        assert_eq!(rendered["outcome"], "receipt_lost");
        assert_eq!(rendered["retry_safe"], true);
        assert_eq!(
            rendered["next_action"],
            "retry_exact_arm_before_starting_host"
        );
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
