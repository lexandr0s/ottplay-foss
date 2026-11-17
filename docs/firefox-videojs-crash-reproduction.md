# Firefox initial Video.js teardown: controlled phase diagnostic

Issue #1005 remains open. Native content-process crashes have recurred in the
original full-app five-scenario Linux Firefox sequence. Later passes and isolated
append probes do not prove a fix. This opt-in diagnostic changes **when** the
original initial channel transition runs; it does not replace ordinary acceptance.
No production lifecycle, decoder, ordinary acceptance matrix, retries, assertion or timeout changes.

## Actual phase and owner

`CI_FIREFOX_VIDEOJS_PHASE=append-active` observes the original native
`SourceBuffer.appendBuffer` return, then queues a microtask. The transition runs
only if the **real** video buffer is still updating after the append JavaScript
stack unwinds. `append-completed` instead observes a real video `updateend`, then
requires `updating === false` and a nonempty buffered range in that microtask.
This observes a microtask checkpoint after our updateend listener returns; it
does not claim that every other native-event listener has completed.
Neither mode forces buffer state, blocks responses or suppresses audio requests.
Both require actual audio and video SourceBuffers on the same MediaSource.
Both buffers must still be members of its live `sourceBuffers` list at
observation and deferred dispatch. Historical map entries cannot authorize a
removed buffer. Throwing native `buffered` getters fail the precondition without
throwing into the application event/append stack.

The object URL must belong to that MediaSource and equal the actual video
`currentSrc`. `window.video` must be an HTMLVideoElement and the exact element
returned by `videojs.getPlayer("video").tech(...).el()`, not the `#video` wrapper.
The original `__ottMedia.current()` handle and video element are captured at arm
and rechecked before dispatch. Revoked URLs, changed owners, other tech elements,
unknown buffers or missed phases cannot authorize the transition.

Without opt-in the page callback stays synchronous and returns undefined, as
before. Only the opt-in branch returns the observer Promise.

The four historical calls, their order and the existing step observations remain:
`stbStop(); closeList(); __ottMedia.useSource(null); __ottMedia.open(null);`.
They run at most once. An 8-second precondition deadline fails explicitly if no
valid phase occurs. It is inside the unchanged 60-second test deadline. No retry
or fallback transition is performed. Hooks/listeners are restored after success,
transition failure, precondition expiry, explicit retirement and pagehide. A stale
owner cannot dispatch; without another native event, expiry provides bounded hook
retirement. Context teardown also destroys page-local hooks.

Only a closed `{attempts:1,mode,phaseObserved:true}` console record is forwarded to
the host; URLs, media bytes, raw errors and native crash dumps are not forwarded.
A missing record on crash proves no particular phase. Existing crash collection
retains bounded module-relative signatures/build IDs, not raw dumps. See
[collector policy](firefox-crash-diagnostics.md).

## Hosted execution

The CI workflow has a manual-only `firefox-videojs-phase` choice: `off` by default,
or `both`. The separate diagnostic job waits for ordinary browser acceptance to
finish and requires its build artifact, then runs two fixed phase arms in sequence
with `max-parallel: 1`. Ordinary acceptance is unchanged and its failure is never
replaced by a diagnostic pass; diagnostics may still run after an ordinary failure
when the build succeeded.

Each arm uses the existing pinned toolkit with one declared Linux Firefox 153.0 /
Playwright 1.62.1 target, existing codec admission, all five original full-app
scenarios in order, worker 1, retries 0 and the unchanged deadlines. The static
single-row diagnostic selection does not modify the ordinary 35-row inventory.
Evidence prefixes `firefox-phase-append-active` and
`firefox-phase-append-completed` distinguish these timing interventions from
ordinary acceptance and release qualification. No release channel is attached.

The command is selected only from the fixed phase matrix, never from free-form
workflow input:

```sh
CI_FIREFOX_CRASH_DIAGNOSTICS=1 CI_FIREFOX_VIDEOJS_PHASE=append-active node scripts/browser-acceptance.cjs
```

The completed arm sets `append-completed`. Both use the same prepared
`CI_BROWSER_DIAGNOSTICS`, `CI_PLAYWRIGHT_MODULE`, candidate assets and evidence
paths as ordinary acceptance. Do not loop or retry until green. A missed phase is
an explicit diagnostic precondition failure, not a reason to change readiness.
Do not set this variable across all 35 targets; a non-Firefox Video.js scenario
is rejected. The manual Linux diagnostic has not yet been executed.

## Local proof boundary

The existing `tests/test_firefox_crash_diagnostics.cjs` suite contains pure fake
native-object tests for deferred dispatch, identical append bytes, actual
updateend/nonempty-range requirements, no audio, stale owners, foreign source or
tech, revoked URLs, missed phases and cleanup on failure. These prove observer
contracts only. They do not exercise Firefox, VHS, codecs or native crash capture.
Native feasibility runs are described separately below; these pure tests do not establish a causal production fix.

## Local native feasibility results

Two separately declared, isolated macOS arm64 preflights used actual Firefox
153.0 and Playwright 1.62.1. Each ran only the original selected Video.js case
once, with one result, zero retries and unchanged case deadline:

- `append-active`: passed; the phase record and original transition steps 0–7
  were observed, and the channel transition completed in 213 ms.
- `append-completed`: passed; the real buffered range was present, the phase
  record and steps 0–7 were observed, and the transition completed in 29 ms.

Owned process groups and browsers were cleaned up. An earlier selector setup
error selected no tests and launched no browser; that failed setup was retained,
then its selector was corrected before these native attempts.

These results establish that both observers are reachable in the full
application on macOS. They do not reproduce Linux's crash, qualify the complete
five-case Linux sequence, exercise native crash collection on a crash, or prove a
production repair. Issue #1005 remains open.
