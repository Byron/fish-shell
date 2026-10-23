//! The native host's owned jobs. No process-control operation accepts an
//! arbitrary system PID: operands always resolve through this table first.

use super::*;
use std::process::Child;
use std::time::{Duration, Instant};

#[cfg(windows)]
fn prepare_command(stage: &Command) -> Result<io::Result<ProcessCommand>> {
    use crate::native::{command_resolution, utilities};
    let name = checked_os(&stage.argv[0])?;
    let args = stage.argv[1..]
        .iter()
        .map(|arg| checked_os(arg))
        .collect::<Result<Vec<_>>>()?;
    let path = command_resolution::environment(&stage.env, b"PATH")
        .map(checked_os)
        .transpose()?;
    let pathext = command_resolution::environment(&stage.env, b"PATHEXT")
        .map(checked_os)
        .transpose()?;
    Ok((|| {
        if let Some(path) = command_resolution::first(&name, path.as_deref(), pathext.as_deref())? {
            let mut command = ProcessCommand::new(path);
            command.args(args);
            Ok(command)
        } else if !command_resolution::is_explicit(&name)
            && name.to_str().is_some_and(utilities::available)
        {
            utilities::command(name.to_str().unwrap(), &args)
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "command not found"))
        }
    })())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum State {
    Running,
    Stopped(i32),
    Done(i32),
}

/// A fork-time query snapshot. No process handles or ownership cross this boundary.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct JobView {
    pub(super) id: u32,
    display: String,
    /// Original successfully spawned PIDs, including members which have exited.
    pids: Vec<u32>,
    state: State,
}

impl JobView {
    fn is_live(&self) -> bool {
        !matches!(self.state, State::Done(_))
    }

    pub(super) fn description(&self, long: bool) -> String {
        let state = match self.state {
            State::Running => "Running".to_owned(),
            State::Stopped(_) => "Stopped".to_owned(),
            State::Done(0) => "Done".to_owned(),
            State::Done(code) => format!("Exit {code}"),
        };
        let pids = if long {
            format!(
                "{} ",
                self.pids
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        } else {
            String::new()
        };
        format!("[{}] {pids}{state}\t{}", self.id, self.display)
    }
}

struct Process {
    child: Option<Child>,
    state: State,
}

pub(super) struct Job {
    id: u32,
    display: String,
    processes: Vec<Process>,
    group: process::Group,
    background: bool,
    owned: bool,
    notified: Option<State>,
    output: Option<Arc<File>>,
    output_offset: u64,
    diagnostics: Vec<u8>,
    _worker_files: Vec<super::super::worker::PayloadFile>,
    _temporary_directories: Vec<super::super::temporary::Directory>,
}

impl Job {
    fn state(&self) -> State {
        if self
            .processes
            .iter()
            .all(|p| matches!(p.state, State::Done(_)))
        {
            return self.processes.last().map_or(State::Done(0), |p| p.state);
        }
        if self
            .processes
            .iter()
            .all(|p| !matches!(p.state, State::Running))
        {
            return self
                .processes
                .iter()
                .find_map(|p| {
                    if let State::Stopped(signal) = p.state {
                        Some(State::Stopped(signal))
                    } else {
                        None
                    }
                })
                .unwrap_or(State::Done(0));
        }
        State::Running
    }

    fn last_pid(&self) -> Option<u32> {
        self.processes
            .iter()
            .rev()
            .find_map(|process| process.child.as_ref().map(Child::id))
    }

    pub(super) fn description(&self, long: bool) -> String {
        self.view().description(long)
    }

    fn view(&self) -> JobView {
        JobView {
            id: self.id,
            display: self.display.clone(),
            // Child handles remain in stage order until the whole job is removed.
            // The first PID therefore stays a stable group identifier after exit.
            pids: self
                .processes
                .iter()
                .filter_map(|process| process.child.as_ref().map(Child::id))
                .collect(),
            state: self.state(),
        }
    }

    fn poll(&mut self) -> Result<()> {
        for process in &mut self.processes {
            if matches!(process.state, State::Done(_)) {
                continue;
            }
            let Some(child) = &mut process.child else {
                continue;
            };
            if let Some(event) = process::poll(child)? {
                process.state = match event {
                    process::Event::Exited(code) => State::Done(code),
                    process::Event::Stopped(signal) => State::Stopped(signal),
                    process::Event::Continued => State::Running,
                };
            }
        }
        Ok(())
    }

    fn signal(&mut self, signal: process::Signal) -> Result<()> {
        self.poll()?;
        if matches!(self.state(), State::Done(_)) {
            return Ok(());
        }
        self.group.signal(signal)?;
        for process in &mut self.processes {
            if matches!(process.state, State::Done(_)) {
                continue;
            }
            if signal == process::Signal::Stop {
                process.state = State::Stopped(20);
            }
            if signal == process::Signal::Continue {
                process.state = State::Running;
            }
        }
        Ok(())
    }

    fn result(&mut self) -> Result<RunResult> {
        let pipeline = self
            .processes
            .iter()
            .map(|process| match process.state {
                State::Done(code) => code,
                State::Stopped(signal) => 128 + signal,
                State::Running => 0,
            })
            .collect::<Vec<_>>();
        let stopped = matches!(self.state(), State::Stopped(_));
        let mut stdout = std::mem::take(&mut self.diagnostics);
        if let Some(file) = self.output.as_ref().filter(|_| !stopped) {
            let mut file = &**file;
            file.seek(io::SeekFrom::Start(self.output_offset))?;
            file.take(CAPTURE_BYTE_LIMIT.saturating_sub(stdout.len()) as u64 + 1)
                .read_to_end(&mut stdout)?;
            if stdout.len() > CAPTURE_BYTE_LIMIT {
                return Err(Error::new("command substitution output limit exceeded"));
            }
            self.output_offset = file.stream_position()?;
        }
        Ok(RunResult {
            status: pipeline.last().copied().unwrap_or(0),
            stopped,
            pipeline,
            stdout,
            last_pid: None,
        })
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if !self.owned || matches!(self.state(), State::Done(_)) {
            return;
        }
        let _ = self.group.terminate(137);
        for process in &mut self.processes {
            if matches!(process.state, State::Done(_)) {
                continue;
            }
            if let Some(child) = &mut process.child {
                let _ = child.kill();
                // Terminated children must be reaped even on setup/start failures.
                let _ = child.wait();
            }
        }
    }
}

impl NativeHost {
    pub(super) fn job_views(&self) -> BTreeMap<u32, JobView> {
        self.inherited_jobs
            .iter()
            .cloned()
            .chain(self.jobs.values().map(Job::view))
            .map(|view| (view.id, view))
            .collect()
    }

    pub(super) fn worker_job_views(&mut self) -> Result<Vec<JobView>> {
        self.refresh_jobs()?;
        Ok(self
            .job_views()
            .into_values()
            .filter(JobView::is_live)
            .collect())
    }

    pub(crate) fn keep_worker(&mut self, file: super::super::worker::PayloadFile) {
        self.pending_workers.push(file);
    }

    pub(super) fn retain_directories(&mut self, last_pid: u32, paths: &[PathBuf]) -> Result<()> {
        let job = self
            .jobs
            .values_mut()
            .find(|job| job.last_pid() == Some(last_pid))
            .ok_or_else(|| Error::new("stopped job is no longer available"))?;
        job._temporary_directories.extend(
            paths
                .iter()
                .cloned()
                .map(super::super::temporary::Directory::from_reserved),
        );
        Ok(())
    }

    pub fn clear_console_requests(&mut self) {
        self.controls = process::controls();
        self.interrupted = false;
    }

    pub fn take_console_interrupt(&mut self) -> bool {
        let now = process::controls();
        let pending = self.interrupted || now.interrupt != self.controls.interrupt;
        self.controls = now;
        self.interrupted = false;
        pending
    }

    fn refresh_jobs(&mut self) -> Result<()> {
        for job in self.jobs.values_mut() {
            job.poll()?;
            let state = job.state();
            if job.background && state != State::Running && job.notified != Some(state) {
                self.notifications.push_back(job.description(false));
                job.notified = Some(state);
            }
        }
        Ok(())
    }

    pub fn poll_jobs(&mut self) -> Result<Vec<String>> {
        self.refresh_jobs()?;
        Ok(self.notifications.drain(..).collect())
    }

    pub(super) fn launch(
        &mut self,
        stages: &[Command],
        background: bool,
        capture: bool,
    ) -> Result<RunResult> {
        let worker_files = std::mem::take(&mut self.pending_workers);
        self.refresh_jobs()?;
        if stages.is_empty() {
            if background {
                return Err(Error::new("empty background job"));
            }
            return Ok(RunResult::default());
        }
        if stages.iter().any(|stage| stage.builtin) {
            if stages.len() != 1 || background {
                return Err(Error::new(
                    "job-control builtins must run as foreground commands",
                ));
            }
            return self.job_builtin(&stages[0], capture);
        }
        if capture && background {
            return Err(Error::new(
                "background jobs inside command substitutions are not supported",
            ));
        }
        let id = self.next_job;
        self.next_job = id
            .checked_add(1)
            .ok_or_else(|| Error::new("job IDs exhausted"))?;
        let mut job = Job {
            id,
            display: stages
                .iter()
                .map(|stage| {
                    stage.display.as_deref().map_or_else(
                        || {
                            stage
                                .argv
                                .iter()
                                .map(|s| String::from_utf8_lossy(s).into_owned())
                                .collect::<Vec<_>>()
                                .join(" ")
                        },
                        |s| String::from_utf8_lossy(s).into_owned(),
                    )
                })
                .collect::<Vec<_>>()
                .join(" | "),
            processes: Vec::new(),
            // Worker descendants are already covered by the ancestor shell's
            // Windows Job Object. Its lifetime policy must also govern orphaned
            // descendants after an inner foreground command has completed.
            group: process::Group::new(!self.worker)?,
            background,
            owned: true,
            notified: None,
            output: capture.then(|| temporary_file(&[], false)).transpose()?,
            output_offset: 0,
            diagnostics: Vec::new(),
            _worker_files: worker_files,
            _temporary_directories: Vec::new(),
        };
        let pipes = (1..stages.len())
            .map(|_| process::pipe().map(|(read, write)| (Arc::new(read), Arc::new(write))))
            .collect::<io::Result<Vec<_>>>()?;
        let mut diagnostics = Vec::new();
        let mut terminal = None;
        for (index, stage) in stages.iter().enumerate() {
            let Some(name) = stage.argv.first() else {
                return Err(Error::new("empty external command"));
            };
            #[cfg(windows)]
            let (mut command, resolution_error) = match prepare_command(stage)? {
                Ok(command) => (command, None),
                Err(error) => (ProcessCommand::new(checked_os(name)?), Some(error)),
            };
            #[cfg(not(windows))]
            let (mut command, resolution_error) = {
                let mut command = ProcessCommand::new(checked_os(name)?);
                for arg in &stage.argv[1..] {
                    command.arg(checked_os(arg)?);
                }
                (command, None::<io::Error>)
            };
            command.env_clear();
            for (name, value) in &stage.env {
                if cfg!(windows)
                    && name.as_slice() != b"PATH"
                    && name.eq_ignore_ascii_case(b"PATH")
                    && stage.env.contains_key(b"PATH".as_slice())
                {
                    continue;
                }
                if name.is_empty() || name.contains(&b'=') {
                    return Err(Error::new("invalid native environment name"));
                }
                command.env(checked_os(name)?, checked_os(value)?);
            }
            let mut defaults = [
                if index > 0 {
                    Target::File(pipes[index - 1].0.clone())
                } else if background {
                    Target::Null
                } else {
                    Target::Inherited(0)
                },
                if capture {
                    Target::Capture
                } else {
                    Target::Inherited(1)
                },
                Target::Inherited(2),
            ];
            if index + 1 < stages.len() {
                if !matches!(stage.pipe_fd, 1 | 2) {
                    return Err(Error::new("only stdout or stderr may be piped"));
                }
                defaults[stage.pipe_fd as usize] = Target::File(pipes[index].1.clone());
            }
            let targets = apply_redirects(&stage.redirects, defaults)?;
            command.stdin(child_stdio(&targets[0], 0, job.output.as_deref())?);
            command.stdout(child_stdio(&targets[1], 1, job.output.as_deref())?);
            command.stderr(child_stdio(&targets[2], 2, job.output.as_deref())?);
            job.group.configure(&mut command);
            let spawned = match resolution_error {
                Some(error) => Err(error),
                None => process::spawn(&mut command),
            };
            match spawned {
                Ok(mut child) => {
                    if let Err(error) = job.group.add(&child) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(error.into());
                    }
                    job.processes.push(Process {
                        child: Some(child),
                        state: State::Running,
                    });
                    if !background && terminal.is_none() {
                        terminal = Some(job.group.foreground()?);
                    }
                }
                Err(error) => {
                    let status = if error.kind() == io::ErrorKind::NotFound {
                        127
                    } else {
                        126
                    };
                    diagnostics.push((
                        targets[2].clone(),
                        format!("{}: {error}\n", String::from_utf8_lossy(name)).into_bytes(),
                    ));
                    job.processes.push(Process {
                        child: None,
                        state: State::Done(status),
                    });
                }
            }
        }
        // Release all parent's pipe endpoints before waiting, otherwise readers
        // never see EOF after their writers exit.
        drop(pipes);
        for process in &job.processes {
            if let Some(child) = &process.child {
                job.group.start(child)?;
            }
        }
        for (target, message) in diagnostics {
            job.diagnostics.extend(write_target(&target, &message)?);
        }
        let last_pid = job.last_pid();
        self.jobs.insert(id, job);
        if background {
            self.notifications.push_back(format!(
                "[{id}] {}",
                last_pid.map_or_else(|| "completed".to_owned(), |pid| pid.to_string())
            ));
            Ok(RunResult {
                status: 0,
                stopped: false,
                pipeline: vec![0],
                stdout: Vec::new(),
                last_pid,
            })
        } else {
            let result = self.wait_job(id, true);
            drop(terminal);
            result
        }
    }

    fn wait_job(&mut self, id: u32, foreground: bool) -> Result<RunResult> {
        let job = self
            .jobs
            .get_mut(&id)
            .ok_or_else(|| Error::new("no such job"))?;
        let terminal = if foreground {
            Some(job.group.foreground()?)
        } else {
            None
        };
        if foreground && matches!(job.state(), State::Stopped(_)) {
            job.signal(process::Signal::Continue)?;
        }
        let mut interrupt_at = None;
        let mut cancelled_wait = false;
        loop {
            self.refresh_jobs()?;
            let now = process::controls();
            let job = self.jobs.get_mut(&id).unwrap();
            if now.interrupt != self.controls.interrupt {
                self.controls.interrupt = now.interrupt;
                self.interrupted = true;
                cancelled_wait = true;
                if !foreground {
                    drop(terminal);
                    return Ok(RunResult {
                        status: 130,
                        pipeline: vec![130],
                        ..Default::default()
                    });
                }
                job.signal(process::Signal::Interrupt)?;
                interrupt_at = Some(Instant::now());
            }
            if now.stop != self.controls.stop {
                self.controls.stop = now.stop;
                if foreground {
                    job.signal(process::Signal::Stop)?;
                }
            }
            if interrupt_at.is_some_and(|at| at.elapsed() >= Duration::from_millis(500))
                && !matches!(job.state(), State::Done(_))
            {
                job.group.terminate(130)?;
                interrupt_at = None;
            }
            match job.state() {
                State::Done(_) => {
                    let mut job = self.jobs.remove(&id).unwrap();
                    let mut result = job.result();
                    if cancelled_wait {
                        if let Ok(result) = &mut result {
                            result.status = 130;
                        }
                    }
                    drop(terminal);
                    return result;
                }
                State::Stopped(_) => {
                    job.background = true;
                    if job.notified != Some(job.state()) {
                        self.notifications.push_back(job.description(false));
                        job.notified = Some(job.state());
                    }
                    let result = job.result();
                    drop(terminal);
                    return result;
                }
                State::Running => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    fn resolve_job(&self, name: &[u8]) -> Result<u32> {
        let active = self
            .jobs
            .iter()
            .rev()
            .filter(|(_, job)| !matches!(job.state(), State::Done(_)))
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        match name {
            b"%" | b"%%" | b"%+" => active
                .first()
                .copied()
                .ok_or_else(|| Error::new("no current job")),
            b"%-" => active
                .get(1)
                .copied()
                .ok_or_else(|| Error::new("no previous job")),
            _ => {
                if let Some(number) = name.strip_prefix(b"%") {
                    let id = parse_number(number)?;
                    return self
                        .jobs
                        .contains_key(&id)
                        .then_some(id)
                        .ok_or_else(|| Error::new("no such job"));
                }
                let pid = parse_number(name)?;
                self.jobs
                    .iter()
                    .find(|(_, job)| {
                        job.processes
                            .iter()
                            .any(|p| p.child.as_ref().is_some_and(|child| child.id() == pid))
                    })
                    .map(|(id, _)| *id)
                    .ok_or_else(|| Error::new("process is not owned by this shell"))
            }
        }
    }

    fn builtin_write(
        &mut self,
        fd: i32,
        bytes: &[u8],
        command: &Command,
        capture: bool,
        output: &mut Vec<u8>,
    ) -> Result<()> {
        output.extend(self.write(fd, bytes, &command.redirects, capture)?);
        Ok(())
    }

    fn job_builtin(&mut self, command: &Command, capture: bool) -> Result<RunResult> {
        self.refresh_jobs()?;
        let Some(name) = command.argv.first() else {
            return Err(Error::new("empty host builtin"));
        };
        let mut args = &command.argv[1..];
        let mut stdout = Vec::new();
        let mut status = 0;
        match name.as_slice() {
            b"jobs" => {
                let mut long = false;
                let mut pid_only = false;
                let mut group_only = false;
                let mut running = false;
                let mut stopped = false;
                while let Some(option) = args.first().filter(|arg| arg.starts_with(b"-")) {
                    match option.as_slice() {
                        b"-l" => long = true,
                        b"-p" => {
                            pid_only = true;
                            group_only = false;
                        }
                        b"-g" | b"--group" => {
                            group_only = true;
                            pid_only = false;
                        }
                        b"-r" => running = true,
                        b"-s" => stopped = true,
                        b"--" => {
                            args = &args[1..];
                            break;
                        }
                        _ => return Err(Error::new("jobs: unsupported option")),
                    }
                    args = &args[1..];
                }
                let views = self.job_views();
                let ids = if args.is_empty() {
                    views.keys().copied().collect::<Vec<_>>()
                } else {
                    args.iter()
                        .map(|arg| resolve_job_view(&views, arg))
                        .collect::<Result<Vec<_>>>()?
                };
                if group_only {
                    status = 1;
                }
                for id in ids {
                    let job = &views[&id];
                    if running && !matches!(job.state, State::Running)
                        || stopped && !matches!(job.state, State::Stopped(_))
                        || group_only && !job.is_live()
                    {
                        continue;
                    }
                    let line = if group_only {
                        let Some(pid) = job.pids.first() else {
                            continue;
                        };
                        status = 0;
                        format!("{pid}\n")
                    } else if pid_only {
                        job.pids
                            .last()
                            .map(|pid| format!("{pid}\n"))
                            .unwrap_or_default()
                    } else {
                        format!("{}\n", job.description(long))
                    };
                    self.builtin_write(1, line.as_bytes(), command, capture, &mut stdout)?;
                }
            }
            b"fg" | b"bg" => {
                if args.len() > 1 {
                    return Err(Error::new("expected at most one job"));
                }
                let id = self.resolve_job(args.first().map_or(b"%+".as_slice(), Vec::as_slice))?;
                if name == b"fg" {
                    let result = self.wait_job(id, true)?;
                    status = result.status;
                    stdout.extend(result.stdout);
                } else {
                    let job = self.jobs.get_mut(&id).unwrap();
                    job.signal(process::Signal::Continue)?;
                    job.background = true;
                    job.notified = None;
                    let line = format!("[{}] {} &\n", job.id, job.display);
                    self.builtin_write(1, line.as_bytes(), command, capture, &mut stdout)?;
                }
            }
            b"wait" => {
                let next = args.first().is_some_and(|arg| arg == b"-n");
                if next {
                    args = &args[1..];
                }
                if args.first().is_some_and(|arg| arg == b"--") {
                    args = &args[1..];
                }
                let ids = if args.is_empty() {
                    self.jobs.keys().copied().collect::<Vec<_>>()
                } else {
                    args.iter()
                        .map(|arg| self.resolve_job(arg))
                        .collect::<Result<Vec<_>>>()?
                };
                if next {
                    if ids.is_empty() {
                        status = 127;
                    } else {
                        loop {
                            self.refresh_jobs()?;
                            if let Some(id) = ids.iter().find(|id| {
                                self.jobs
                                    .get(id)
                                    .is_some_and(|job| !matches!(job.state(), State::Running))
                            }) {
                                let result = self.wait_job(*id, false)?;
                                status = result.status;
                                stdout.extend(result.stdout);
                                break;
                            }
                            if self.take_console_interrupt() {
                                // Keep enclosing interpreter loops cancelled
                                // after consuming the process-wide notification.
                                self.interrupted = true;
                                status = 130;
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(10));
                        }
                    }
                } else {
                    for id in ids {
                        let result = self.wait_job(id, false)?;
                        status = result.status;
                        stdout.extend(result.stdout);
                        if status == 130 {
                            break;
                        }
                    }
                    if args.is_empty() && status != 130 {
                        status = 0;
                    }
                }
            }
            b"kill" => match parse_kill(args) {
                Ok(KillAction::List(bytes)) => {
                    self.builtin_write(1, &bytes, command, capture, &mut stdout)?;
                }
                Ok(KillAction::Send(signal, operands)) => {
                    for name in operands {
                        let result = self
                            .resolve_job(name)
                            .and_then(|id| self.jobs.get_mut(&id).unwrap().signal(signal));
                        if let Err(error) = result {
                            status = 1;
                            self.builtin_write(
                                2,
                                format!("kill: {}\n", error.message).as_bytes(),
                                command,
                                capture,
                                &mut stdout,
                            )?;
                        }
                    }
                }
                Err(error) => {
                    // Invalid options are command failures. Completion helpers
                    // probe optional kill features behind redirected stderr.
                    status = error.status;
                    self.builtin_write(
                        2,
                        format!("kill: {}\n", error.message).as_bytes(),
                        command,
                        capture,
                        &mut stdout,
                    )?;
                }
            },
            b"disown" => {
                let all = args.first().is_some_and(|arg| arg == b"-a");
                if all {
                    args = &args[1..];
                }
                let ids = if all {
                    self.jobs.keys().copied().collect::<Vec<_>>()
                } else if args.is_empty() {
                    vec![self.resolve_job(b"%+")?]
                } else {
                    args.iter()
                        .map(|arg| self.resolve_job(arg))
                        .collect::<Result<Vec<_>>>()?
                };
                for id in ids {
                    let job = self.jobs.get_mut(&id).unwrap();
                    job.group.disown()?;
                    job.owned = false;
                    let mut job = self.jobs.remove(&id).unwrap();
                    // Keep child handles and worker snapshots until the detached
                    // job finishes, without retaining shell ownership or killing it.
                    std::thread::spawn(move || {
                        while !matches!(job.state(), State::Done(_)) {
                            if job.poll().is_err() {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(25));
                        }
                    });
                }
            }
            b"exec" => {
                return Err(Error::new(
                    "exec process replacement is not supported by the native host",
                ));
            }
            _ => return Err(Error::new("unsupported host builtin")),
        }
        Ok(RunResult {
            status,
            stopped: false,
            pipeline: vec![status],
            stdout,
            last_pid: None,
        })
    }
}

/// Query resolution includes inherited snapshots; control resolution above does not.
fn resolve_job_view(views: &BTreeMap<u32, JobView>, name: &[u8]) -> Result<u32> {
    let mut active = views.values().rev().filter(|view| view.is_live());
    match name {
        b"%" | b"%%" | b"%+" => active
            .next()
            .map(|view| view.id)
            .ok_or_else(|| Error::new("no current job")),
        b"%-" => active
            .nth(1)
            .map(|view| view.id)
            .ok_or_else(|| Error::new("no previous job")),
        _ => {
            if let Some(number) = name.strip_prefix(b"%") {
                let id = parse_number(number)?;
                return views
                    .contains_key(&id)
                    .then_some(id)
                    .ok_or_else(|| Error::new("no such job"));
            }
            let pid = parse_number(name)?;
            views
                .values()
                .find(|view| view.pids.contains(&pid))
                .map(|view| view.id)
                .ok_or_else(|| Error::new("no such job or process"))
        }
    }
}

fn parse_number(bytes: &[u8]) -> Result<u32> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|n| *n != 0)
        .ok_or_else(|| Error::new("expected a positive owned job or process ID"))
}

struct NativeSignal {
    number: u8,
    name: &'static str,
    signal: process::Signal,
}

const NATIVE_SIGNALS: &[NativeSignal] = &[
    NativeSignal {
        number: 2,
        name: "INT",
        signal: process::Signal::Interrupt,
    },
    NativeSignal {
        number: 9,
        name: "KILL",
        signal: process::Signal::Kill,
    },
    NativeSignal {
        number: 15,
        name: "TERM",
        signal: process::Signal::Terminate,
    },
    NativeSignal {
        number: 18,
        name: "CONT",
        signal: process::Signal::Continue,
    },
    NativeSignal {
        number: 19,
        name: "STOP",
        signal: process::Signal::Stop,
    },
    NativeSignal {
        number: 20,
        name: "TSTP",
        signal: process::Signal::Stop,
    },
];

fn signal_name(bytes: &[u8]) -> &[u8] {
    if bytes
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"SIG"))
    {
        &bytes[3..]
    } else {
        bytes
    }
}

fn unsupported_signal() -> Error {
    Error::new("unsupported signal; use INT, TERM, KILL, STOP, TSTP or CONT")
}

fn parse_signal(bytes: &[u8]) -> Result<process::Signal> {
    let bytes = signal_name(bytes);
    if bytes == b"0" {
        return Ok(process::Signal::Check);
    }
    let number = std::str::from_utf8(bytes)
        .ok()
        .and_then(|value| value.parse::<u8>().ok());
    NATIVE_SIGNALS
        .iter()
        .find(|entry| {
            number == Some(entry.number)
                || entry
                    .name
                    .as_bytes()
                    .eq_ignore_ascii_case(signal_name(bytes))
        })
        .map(|entry| entry.signal)
        .ok_or_else(unsupported_signal)
}

enum KillAction<'a> {
    List(Vec<u8>),
    Send(process::Signal, &'a [Vec<u8>]),
}

fn parse_kill(mut args: &[Vec<u8>]) -> Result<KillAction<'_>> {
    if args
        .first()
        .is_some_and(|arg| arg == b"-l" || arg == b"--list")
    {
        args = &args[1..];
        if args.first().is_some_and(|arg| arg == b"--") {
            args = &args[1..];
        }
        let mut output = String::new();
        if args.is_empty() {
            output.push_str(
                &NATIVE_SIGNALS
                    .iter()
                    .map(|entry| entry.name)
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            output.push('\n');
        } else {
            for argument in args {
                let number = std::str::from_utf8(argument)
                    .ok()
                    .and_then(|value| value.parse::<u32>().ok());
                let entry = NATIVE_SIGNALS
                    .iter()
                    .find(|entry| {
                        if let Some(number) = number {
                            // Like POSIX kill, also decode a shell's 128+signal status.
                            u32::from(entry.number)
                                == if number >= 128 { number - 128 } else { number }
                        } else {
                            entry
                                .name
                                .as_bytes()
                                .eq_ignore_ascii_case(signal_name(argument))
                        }
                    })
                    .ok_or_else(unsupported_signal)?;
                if number.is_some() {
                    output.push_str(entry.name);
                } else {
                    output.push_str(&entry.number.to_string());
                }
                output.push('\n');
            }
        }
        return Ok(KillAction::List(output.into_bytes()));
    }
    let mut signal = process::Signal::Terminate;
    if let Some(option) = args.first() {
        if option == b"-s" || option == b"--signal" {
            signal = parse_signal(
                args.get(1)
                    .ok_or_else(|| Error::new("--signal requires a signal"))?,
            )?;
            args = &args[2..];
        } else if let Some(name) = option.strip_prefix(b"--signal=") {
            signal = parse_signal(name)?;
            args = &args[1..];
        } else if option != b"--" {
            if let Some(name) = option.strip_prefix(b"-") {
                signal = parse_signal(name).or_else(|error| {
                    if let Some(name) = name.strip_prefix(b"s").filter(|name| !name.is_empty()) {
                        parse_signal(name)
                    } else {
                        Err(error)
                    }
                })?;
                args = &args[1..];
            }
        }
    }
    if args.first().is_some_and(|arg| arg == b"--") {
        args = &args[1..];
    }
    if args.is_empty() {
        return Err(Error::new("expected an owned job or process"));
    }
    Ok(KillAction::Send(signal, args))
}

#[cfg(test)]
#[path = "jobs_snapshot_tests.rs"]
mod snapshot_tests;
