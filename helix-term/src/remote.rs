//! Bounded local transport shared by the native Unix listener and Steel TCP bridge.
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::Shutdown,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const STOP_POLL_INTERVAL: Duration = Duration::from_millis(20);
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

trait Connection: Read + Write + Send + Sized + 'static {
    fn try_clone(&self) -> io::Result<Self>;
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    #[cfg(feature = "steel")]
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    fn set_nonblocking(&self, value: bool) -> io::Result<()>;
    fn shutdown(&self, how: Shutdown) -> io::Result<()>;
}

trait Listener: Send + 'static {
    type Stream: Connection;
    fn accept(&self) -> io::Result<Self::Stream>;
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()>;
    #[cfg(unix)]
    fn raw_fd(&self) -> std::os::fd::RawFd;
}

macro_rules! socket_impls {
    ($listener:ty, $stream:ty) => {
        impl Listener for $listener {
            type Stream = $stream;
            fn accept(&self) -> io::Result<Self::Stream> {
                self.accept().map(|(stream, _)| stream)
            }
            fn set_nonblocking(&self, value: bool) -> io::Result<()> {
                self.set_nonblocking(value)
            }
            #[cfg(unix)]
            fn raw_fd(&self) -> std::os::fd::RawFd {
                std::os::fd::AsRawFd::as_raw_fd(self)
            }
        }
        impl Connection for $stream {
            fn try_clone(&self) -> io::Result<Self> {
                self.try_clone()
            }
            fn set_read_timeout(&self, value: Option<Duration>) -> io::Result<()> {
                self.set_read_timeout(value)
            }
            #[cfg(feature = "steel")]
            fn set_write_timeout(&self, value: Option<Duration>) -> io::Result<()> {
                self.set_write_timeout(value)
            }
            fn set_nonblocking(&self, value: bool) -> io::Result<()> {
                self.set_nonblocking(value)
            }
            fn shutdown(&self, how: Shutdown) -> io::Result<()> {
                self.shutdown(how)
            }
        }
    };
}
#[cfg(feature = "steel")]
socket_impls!(std::net::TcpListener, std::net::TcpStream);
#[cfg(unix)]
socket_impls!(
    std::os::unix::net::UnixListener,
    std::os::unix::net::UnixStream
);

struct Server<S: Connection> {
    state: Arc<ServerState<S>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    #[cfg(unix)]
    wake: std::os::unix::net::UnixStream,
}

struct ServerState<S> {
    stopping: AtomicBool,
    connection: Mutex<Option<S>>,
}

impl<S: Connection> Server<S> {
    fn start(
        listener: impl Listener<Stream = S>,
        serve: impl Fn(S, &AtomicBool) -> io::Result<()> + Send + 'static,
    ) -> io::Result<Self> {
        listener.set_nonblocking(true)?;
        #[cfg(unix)]
        let (stop_reader, wake) = std::os::unix::net::UnixStream::pair()?;
        let state = Arc::new(ServerState {
            stopping: AtomicBool::new(false),
            connection: Mutex::new(None),
        });
        let worker_state = state.clone();
        let thread = thread::Builder::new()
            .name("helix-remote".into())
            .spawn(move || {
                while !worker_state.stopping.load(Ordering::Acquire) {
                    #[cfg(unix)]
                    match wait_listener(
                        listener.raw_fd(),
                        std::os::fd::AsRawFd::as_raw_fd(&stop_reader),
                    ) {
                        Ok(true) => (),
                        Ok(false) => break,
                        Err(error) => {
                            log::warn!("Remote listener wait failed: {error}");
                            break;
                        }
                    }
                    match listener.accept() {
                        Ok(stream) => {
                            let result = stream
                                .set_nonblocking(false)
                                .and_then(|_| stream.try_clone())
                                .and_then(|active| {
                                    *worker_state.connection.lock().unwrap() = Some(active);
                                    let result = if worker_state.stopping.load(Ordering::Acquire) {
                                        Ok(())
                                    } else {
                                        serve(stream, &worker_state.stopping)
                                    };
                                    worker_state.connection.lock().unwrap().take();
                                    result
                                });
                            if let Err(error) = result {
                                log::debug!("Remote request failed: {error}");
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            #[cfg(not(unix))]
                            thread::sleep(STOP_POLL_INTERVAL);
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
                        Err(error) => {
                            log::warn!("Remote listener failed: {error}");
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            state,
            thread: Mutex::new(Some(thread)),
            #[cfg(unix)]
            wake,
        })
    }

    fn stop(&self) {
        if !self.state.stopping.swap(true, Ordering::AcqRel) {
            #[cfg(unix)]
            let _ = (&self.wake).write_all(&[1]);
        }
        if let Some(stream) = self.state.connection.lock().unwrap().as_ref() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if let Some(thread) = self.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}

impl<S: Connection> Drop for Server<S> {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(unix)]
fn wait_listener(listener: std::os::fd::RawFd, stop: std::os::fd::RawFd) -> io::Result<bool> {
    let mut fds = [
        libc::pollfd {
            fd: listener,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: stop,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // Both descriptor owners remain in the worker until this call returns.
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } >= 0 {
            return Ok(fds[1].revents == 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn read_frame(stream: &mut impl Connection, timeout: Duration) -> io::Result<Vec<u8>> {
    struct DeadlineReader<'a, S> {
        stream: &'a mut S,
        deadline: Instant,
    }
    impl<S: Connection> Read for DeadlineReader<'_, S> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            self.stream.set_read_timeout(Some(remaining))?;
            self.stream.read(buf)
        }
    }
    let mut line = Vec::new();
    BufReader::new(DeadlineReader {
        stream,
        deadline: Instant::now() + timeout,
    })
    .take((MAX_REQUEST_BYTES + 1) as u64)
    .read_until(b'\n', &mut line)?;
    Ok(line)
}

#[cfg(feature = "steel")]
mod steel;
#[cfg(feature = "steel")]
pub(crate) use steel::{BridgeServer, HandlerReceiver, HandlerRequest};
#[cfg(unix)]
mod native;
#[cfg(unix)]
pub(crate) use native::OpenServer;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        os::unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
        sync::mpsc::sync_channel,
    };

    #[test]
    fn stop_interrupts_an_idle_read() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("idle.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (started, ready) = sync_channel(1);
        let (completed, result) = sync_channel(1);
        let server = Server::start(listener, move |mut stream, _| {
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            started.send(()).unwrap();
            let outcome = stream.read(&mut [0]).map_err(|error| error.kind());
            completed.send(outcome).unwrap();
            Ok(())
        })
        .unwrap();
        let _idle = UnixStream::connect(path).unwrap();
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        server.stop();
        assert_eq!(result.recv_timeout(Duration::from_secs(1)).unwrap(), Ok(0));
    }
}
