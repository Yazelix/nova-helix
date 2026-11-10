# Steel plugin integration notes

This note records the Steel and Helix integration experience from the minimal
Yazelix Helix fork for Steel and Helix's Steel maintainers.

## Reviewed revisions

- Helix Steel branch:
  [`5a8635be`](https://github.com/mattwparas/helix/commit/5a8635beda77414850a2b9604aa0643e4713db3b)
- Steel 0.8.3:
  [`1b785a4e`](https://github.com/mattwparas/steel/commit/1b785a4e9d24e3553b242522b35d4498dae72816)

The Yazelix use case sends an authenticated request from another local process
to an existing Helix instance. Helix must open files or a directory picker on
its editor thread. The server must bound each frame and timeout, reject invalid
authentication before dispatch, and stop without leaving a listener thread
behind during Steel engine reload or editor shutdown.

## What worked well

Steel owns the Nova bridge's editor behavior. The complete action module is 15
lines of Scheme:

- `change-current-directory` adopts the managed workspace.
- `open` opens one or more files or a directory picker.
- Scheme variadic arguments preserve the caller's file order without an
  adapter or native action table.

Helix's `provide` and `require` module flow made the code easy to isolate and
package. Nested filesystem modules must import siblings relative to their own
file. The integration test registers only a stub `helix/commands.scm`, then
loads the real Yazelix modules through Steel's filesystem resolver and checks
command order without starting the editor UI.
Steel also handles the request boundary cleanly: a 39-line composition module
validates the two action payloads, rejects unknown actions before editor
commands run, and starts the transport with a caller-provided token.

The embedding API also supports a useful hybrid boundary. Rust retains a
rooted Steel closure, sends work through Helix's editor job queue, installs the
existing command context, and calls the closure. The closure receives only
`request_id`, `action`, and `payload`. Steel decides what the action means, so
Rust contains no file-opening, picker, workspace, or session policy.

Relevant source:

- [Steel actions](../yazelix/steel/bridge-actions.scm)
- [Steel request composition](../yazelix/steel/bridge.scm)
- [Steel integration test program](../helix-term/tests/yazelix_steel.scm)
- [Steel integration test harness](../helix-term/tests/yazelix_steel.rs)
- [Native transport seam](../helix-term/src/commands/engine/steel/yazelix_transport.rs)

## Native open-file listener

On Unix, `hx --listen /absolute/private/editor.sock [FILE...]` accepts open-file
requests with or without Steel. It starts after argument files open. The caller
creates the parent directory, owned by the current user with no group or other
permissions (normally 0700). Helix creates the socket with mode 0600 without
changing the process umask. The private directory protects socket creation too.
Helix resolves the parent once and validates every ancestor of that canonical
path: current-user/root ownership, no group/other writes unless sticky. This
permits trusted sticky roots such as `/tmp` while rejecting a replaceable shared
ancestor. Bind, stale probing/removal and cleanup use only the canonical path;
retargeting an original alias or intermediate symlink cannot redirect them.
Permissions are set relative to the held verified parent directory, without a
process-global umask.
Access is granted to the same user; this listener needs no token or registry.

Give each editor its own absolute socket path. The canonical path including its terminating
NUL must fit the Unix socket limit (108 bytes on Linux, 104 on macOS). Helix
refuses non-socket paths, symlinks and live sockets. It removes a stale socket
only after connection refused and a matching device/inode check. Two editors
racing for one path are unsupported. A bind failure logs and displays a
`--listen` error; editing remains available without listening. Windows rejects
the flag.

Send one newline-terminated UTF-8 JSON object per connection. Frames including
the newline fit within 64 KiB. Native v1 is independent of Steel's schema 2:

```json
{"version":1,"id":"open-1","action":"open","paths":["/absolute/file.rs:12:3"],"timeout_ms":750}
{"version":1,"id":"open-1","status":"ok"}
{"version":1,"id":"open-1","status":"error","error":{"class":"rejected","message":"...","path_index":0}}
```

`version`, `id`, `action` and `paths` are required. `id` contains 1–256 UTF-8
bytes and is echoed when valid. Malformed JSON or an invalid id gets an empty
`id`. Unknown fields are rejected. A different unsigned version gets a v1
`unsupported` response, even when its other fields differ. Optional `timeout_ms`
is an integer from 1 to 2000 (default 2000), bounding editor wait after the frame.

`open` is the only action. `paths` is a nonempty list of absolute UTF-8 file
paths without NUL; directories are rejected. Paths are validated before dispatch
and opened in order in the focused view. `FILE:LINE[:COL]` uses command-line
parsing (1-based positions clamped to the document); an existing literal filename
takes precedence. A suffix moves the cursor and centers the view. Without one,
an already-open buffer keeps its selection and is not reloaded. A missing file
becomes a new named buffer. Multiple opens are not a transaction: an error can
leave earlier paths open; zero-based `error.path_index` identifies the failure.

Modified buffers and insert undo history remain. Insert mode changes to normal
mode and closes completion/signature UI for the old document. Document-bound
prompts close on focus loss, following upstream behaviour; this
includes a half-typed search or command prompt. Pending keys and next-key
callbacks are cancelled against the old document before switching. A picker
keeps its own filter/selection. An unmodified scratch buffer is replaced according to
ordinary buffer switching.

Error classes are closed in v1:

| Class | Meaning |
| --- | --- |
| `invalid` | Bad/oversized frame, fields, id, timeout or paths. No open dispatched. |
| `unsupported` | Unknown version or action. No open dispatched. |
| `rejected` | Editor/filesystem open failed; `path_index` identifies it. Earlier paths may have opened. |
| `timeout` | Queued request expired and was abandoned. Its callback cannot open later. |
| `unavailable` | Listener stopped, editor queue unavailable, or execution began without a timely result. Outcome may be unknown. |

Connections are served serially; additional clients wait in the operating
system's socket queue. An idle or slow client can delay them. Frame reads share
a total 2-second deadline, so trickling bytes cannot hold the server indefinitely.
Editor wait uses the earlier of that connection deadline and `timeout_ms`.
Replies use nonblocking writes so a client that will not read cannot hold the
server. Incomplete/timed-out I/O can close without a JSON reply.

A client timeout/disconnect does **not** cancel a request. Allow time for the
server response, for example a 750-ms server budget within a 1-second client
deadline. Only a `timeout` reply promises abandonment. A lost reply or
`unavailable` does not prove whether a file opened; do not retry an uncertain
request assuming it did not run.

Close/drop stops and joins the listener. On Unix, poll waits for listener
readiness or a private stop channel, with no
periodic idle wakeups. Active-stream shutdown interrupts blocked I/O; exit never
connects to a possibly replaced socket path. The non-Unix Steel fallback still
uses a short accept poll interval. Cleanup unlinks
only a matching device/inode. SIGHUP uses graceful shutdown like SIGTERM/SIGINT.
SIGKILL, aborts and machine failure can leave stale sockets for the next launch's
recovery or the caller's runtime-directory sweep.

The shared [transport](../helix-term/src/remote.rs) owns framing and lifecycle.
The [native adapter](../helix-term/src/remote/native.rs) owns v1 validation and
editor dispatch. The [Steel adapter](../helix-term/src/remote/steel.rs) retains
schema 2, constant-time token checks, the 5-second maximum, response bounds and
existing error classes. Its builtin wrapper retains the rooted closure and
engine-generation guard; Nova's actions and registration stay unchanged.
The mechanism uses the standard library and existing Serde/libc dependencies.
Steel lacks bounded/cancellable I/O and an equivalent editor request ingress.

Reusable request/error fixtures for clients and ports are in
[`native-listener-v1.json`](../helix-term/tests/fixtures/native-listener-v1.json).
Error messages are diagnostic text, not strings clients should match.

## Steel improvements that would change this decision

The smallest useful additions would be:

1. TCP stream read and write timeout setters.
2. A listener accept operation with a timeout or cancellation contract.
3. A documented host pattern for sending a value from a background Steel task
   to the editor thread, awaiting its result, and cancelling the task on engine
   replacement.

Those capabilities would support a Steel prototype with bounded I/O and a
joined lifecycle. Yazelix could then reassess the remaining native transport.

## Evidence boundary

Focused tests cover loopback allocation, authentication before dispatch,
malformed and oversized input, response bounds, handler timeout, explicit
stop, idle-connection shutdown, and owner drop. The original Steel bridge was
checked with the Nix-built Linux binary and isolated Steel module; native
listener proof is recorded with its change. Yazelix has not exercised this
runtime on macOS.

Accept errors other than interruption stop the server (inherited from the Steel
transport). Pathname races with other processes of the same trusted user remain
unsupported. Filesystem opens retain Helix's usual blocking I/O semantics,
including a FIFO or stalled filesystem blocking the editor thread; a client
must treat a lost response as an uncertain outcome.
