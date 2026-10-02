//! Runs the Jellyfin server as a child process that dies with us: a
//! kill-on-close job object on Windows, a process group plus parent-death
//! signal on Unix.

use super::installer::Installed;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{debug, error, info, warn};

pub struct JellyfinProcess {
    child: Child,
    #[cfg(windows)]
    _job: Option<job::Job>,
}

pub struct DataDirs {
    pub data: std::path::PathBuf,
    pub config: std::path::PathBuf,
    pub cache: std::path::PathBuf,
    pub log: std::path::PathBuf,
}

impl DataDirs {
    pub fn under(root: &Path) -> Self {
        Self {
            data: root.join("data"),
            config: root.join("config"),
            cache: root.join("cache"),
            log: root.join("log"),
        }
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for d in [&self.data, &self.config, &self.cache, &self.log] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }
}

fn forward_output<R: tokio::io::AsyncRead + Unpin + Send + 'static>(reader: R) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim_end();
            if line.is_empty() {
                continue;
            }
            if line.contains("[ERR]") || line.contains("[FTL]") {
                error!(target: "yarmiplayservertv_lib::jellyfin::server", "{line}");
            } else if line.contains("[WRN]") {
                warn!(target: "yarmiplayservertv_lib::jellyfin::server", "{line}");
            } else if line.contains("[DBG]") || line.contains("[VRB]") {
                debug!(target: "yarmiplayservertv_lib::jellyfin::server", "{line}");
            } else {
                info!(target: "yarmiplayservertv_lib::jellyfin::server", "{line}");
            }
        }
    });
}

impl JellyfinProcess {
    pub fn spawn(installed: &Installed, dirs: &DataDirs) -> Result<Self, String> {
        dirs.ensure().map_err(|e| format!("Jellyfin folders: {e}"))?;
        let mut cmd = Command::new(&installed.exe);
        cmd.arg("--datadir")
            .arg(&dirs.data)
            .arg("--configdir")
            .arg(&dirs.config)
            .arg("--cachedir")
            .arg(&dirs.cache)
            .arg("--logdir")
            .arg(&dirs.log)
            .arg("--webdir")
            .arg(&installed.web);
        if let Some(ffmpeg) = &installed.ffmpeg {
            cmd.arg("--ffmpeg").arg(ffmpeg);
        }
        if let Some(dir) = installed.exe.parent() {
            cmd.current_dir(dir);
        }
        cmd.env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                #[cfg(target_os = "linux")]
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }

        let mut child = cmd.spawn().map_err(|e| format!("could not start Jellyfin: {e}"))?;
        if let Some(out) = child.stdout.take() {
            forward_output(out);
        }
        if let Some(err) = child.stderr.take() {
            forward_output(err);
        }
        info!(pid = child.id(), "Jellyfin started");

        #[cfg(windows)]
        let job = {
            let job = job::Job::kill_on_close();
            if let (Some(job), Some(handle)) = (&job, child.raw_handle()) {
                job.assign(handle);
            }
            job
        };
        Ok(Self {
            child,
            #[cfg(windows)]
            _job: job,
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Resolves when the process exits.
    pub async fn wait(&mut self) -> String {
        match self.child.wait().await {
            Ok(status) => status.to_string(),
            Err(e) => e.to_string(),
        }
    }

    /// SIGTERM the group on Unix (on Windows the caller asks through the
    /// API first), wait up to `grace`, then kill.
    pub async fn stop(mut self, grace: std::time::Duration) {
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGTERM);
            }
        }
        if tokio::time::timeout(grace, self.child.wait()).await.is_err() {
            #[cfg(unix)]
            if let Some(pid) = self.child.id() {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = self.child.kill().await;
        }
        info!("Jellyfin stopped");
    }
}

/// Poll `/System/Info/Public` until Jellyfin answers.
pub async fn wait_ready(port: u16, proc: &mut JellyfinProcess, timeout: std::time::Duration) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("http://127.0.0.1:{port}/System/Info/Public");
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(r) = client.get(&url).send().await {
            if r.status().is_success() {
                return Ok(());
            }
        }
        if tokio::time::Instant::now() > deadline {
            return Err("Jellyfin did not become ready in time".into());
        }
        tokio::select! {
            status = proc.wait() => return Err(format!("Jellyfin exited during startup ({status})")),
            _ = tokio::time::sleep(std::time::Duration::from_millis(750)) => {}
        }
    }
}

#[cfg(windows)]
mod job {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub struct Job(HANDLE);

    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn kill_on_close() -> Option<Self> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    CloseHandle(handle);
                    return None;
                }
                Some(Self(handle))
            }
        }

        pub fn assign(&self, process: std::os::windows::io::RawHandle) {
            unsafe {
                AssignProcessToJobObject(self.0, process as HANDLE);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
