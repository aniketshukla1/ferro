//! `ferro ssh` remote bootstrap (B8b).

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ferro_core::dirs::FerroDirs;
use ferro_server::guard;

use super::{
    cached_binary_path, parse_ssh_target, parse_uname_sm, release_triple, ReleaseTriple,
    SshParseError, SshTarget,
};

#[derive(Debug, thiserror::Error)]
pub enum SshError {
    #[error(transparent)]
    Parse(#[from] SshParseError),
    #[error("ssh failed: {0}")]
    Ssh(String),
    #[error("scp failed: {0}")]
    Scp(String),
    #[error("no cached ferro binary for {triple} (run `ferro --update` or set FERRO_SSH_BINARY)")]
    MissingBinary { triple: String },
    #[error("remote ferro did not start within {secs}s")]
    RemoteStartTimeout { secs: u64 },
    #[error("could not parse ferro URL from remote log")]
    BadReadyLine,
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct RemoteBootstrap {
    pub dest: String,
    pub stage: String,
    pub session_token: String,
    pub remote_port: u16,
    pub ready_line: String,
}

pub fn setup_remote_bootstrap(target_raw: &str) -> Result<RemoteBootstrap, SshError> {
    let target = parse_ssh_target(target_raw)?;
    let dest = ssh_dest(&target);
    let remote_root = target
        .remote_path
        .clone()
        .unwrap_or_else(|| ".".to_string());

    let uname_out = ssh_capture(&dest, "uname -sm")?;
    let uname = parse_uname_sm(&uname_out)?;
    let triple = release_triple(&uname)?;
    let dirs = FerroDirs::resolve();
    let local_bin = resolve_local_binary(&dirs, triple)?;

    let token = guard::generate_token();
    let session = std::process::id();
    let stage = format!("$HOME/.ferro-ssh-{session}");

    ssh_run(&dest, &format!("mkdir -p {stage} && chmod 700 {stage}"))?;

    scp_to(&local_bin, &format!("{dest}:{stage}/ferro"))?;
    upload_secret_file(&dest, &stage, "token", token.as_bytes())?;
    upload_start_script(&dest, &stage, &remote_root)?;

    ssh_run(
        &dest,
        &format!(
            "chmod +x {stage}/ferro {stage}/start.sh && nohup {stage}/start.sh </dev/null >/dev/null 2>&1 &"
        ),
    )?;

    let log = wait_for_log(&dest, &stage, 30)?;
    let line = log
        .lines()
        .find(|l| l.trim_start().starts_with("ferro http://"))
        .ok_or(SshError::BadReadyLine)?
        .to_string();
    let (remote_port, _) = parse_ferro_ready_line(&line).ok_or(SshError::BadReadyLine)?;

    Ok(RemoteBootstrap {
        dest,
        stage,
        session_token: token,
        remote_port,
        ready_line: line,
    })
}

pub async fn run_ssh(target_raw: &str, no_open: bool) -> Result<(), SshError> {
    let boot = setup_remote_bootstrap(target_raw)?;
    let local_port = pick_local_port()?;
    let browser_url =
        local_browser_url(&boot.ready_line, local_port).ok_or(SshError::BadReadyLine)?;

    let mut tunnel = spawn_tunnel(&boot.dest, local_port, boot.remote_port)?;

    if !no_open {
        let _ = open::that(&browser_url);
    } else {
        println!("ferro {browser_url}");
    }

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("ferro ssh: interrupted, cleaning up remote…");
        }
    }
    let _ = tunnel.kill();
    let _ = tunnel.wait();
    cleanup_remote(&boot.dest, &boot.stage)?;
    Ok(())
}

pub fn ssh_dest(target: &SshTarget) -> String {
    match &target.user {
        Some(u) => format!("{u}@{}", target.host),
        None => target.host.clone(),
    }
}

pub fn parse_ferro_ready_line(line: &str) -> Option<(u16, String)> {
    let line = line.trim();
    let url = line.strip_prefix("ferro ")?;
    let url = url.strip_prefix("http://")?;
    let (host_port, tail) = url.split_once('/')?;
    let (_, port_s) = host_port.rsplit_once(':')?;
    let port: u16 = port_s.parse().ok()?;
    let tail = tail.trim_start_matches('/');
    let path = if tail.is_empty() {
        String::new()
    } else {
        format!("/{tail}")
    };
    Some((port, format!("http://127.0.0.1:{port}{path}")))
}

pub fn local_browser_url(ready_line: &str, local_port: u16) -> Option<String> {
    let (_, remote_url) = parse_ferro_ready_line(ready_line)?;
    let query = remote_url.split_once('?').map(|(_, q)| q).unwrap_or("");
    if query.is_empty() {
        Some(format!("http://127.0.0.1:{local_port}/"))
    } else {
        Some(format!("http://127.0.0.1:{local_port}/?{query}"))
    }
}

pub fn remote_start_script(stage: &str, remote_root: &str) -> String {
    let root = remote_root.replace('\'', "'\\''");
    format!(
        "#!/bin/sh\n\
set -e\n\
STAGE={stage}\n\
export FERRO_TOKEN=\"$(cat \"$STAGE/token\")\"\n\
\"$STAGE/ferro\" --no-open --port 0 --host 127.0.0.1 '{root}' >\"$STAGE/log\" 2>&1 &\n\
echo $! >\"$STAGE/pid\"\n"
    )
}

pub fn cleanup_remote(dest: &str, stage: &str) -> Result<(), SshError> {
    ssh_run(
        dest,
        &format!(
            "if [ -f {stage}/pid ]; then kill \"$(cat {stage}/pid)\" 2>/dev/null || true; fi; \
             rm -rf {stage}"
        ),
    )
}

/// True when `ps` output on the remote would not contain the raw token in argv (AC4).
pub fn remote_argv_hides_token(ps_line: &str, token: &str) -> bool {
    !ps_line.contains(token) && !ps_line.contains("--token")
}

fn resolve_local_binary(dirs: &FerroDirs, triple: ReleaseTriple) -> Result<PathBuf, SshError> {
    let path = cached_binary_path(dirs, triple.as_str());
    if path.is_file() {
        return Ok(path);
    }
    if let Ok(p) = std::env::var("FERRO_SSH_BINARY") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
    }
    let local_uname = local_uname()?;
    if release_triple(&local_uname)? == triple {
        let exe = std::env::current_exe().map_err(|e| SshError::Other(e.to_string()))?;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
            let _ = std::fs::copy(&exe, &path);
        }
        if path.is_file() {
            return Ok(path);
        }
        return Ok(exe);
    }
    Err(SshError::MissingBinary {
        triple: triple.as_str().to_string(),
    })
}

fn local_uname() -> Result<super::UnameSm, SshParseError> {
    let out = Command::new("uname")
        .args(["-sm"])
        .output()
        .map_err(|_| SshParseError::MissingHost)?;
    parse_uname_sm(&String::from_utf8_lossy(&out.stdout))
}

fn ssh_base_args() -> Vec<&'static str> {
    vec![
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=30",
        "-o",
        "StrictHostKeyChecking=accept-new",
    ]
}

pub fn ssh_capture(dest: &str, remote_cmd: &str) -> Result<String, SshError> {
    let out = Command::new("ssh")
        .args(ssh_base_args())
        .arg(dest)
        .arg(remote_cmd)
        .output()
        .map_err(|e| SshError::Ssh(e.to_string()))?;
    if !out.status.success() {
        return Err(SshError::Ssh(format!(
            "{} (status {})",
            String::from_utf8_lossy(&out.stderr).trim(),
            out.status
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn ssh_run(dest: &str, remote_cmd: &str) -> Result<(), SshError> {
    let status = Command::new("ssh")
        .args(ssh_base_args())
        .arg(dest)
        .arg(remote_cmd)
        .status()
        .map_err(|e| SshError::Ssh(e.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(SshError::Ssh(format!("exit {status}")))
    }
}

fn scp_to(local: &Path, dest_path: &str) -> Result<(), SshError> {
    let status = Command::new("scp")
        .args(ssh_base_args())
        .arg(local)
        .arg(dest_path)
        .status()
        .map_err(|e| SshError::Scp(e.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(SshError::Scp(format!("exit {status}")))
    }
}

fn upload_secret_file(dest: &str, stage: &str, name: &str, bytes: &[u8]) -> Result<(), SshError> {
    let scratch = std::env::var("PAPERCLIP_TASK_SCRATCH_DIR")
        .or_else(|_| std::env::var("PAPERCLIP_SCRATCH_DIR"))
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let local = PathBuf::from(scratch).join(format!("ferro-ssh-{name}-{}", std::process::id()));
    std::fs::write(&local, bytes).map_err(|e| SshError::Other(e.to_string()))?;
    let remote = format!("{dest}:{stage}/{name}");
    let result = scp_to(&local, &remote);
    let _ = std::fs::remove_file(&local);
    result?;
    ssh_run(dest, &format!("chmod 600 {stage}/{name}"))
}

fn upload_start_script(dest: &str, stage: &str, remote_root: &str) -> Result<(), SshError> {
    let body = remote_start_script(stage, remote_root);
    let scratch = std::env::var("PAPERCLIP_TASK_SCRATCH_DIR")
        .or_else(|_| std::env::var("PAPERCLIP_SCRATCH_DIR"))
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let local = PathBuf::from(scratch).join(format!("ferro-ssh-start-{}", std::process::id()));
    std::fs::write(&local, body).map_err(|e| SshError::Other(e.to_string()))?;
    let remote = format!("{dest}:{stage}/start.sh");
    let result = scp_to(&local, &remote);
    let _ = std::fs::remove_file(&local);
    result
}

fn wait_for_log(dest: &str, stage: &str, timeout_secs: u64) -> Result<String, SshError> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        let out =
            ssh_capture(dest, &format!("cat {stage}/log 2>/dev/null || true")).unwrap_or_default();
        if out.lines().any(|l| l.contains("ferro http://")) {
            return Ok(out);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(SshError::RemoteStartTimeout { secs: timeout_secs })
}

fn pick_local_port() -> Result<u16, SshError> {
    TcpListener::bind("127.0.0.1:0")
        .map_err(|e| SshError::Other(e.to_string()))
        .map(|l| l.local_addr().map(|a| a.port()).unwrap_or(0))
}

fn spawn_tunnel(dest: &str, local_port: u16, remote_port: u16) -> Result<Child, SshError> {
    let spec = format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}");
    Command::new("ssh")
        .args(ssh_base_args())
        .arg("-N")
        .arg("-L")
        .arg(&spec)
        .arg(dest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| SshError::Ssh(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ready_line() {
        let line = "ferro http://127.0.0.1:9123/?token=abc&file=x";
        let (port, url) = parse_ferro_ready_line(line).unwrap();
        assert_eq!(port, 9123);
        assert!(url.contains("token=abc"));
    }

    #[test]
    fn local_browser_rewrites_port() {
        let line = "ferro http://127.0.0.1:5000/?token=sekret";
        let u = local_browser_url(line, 7000).unwrap();
        assert_eq!(u, "http://127.0.0.1:7000/?token=sekret");
    }

    #[test]
    fn start_script_uses_env_not_argv_token() {
        let s = remote_start_script("$HOME/.ferro-ssh-1", "/repo");
        assert!(!s.contains("--token"));
        assert!(s.contains("FERRO_TOKEN"));
    }
}
