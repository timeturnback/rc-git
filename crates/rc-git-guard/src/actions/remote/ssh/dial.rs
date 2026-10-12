//! Dialing the ssh server: resolve the host, then try each address in turn, each with a
//! fair share of what is left of the connect budget. A plain `TcpStream::connect` waits on
//! every address until the OS gives up (about 75 s on macOS), so an address that silently
//! drops SYNs (a broken IPv6 route) would eat the whole budget before the next is tried.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::net::TcpStream;

use crate::actions::{ActionResult, git_err};

use super::SshTarget;

/// Resolves `target` and connects to the first address that answers within `budget`.
///
/// # Errors
/// [`crate::actions::ActionError::Git`] when the host does not resolve, resolves to no
/// address, or no address connects in time; the message names the last address tried.
#[expect(
    clippy::disallowed_methods,
    reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
)]
pub(super) async fn dial(
    target: &SshTarget,
    budget: Duration,
    op: &'static str,
) -> ActionResult<TcpStream> {
    let deadline = Instant::now() + budget;
    let endpoint = format!("{}:{}", target.host_name, target.port);
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((target.host_name.as_str(), target.port))
        .await
        .map_err(|err| io::Error::new(err.kind(), format!("resolve '{endpoint}': {err}")))
        .map_err(git_err(op, &target.host))?
        .collect();
    if addrs.is_empty() {
        return Err(git_err(op, &target.host)(io::Error::new(
            io::ErrorKind::NotFound,
            format!("'{endpoint}' resolved to no address"),
        )));
    }
    first_to_connect(&addrs, deadline, connect_addr)
        .await
        .map_err(git_err(op, &target.host))
}

/// One TCP connect to one address, with no timeout of its own.
#[expect(
    clippy::disallowed_methods,
    reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
)]
async fn connect_addr(addr: SocketAddr) -> io::Result<TcpStream> {
    TcpStream::connect(addr).await
}

/// Tries `addrs` in order with `connect`, giving each the remaining time until `deadline`
/// split evenly over the addresses still untried, so the last one gets all that is left.
/// Returns the first stream that connects, or the last failure.
async fn first_to_connect<F, Fut>(
    addrs: &[SocketAddr],
    deadline: Instant,
    connect: F,
) -> io::Result<TcpStream>
where
    F: Fn(SocketAddr) -> Fut,
    Fut: Future<Output = io::Result<TcpStream>>,
{
    let mut last_err = io::Error::new(io::ErrorKind::NotFound, "no address to connect to");
    for (index, &addr) in addrs.iter().enumerate() {
        let untried = u32::try_from(addrs.len() - index).unwrap_or(u32::MAX);
        let share = deadline.saturating_duration_since(Instant::now()) / untried;
        match tokio::time::timeout(share, connect(addr)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(err)) => {
                last_err = io::Error::new(err.kind(), format!("connect to {addr}: {err}"));
            }
            Err(_elapsed) => {
                last_err = io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("connect to {addr} timed out after {share:?}"),
                );
            }
        }
    }
    Err(last_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEAD: SocketAddr = SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1)),
        22,
    );

    /// Connects for real, except to [`DEAD`], which never answers (a blackholed route).
    async fn connect_unless_dead(addr: SocketAddr) -> io::Result<TcpStream> {
        if addr == DEAD {
            std::future::pending::<()>().await;
        }
        connect_addr(addr).await
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }

    #[test]
    fn a_dead_first_address_falls_through_to_the_next() {
        let runtime = runtime();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let live = listener.local_addr().expect("listener address");
        let budget = Duration::from_secs(2);
        let started = Instant::now();
        let stream = runtime
            .block_on(first_to_connect(
                &[DEAD, live],
                started + budget,
                connect_unless_dead,
            ))
            .expect("the second address connects");
        assert_eq!(stream.peer_addr().expect("peer address"), live);
        assert!(started.elapsed() < budget, "took {:?}", started.elapsed());
    }

    #[test]
    fn no_address_answering_in_time_names_the_last_one() {
        let runtime = runtime();
        let budget = Duration::from_secs(1);
        let started = Instant::now();
        let err = runtime
            .block_on(first_to_connect(
                &[DEAD],
                started + budget,
                connect_unless_dead,
            ))
            .expect_err("nothing answers");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(err.to_string().contains("192.0.2.1:22"), "{err}");
        assert!(
            started.elapsed() < budget + Duration::from_millis(500),
            "took {:?}",
            started.elapsed()
        );
    }
}
