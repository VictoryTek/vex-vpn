//! Running the `vexos-vpn` CLI: unprivileged reads (`status`, `regions`) and
//! root-only actions through `pkexec` (polkit admin prompt).
//!
//! Credentials are only ever written to the child's stdin — never to argv,
//! the environment, the log or disk — and our own copies are wiped after use.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::vexos::{parse_regions, Region, Status};

/// setuid pkexec wrapper on NixOS.
pub const PKEXEC: &str = "/run/wrappers/bin/pkexec";
/// Stable path of the system's vexos-vpn (pkexec needs an absolute path).
pub const SYSTEM_CLI: &str = "/run/current-system/sw/bin/vexos-vpn";

const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Locate `vexos-vpn`: first on `PATH`, then the system profile.
pub fn find_backend() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("vexos-vpn"))
            .find(|p| is_executable(p))
    });
    on_path.or_else(|| {
        let p = PathBuf::from(SYSTEM_CLI);
        is_executable(&p).then_some(p)
    })
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// `vexos-vpn status --json` (unprivileged).
pub async fn status(cli: &Path) -> Result<Status> {
    Status::parse(&run_unprivileged(cli, &["status", "--json"]).await?)
}

/// `vexos-vpn regions --json` (unprivileged). Fails with the CLI's stderr when
/// there is no cached server list yet.
pub async fn regions(cli: &Path) -> Result<Vec<Region>> {
    parse_regions(&run_unprivileged(cli, &["regions", "--json"]).await?)
}

async fn run_unprivileged(cli: &Path, args: &[&str]) -> Result<String> {
    let child = Command::new(cli)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("run {}", cli.display()))?;
    let out = tokio::time::timeout(READ_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| anyhow!("vexos-vpn {} timed out", args[0]))??;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("{}", clean_message(&stderr)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Strip the CLI's `vexos-vpn: error: ` prefix for display.
pub fn clean_message(stderr: &str) -> String {
    stderr
        .lines()
        .map(|l| {
            l.trim()
                .trim_start_matches("vexos-vpn: ")
                .trim_start_matches("error: ")
        })
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

/// PIA username + password, held only as long as needed and wiped on drop.
/// Deliberately implements neither `Debug` nor `Display`.
pub struct Credentials {
    user: String,
    pass: String,
}

/// Why a username/password pair cannot be sent to `vexos-vpn login --stdin`.
pub fn validate(user: &str, pass: &str) -> Result<(), &'static str> {
    if user.is_empty() || pass.is_empty() {
        return Err("Username and password are required");
    }
    let bad = |s: &str| s.contains(['\n', '\r', '\0']);
    if bad(user) || bad(pass) {
        return Err("Username and password cannot contain line breaks");
    }
    Ok(())
}

impl Credentials {
    /// Takes ownership of both strings; they are wiped even if invalid.
    pub fn new(user: String, pass: String) -> Result<Self, &'static str> {
        let creds = Self { user, pass };
        validate(&creds.user, &creds.pass)?;
        Ok(creds)
    }

    /// `user\npass\n` — the format `vexos-vpn login --stdin` reads. Exact
    /// capacity so the buffer never reallocates (which would leave a copy).
    fn payload(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.user.len() + self.pass.len() + 2);
        buf.extend_from_slice(self.user.as_bytes());
        buf.push(b'\n');
        buf.extend_from_slice(self.pass.as_bytes());
        buf.push(b'\n');
        buf
    }
}

impl Drop for Credentials {
    fn drop(&mut self) {
        // SAFETY: only zero bytes are written, which is valid UTF-8.
        unsafe {
            wipe(self.user.as_mut_vec());
            wipe(self.pass.as_mut_vec());
        }
    }
}

/// Overwrite a buffer with zeros in a way the optimiser may not elide.
pub fn wipe(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: `b` is a valid, aligned, exclusive reference.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// Privileged actions (pkexec)
// ---------------------------------------------------------------------------

/// Result of a `pkexec vexos-vpn …` run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrivOutcome {
    Ok {
        stdout: String,
    },
    /// pkexec exit 126 (dialog dismissed) or 127 (not authorized). `no_agent`
    /// is set when no polkit authentication agent is running. (127 with
    /// "No such file or directory" means the backend is missing: `Failed`.)
    Cancelled {
        no_agent: bool,
    },
    Failed {
        code: i32,
        stdout: String,
        stderr: String,
    },
}

/// Paths used to run root-only vexos-vpn commands. Tests inject fakes.
#[derive(Debug, Clone)]
pub struct Privileged {
    pub pkexec: PathBuf,
    pub cli: PathBuf,
}

impl Default for Privileged {
    fn default() -> Self {
        Self {
            pkexec: PathBuf::from(PKEXEC),
            cli: PathBuf::from(SYSTEM_CLI),
        }
    }
}

impl Privileged {
    /// `pkexec vexos-vpn login --stdin`, credentials on stdin only.
    pub async fn login(&self, creds: Credentials) -> Result<PrivOutcome> {
        let mut payload = creds.payload();
        drop(creds);
        let result = self.run(&["login", "--stdin"], Some(&payload)).await;
        wipe(&mut payload);
        result
    }

    pub async fn logout(&self) -> Result<PrivOutcome> {
        self.run(&["logout"], None).await
    }

    pub async fn selftest(&self) -> Result<PrivOutcome> {
        self.run(&["selftest"], None).await
    }

    pub async fn refresh(&self) -> Result<PrivOutcome> {
        self.run(&["refresh"], None).await
    }

    async fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<PrivOutcome> {
        // The command line is fixed before any secret is touched.
        let mut cmd = Command::new(&self.pkexec);
        cmd.arg(&self.cli)
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Never kill a half-finished privileged action.
            .kill_on_drop(false);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("run {}", self.pkexec.display()))?;

        if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
            // If pkexec exits early (auth dismissed) the pipe may be closed;
            // the exit code below tells the real story.
            if let Err(e) = pipe.write_all(data).await {
                if e.kind() != std::io::ErrorKind::BrokenPipe {
                    return Err(e).context("write to pkexec stdin");
                }
            }
            drop(pipe); // EOF
        }

        let out = child.wait_with_output().await.context("wait for pkexec")?;
        let code = out.status.code().unwrap_or(-1);
        tracing::info!("pkexec vexos-vpn {}: exit {}", args[0], code);
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        Ok(match code {
            0 => PrivOutcome::Ok { stdout },
            126 | 127 if !stderr.contains("No such file or directory") => PrivOutcome::Cancelled {
                no_agent: stderr.contains("No authentication agent"),
            },
            _ => PrivOutcome::Failed {
                code,
                stdout,
                stderr: clean_message(&stderr),
            },
        })
    }
}
