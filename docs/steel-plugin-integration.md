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

Steel owns all editor behavior in the fork. The complete action module is 15
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

## Why the server stayed in Rust

Steel `1b785a4e` provides a useful TCP floor: connect, listen, local-address
lookup, blocking accept, nonblocking listener and stream modes, stream ports,
and stream shutdown. Steel also provides native threads, channels, and an
explicit thread join.

The following lifecycle pieces were missing from the reviewed surface:

| Needed contract | Reviewed Steel surface | Consequence |
| --- | --- | --- |
| Bound idle reads and writes | No TCP read-timeout or write-timeout setter | A blocking stream can hold the server past its request budget; nonblocking mode requires a polling state machine. |
| Interrupt a blocked listener | No listener close, shutdown, timed accept, or cancellable accept | A thread blocked in `tcp-accept` cannot stop in response to engine reload. |
| Cancel native I/O before joining a plugin thread | `thread-join!` waits; `thread-suspend` does not interrupt native code | A plugin must arrange its own socket wakeup and cancellation before join. |
| Deliver a background request to Helix and await its editor-thread result | Helix exposes editor-thread callbacks, but the reviewed plugin surface has no request-response ingress primitive for a background Steel server | A Steel implementation needs a worker channel, editor polling callback, response channel, generation handling, and shutdown coordination. |

A Steel-only implementation could put both listener and streams in
nonblocking mode and build polling, framing, channels, and reload coordination
in Scheme. That design adds more lifecycle machinery than the native seam and
makes prompt shutdown harder to prove.

The Rust module owns the mechanism Steel could not express with the same
bounded lifecycle:

- bind `127.0.0.1:0` and return the selected address;
- apply socket deadlines and 64 KiB request and response limits;
- authenticate before editor dispatch;
- track the active stream, wake blocked accept, stop, and join the listener;
- dispatch through Helix's job queue with an engine-generation guard.

The module uses the Rust standard library plus Helix's existing Serde stack. It
does not publish endpoints or tokens, select an editor instance, or interpret
editor actions. This boundary costs 384 production Rust lines and 167 focused
test lines in the fork.

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
stop, idle-connection shutdown, and owner drop. The exact Nix-built Linux
binary and isolated Steel module pass their checks. The code uses portable
standard TCP. Yazelix has not exercised this runtime on macOS.
