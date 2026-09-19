//! Config validation CLI tool
//!
//! Validates a lunchboxd configuration file and reports any errors.

use lunchbox_api::EntryKind;
use lunchbox_util::default_config_path;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    let config_path = match args.get(1) {
        Some(path) => PathBuf::from(path),
        None => {
            let default_path = default_config_path();
            eprintln!("Usage: lunchbox-validate-config [config-file]");
            eprintln!();
            eprintln!("Validates a lunchboxd configuration file.");
            eprintln!();
            eprintln!("If no path is provided, uses: {}", default_path.display());
            eprintln!();
            eprintln!("Example:");
            eprintln!("  lunchbox-validate-config {}", default_path.display());
            eprintln!("  lunchbox-validate-config config.example.toml");
            return ExitCode::from(2);
        }
    };

    // Check file exists
    if !config_path.exists() {
        eprintln!(
            "Error: Configuration file not found: {}",
            config_path.display()
        );
        return ExitCode::from(1);
    }

    // Try to load and validate
    match lunchbox_config::load_config(&config_path) {
        Ok(policy) => {
            println!("✓ Configuration is valid");
            println!();
            println!("Summary:");
            println!(
                "  Config version: {}",
                lunchbox_config::CURRENT_CONFIG_VERSION
            );
            println!("  Entries: {}", policy.entries.len());

            // Show entry summary
            if !policy.entries.is_empty() {
                println!();
                println!("Entries:");
                for entry in &policy.entries {
                    let kind_str = match &entry.kind {
                        EntryKind::Process { command, .. } => {
                            format!("process ({})", command)
                        }
                        EntryKind::Snap { snap_name, .. } => {
                            format!("snap ({})", snap_name)
                        }
                        EntryKind::Steam { app_id, .. } => {
                            format!("steam ({})", app_id)
                        }
                        EntryKind::Flatpak { app_id, .. } => {
                            format!("flatpak ({})", app_id)
                        }
                        EntryKind::Vm { driver, .. } => {
                            format!("vm ({})", driver)
                        }
                        EntryKind::Media {
                            library,
                            mode,
                            item,
                            ..
                        } => match item {
                            Some(item) => {
                                format!("media {} ({}: {})", mode.subcommand(), library, item)
                            }
                            None => format!("media {} ({})", mode.subcommand(), library),
                        },
                        EntryKind::Ebook { book, viewer, .. } => {
                            format!("ebook {} ({})", viewer.default_command(), book.display())
                        }
                        EntryKind::Retroarch {
                            core,
                            core_path,
                            content,
                            ..
                        } => {
                            let core = core.clone().unwrap_or_else(|| {
                                core_path
                                    .as_ref()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_default()
                            });
                            format!("retroarch ({}, {})", core, content.display())
                        }
                        EntryKind::Custom { type_name, .. } => {
                            format!("custom ({})", type_name)
                        }
                    };
                    println!("  - {} [{}]: {}", entry.id.as_str(), kind_str, entry.label);
                }
            }

            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("✗ Configuration validation failed");
            eprintln!();
            match &e {
                lunchbox_config::ConfigError::ReadError(io_err) => {
                    eprintln!("Failed to read file: {}", io_err);
                }
                lunchbox_config::ConfigError::ParseError(parse_err) => {
                    eprintln!("TOML parse error:");
                    eprintln!("  {}", parse_err);
                }
                lunchbox_config::ConfigError::ValidationFailed { errors } => {
                    eprintln!("Validation errors ({}):", errors.len());
                    for err in errors {
                        eprintln!("  - {}", err);
                    }
                }
                lunchbox_config::ConfigError::UnsupportedVersion(ver) => {
                    eprintln!(
                        "Unsupported config version: {} (expected {})",
                        ver,
                        lunchbox_config::CURRENT_CONFIG_VERSION
                    );
                }
            }
            ExitCode::from(1)
        }
    }
}
