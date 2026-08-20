//! A connected `SOCK_SEQPACKET` pair, one half driven by tokio readiness.
//!
//! `SOCK_SEQPACKET` is what a datagram API wants from a local socket: it keeps
//! message boundaries, it is flow controlled, and it reports end of file when
//! the peer closes. `SOCK_STREAM` loses the boundaries and `SOCK_DGRAM` gives a
//! weaker close signal.
//!
//! Tokio ships no type for it — `UnixStream` is `SOCK_STREAM` and
//! `UnixDatagram` is `SOCK_DGRAM` — so the daemon's half is driven through
//! [`AsyncFd`], which is tokio's supported way to put an arbitrary file
//! descriptor under the reactor.
//!
//! **Not available on macOS**, which does not implement `SOCK_SEQPACKET` for
//! `AF_UNIX`. Everything else this module needs does work there: `SCM_RIGHTS`
//! is supported, and `AsyncFd` is backed by kqueue. So a macOS port is a change
//! of socket type to `SOCK_DGRAM`, which macOS does support and which also
//! keeps message boundaries, plus one thing that has to be **measured on a Mac
//! rather than assumed**: whether a closed peer on a connected `SOCK_DGRAM`
//! pair is distinguishable from an empty datagram. The `POLLHUP` result below
//! was measured on Linux and BSD poll semantics for datagram sockets differ.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;
use tokio::io::unix::AsyncFd;

/// What one receive attempt produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Received {
    /// A datagram of this many bytes. Zero is a legitimate value: a client may
    /// send an empty datagram, and that is not the same as closing.
    Datagram(usize),
    /// The peer closed its half of the pair.
    Eof,
}

/// Create a connected `SOCK_SEQPACKET` pair.
///
/// Both descriptors are close-on-exec so neither leaks into a child process.
/// Neither is set non-blocking here: the two halves are independent sockets, so
/// [`Seqpacket::new`] can make the daemon's half non-blocking for the reactor
/// while the half handed to the client stays blocking, which is what a client
/// calling `recv` in a loop expects.
pub fn pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` is a two-element array of the type socketpair writes, and
    // the return value is checked before either descriptor is read.
    let rc = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: socketpair reported success, so both entries are open
    // descriptors this process now owns.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Size the send buffer of `fd`, in bytes.
///
/// Used on the daemon's half of a listener's pair, where the buffer is the only
/// bound on arrivals a client has stopped reading. `SO_SNDBUF` on an `AF_UNIX`
/// socket accounts bytes plus per-message overhead rather than messages, so a
/// caller converting a message count to bytes is approximating and should
/// approximate generously. Linux doubles what it is given and clamps to its own
/// minimum, which is why nothing here reads the value back and asserts on it.
pub fn set_sndbuf(fd: &OwnedFd, bytes: usize) -> io::Result<()> {
    let size = libc::c_int::try_from(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "send buffer size too large"))?;
    // SAFETY: the descriptor is open for the call, and the pointer and length
    // describe one `c_int` that outlives it.
    let rc = unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            std::ptr::addr_of!(size).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The daemon's half of a flow's or a listener's socket pair, registered with
/// the reactor.
pub struct Seqpacket {
    inner: AsyncFd<OwnedFd>,
}

impl Seqpacket {
    /// Put `fd` under the reactor, making it non-blocking first.
    ///
    /// `AsyncFd` requires a non-blocking descriptor: a blocking one would stall
    /// the whole runtime thread inside a syscall the reactor believed would
    /// return at once.
    pub fn new(fd: OwnedFd) -> io::Result<Self> {
        set_nonblocking(fd.as_raw_fd(), true)?;
        Ok(Self {
            inner: AsyncFd::new(fd)?,
        })
    }

    /// The raw descriptor, for a caller that must perform its own syscall.
    ///
    /// The one such caller is the listener's task, which needs a send that
    /// reports a full buffer rather than waiting for one; see
    /// [`try_send`](super::fdpass::try_send). Everything else goes through
    /// [`Seqpacket::send`] and [`Seqpacket::recv`].
    pub(super) fn raw(&self) -> RawFd {
        self.inner.get_ref().as_raw_fd()
    }

    /// Receive one datagram, or report that the peer closed.
    ///
    /// A datagram longer than `buf` is truncated and the remainder discarded,
    /// which is `SOCK_SEQPACKET` behaviour. Callers size `buf` at the largest
    /// payload the API accepts, so a truncation means the client exceeded it.
    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<Received> {
        loop {
            let mut guard = self.inner.readable().await?;
            let attempt = guard.try_io(|inner| recv_once(inner.get_ref().as_raw_fd(), buf));
            match attempt {
                Ok(result) => return result,
                // The reactor said readable and the syscall disagreed. Clear
                // the readiness and wait again rather than reporting an error.
                Err(_would_block) => continue,
            }
        }
    }

    /// Send one datagram.
    ///
    /// `SOCK_SEQPACKET` delivers it whole or not at all, so a short write is
    /// not a case the caller has to handle.
    pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        loop {
            let mut guard = self.inner.writable().await?;
            let attempt = guard.try_io(|inner| {
                // SAFETY: the descriptor is owned and open, and the pointer and
                // length describe `buf`.
                let n = unsafe {
                    libc::send(
                        inner.get_ref().as_raw_fd(),
                        buf.as_ptr().cast(),
                        buf.len(),
                        libc::MSG_NOSIGNAL,
                    )
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            });
            match attempt {
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }
}

/// One receive, distinguishing an empty datagram from end of file.
///
/// Both produce a zero-byte read, so something else has to tell them apart.
/// `MSG_EOR` is the technique the manual pages suggest and it **does not work
/// here**: measured on Linux 6.8, `recvmsg` on an `AF_UNIX` `SOCK_SEQPACKET`
/// socket returns `msg_flags == 0` for a normal message, for an empty message
/// and at end of file alike, so the flag carries no information.
///
/// `POLLHUP` does discriminate, measured the same way. After a zero-byte read,
/// a queued empty datagram leaves the socket with no events pending, while a
/// closed peer leaves `POLLHUP` set and latched. So a zero-byte read is end of
/// file only when the peer has hung up.
///
/// This matters because reading an empty datagram as a close would let a client
/// tear down its own flow by sending nothing, and the defect would present as a
/// spurious disconnect.
fn recv_once(fd: RawFd, buf: &mut [u8]) -> io::Result<Received> {
    // SAFETY: the descriptor is open and the pointer and length describe `buf`.
    let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    if n == 0 && peer_hung_up(fd) {
        return Ok(Received::Eof);
    }
    Ok(Received::Datagram(n as usize))
}

/// Whether the peer has closed its half of the pair.
///
/// `events` is left empty on purpose: `POLLHUP` is reported in `revents`
/// whether or not it was requested, which was confirmed by measurement rather
/// than assumed. The poll does not block, and runs only on the zero-byte path.
///
/// Visible within [`super`] because the client half needs the same
/// discrimination on the same socket pair: the rule belongs to the pair, not to
/// the end that reads it.
pub(super) fn peer_hung_up(fd: RawFd) -> bool {
    let mut poll = libc::pollfd {
        fd,
        events: 0,
        revents: 0,
    };
    // SAFETY: `poll` points at one live pollfd and the call cannot block.
    let rc = unsafe { libc::poll(&mut poll, 1, 0) };
    rc > 0 && (poll.revents & libc::POLLHUP) != 0
}

/// Set or clear a receive or send timeout on a socket.
///
/// `option` is `SO_RCVTIMEO` or `SO_SNDTIMEO`. `None` clears the timeout, which
/// the kernel spells as a zero `timeval`.
///
/// A zero duration is refused with `EINVAL` rather than passed through, because
/// the kernel reads a zero `timeval` as "no timeout" and a caller asking for
/// zero means the opposite. `std::net` makes the same refusal for the same
/// reason, and silently inverting the request would be worse than failing it.
pub(super) fn set_timeout(fd: RawFd, option: libc::c_int, dur: Option<Duration>) -> io::Result<()> {
    let timeout = match dur {
        Some(d) if d == Duration::ZERO => {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        // Saturating rather than wrapping: a duration past `time_t` becomes the
        // longest wait the kernel can express, which is the caller's intent.
        Some(d) => libc::timeval {
            tv_sec: d.as_secs().min(libc::time_t::MAX as u64) as libc::time_t,
            tv_usec: libc::suseconds_t::from(d.subsec_micros()),
        },
        None => libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
    };
    // SAFETY: the descriptor is open, and the pointer and length describe a
    // `timeval` this frame owns.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            std::ptr::from_ref(&timeout).cast(),
            size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Read back a receive or send timeout, or `None` when none is set.
pub(super) fn timeout(fd: RawFd, option: libc::c_int) -> io::Result<Option<Duration>> {
    let mut timeout = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len = size_of::<libc::timeval>() as libc::socklen_t;
    // SAFETY: the descriptor is open, and both pointers describe values this
    // frame owns and keeps alive across the call.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            std::ptr::from_mut(&mut timeout).cast(),
            &mut len,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    if timeout.tv_sec == 0 && timeout.tv_usec == 0 {
        return Ok(None);
    }
    Ok(Some(
        Duration::from_secs(timeout.tv_sec as u64) + Duration::from_micros(timeout.tv_usec as u64),
    ))
}

/// Set or clear a descriptor's non-blocking mode, preserving its other flags.
///
/// Read-modify-write rather than a bare `F_SETFL`, because the flag word also
/// carries the access mode and `O_APPEND`, and writing `O_NONBLOCK` alone would
/// drop them.
pub(super) fn set_nonblocking(fd: RawFd, nonblocking: bool) -> io::Result<()> {
    // SAFETY: the descriptor is open for the duration of both calls.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let wanted = if nonblocking {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    // SAFETY: as above; `wanted` is the value just read back, one bit changed.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, wanted) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream as StdUnixStream;

    /// Turn the client half into something a blocking test can drive, the way a
    /// client process would after receiving it.
    fn client(fd: OwnedFd) -> StdUnixStream {
        StdUnixStream::from(fd)
    }

    #[tokio::test]
    async fn message_boundaries_survive_in_both_directions() {
        let (daemon, theirs) = pair().unwrap();
        let daemon = Seqpacket::new(daemon).unwrap();
        let mut theirs = client(theirs);

        // Three writes must arrive as three datagrams, not one run of bytes.
        // This is the property SOCK_STREAM would lose.
        theirs.write_all(b"one").unwrap();
        theirs.write_all(b"two").unwrap();
        theirs.write_all(b"three").unwrap();

        let mut buf = [0u8; 64];
        assert_eq!(daemon.recv(&mut buf).await.unwrap(), Received::Datagram(3));
        assert_eq!(&buf[..3], b"one");
        assert_eq!(daemon.recv(&mut buf).await.unwrap(), Received::Datagram(3));
        assert_eq!(&buf[..3], b"two");
        assert_eq!(daemon.recv(&mut buf).await.unwrap(), Received::Datagram(5));
        assert_eq!(&buf[..5], b"three");

        daemon.send(b"alpha").await.unwrap();
        daemon.send(b"beta").await.unwrap();
        let mut got = [0u8; 64];
        assert_eq!(theirs.read(&mut got).unwrap(), 5);
        assert_eq!(&got[..5], b"alpha");
        assert_eq!(theirs.read(&mut got).unwrap(), 4);
        assert_eq!(&got[..4], b"beta");
    }

    #[tokio::test]
    async fn closing_the_client_half_reports_end_of_file() {
        let (daemon, theirs) = pair().unwrap();
        let daemon = Seqpacket::new(daemon).unwrap();
        drop(client(theirs));

        let mut buf = [0u8; 64];
        assert_eq!(daemon.recv(&mut buf).await.unwrap(), Received::Eof);
    }

    #[tokio::test]
    async fn an_empty_datagram_is_not_end_of_file() {
        // The whole reason recv_once uses recvmsg. If an empty datagram read as
        // a close, a client could tear down its own flow by sending nothing,
        // and the bug would look like a spurious disconnect.
        let (daemon, theirs) = pair().unwrap();
        let daemon = Seqpacket::new(daemon).unwrap();
        let mut theirs = client(theirs);

        // `write_all(b"")` is a no-op in Rust and never reaches the socket, so
        // the zero-length datagram has to be sent with `send` directly. The
        // first version of this test used `write_all` and asserted a behaviour
        // it had not exercised.
        // SAFETY: the descriptor is open and owned by `theirs`.
        let sent = unsafe { libc::send(theirs.as_raw_fd(), std::ptr::null(), 0, 0) };
        assert_eq!(sent, 0, "{}", io::Error::last_os_error());
        theirs.write_all(b"after").unwrap();

        let mut buf = [0u8; 64];
        assert_eq!(daemon.recv(&mut buf).await.unwrap(), Received::Datagram(0));
        assert_eq!(daemon.recv(&mut buf).await.unwrap(), Received::Datagram(5));
    }

    #[tokio::test]
    async fn the_client_half_is_left_blocking() {
        // The daemon's half is made non-blocking for the reactor. If that flag
        // reached the client's half, a client doing an ordinary blocking recv
        // would get EAGAIN instead of waiting, which is a trap worth a test
        // rather than a comment.
        let (daemon, theirs) = pair().unwrap();
        let _daemon = Seqpacket::new(daemon).unwrap();

        // SAFETY: the descriptor is open and owned by `theirs`.
        let flags = unsafe { libc::fcntl(theirs.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(flags & libc::O_NONBLOCK, 0);
    }
}
