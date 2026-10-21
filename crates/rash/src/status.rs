//! Process and pipeline status rules ported from GNU Bash 3.2.57.
//!
//! Sources: jobs.c (process_exit_status, process_exit_signal, raw_job_exit_status)
//! and execute_cmd.c (return-value inversion).
//
// Copyright (C) 1989-2006 Free Software Foundation, Inc. (jobs.c)
// Copyright (C) 1987-2005 Free Software Foundation, Inc. (execute_cmd.c)
//
// This file is part of Rash, derived from GNU Bash, the Bourne Again SHell.
//
// This program is free software; you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the Free
// Software Foundation; either version 2, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
// FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
// more details.
//
// You should have received a copy of the GNU General Public License along
// with this program; see COPYING. If not, write to the Free Software
// Foundation, 59 Temple Place, Suite 330, Boston, MA 02111 USA.

/// A process outcome, retaining the distinction between exit, signal, and stop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessStatus {
    Exited(u8),
    Signaled(i32),
    Stopped(i32),
}

impl ProcessStatus {
    /// Bash's process exit value. Its wait loop handles stopped jobs separately.
    pub const fn exit_status(self) -> i32 {
        match self {
            Self::Exited(code) => code as i32,
            Self::Signaled(signal) => 128 + signal,
            Self::Stopped(_) => 0,
        }
    }

    /// The terminating signal, or zero for an ordinary exit or stop.
    pub const fn exit_signal(self) -> i32 {
        match self {
            Self::Signaled(signal) => signal,
            Self::Exited(_) | Self::Stopped(_) => 0,
        }
    }
}

/// Select the pipeline's raw outcome; an empty pipeline has no outcome.
///
/// Bash 3.2 reads the effective global `pipefail` option when reducing statuses.
/// This helper takes that value explicitly; it does not capture launch options.
pub fn pipeline_status(stages: &[ProcessStatus], pipefail: bool) -> Option<ProcessStatus> {
    let last = *stages.last()?;
    if !pipefail {
        return Some(last);
    }

    // A stopped status is nonzero before conversion, even though exit_status is zero.
    Some(
        stages
            .iter()
            .rev()
            .copied()
            .find(|status| *status != ProcessStatus::Exited(0))
            .unwrap_or(ProcessStatus::Exited(0)),
    )
}

/// Apply the command's `!` inversion without changing its individual stage statuses.
pub const fn invert_status(status: i32) -> i32 {
    if status == 0 { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::{ProcessStatus::*, invert_status, pipeline_status};

    #[test]
    fn process_outcomes_keep_signal_information() {
        assert_eq!(Exited(0).exit_status(), 0);
        assert_eq!(Exited(255).exit_status(), 255);
        assert_eq!(Exited(143).exit_signal(), 0);
        assert_eq!(Signaled(15).exit_status(), 143);
        assert_eq!(Signaled(15).exit_signal(), 15);
        assert_eq!(Stopped(19).exit_status(), 0);
        assert_eq!(Stopped(19).exit_signal(), 0);
    }

    #[test]
    fn pipeline_uses_last_stage_or_rightmost_failure() {
        assert_eq!(pipeline_status(&[], false), None);
        assert_eq!(pipeline_status(&[], true), None);
        assert_eq!(
            pipeline_status(&[Exited(0), Exited(0)], true),
            Some(Exited(0))
        );

        let stages = [Exited(7), Signaled(15), Exited(3), Exited(0)];
        assert_eq!(pipeline_status(&stages, false), Some(Exited(0)));
        assert_eq!(pipeline_status(&stages, true), Some(Exited(3)));

        let stages = [Exited(7), Signaled(15), Exited(0)];
        assert_eq!(pipeline_status(&stages, true), Some(Signaled(15)));
    }

    #[test]
    fn pipefail_selects_raw_stopped_outcome_before_converting_it() {
        let stages = [Exited(7), Stopped(19), Exited(0)];
        let result = pipeline_status(&stages, true).unwrap();
        assert_eq!(result, Stopped(19));
        assert_eq!(result.exit_status(), 0);
    }

    #[test]
    fn inversion_changes_only_success_or_failure() {
        assert_eq!(invert_status(0), 1);
        for status in [1, 7, 127, 143, 255] {
            assert_eq!(invert_status(status), 0);
        }
    }
}
