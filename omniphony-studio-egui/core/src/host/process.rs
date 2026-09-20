//! Bounded capture for small host utility commands (service status, discovery).
//! File-backed capture avoids pipe deadlocks and reader threads retained by a
//! descendant inheriting stdout. Only a bounded prefix is retained in memory.
use std::{
    io::{self, Read, Seek, SeekFrom},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

pub fn capture(command: &mut Command, timeout: Duration) -> io::Result<Output> {
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?))
        .spawn()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match result {
                    Err(error) => error,
                    _ => io::Error::new(io::ErrorKind::TimedOut, "host command timed out"),
                });
            }
        }
    };
    fn prefix(file: &mut std::fs::File) -> io::Result<Vec<u8>> {
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        file.take(64 * 1024).read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    Ok(Output {
        status,
        stdout: prefix(&mut stdout)?,
        stderr: prefix(&mut stderr)?,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn captures_both_streams_and_kills_a_timed_out_child() {
        let output = capture(
            Command::new("sh").args(["-c", "printf out; printf err >&2"]),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
        let error = capture(
            Command::new("sh").args(["-c", "exec sleep 2"]),
            Duration::from_millis(30),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
