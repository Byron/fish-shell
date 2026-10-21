use super::{Controls, Event, Signal};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::os::windows::process::CommandExt as _;
use std::process::{Child, Command};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::Win32::Foundation::{
    ERROR_INVALID_PARAMETER, ERROR_MORE_DATA, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, GenerateConsoleCtrlEvent, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, GetExitCodeThread, GetProcessIdOfThread,
    OpenProcess, OpenThread, PROCESS_QUERY_LIMITED_INFORMATION, ResumeThread, SuspendThread,
    THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
};

static INTERRUPTS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn console_handler(event: u32) -> i32 {
    match event {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => {
            INTERRUPTS.fetch_add(1, Ordering::Relaxed);
            1
        }
        _ => 0,
    }
}

pub fn install_controls(receive_ctrl_c: bool) -> io::Result<()> {
    static INSTALLED: OnceLock<Result<(), i32>> = OnceLock::new();
    match INSTALLED.get_or_init(|| {
        // The handler only updates atomics and remains valid for the process lifetime.
        if unsafe { SetConsoleCtrlHandler(Some(console_handler), 1) } == 0 {
            Err(io::Error::last_os_error().raw_os_error().unwrap_or(1))
        } else {
            Ok(())
        }
    }) {
        Ok(()) => {
            // A shell launched with CREATE_NEW_PROCESS_GROUP must undo the
            // inherited Ctrl-C-ignore flag. Interpreter workers keep it, so
            // keyboard Ctrl-C cannot interrupt unrelated background jobs;
            // targeted Ctrl-Break still reaches their registered handler.
            if unsafe { SetConsoleCtrlHandler(None, i32::from(!receive_ctrl_c)) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
        Err(code) => Err(io::Error::from_raw_os_error(*code)),
    }
}

pub fn controls() -> Controls {
    Controls {
        interrupt: INTERRUPTS.load(Ordering::Relaxed),
        stop: 0,
    }
}

#[cfg(test)]
pub fn request_interrupt_for_test() {
    INTERRUPTS.fetch_add(1, Ordering::Relaxed);
}

pub fn pipe() -> io::Result<(File, File)> {
    let mut read = std::ptr::null_mut();
    let mut write = std::ptr::null_mut();
    // Parent handles are not inheritable. std::process duplicates only the selected
    // Stdio handles as inheritable when creating each child.
    if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // CreatePipe returned two distinct owned handles.
    Ok(unsafe { (File::from_raw_handle(read), File::from_raw_handle(write)) })
}

pub fn poll(child: &mut Child) -> io::Result<Option<Event>> {
    child
        .try_wait()
        .map(|status| status.map(|status| Event::Exited(status.code().unwrap_or(1))))
}

pub struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {}
}

pub struct Group {
    job: OwnedHandle,
    roots: Vec<u32>,
    suspended: BTreeMap<u32, OwnedHandle>,
}

impl Group {
    pub fn new() -> io::Result<Self> {
        // An unnamed private job cannot be opened by name from another shell.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let group = Self {
            job: unsafe { OwnedHandle::from_raw_handle(job) },
            roots: Vec::new(),
            suspended: BTreeMap::new(),
        };
        group.set_lifetime(true)?;
        Ok(group)
    }

    fn handle(&self) -> HANDLE {
        self.job.as_raw_handle()
    }

    fn set_lifetime(&self, kill_on_close: bool) -> io::Result<()> {
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        if kill_on_close {
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        }
        if unsafe {
            SetInformationJobObject(
                self.handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn configure(&self, command: &mut Command) {
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED);
    }

    pub fn add(&mut self, child: &Child) -> io::Result<()> {
        // The child is still suspended, so it cannot create descendants before
        // entering the owned job. Descendants inherit this association.
        if unsafe { AssignProcessToJobObject(self.handle(), child.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        self.roots.push(child.id());
        Ok(())
    }

    pub fn start(&mut self, child: &Child) -> io::Result<()> {
        let threads = threads(&BTreeSet::from([child.id()]), self.handle())?;
        if threads.is_empty() {
            return Err(io::Error::other("suspended child has no primary thread"));
        }
        for (_, thread) in threads {
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    fn members(&self) -> io::Result<BTreeSet<u32>> {
        let mut words = 64;
        loop {
            // usize storage provides proper alignment for the variable-length
            // JOBOBJECT_BASIC_PROCESS_ID_LIST and its pointer-sized PID entries.
            let mut buffer = vec![0usize; words];
            if unsafe {
                QueryInformationJobObject(
                    self.handle(),
                    JobObjectBasicProcessIdList,
                    buffer.as_mut_ptr().cast(),
                    (buffer.len() * std::mem::size_of::<usize>()) as u32,
                    std::ptr::null_mut(),
                )
            } != 0
            {
                let header = unsafe { &*buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() };
                let count = header.NumberOfProcessIdsInList as usize;
                let entries =
                    unsafe { std::slice::from_raw_parts(header.ProcessIdList.as_ptr(), count) };
                return Ok(entries.iter().map(|id| *id as u32).collect());
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
                return Err(error);
            }
            words = words
                .checked_mul(2)
                .filter(|n| *n < 1_000_000)
                .ok_or_else(|| io::Error::other("job contains too many processes"))?;
        }
    }

    pub fn signal(&mut self, signal: Signal) -> io::Result<()> {
        match signal {
            Signal::Check => {
                self.members()?;
            }
            Signal::Interrupt => {
                let mut sent = false;
                for pid in &self.roots {
                    // Only groups created for our own pipeline stages are addressed.
                    sent |= unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, *pid) } != 0;
                }
                if !sent {
                    self.terminate(130)?;
                }
            }
            Signal::Terminate => self.terminate(143)?,
            Signal::Kill => self.terminate(137)?,
            Signal::Stop => self.suspend()?,
            Signal::Continue => self.resume()?,
        }
        Ok(())
    }

    fn suspend(&mut self) -> io::Result<()> {
        let result = (|| {
            // Re-snapshot until no running thread could have created another.
            for _ in 0..16 {
                let found = threads(&self.members()?, self.handle())?;
                let mut added = false;
                for (id, thread) in found {
                    if self.suspended.contains_key(&id) {
                        continue;
                    }
                    if unsafe { SuspendThread(thread.as_raw_handle()) } == u32::MAX {
                        let error = io::Error::last_os_error();
                        if thread_exited(&thread) {
                            continue;
                        }
                        return Err(error);
                    }
                    self.suspended.insert(id, thread);
                    added = true;
                }
                if !added {
                    return Ok(());
                }
            }
            Err(io::Error::other("could not reach a stable suspended job"))
        })();
        // Failed suspension must not leave a partially stopped job behind.
        if result.is_err() {
            let _ = self.resume();
        }
        result
    }

    fn resume(&mut self) -> io::Result<()> {
        let suspended = std::mem::take(&mut self.suspended);
        let mut error = None;
        for (id, thread) in suspended {
            // An exited thread needs no resume; a live thread loses only the
            // suspend count this shell added, preserving other owners' counts.
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX
                && !thread_exited(&thread)
            {
                error.get_or_insert_with(io::Error::last_os_error);
                self.suspended.insert(id, thread);
            }
        }
        error.map_or(Ok(()), Err)
    }

    pub fn terminate(&mut self, status: i32) -> io::Result<()> {
        if unsafe { TerminateJobObject(self.handle(), status as u32) } == 0 {
            return Err(io::Error::last_os_error());
        }
        self.suspended.clear();
        Ok(())
    }

    pub fn foreground(&self) -> io::Result<TerminalGuard> {
        Ok(TerminalGuard)
    }

    pub fn disown(&mut self) -> io::Result<()> {
        self.set_lifetime(false)
    }
}

fn thread_exited(thread: &OwnedHandle) -> bool {
    let mut status = 259; // STILL_ACTIVE
    (unsafe { GetExitCodeThread(thread.as_raw_handle(), &mut status) }) != 0 && status != 259
}

fn threads(pids: &BTreeSet<u32>, job: HANDLE) -> io::Result<Vec<(u32, OwnedHandle)>> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut result = Vec::new();
    let mut present = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
    while present != 0 {
        if pids.contains(&entry.th32OwnerProcessID) {
            let thread = unsafe {
                OpenThread(
                    THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
                    0,
                    entry.th32ThreadID,
                )
            };
            if thread.is_null() {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_INVALID_PARAMETER as i32) {
                    return Err(error);
                }
            } else {
                let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
                let pid = unsafe { GetProcessIdOfThread(thread.as_raw_handle()) };
                // Thread IDs and PIDs may be recycled after a snapshot. Verify
                // identity and live job membership before changing any thread.
                if pid == entry.th32OwnerProcessID {
                    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
                    if !process.is_null() {
                        let process = unsafe { OwnedHandle::from_raw_handle(process) };
                        let mut belongs = 0;
                        if unsafe { IsProcessInJob(process.as_raw_handle(), job, &mut belongs) }
                            == 0
                        {
                            return Err(io::Error::last_os_error());
                        }
                        if belongs != 0 {
                            result.push((entry.th32ThreadID, thread));
                        }
                    } else if !thread_exited(&thread) {
                        return Err(io::Error::last_os_error());
                    }
                }
            }
        }
        present = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
    }
    Ok(result)
}
