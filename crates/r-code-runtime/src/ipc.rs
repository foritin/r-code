//! Local IPC transport for the application protocol.
//!
//! Windows uses a current-user named pipe; Unix uses a 0600 domain socket.
//! Both carry newline-framed JSON. The daemon binds with `first_pipe_instance`
//! / an exclusive bind so two daemons cannot share a profile endpoint.

use std::io;
use tokio::io::{AsyncRead, AsyncWrite};

/// Transport-neutral stream over which application frames flow (F1): the
/// named-pipe/Unix listener and the future remote transports (R04's
/// WebSocket over TLS, R17's relay) feed the *same* connection loop through
/// this seam — there is no second command dispatch path.
pub trait AppStream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T> AppStream for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

/// Platform stream over which application frames flow.
pub struct IpcStream {
    #[cfg(windows)]
    inner: tokio::net::windows::named_pipe::NamedPipeServer,
    #[cfg(unix)]
    inner: tokio::net::UnixStream,
}

impl IpcStream {
    #[cfg(windows)]
    pub fn from_pipe(server: tokio::net::windows::named_pipe::NamedPipeServer) -> Self {
        Self { inner: server }
    }

    #[cfg(unix)]
    pub fn from_unix(stream: tokio::net::UnixStream) -> Self {
        Self { inner: stream }
    }
}

impl AsyncRead for IpcStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for IpcStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Bound endpoint accepting client connections.
pub struct IpcListener {
    #[cfg(windows)]
    pipe_name: String,
    #[cfg(windows)]
    pending: Option<tokio::net::windows::named_pipe::NamedPipeServer>,
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
}

impl IpcListener {
    /// Bind the endpoint. `first` enforces single ownership of the name on
    /// Windows (the named pipe equivalent of an exclusive bind).
    #[cfg(windows)]
    pub fn bind(name: &str) -> io::Result<Self> {
        use tokio::net::windows::named_pipe::ServerOptions;
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(name)?;
        Ok(Self {
            pipe_name: name.to_string(),
            pending: Some(server),
        })
    }

    #[cfg(unix)]
    pub fn bind(path: &Path) -> io::Result<Self> {
        // Remove a stale socket only when nothing can accept on it; binding
        // over a live socket fails, which is exactly the protection wanted.
        let listener = tokio::net::UnixListener::bind(path)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self { inner: listener })
    }

    /// Accept the next client stream.
    #[cfg(windows)]
    pub async fn accept(&mut self) -> io::Result<IpcStream> {
        use tokio::net::windows::named_pipe::ServerOptions;
        let server = match self.pending.take() {
            Some(server) => server,
            None => ServerOptions::new().create(&self.pipe_name)?,
        };
        server.connect().await?;
        // Prepare the next instance for concurrent clients.
        self.pending = Some(ServerOptions::new().create(&self.pipe_name)?);
        Ok(IpcStream::from_pipe(server))
    }

    #[cfg(unix)]
    pub async fn accept(&mut self) -> io::Result<IpcStream> {
        let (stream, _addr) = self.inner.accept().await?;
        Ok(IpcStream::from_unix(stream))
    }
}

/// Bind the platform-appropriate listener for an endpoint.
pub fn bind_endpoint(endpoint: &r_code_harness_protocol::IpcEndpoint) -> io::Result<IpcListener> {
    match endpoint {
        #[cfg(windows)]
        r_code_harness_protocol::IpcEndpoint::NamedPipe { name } => IpcListener::bind(name),
        #[cfg(unix)]
        r_code_harness_protocol::IpcEndpoint::UnixSocket { path } => IpcListener::bind(path),
        #[cfg(not(windows))]
        r_code_harness_protocol::IpcEndpoint::NamedPipe { .. } => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "named pipes unavailable on this platform",
        )),
        #[cfg(not(unix))]
        r_code_harness_protocol::IpcEndpoint::UnixSocket { .. } => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unix sockets unavailable on this platform",
        )),
    }
}
