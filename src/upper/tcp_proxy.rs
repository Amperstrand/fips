use std::net::{Ipv6Addr, SocketAddrV6};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::{debug, info, warn};

use crate::NodeAddr;

const TCP_PROXY_BUFFER_SIZE: usize = 512;

/// Handle for a per-leaf TCP proxy listener.
///
/// Each proxied leaf gets its own TCP listener bound on the leaf's
/// FipsAddress. Data flows bidirectionally between TCP clients and
/// BLE via FMP 0x60 messages.
pub struct LeafTcpProxyHandle {
    /// The leaf's NodeAddr (BLE peer identity and FSP session identity).
    pub leaf_node_addr: NodeAddr,
    /// The IPv6 address this listener is bound to.
    pub listen_addr: Ipv6Addr,
    /// Port number.
    pub port: u16,
    /// Sender half for inbound data (BLE → TCP). Dispatch stores this
    /// to route incoming FMP 0x60 to this leaf's TCP connection.
    pub inbound_tx: Sender<Vec<u8>>,
    /// Task handle for the spawned proxy loop.
    pub task_handle: tokio::task::JoinHandle<()>,
}

/// Run a per-leaf TCP proxy listener.
///
/// Binds a TCP listener on `[fips_addr]:port` and bridges bidirectional
/// data between TCP clients and BLE via channels.
///
/// # Arguments
/// * `fips_addr` - The leaf's FipsAddress as an IPv6 address to bind on.
/// * `port` - TCP port to listen on (e.g., 6053).
/// * `leaf_node_addr` - The leaf's NodeAddr, used for logging and as a tag
///   on outbound messages so the rx_loop can route FMP 0x60 to the correct
///   BLE peer.
/// * `outbound_tx` - Channel to send `(NodeAddr, Vec<u8>)` tuples: TCP data
///   tagged with the leaf identity for BLE forwarding.
/// * `inbound_rx` - Channel to receive data from BLE (FMP 0x60) for writing
///   to the TCP client.
pub async fn run_leaf_tcp_proxy(
    fips_addr: Ipv6Addr,
    port: u16,
    leaf_node_addr: NodeAddr,
    outbound_tx: Sender<(NodeAddr, Vec<u8>)>,
    mut inbound_rx: Receiver<Vec<u8>>,
) {
    let bind_addr = SocketAddrV6::new(fips_addr, port, 0, 0);
    let listener = match TcpListener::bind(bind_addr).await {
        Ok(l) => {
            info!(
                listen = %bind_addr,
                leaf = %leaf_node_addr,
                "Per-leaf TCP proxy started"
            );
            l
        }
        Err(e) => {
            warn!(
                listen = %bind_addr,
                leaf = %leaf_node_addr,
                error = %e,
                "Per-leaf TCP proxy failed to bind"
            );
            return;
        }
    };

    let mut client_reader: Option<OwnedReadHalf> = None;
    let mut client_writer: Option<OwnedWriteHalf> = None;
    let mut client_addr: Option<std::net::SocketAddr> = None;
    let mut read_buf = [0u8; TCP_PROXY_BUFFER_SIZE];

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, addr)) => {
                        if let Some(old_addr) = client_addr.take() {
                            info!(old_client = %old_addr, new_client = %addr, leaf = %leaf_node_addr, "Leaf TCP proxy replacing active client");
                        } else {
                            info!(client = %addr, leaf = %leaf_node_addr, "Leaf TCP proxy client connected");
                        }

                        client_reader.take();
                        client_writer.take();

                        let (reader, writer) = stream.into_split();
                        client_reader = Some(reader);
                        client_writer = Some(writer);
                        client_addr = Some(addr);
                    }
                    Err(e) => {
                        warn!(error = %e, leaf = %leaf_node_addr, "Leaf TCP proxy accept failed");
                    }
                }
            }
            read_result = async {
                match client_reader.as_mut() {
                    Some(reader) => reader.read(&mut read_buf).await,
                    None => std::future::pending().await,
                }
            } => {
                match read_result {
                    Ok(0) => {
                        if let Some(addr) = client_addr.take() {
                            info!(client = %addr, leaf = %leaf_node_addr, "Leaf TCP proxy client disconnected");
                        }
                        client_reader.take();
                        client_writer.take();
                    }
                    Ok(n) => {
                        if outbound_tx.send((leaf_node_addr, read_buf[..n].to_vec())).await.is_err() {
                            warn!(leaf = %leaf_node_addr, "Leaf TCP proxy outbound channel closed");
                            break;
                        }
                    }
                    Err(e) => {
                        if let Some(addr) = client_addr.take() {
                            info!(client = %addr, error = %e, leaf = %leaf_node_addr, "Leaf TCP proxy client disconnected");
                        } else {
                            debug!(error = %e, leaf = %leaf_node_addr, "Leaf TCP proxy read failed without active client");
                        }
                        client_reader.take();
                        client_writer.take();
                    }
                }
            }
            inbound = inbound_rx.recv() => {
                match inbound {
                    Some(data) => {
                        if let Some(writer) = client_writer.as_mut() {
                            if let Err(e) = writer.write_all(&data).await {
                                if let Some(addr) = client_addr.take() {
                                    info!(client = %addr, error = %e, leaf = %leaf_node_addr, "Leaf TCP proxy client disconnected during write");
                                } else {
                                    debug!(error = %e, leaf = %leaf_node_addr, "Leaf TCP proxy write failed without active client");
                                }
                                client_reader.take();
                                client_writer.take();
                            }
                        }
                    }
                    None => {
                        debug!(leaf = %leaf_node_addr, "Leaf TCP proxy inbound channel closed");
                        break;
                    }
                }
            }
        }

        if client_writer.is_none() {
            while inbound_rx.try_recv().is_ok() {}
        }
    }
}

/// Start a per-leaf TCP proxy listener.
///
/// Creates channel pair, spawns the proxy task, and returns a handle
/// with the inbound sender (for dispatch routing) and the task handle.
///
/// The `outbound_tx` sender should be shared across all leaf proxies so
/// the rx_loop can receive from a single merged channel.
pub fn start_leaf_tcp_proxy(
    fips_addr: Ipv6Addr,
    port: u16,
    leaf_node_addr: NodeAddr,
    outbound_tx: Sender<(NodeAddr, Vec<u8>)>,
) -> LeafTcpProxyHandle {
    let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);

    let task_handle = tokio::spawn(run_leaf_tcp_proxy(
        fips_addr,
        port,
        leaf_node_addr,
        outbound_tx,
        inbound_rx,
    ));

    LeafTcpProxyHandle {
        leaf_node_addr,
        listen_addr: fips_addr,
        port,
        inbound_tx,
        task_handle,
    }
}

/// Run the legacy (single-target) TCP proxy.
///
/// This is the original proxy that binds on a single address (e.g.,
/// 127.0.0.1:6053) and bridges to a single BLE peer. Kept for backward
/// compatibility when `tcp_proxy.target_npub` is configured directly.
pub async fn run_tcp_proxy(
    listener: TcpListener,
    target_npub: String,
    outbound_tx: Sender<Vec<u8>>,
    mut inbound_rx: Receiver<Vec<u8>>,
) {
    let mut client_reader: Option<OwnedReadHalf> = None;
    let mut client_writer: Option<OwnedWriteHalf> = None;
    let mut client_addr: Option<std::net::SocketAddr> = None;
    let mut read_buf = [0u8; TCP_PROXY_BUFFER_SIZE];

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, addr)) => {
                        if let Some(old_addr) = client_addr.take() {
                            info!(old_client = %old_addr, new_client = %addr, target = %target_npub, "TCP proxy replacing active client");
                        } else {
                            info!(client = %addr, target = %target_npub, "TCP proxy client connected");
                        }

                        client_reader.take();
                        client_writer.take();

                        let (reader, writer) = stream.into_split();
                        client_reader = Some(reader);
                        client_writer = Some(writer);
                        client_addr = Some(addr);
                    }
                    Err(e) => {
                        warn!(error = %e, target = %target_npub, "TCP proxy accept failed");
                    }
                }
            }
            read_result = async {
                match client_reader.as_mut() {
                    Some(reader) => reader.read(&mut read_buf).await,
                    None => std::future::pending().await,
                }
            } => {
                match read_result {
                    Ok(0) => {
                        if let Some(addr) = client_addr.take() {
                            info!(client = %addr, target = %target_npub, "TCP proxy client disconnected");
                        }
                        client_reader.take();
                        client_writer.take();
                    }
                    Ok(n) => {
                        if outbound_tx.send(read_buf[..n].to_vec()).await.is_err() {
                            warn!(target = %target_npub, "TCP proxy outbound channel closed");
                            break;
                        }
                    }
                    Err(e) => {
                        if let Some(addr) = client_addr.take() {
                            info!(client = %addr, error = %e, target = %target_npub, "TCP proxy client disconnected");
                        } else {
                            debug!(error = %e, target = %target_npub, "TCP proxy read failed without active client");
                        }
                        client_reader.take();
                        client_writer.take();
                    }
                }
            }
            inbound = inbound_rx.recv() => {
                match inbound {
                    Some(data) => {
                        if let Some(writer) = client_writer.as_mut() {
                            if let Err(e) = writer.write_all(&data).await {
                                if let Some(addr) = client_addr.take() {
                                    info!(client = %addr, error = %e, target = %target_npub, "TCP proxy client disconnected during write");
                                } else {
                                    debug!(error = %e, target = %target_npub, "TCP proxy write failed without active client");
                                }
                                client_reader.take();
                                client_writer.take();
                            }
                        }
                    }
                    None => {
                        debug!(target = %target_npub, "TCP proxy inbound channel closed");
                        break;
                    }
                }
            }
        }

        if client_writer.is_none() {
            while inbound_rx.try_recv().is_ok() {}
        }
    }
}
