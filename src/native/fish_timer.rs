//! Windows timing snapshots. Reports bypass command redirects, as on POSIX.
use super::*;
use rash::host::CpuTimes;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

pub(super) fn wants_timing(job: &ast::JobPipeline) -> bool {
    if job.time.is_some() {
        return true;
    }
    let mut statement = &job.statement;
    while let ast::Statement::Not(node) = statement {
        if node.time.is_some() {
            return true;
        }
        statement = &node.contents;
    }
    false
}

pub(super) fn validate(job: &ast::JobPipeline, source: &wstr) -> Result<()> {
    if job.bg.is_some() && wants_timing(job) {
        return Err(Error::new(
            "The 'time' command cannot be used with background jobs",
        ));
    }
    for stage in &job.continuation {
        let mut statement = &stage.statement;
        let mut timed = false;
        while let ast::Statement::Not(node) = statement {
            timed |= node.time.is_some();
            statement = &node.contents;
        }
        // In this position the parser treats plain `time` as a command.
        // Reject it before worker source is reparsed as a standalone job.
        if let ast::Statement::Decorated(node) = statement {
            timed |= node.decoration() == StatementDecoration::None
                && text(source, &node.command) == "time";
        }
        if timed {
            return Err(Error::new(
                "The 'time' command may only be at the beginning of a pipeline",
            ));
        }
    }
    Ok(())
}

fn process_times() -> Result<CpuTimes> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // The current-process pseudo handle needs no ownership or cleanup.
    if unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    fn duration(time: FILETIME) -> Duration {
        let ticks = (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
        Duration::from_secs(ticks / 10_000_000) + Duration::from_nanos((ticks % 10_000_000) * 100)
    }
    Ok(CpuTimes {
        user: duration(user),
        system: duration(kernel),
    })
}

pub(super) struct Snapshot {
    wall: Instant,
    fish: CpuTimes,
    children: CpuTimes,
}

impl Snapshot {
    pub(super) fn take(host: &impl Host) -> Result<Self> {
        Ok(Self {
            wall: Instant::now(),
            fish: process_times()?,
            children: host.child_cpu_times()?,
        })
    }
}

pub(super) fn report(start: Result<Snapshot>, host: &mut impl Host) {
    let output = (|| {
        let start = start?;
        let end = Snapshot::take(host)?;
        Ok(crate::timer_format::format(
            end.wall - start.wall,
            end.fish.system.saturating_sub(start.fish.system),
            end.fish.user.saturating_sub(start.fish.user),
            end.children.system.saturating_sub(start.children.system),
            end.children.user.saturating_sub(start.children.user),
            true,
        ) + "\n")
    })();
    let output = output.unwrap_or_else(|error: Error| format!("time: {}\n", error.message));
    // A report (or a closed stderr) must never replace the command's status.
    let _ = host.write(2, output.as_bytes(), &[], false);
}
