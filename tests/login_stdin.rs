//! The login path must hand PIA credentials to `pkexec vexos-vpn login
//! --stdin` on stdin only — never in argv or the environment.
//!
//! A fake pkexec (a /bin/sh script) records its argv, environment and stdin
//! into a temp directory and exits with a chosen code.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use vex_vpn::cli::{validate, Credentials, PrivOutcome, Privileged};

/// Tests run in parallel threads: writing one test's script while another
/// test forks makes exec fail with ETXTBSY ("Text file busy"). Serialize.
static SPAWN_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const USER: &str = "p1234567";
const PASS: &str = "s3cr3t-Pa55word";

fn fake_pkexec(dir: &Path, exit: i32, stdout: &str, stderr: &str) -> Privileged {
    let script = dir.join("pkexec");
    let body = format!(
        "#!/bin/sh\n\
         printf '%s\\n' \"$@\" > '{d}/argv'\n\
         env > '{d}/env'\n\
         cat > '{d}/stdin'\n\
         printf '%s' '{out}'\n\
         printf '%s' '{err}' >&2\n\
         exit {exit}\n",
        d = dir.display(),
        out = stdout,
        err = stderr,
        exit = exit,
    );
    std::fs::write(&script, body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    Privileged {
        pkexec: script,
        cli: PathBuf::from("/run/current-system/sw/bin/vexos-vpn"),
    }
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap()
}

fn creds() -> Credentials {
    Credentials::new(USER.to_string(), PASS.to_string()).unwrap()
}

#[tokio::test]
async fn login_writes_credentials_to_stdin_only() {
    let _lock = SPAWN_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let p = fake_pkexec(dir.path(), 0, "", "");

    let outcome = p.login(creds()).await.unwrap();
    assert_eq!(
        outcome,
        PrivOutcome::Ok {
            stdout: String::new()
        }
    );

    assert_eq!(read(dir.path(), "stdin"), format!("{USER}\n{PASS}\n"));

    let argv = read(dir.path(), "argv");
    assert_eq!(
        argv.lines().collect::<Vec<_>>(),
        ["/run/current-system/sw/bin/vexos-vpn", "login", "--stdin"]
    );

    let env = read(dir.path(), "env");
    for secret in [USER, PASS] {
        assert!(!argv.contains(secret), "secret in argv");
        assert!(!env.contains(secret), "secret in environment");
    }
}

#[tokio::test]
async fn dismissed_or_denied_auth_is_cancelled() {
    let _lock = SPAWN_LOCK.lock().await;
    for code in [126, 127] {
        let dir = tempfile::tempdir().unwrap();
        let p = fake_pkexec(dir.path(), code, "", "");
        assert_eq!(
            p.login(creds()).await.unwrap(),
            PrivOutcome::Cancelled { no_agent: false }
        );
    }
}

#[tokio::test]
async fn missing_polkit_agent_is_reported() {
    let _lock = SPAWN_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let p = fake_pkexec(
        dir.path(),
        127,
        "",
        "Error executing command as another user: No authentication agent found.",
    );
    assert_eq!(
        p.logout().await.unwrap(),
        PrivOutcome::Cancelled { no_agent: true }
    );
}

#[tokio::test]
async fn failure_carries_stderr() {
    let _lock = SPAWN_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let p = fake_pkexec(
        dir.path(),
        1,
        "",
        "vexos-vpn: error: credentials are managed by the NixOS config",
    );
    match p.login(creds()).await.unwrap() {
        PrivOutcome::Failed { code, stderr, .. } => {
            assert_eq!(code, 1);
            assert_eq!(stderr, "credentials are managed by the NixOS config");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn selftest_returns_stdout() {
    let _lock = SPAWN_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let p = fake_pkexec(dir.path(), 0, "PIA login:       OK", "");
    assert_eq!(
        p.selftest().await.unwrap(),
        PrivOutcome::Ok {
            stdout: "PIA login:       OK".to_string()
        }
    );
    assert_eq!(
        read(dir.path(), "argv").lines().collect::<Vec<_>>(),
        ["/run/current-system/sw/bin/vexos-vpn", "selftest"]
    );
}

#[test]
fn invalid_credentials_are_rejected_before_spawning() {
    assert!(validate("", PASS).is_err());
    assert!(validate(USER, "").is_err());
    for (u, p) in [
        ("a\nb", PASS),
        (USER, "line\nbreak"),
        (USER, "cr\rpass"),
        ("nul\0", PASS),
    ] {
        assert!(validate(u, p).is_err());
        assert!(Credentials::new(u.to_string(), p.to_string()).is_err());
    }
    assert!(validate(USER, PASS).is_ok());
}
