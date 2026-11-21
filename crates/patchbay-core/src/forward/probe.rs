//! Finding out whether a local forward's target speaks HTTP, so the UI can
//! offer to open it in a browser whatever its port.
//!
//! While the forward is active, [`run`] opens a connection of its own to
//! the target (not through the listener, and not counted as traffic) and
//! [`classify`] tells what is there: a server that says something
//! unprompted (SSH, SMTP, MySQL banners) is not HTTP and is sent nothing; a
//! silent one gets a `HEAD /` and is HTTP if its reply starts with `HTTP/`,
//! whatever the status. TLS is not detected: a TLS-only server does not
//! answer a plain request with HTTP.
//!
//! A target that answered either way is never probed again while the
//! forward stays active: a database should not keep getting HTTP requests.
//! Undecided is a target that cannot be connected to (nothing listens there
//! yet), or that hangs up before it was sent anything (a proxy such as
//! docker's or `kubectl port-forward`, whose backend is still starting). It
//! is tried again after each of [`RETRY_DELAYS`], and after those only when
//! a real connection got through ([`StatusCell::kick_probe`]), never more
//! than once per [`MIN_PAUSE`]: every refused attempt shows up in the
//! server's log.

use std::time::Duration;

use russh::ChannelOpenFailure;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;

use super::status::StatusCell;
use crate::{Error, Result};

/// How long a server gets to speak first.
const BANNER_WAIT: Duration = Duration::from_millis(300);
/// Bound for sending the request and reading the start of the reply.
/// Generous, as nothing waits for the probe: what took the request and is
/// still quiet is rather a slow web server (a dev server compiling on its
/// first request) than something else, which would have hung up on it.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound for opening the connection to the target.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// Pauses between attempts while undecided.
const RETRY_DELAYS: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(15),
    Duration::from_secs(30),
];
/// Least pause after an undecided attempt, however often the probe is
/// kicked (clients may be retrying in a loop).
const MIN_PAUSE: Duration = Duration::from_secs(1);
/// How every HTTP reply starts.
const REPLY_START: &[u8] = b"HTTP/";

/// What a connection to the target showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Http,
    NotHttp,
    /// It hung up before it was sent anything: worth another try.
    ClosedEarly,
}

/// Tells what the server at the other end of `stream` (just connected)
/// is. `host` is the target as `host:port`, for the `Host` header.
pub(crate) async fn classify<S>(mut stream: S, host: &str) -> Verdict
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut reply = [0u8; REPLY_START.len()];
    match timeout(BANNER_WAIT, stream.read(&mut reply)).await {
        // Silent, as HTTP servers are: ask.
        Err(_) => {}
        // EOF or an error.
        Ok(Ok(0) | Err(_)) => return Verdict::ClosedEarly,
        // A banner.
        Ok(Ok(_)) => return Verdict::NotHttp,
    }
    let request = format!(
        "HEAD / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Patchbay\r\nAccept: */*\r\n\
         Connection: close\r\n\r\n"
    );
    let exchange = async {
        stream.write_all(request.as_bytes()).await?;
        stream.flush().await?;
        let mut filled = 0;
        while filled < reply.len() {
            match stream.read(&mut reply[filled..]).await? {
                0 => break,
                n => filled += n,
            }
        }
        std::io::Result::Ok(reply[..filled] == *REPLY_START)
    };
    // From here on every outcome is final, also hanging up on the request
    // (Redis does): it is not to be sent twice.
    match timeout(REPLY_TIMEOUT, exchange).await {
        Ok(Ok(true)) => Verdict::Http,
        _ => Verdict::NotHttp,
    }
}

/// Probes the target of `cell`'s forward until it is known whether it
/// speaks HTTP, recording a yes in the cell. `open` connects to the target
/// (`host`, as `host:port`). Returns once decided; otherwise runs until
/// dropped, which the caller does when the forward stops being active.
pub(crate) async fn run<S, O>(cell: &StatusCell, host: &str, mut open: impl FnMut() -> O)
where
    S: AsyncRead + AsyncWrite + Unpin,
    O: Future<Output = Result<S>>,
{
    let mut delays = RETRY_DELAYS.iter();
    loop {
        // Listen before trying, so a kick during the attempt is not lost.
        let kicked = cell.probe_kicked();
        tokio::pin!(kicked);
        kicked.as_mut().enable();

        match timeout(OPEN_TIMEOUT, open()).await {
            Ok(Ok(stream)) => match classify(stream, host).await {
                Verdict::Http => {
                    tracing::debug!(forward = %cell.id(), "probe: {host} speaks HTTP");
                    cell.set_speaks_http();
                    return;
                }
                Verdict::NotHttp => {
                    tracing::debug!(forward = %cell.id(), "probe: {host} does not speak HTTP");
                    return;
                }
                Verdict::ClosedEarly => {
                    tracing::debug!(forward = %cell.id(), "probe: {host} hung up at once");
                }
            },
            // Forwarding is switched off on the server: nothing to wait for.
            Ok(Err(Error::Ssh(russh::Error::ChannelOpenFailure(
                ChannelOpenFailure::AdministrativelyProhibited,
            )))) => {
                tracing::debug!(forward = %cell.id(), "probe: the server refuses to connect to {host}");
                return;
            }
            Ok(Err(e)) => {
                tracing::debug!(forward = %cell.id(), "probe: cannot connect to {host}: {e}");
            }
            Err(_) => {
                tracing::debug!(forward = %cell.id(), "probe: connecting to {host} timed out");
            }
        }

        let pause = tokio::time::sleep(MIN_PAUSE);
        match delays.next() {
            Some(&delay) => {
                let _ = timeout(delay, kicked).await;
            }
            None => kicked.await,
        }
        pause.await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use tokio::io::{DuplexStream, duplex};
    use tokio::sync::broadcast;

    use super::*;
    use crate::model::PortForwardState;

    #[derive(Clone, Copy)]
    enum Server {
        /// Speaks first.
        Banner(&'static [u8]),
        /// Hangs up at once.
        Hangup,
        /// Waits for a request, then hangs up.
        Refuse,
        /// Waits for a request, then sends these chunks and waits.
        Reply(&'static [&'static [u8]]),
    }

    /// A connection to `server`, and everything it will have been sent once
    /// the connection is closed.
    fn connect(server: Server) -> (DuplexStream, tokio::task::JoinHandle<Vec<u8>>) {
        let (client, mut remote) = duplex(1024);
        let received = tokio::spawn(async move {
            let mut seen = Vec::new();
            match server {
                Server::Hangup => return seen,
                Server::Banner(banner) => remote.write_all(banner).await.unwrap(),
                Server::Refuse | Server::Reply(_) => {
                    let mut buf = [0u8; 256];
                    while !seen.ends_with(b"\r\n\r\n") {
                        match remote.read(&mut buf).await.unwrap() {
                            0 => break,
                            n => seen.extend_from_slice(&buf[..n]),
                        }
                    }
                    let Server::Reply(chunks) = server else {
                        return seen;
                    };
                    for chunk in chunks {
                        remote.write_all(chunk).await.unwrap();
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
            let _ = remote.read_to_end(&mut seen).await;
            seen
        });
        (client, received)
    }

    #[tokio::test(start_paused = true)]
    async fn classifies_servers() {
        use Server::{Banner, Hangup, Refuse, Reply};
        use Verdict::{ClosedEarly, Http, NotHttp};
        for (name, server, verdict) in [
            ("ok", Reply(&[b"HTTP/1.1 200 OK\r\n\r\n"]), Http),
            (
                "not found",
                Reply(&[b"HTTP/1.0 404 Not Found\r\n\r\n"]),
                Http,
            ),
            // nginx, asked for plain HTTP on a TLS port.
            (
                "bad request",
                Reply(&[b"HTTP/1.1 400 Bad Request\r\n"]),
                Http,
            ),
            (
                "split reply",
                Reply(&[b"HT", b"TP/1.1 200 OK\r\n\r\n"]),
                Http,
            ),
            (
                "TLS alert",
                Reply(&[b"\x15\x03\x01\x00\x02\x02\x46"]),
                NotHttp,
            ),
            ("cut short", Reply(&[b"HTT"]), NotHttp),
            ("silent", Reply(&[]), NotHttp),
            // Redis.
            ("hangs up on the request", Refuse, NotHttp),
            ("ssh", Banner(b"SSH-2.0-OpenSSH_9.2\r\n"), NotHttp),
            // Even one that looks like a reply: HTTP servers wait.
            ("eager", Banner(b"HTTP/1.1 200 OK\r\n\r\n"), NotHttp),
            ("hangs up at once", Hangup, ClosedEarly),
        ] {
            let (stream, received) = connect(server);
            assert_eq!(
                classify(stream, "db.internal:8765").await,
                verdict,
                "{name}"
            );
            let received = received.await.unwrap();
            match server {
                Banner(_) | Hangup => assert!(received.is_empty(), "{name}: sent {received:?}"),
                Refuse | Reply(_) => assert!(
                    received.starts_with(b"HEAD / HTTP/1.1\r\nHost: db.internal:8765\r\n")
                        && received.ends_with(b"\r\n\r\n"),
                    "{name}: sent {received:?}"
                ),
            }
        }
    }

    fn cell() -> Arc<StatusCell> {
        let address = "127.0.0.1:1".into();
        StatusCell::new(
            "fwd:t".into(),
            broadcast::channel(16).0,
            PortForwardState::Active { address },
        )
    }

    #[tokio::test(start_paused = true)]
    async fn retries_an_unreachable_target_then_only_when_kicked() {
        let cell = cell();
        let up = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(AtomicU32::new(0));
        let task = tokio::spawn({
            let (cell, up, attempts) = (cell.clone(), up.clone(), attempts.clone());
            async move {
                let open = || {
                    let n = attempts.fetch_add(1, Ordering::Relaxed);
                    let up = up.load(Ordering::Relaxed);
                    async move {
                        if up {
                            return Ok(connect(Server::Reply(&[b"HTTP/1.1 200 OK\r\n\r\n"])).0);
                        }
                        // Down: refused, or (every other time) no answer.
                        if n % 2 == 1 {
                            std::future::pending::<()>().await;
                        }
                        Err(
                            russh::Error::ChannelOpenFailure(ChannelOpenFailure::ConnectFailed)
                                .into(),
                        )
                    }
                };
                run(&cell, "localhost:8765", open).await
            }
        });
        let attempts_after = async |wait: u64| {
            tokio::time::sleep(Duration::from_secs(wait)).await;
            attempts.load(Ordering::Relaxed)
        };

        assert_eq!(attempts_after(2).await, 2, "retries soon");
        let tries = 1 + RETRY_DELAYS.len() as u32;
        assert_eq!(attempts_after(3600).await, tries, "then gives up");

        cell.kick_probe();
        assert_eq!(attempts_after(3600).await, tries + 1, "one more per kick");
        assert!(!task.is_finished() && !cell.snapshot().speaks_http);

        up.store(true, Ordering::Relaxed);
        cell.kick_probe();
        task.await.unwrap();
        assert!(cell.snapshot().speaks_http);
        assert_eq!(attempts.load(Ordering::Relaxed), tries + 2);
    }

    #[tokio::test(start_paused = true)]
    async fn kicks_retry_a_target_that_hangs_up_at_most_once_a_second() {
        let cell = cell();
        let attempts = Arc::new(AtomicU32::new(0));
        let task = tokio::spawn({
            let (cell, attempts) = (cell.clone(), attempts.clone());
            async move {
                let open = || {
                    attempts.fetch_add(1, Ordering::Relaxed);
                    async { Ok(connect(Server::Hangup).0) }
                };
                run(&cell, "localhost:8765", open).await
            }
        });
        // Ten kicks a second: one attempt at the start, then one per second,
        // where the timed retries alone would make four.
        for _ in 0..95 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cell.kick_probe();
        }
        assert_eq!(attempts.load(Ordering::Relaxed), 10);
        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_is_final() {
        // A target that is not HTTP, and a server that forwards nothing.
        for prohibited in [false, true] {
            let cell = cell();
            let mut attempts = 0;
            let open = || {
                attempts += 1;
                async move {
                    if prohibited {
                        let failure = ChannelOpenFailure::AdministrativelyProhibited;
                        return Err(russh::Error::ChannelOpenFailure(failure).into());
                    }
                    Ok(connect(Server::Banner(b"220 mail ESMTP\r\n")).0)
                }
            };
            // Returning is what ends the probing: later kicks reach nobody.
            run(&cell, "localhost:25", open).await;
            assert_eq!(attempts, 1, "prohibited: {prohibited}");
            assert!(!cell.snapshot().speaks_http);
        }
    }
}
