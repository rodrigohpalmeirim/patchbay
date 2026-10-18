//! Moving bytes between the two ends of a tunneled connection.
//!
//! Both directions are copied concurrently. When one side finishes sending
//! (EOF), the other side's write half is shut down (TCP `FIN` / SSH channel
//! `EOF`) and the opposite direction keeps flowing until it ends too, so
//! half-closed protocols work. Everything is readiness-driven; an idle
//! connection costs no CPU. Bytes are counted as they are read.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::status::StatusCell;

/// Copy buffer per direction.
const BUFFER: usize = 64 * 1024;

#[derive(Clone, Copy)]
enum Direction {
    /// Read from the client: bytes towards the destination.
    Out,
    /// Read from the destination: bytes back to the client.
    In,
}

/// Counts what is read from `inner` into a [`StatusCell`].
struct Metered<S> {
    inner: S,
    cell: Arc<StatusCell>,
    direction: Direction,
}

impl<S: AsyncRead + Unpin> AsyncRead for Metered<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        let n = buf.filled().len() - before;
        if n > 0 {
            match self.direction {
                Direction::Out => self.cell.add_out(n),
                Direction::In => self.cell.add_in(n),
            }
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Metered<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Relay between `client` (the side that initiated the connection) and
/// `destination` until both directions have ended or one fails.
pub(crate) async fn pump<C, D>(client: C, destination: D, cell: &Arc<StatusCell>)
where
    C: AsyncRead + AsyncWrite + Unpin,
    D: AsyncRead + AsyncWrite + Unpin,
{
    let mut client = Metered {
        inner: client,
        cell: cell.clone(),
        direction: Direction::Out,
    };
    let mut destination = Metered {
        inner: destination,
        cell: cell.clone(),
        direction: Direction::In,
    };
    if let Err(e) =
        tokio::io::copy_bidirectional_with_sizes(&mut client, &mut destination, BUFFER, BUFFER)
            .await
    {
        tracing::debug!("tunneled connection ended: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};
    use tokio::sync::broadcast;

    #[tokio::test]
    async fn relays_counts_and_half_closes() {
        let (events, _rx) = broadcast::channel(16);
        let cell = StatusCell::new(
            "fwd:t".into(),
            events,
            crate::model::PortForwardState::Active {
                address: "x".into(),
            },
        );
        let (mut client, client_end) = duplex(1024);
        let (destination_end, mut destination) = duplex(1024);
        let task = tokio::spawn({
            let cell = cell.clone();
            async move { pump(client_end, destination_end, &cell).await }
        });

        client.write_all(b"request").await.unwrap();
        client.shutdown().await.unwrap();
        let mut got = Vec::new();
        destination.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"request", "EOF reaches the destination");

        // The other direction still works after the client's half-close.
        destination.write_all(b"long response").await.unwrap();
        destination.shutdown().await.unwrap();
        let mut got = Vec::new();
        client.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"long response");

        task.await.unwrap();
        let s = cell.snapshot();
        assert_eq!((s.bytes_out, s.bytes_in), (7, 13));
    }
}
