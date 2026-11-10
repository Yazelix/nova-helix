use super::*;
use crate::{
    args, commands,
    compositor::{self, Compositor},
    job::{self, Jobs},
    ui,
};
use helix_core::{pos_at_coords, Position, Selection};
use helix_view::{align_view, editor::Action, Align, Editor};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs::{self, File, Metadata},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, MetadataExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::{
        atomic::AtomicU8,
        mpsc::{sync_channel, Receiver, RecvTimeoutError},
    },
};

const VERSION: u64 = 1;
const TIMEOUT: Duration = Duration::from_secs(2);
const PENDING: u8 = 0;
const RUNNING: u8 = 1;
const ABANDONED: u8 = 2;
const DONE: u8 = 3;

type OpenPaths = Vec<(PathBuf, Option<Position>)>;
type Reply = Result<(), Failure>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u64,
    id: String,
    action: String,
    paths: Vec<String>,
    timeout_ms: Option<u64>,
}

struct Failure {
    class: &'static str,
    message: String,
    path_index: Option<usize>,
}

impl Failure {
    fn new(class: &'static str, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
            path_index: None,
        }
    }
}

struct Pending {
    state: AtomicU8,
    deadline: Instant,
}

impl Pending {
    fn begin(&self) -> bool {
        if Instant::now() >= self.deadline {
            self.state
                .compare_exchange(PENDING, ABANDONED, Ordering::AcqRel, Ordering::Acquire)
                .ok();
            return false;
        }
        self.state
            .compare_exchange(PENDING, RUNNING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn abandon(&self) -> Failure {
        if self
            .state
            .compare_exchange(PENDING, ABANDONED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            || self.state.load(Ordering::Acquire) == ABANDONED
        {
            Failure::new("timeout", "request expired before execution")
        } else if self.state.load(Ordering::Acquire) == DONE {
            Failure::new("unavailable", "request completed; reply unavailable")
        } else {
            Failure::new("unavailable", "request started; outcome unknown")
        }
    }
}

struct SocketPath {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl SocketPath {
    fn new(path: &Path, metadata: &Metadata) -> Self {
        Self {
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
    fn matches(&self) -> bool {
        fs::symlink_metadata(&self.path).is_ok_and(|meta| {
            meta.file_type().is_socket() && meta.dev() == self.device && meta.ino() == self.inode
        })
    }
}

impl Drop for SocketPath {
    fn drop(&mut self) {
        if self.matches() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(crate) struct OpenServer {
    server: Server<UnixStream>,
    _socket: SocketPath,
}

impl OpenServer {
    pub(crate) fn start(path: &Path) -> io::Result<Self> {
        let runtime = tokio::runtime::Handle::current();
        Self::with_dispatch(path, move |paths, pending| {
            let _runtime = runtime.enter();
            let (sender, receiver) = sync_channel(1);
            job::dispatch_blocking_jobs(move |editor, compositor, jobs| {
                if !pending.begin() {
                    return;
                }
                let result = open(editor, compositor, jobs, paths);
                let _ = sender.send(result);
                pending.state.store(DONE, Ordering::Release);
            });
            receiver
        })
    }

    fn with_dispatch(
        path: &Path,
        dispatch: impl Fn(OpenPaths, Arc<Pending>) -> Receiver<Reply> + Send + 'static,
    ) -> io::Result<Self> {
        let (listener, socket) = bind(path)?;
        let server = Server::start(listener, move |stream, stopping| {
            serve(stream, &dispatch, stopping)
        })?;
        Ok(Self {
            server,
            _socket: socket,
        })
    }
}

impl Drop for OpenServer {
    fn drop(&mut self) {
        self.server.stop();
    }
}

// Probe without blocking when a live listener's connection backlog is full.
fn probe(path: &Path) -> io::Result<()> {
    let socket = tokio::net::UnixSocket::new_stream()?;
    let mut addr = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    addr.sun_family = libc::AF_UNIX as _;
    for (target, byte) in addr.sun_path.iter_mut().zip(path.as_os_str().as_bytes()) {
        *target = *byte as _;
    }
    let len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.as_os_str().len() + 1)
        as libc::socklen_t;
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        addr.sun_len = len as _;
    }
    // addr has the platform's Unix layout, a checked path and a terminating NUL.
    if unsafe {
        libc::connect(
            socket.as_raw_fd(),
            (&addr as *const libc::sockaddr_un).cast(),
            len,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn check_ancestors(parent: &Path) -> io::Result<()> {
    let uid = unsafe { libc::geteuid() };
    for ancestor in parent.ancestors() {
        let directory = fs::symlink_metadata(ancestor)?;
        if (directory.uid() != uid && directory.uid() != 0)
            || !directory.is_dir()
            || (directory.mode() & 0o022 != 0 && directory.mode() & 0o1000 == 0)
        {
            return Err(io::Error::other("--listen ancestor must be owned by the current user or root, without group/other write access unless sticky"));
        }
    }
    Ok(())
}

fn canonical_socket_path(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() || path.as_os_str().as_bytes().contains(&0) {
        return Err(io::Error::other(
            "--listen requires an absolute socket path",
        ));
    }
    let filename = path
        .file_name()
        .ok_or_else(|| io::Error::other("--listen requires a socket filename"))?;
    let parent = fs::canonicalize(
        path.parent()
            .ok_or_else(|| io::Error::other("--listen requires a socket parent"))?,
    )?;
    check_ancestors(&parent)?;
    let path = parent.join(filename);
    // Leave room for the terminating NUL on every Unix platform.
    let capacity = unsafe { std::mem::zeroed::<libc::sockaddr_un>() }
        .sun_path
        .len();
    if path.as_os_str().len() >= capacity {
        return Err(io::Error::other(format!(
            "--listen socket path must be shorter than {capacity} bytes"
        )));
    }
    Ok(path)
}

fn bind(path: &Path) -> io::Result<(UnixListener, SocketPath)> {
    bind_canonical(&canonical_socket_path(path)?)
}

fn bind_canonical(path: &Path) -> io::Result<(UnixListener, SocketPath)> {
    let filename = path
        .file_name()
        .ok_or_else(|| io::Error::other("--listen requires a socket filename"))?;
    let parent_path = path
        .parent()
        .ok_or_else(|| io::Error::other("--listen requires a socket parent"))?;
    let parent_file = File::open(parent_path)?;
    let parent = parent_file.metadata()?;
    if !parent.is_dir() || parent.uid() != unsafe { libc::geteuid() } || parent.mode() & 0o077 != 0
    {
        return Err(io::Error::other(
            "--listen parent must be a private directory owned by the current user (mode 0700)",
        ));
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() {
                return Err(io::Error::other(
                    "--listen refuses to replace a non-socket path",
                ));
            }
            let stale = (metadata.dev(), metadata.ino());
            match probe(path) {
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                    if !fs::symlink_metadata(path).is_ok_and(|meta| {
                        meta.file_type().is_socket() && (meta.dev(), meta.ino()) == stale
                    }) {
                        return Err(io::Error::other(
                            "--listen socket changed during stale check",
                        ));
                    }
                    fs::remove_file(path)?;
                }
                _ => {
                    return Err(io::Error::other(
                        "--listen socket is already in use or cannot be verified stale",
                    ))
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(path)?;
    let socket = SocketPath::new(path, &fs::symlink_metadata(path)?);
    let filename = std::ffi::CString::new(filename.as_bytes())
        .map_err(|_| io::Error::other("invalid socket filename"))?;
    // The verified 0700 parent prevents access during bind; chmod is anchored to it.
    if unsafe { libc::fchmodat(parent_file.as_raw_fd(), filename.as_ptr(), 0o600, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((listener, socket))
}

fn parse(line: &[u8]) -> Result<(Request, OpenPaths), Failure> {
    if line.len() > MAX_REQUEST_BYTES {
        return Err(Failure::new("invalid", "request exceeds 64 KiB"));
    }
    if !line.ends_with(b"\n") {
        return Err(Failure::new("invalid", "request must end with a newline"));
    }
    let value: Value =
        serde_json::from_slice(line).map_err(|error| Failure::new("invalid", error.to_string()))?;
    if value
        .get("version")
        .and_then(Value::as_u64)
        .is_some_and(|version| version != VERSION)
    {
        return Err(Failure::new("unsupported", "expected version 1"));
    }
    if value
        .get("timeout_ms")
        .is_some_and(|timeout| timeout.as_u64().is_none())
    {
        return Err(Failure::new(
            "invalid",
            "timeout_ms must be an integer from 1 to 2000",
        ));
    }
    let request: Request = serde_json::from_value(value)
        .map_err(|error| Failure::new("invalid", error.to_string()))?;
    if request.version != VERSION {
        return Err(Failure::new("unsupported", "expected version 1"));
    }
    if request.action != "open" {
        return Err(Failure::new("unsupported", "expected action open"));
    }
    if request
        .timeout_ms
        .is_some_and(|timeout| !(1..=2000).contains(&timeout))
    {
        return Err(Failure::new(
            "invalid",
            "timeout_ms must be an integer from 1 to 2000",
        ));
    }
    if request.paths.is_empty() {
        return Err(Failure::new("invalid", "paths must be nonempty"));
    }
    let paths = request
        .paths
        .iter()
        .map(|raw| {
            if raw.contains('\0') || !Path::new(raw).is_absolute() {
                return Err(Failure::new(
                    "invalid",
                    "paths must be absolute UTF-8 file paths without NUL",
                ));
            }
            let (path, position) = args::parse_file(raw);
            if path.is_dir() {
                return Err(Failure::new("invalid", "paths must not name directories"));
            }
            let position = (path != Path::new(raw)).then_some(position);
            Ok((path, position))
        })
        .collect::<Result<_, _>>()?;
    Ok((request, paths))
}

fn serve(
    mut stream: UnixStream,
    dispatch: &impl Fn(OpenPaths, Arc<Pending>) -> Receiver<Reply>,
    stopping: &AtomicBool,
) -> io::Result<()> {
    let deadline = Instant::now() + TIMEOUT;
    let line = read_frame(&mut stream, TIMEOUT)?;
    let id = serde_json::from_slice::<Value>(&line)
        .ok()
        .and_then(|value| value.get("id").and_then(Value::as_str).map(str::to_owned))
        .filter(|id| !id.is_empty() && id.len() <= 256)
        .unwrap_or_default();
    let result = if id.is_empty() {
        Err(Failure::new(
            "invalid",
            "id must contain 1 to 256 UTF-8 bytes",
        ))
    } else {
        parse(&line).and_then(|(request, paths)| {
            debug_assert_eq!(request.id, id);
            let pending = Arc::new(Pending {
                state: AtomicU8::new(PENDING),
                deadline: deadline.min(
                    Instant::now() + Duration::from_millis(request.timeout_ms.unwrap_or(2000)),
                ),
            });
            let receiver = dispatch(paths, pending.clone());
            wait_for_open(receiver, &pending, stopping)
        })
    };
    let mut response = json!({"version": VERSION, "id": id, "status": "ok"});
    if let Err(error) = result {
        response["status"] = json!("error");
        response["error"] = json!({"class": error.class, "message": error.message.chars().take(1024).collect::<String>()});
        if let Some(index) = error.path_index {
            response["error"]["path_index"] = json!(index);
        }
    }
    let mut encoded = serde_json::to_vec(&response)?;
    encoded.push(b'\n');
    debug_assert!(encoded.len() <= MAX_RESPONSE_BYTES);
    // Replies are small; a client that won't read must not hold the serial server.
    stream.set_nonblocking(true)?;
    stream.write_all(&encoded)
}

fn wait_for_open(receiver: Receiver<Reply>, pending: &Pending, stopping: &AtomicBool) -> Reply {
    loop {
        if let Ok(result) = receiver.try_recv() {
            return result;
        }
        if stopping.load(Ordering::Acquire) {
            pending.abandon();
            return receiver
                .try_recv()
                .unwrap_or_else(|_| Err(Failure::new("unavailable", "listener is stopping")));
        }
        let remaining = pending.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let failure = pending.abandon();
            return receiver.try_recv().unwrap_or(Err(failure));
        }
        match receiver.recv_timeout(remaining.min(STOP_POLL_INTERVAL)) {
            Ok(result) => return result,
            Err(RecvTimeoutError::Disconnected) => {
                return Err(if pending.state.load(Ordering::Acquire) == ABANDONED {
                    pending.abandon()
                } else {
                    Failure::new("unavailable", "editor handler unavailable")
                })
            }
            Err(RecvTimeoutError::Timeout) => (),
        }
    }
}

fn open(
    editor: &mut Editor,
    compositor: &mut Compositor,
    jobs: &mut Jobs,
    paths: OpenPaths,
) -> Reply {
    use helix_view::handlers::{completion::CompletionEvent, lsp::SignatureHelpEvent};
    editor.handlers.completions.event(CompletionEvent::Cancel);
    let callbacks = {
        let mut cx = commands::Context {
            editor: &mut *editor,
            jobs: &mut *jobs,
            register: None,
            count: None,
            callback: Vec::new(),
            on_next_key_callback: None,
        };
        if let Some(view) = compositor.find::<ui::EditorView>() {
            view.cancel_remote_input(&mut cx);
            view.clear_completion(cx.editor);
        }
        cx.callback
    };
    for callback in callbacks {
        callback(
            compositor,
            &mut compositor::Context {
                editor: &mut *editor,
                jobs: &mut *jobs,
                scroll: None,
            },
        );
    }
    helix_event::send_blocking(&editor.handlers.signature_hints, SignatureHelpEvent::Cancel);
    compositor.remove(ui::lsp::signature_help::SignatureHelp::ID);
    for (index, (path, position)) in paths.into_iter().enumerate() {
        let id = editor
            .open(&path, Action::Replace)
            .map_err(|error| Failure {
                class: "rejected",
                message: error.to_string(),
                path_index: Some(index),
            })?;
        // Close before acknowledgement; the focus-loss hook otherwise queues cleanup.
        compositor.remove_type::<ui::Prompt>();
        if let Some(position) = position {
            let view_id = editor.tree.focus;
            let doc = doc_mut!(editor, &id);
            let cursor = pos_at_coords(doc.text().slice(..), position, true);
            doc.set_selection(view_id, Selection::single(cursor, cursor));
            let (view, doc) = current!(editor);
            align_view(doc, view, Align::Center);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::atomic::AtomicUsize;
    use tempfile::TempDir;

    fn private_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    fn exchange(path: &Path, bytes: &[u8]) -> Value {
        let mut stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        stream.write_all(bytes).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut line = Vec::new();
        BufReader::new(stream).read_until(b'\n', &mut line).unwrap();
        assert!(line.len() <= MAX_RESPONSE_BYTES);
        serde_json::from_slice(&line).unwrap()
    }

    fn request(timeout: u64) -> Vec<u8> {
        format!(
            "{}\n",
            json!({"version":1,"id":"test","action":"open","paths":["/file"],"timeout_ms":timeout})
        )
        .into_bytes()
    }

    fn success(_: OpenPaths, pending: Arc<Pending>) -> Receiver<Reply> {
        let (sender, receiver) = sync_channel(1);
        assert!(pending.begin());
        sender.send(Ok(())).ok();
        receiver
    }

    #[test]
    fn unix_protocol_fixtures_and_bounds() {
        let dir = private_dir();
        let path = dir.path().join("listener.sock");
        let calls = Arc::new(AtomicUsize::new(0));
        let dispatch_calls = calls.clone();
        let server = OpenServer::with_dispatch(&path, move |paths, pending| {
            dispatch_calls.fetch_add(1, Ordering::Relaxed);
            success(paths, pending)
        })
        .unwrap();
        let fixtures: Vec<Value> =
            serde_json::from_str(include_str!("../../tests/fixtures/native-listener-v1.json"))
                .unwrap();
        for fixture in fixtures {
            let result = exchange(&path, format!("{}\n", fixture["request"]).as_bytes());
            let expected = &fixture["response"];
            for key in ["version", "id", "status"] {
                assert_eq!(result[key], expected[key], "{fixture}");
            }
            if expected["status"] == "error" {
                assert_eq!(
                    result["error"]["class"], expected["error"]["class"],
                    "{fixture}"
                );
            }
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        for bytes in [
            b"not json\n".to_vec(),
            b"{}".to_vec(),
            vec![b'x'; MAX_REQUEST_BYTES + 1],
            b"\xff\n".to_vec(),
        ] {
            assert_eq!(exchange(&path, &bytes)["error"]["class"], "invalid");
        }
        let raw = request(2000);
        let mut boundary = raw[..raw.len() - 1].to_vec();
        boundary.resize(MAX_REQUEST_BYTES - 1, b' ');
        boundary.push(b'\n');
        assert_eq!(exchange(&path, &boundary)["status"], "ok");
        let before = calls.load(Ordering::Relaxed);
        boundary.insert(boundary.len() - 1, b' ');
        assert_eq!(boundary.len(), MAX_REQUEST_BYTES + 1);
        assert!(serde_json::from_slice::<Value>(&boundary).is_ok());
        assert_eq!(exchange(&path, &boundary)["error"]["class"], "invalid");
        assert_eq!(calls.load(Ordering::Relaxed), before);
        let id_boundary =
            json!({"version":1,"id":"x".repeat(256),"action":"open","paths":["/file"]});
        assert_eq!(
            exchange(&path, format!("{id_boundary}\n").as_bytes())["status"],
            "ok"
        );
        let too_long = json!({"version":1,"id":"x".repeat(257),"action":"open","paths":["/file"]});
        let response = exchange(&path, format!("{too_long}\n").as_bytes());
        assert_eq!(response["id"], "");
        assert_eq!(response["error"]["class"], "invalid");
        drop(server);
        assert!(!path.exists());
    }

    #[test]
    fn socket_bind_safety_and_owned_cleanup() {
        let dir = private_dir();
        let path = dir.path().join("listener.sock");
        assert!(bind(Path::new("relative.sock")).is_err());
        assert!(bind(Path::new("/")).is_err());
        assert!(bind(&dir.path().join("x".repeat(200))).is_err());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(bind(&path).is_err());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&path, "keep").unwrap();
        assert!(bind(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"keep");
        fs::remove_file(&path).unwrap();
        symlink("missing", &path).unwrap();
        assert!(bind(&path).is_err());
        assert!(fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        fs::remove_file(&path).unwrap();
        drop(UnixListener::bind(&path).unwrap());
        let server = OpenServer::with_dispatch(&path, success).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert!(bind(&path).is_err());
        fs::remove_file(&path).unwrap();
        let replacement = UnixListener::bind(&path).unwrap();
        let started = Instant::now();
        drop(server);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(path.exists(), "old owner removed replacement socket");
        drop(replacement);
    }

    #[test]
    fn socket_rejects_replaceable_ancestors_and_accepts_private_ones() {
        let dir = private_dir();
        let ancestor = dir.path().join("shared");
        let parent = ancestor.join("private");
        fs::create_dir_all(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o770)).unwrap();
        let path = parent.join("editor.sock");
        assert!(bind(&path).is_err());
        assert!(!path.exists());
        fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o700)).unwrap();
        let (listener, socket) = bind(&path).unwrap();
        drop(listener);
        drop(socket);
    }

    #[test]
    fn canonical_parent_survives_intermediate_symlink_substitution() {
        let dir = private_dir();
        let parent = dir.path().join("private");
        let substituted = dir.path().join("substituted");
        fs::create_dir(&parent).unwrap();
        fs::create_dir(&substituted).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&substituted, fs::Permissions::from_mode(0o777)).unwrap();
        let hop = dir.path().join("hop");
        let alias = dir.path().join("alias");
        symlink(&parent, &hop).unwrap();
        symlink(&hop, &alias).unwrap();
        let requested = alias.join("editor.sock");
        let canonical = fs::canonicalize(&parent).unwrap().join("editor.sock");
        drop(UnixListener::bind(&canonical).unwrap());
        let verified = canonical_socket_path(&requested).unwrap();
        assert_eq!(verified, canonical);

        fs::remove_file(&hop).unwrap();
        symlink(&substituted, &hop).unwrap();
        let (listener, socket) = bind_canonical(&verified).unwrap();
        assert_eq!(socket.path, canonical);
        assert_eq!(fs::metadata(&canonical).unwrap().mode() & 0o777, 0o600);
        assert!(!substituted.join("editor.sock").exists());
        let client = UnixStream::connect(&canonical).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        drop((client, accepted));

        let replacement = UnixListener::bind(&requested).unwrap();
        drop(listener);
        drop(socket);
        assert!(!canonical.exists());
        assert!(requested.exists(), "cleanup followed the substituted alias");
        drop(replacement);
    }

    #[test]
    fn queued_completed_reply_wins_over_expired_deadline() {
        let (sender, receiver) = sync_channel(1);
        sender.send(Ok(())).ok();
        let pending = Pending {
            state: AtomicU8::new(DONE),
            deadline: Instant::now() - Duration::from_millis(1),
        };
        assert!(wait_for_open(receiver, &pending, &AtomicBool::new(false)).is_ok());
        assert_eq!(
            pending.abandon().message,
            "request completed; reply unavailable"
        );
    }

    #[test]
    fn live_socket_with_full_backlog_does_not_block_startup() {
        let dir = private_dir();
        let path = dir.path().join("full.sock");
        let listener = UnixListener::bind(&path).unwrap();
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        let _queued = UnixStream::connect(&path).unwrap();
        let started = Instant::now();
        assert!(bind(&path).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(path.exists());
    }

    #[test]
    fn timeout_abandons_queued_work_but_does_not_claim_to_cancel_running_work() {
        let dir = private_dir();
        let path = dir.path().join("listener.sock");
        let held = Arc::new(Mutex::new(None));
        let queued = held.clone();
        let server = OpenServer::with_dispatch(&path, move |_, pending| {
            let (sender, receiver) = sync_channel(1);
            *queued.lock().unwrap() = Some((pending, sender));
            receiver
        })
        .unwrap();
        assert_eq!(exchange(&path, &request(10))["error"]["class"], "timeout");
        assert!(
            !held.lock().unwrap().as_ref().unwrap().0.begin(),
            "expired callback started"
        );
        drop(server);
        let server = OpenServer::with_dispatch(&path, move |_, pending| {
            assert!(pending.begin());
            let (sender, receiver) = sync_channel(1);
            *held.lock().unwrap() = Some((pending, sender));
            receiver
        })
        .unwrap();
        assert_eq!(
            exchange(&path, &request(10))["error"]["class"],
            "unavailable"
        );
        drop(server);
    }

    #[test]
    fn slow_client_has_a_total_deadline_and_the_next_request_runs() {
        let dir = private_dir();
        let path = dir.path().join("listener.sock");
        let server = OpenServer::with_dispatch(&path, success).unwrap();
        let mut slow = UnixStream::connect(&path).unwrap();
        let slow_thread = thread::spawn(move || {
            for _ in 0..20 {
                if slow.write_all(b" ").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(150));
            }
        });
        let started = Instant::now();
        assert_eq!(exchange(&path, &request(2000))["status"], "ok");
        assert!(started.elapsed() < Duration::from_secs(3));
        slow_thread.join().unwrap();
        drop(server);
        assert!(!path.exists());
    }
}
