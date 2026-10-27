//! `hydradb backup` / `hydradb restore` — run an object-store copy.
//!
//! These are the executables the operator's `GraphBackup` / `restoreFrom`
//! jobs call. Configuration comes from the same environment contract the
//! operator injects (`HYDRADB_BACKUP_*`, `AWS_*`), so the command works inside
//! the Job and locally alike.
//!
//! The command never takes credentials as flags: they stay in the environment,
//! where the kubelet put them, and are never echoed.

use clap::Args;

use crate::CliError;
use crate::backup::{Direction, RunConfig};

/// Arguments shared by `backup` and `restore`.
#[derive(Args, Debug, Clone)]
pub struct BackupArgs {
    /// Only list what would be copied; copy nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// Verify the copy after it finishes (overrides `HYDRADB_BACKUP_VERIFY`).
    #[arg(long)]
    pub verify: bool,
}

/// Run a backup (cluster store → backup store).
///
/// # Errors
/// Returns [`CliError`] when the environment contract is incomplete, the copy
/// fails, or the build has no object-store support compiled in.
pub async fn run_backup(args: &BackupArgs) -> Result<(), CliError> {
    run(args, Direction::Backup).await
}

/// Run a restore (backup store → cluster store).
///
/// # Errors
/// As [`run_backup`].
pub async fn run_restore(args: &BackupArgs) -> Result<(), CliError> {
    run(args, Direction::Restore).await
}

async fn run(args: &BackupArgs, direction: Direction) -> Result<(), CliError> {
    let mut config = RunConfig::from_env().map_err(CliError::Backup)?;
    config.direction = direction;
    if args.verify {
        config.verify = true;
    }

    let label = match direction {
        Direction::Backup => "backup",
        Direction::Restore => "restore",
    };
    // Buckets and prefixes are safe to print; credentials never are.
    println!(
        "{label}: {}:{} -> {}:{} (verify={})",
        config.source_bucket,
        config.source_prefix,
        config.target_bucket,
        config.target_prefix,
        config.verify
    );

    #[cfg(feature = "s3")]
    {
        use crate::backup::s3::S3Sink;

        let source = S3Sink::from_env(&config.source_bucket).map_err(CliError::Backup)?;
        let target = S3Sink::from_env(&config.target_bucket).map_err(CliError::Backup)?;

        if args.dry_run {
            let entries = crate::backup::ObjectSink::list(&source, &config.source_prefix)
                .await
                .map_err(CliError::Backup)?;
            let plan =
                crate::backup::plan_copy(&entries, &config.source_prefix, &config.target_prefix);
            println!("{} object(s) to copy:", plan.len());
            for (from, to) in plan {
                println!("  {from} -> {to}");
            }
            return Ok(());
        }

        let copied = crate::backup::run(&source, &target, &config)
            .await
            .map_err(CliError::Backup)?;
        println!("{label} complete: {copied} object(s) copied");
        Ok(())
    }

    #[cfg(not(feature = "s3"))]
    {
        let _ = args;
        Err(CliError::Backup(crate::backup::BackupError::MissingEnv(
            "s3 feature (rebuild with --features s3)",
        )))
    }
}
