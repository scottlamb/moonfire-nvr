// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Unified [`hyper::server::accept::Accept`] impl for TCP and Unix sockets.

use std::pin::Pin;

use hyper::server::accept::Accept;

pub enum Listener {
    Tcp(tokio::net::TcpListener),
    Unix(tokio::net::UnixListener),
}

impl Accept for Listener {
    type Conn = Conn;
    type Error = std::io::Error;

    fn poll_accept(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<Self::Conn, Self::Error>>> {
        match Pin::into_inner(self) {
            Listener::Tcp(l) => Pin::new(l).poll_accept(cx)?.map(|(s, a)| {
                if let Err(e) = s.set_nodelay(true) {
                    return Some(Err(e));
                }
                Some(Ok(Conn {
                    stream: Stream::Tcp(s),
                    data: ConnData {
                        client_unix_uid: None,
                        client_addr: Some(a),
                    },
                }))
            }),
            Listener::Unix(l) => Pin::new(l).poll_accept(cx)?.map(|(s, _a)| {
                let ucred = match s.peer_cred() {
                    Err(e) => return Some(Err(e)),
                    Ok(ucred) => ucred,
                };
                Some(Ok(Conn {
                    stream: Stream::Unix(s),
                    data: ConnData {
                        client_unix_uid: Some(nix::unistd::Uid::from_raw(ucred.uid())),
                        client_addr: None,
                    },
                }))
            }),
        }
    }
}

/// An open connection.
pub struct Conn {
    stream: Stream,
    data: ConnData,
}

/// Extra data associated with a connection.
#[derive(Copy, Clone)]
pub struct ConnData {
    pub client_unix_uid: Option<nix::unistd::Uid>,
    pub client_addr: Option<std::net::SocketAddr>,
}

impl Conn {
    pub fn data(&self) -> &ConnData {
        &self.data
    }
}

impl tokio::io::AsyncRead for Conn {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.stream {
            Stream::Tcp(ref mut s) => Pin::new(s).poll_read(cx, buf),
            Stream::Unix(ref mut s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for Conn {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        match self.stream {
            Stream::Tcp(ref mut s) => Pin::new(s).poll_write(cx, buf),
            Stream::Unix(ref mut s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        match self.stream {
            Stream::Tcp(ref mut s) => Pin::new(s).poll_flush(cx),
            Stream::Unix(ref mut s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        match self.stream {
            Stream::Tcp(ref mut s) => Pin::new(s).poll_shutdown(cx),
            Stream::Unix(ref mut s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// An open stream.
///
/// Ultimately `Tcp` and `Unix` result in the same syscalls, but using an
/// `enum` seems easier for the moment than fighting the tokio API.
enum Stream {
    Tcp(tokio::net::TcpStream),
    Unix(tokio::net::UnixStream),
}
