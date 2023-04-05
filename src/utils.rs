use crate::constants::{BINCODE_BUFFER_SIZE, BINCODE_CONFIG};
use anyhow::{Context, Result};
use bincode::{Decode, Encode};
use bytes::{Bytes, BytesMut};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::SocketAddr;
use tracing::warn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::EnvFilter;

pub fn bind_socket(
    addr: SocketAddr,
    send_buffer_size: usize,
    recv_buffer_size: usize,
) -> Result<std::net::UdpSocket> {
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))
        .context("create socket")?;

    if addr.is_ipv6() {
        socket.set_only_v6(false).context("set_only_v6")?;
    }

    socket
        .bind(&socket2::SockAddr::from(addr))
        .context("binding endpoint")?;
    socket
        .set_send_buffer_size(send_buffer_size)
        .context("send buffer size")?;
    socket
        .set_recv_buffer_size(recv_buffer_size)
        .context("recv buffer size")?;

    let buf_size = socket.send_buffer_size().context("send buffer size")?;
    if buf_size < send_buffer_size {
        warn!(
            "Unable to set desired send buffer size. Desired: {}, Actual: {}",
            send_buffer_size, buf_size
        );
    }

    let buf_size = socket.recv_buffer_size().context("recv buffer size")?;
    if buf_size < recv_buffer_size {
        warn!(
            "Unable to set desired recv buffer size. Desired: {}, Actual: {}",
            recv_buffer_size, buf_size
        );
    }

    Ok(socket.into())
}

pub fn enable_tracing(log_level: &str) {
    let registry = tracing_subscriber::Registry::default();
    let fmt_layer = tracing_subscriber::fmt::Layer::new();
    let filter_layer = EnvFilter::try_new(log_level).unwrap();

    let subscriber = registry.with(filter_layer).with(fmt_layer);
    tracing::subscriber::set_global_default(subscriber).unwrap();
}

pub fn encode_message<M: Encode>(message: M) -> Result<Bytes> {
    let mut message_buf = BytesMut::with_capacity(BINCODE_BUFFER_SIZE);

    bincode::encode_into_slice(message, &mut message_buf, *BINCODE_CONFIG)?;

    Ok(message_buf.into())
}

pub fn decode_message<M: Decode>(data: Bytes) -> Result<M> {
    let (res, _) = bincode::decode_from_slice(&data, *BINCODE_CONFIG)?;

    Ok(res)
}
