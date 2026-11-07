//! The one place that knows how invocations talk to each other: a single
//! per-user Unix socket. The terminal listens; everything else is a client —
//! `wl-dropdown toggle`, and the settings process announcing a write.
//!
//! The client is plain `std`, with no Wayland connection, so forwarding a
//! command works wherever the socket is reachable.

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use crate::paths;

/// Sends one request to the running terminal. Returns false when nothing is
/// listening, which is not an error: the caller may be the first invocation,
/// or the settings window running on its own.
pub fn send(request: &[u8]) -> bool {
    send_to(&paths::socket_path(), request)
}

pub fn send_to(path: &Path, request: &[u8]) -> bool {
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false; // stale socket, or no terminal running
    };
    if stream.write_all(request).is_err() {
        return false;
    }
    // Half-close so the peer sees end-of-input and acts before we drop the
    // connection entirely.
    let _ = stream.shutdown(Shutdown::Write);
    true
}

/// Binds the control socket. Only called after failing to reach a live
/// instance, so a socket file already there is stale and safe to replace.
pub fn listen(path: &Path) -> io::Result<UnixListener> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Reads one complete request from an accepted connection. An `-e` command
/// line is not guaranteed to arrive in a single read, so this reads until the
/// client half-closes. Clients write and close immediately, so the timeout
/// only matters for a misbehaving peer.
pub fn read_request(mut stream: UnixStream) -> io::Result<Vec<u8>> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Accepts every pending connection and returns their requests.
pub fn drain(listener: &UnixListener) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        match listener.accept() {
            Ok((stream, _)) => match read_request(stream) {
                Ok(req) => out.push(req),
                Err(err) => log::warn!("control socket: {err}"),
            },
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
            Err(err) => {
                log::warn!("control socket: {err}");
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_without_listener_is_false() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!send_to(&dir.path().join("none.sock"), b"toggle"));
    }

    #[test]
    fn roundtrip_through_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sock");
        // A stale file in the way is replaced.
        std::fs::write(&path, b"stale").unwrap();
        let listener = listen(&path).unwrap();

        assert!(send_to(&path, b"show"));
        // Larger than a socket buffer, so it cannot be written before the
        // server starts reading.
        let big = vec![b'x'; 300_000];
        let sender = {
            let (path, big) = (path.clone(), big.clone());
            std::thread::spawn(move || send_to(&path, &big))
        };

        // Accepting is non-blocking; give the kernel a moment to queue both.
        let mut got = Vec::new();
        for _ in 0..50 {
            got.extend(drain(&listener));
            if got.len() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(sender.join().unwrap());
        assert_eq!(got, vec![b"show".to_vec(), big]);
    }
}
