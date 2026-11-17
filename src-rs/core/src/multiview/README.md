# Multiview composition and private service

Multiview is disabled by default. Without a protected operator media plan, the
private shell advertises `max_inputs = 0`, no input adapters and no output
profiles. An explicitly configured Unix host can enroll the bounded HTTP MPEG-TS
profile through the private broker; it does not accept player-supplied URLs or
turn public asset endpoints into a media proxy.

See the [operator and deployment guide](../../../../docs/multiview-service.md)
and [broker runbook](broker/README.md) for supported profiles, source authority,
listener/TLS restrictions, native provisioning, renewal and cleanup. Test-only
constructors and synthetic inputs are not a substitute for normal-binary or
native/device qualification. Existing public playback paths remain independent.

## Local pre-push checks

Before pushing new or changed JSON, use Node 22 and the existing lockfile-installed
npm dependencies to run `npm run lint` from the repository root. This runs both the
whole-project Biome check and the separate source-correctness check. Rust tests do
not cover JSON formatting. After a formatting-only change, verify that parsed old
and new JSON are equivalent, rejecting duplicate keys and non-finite values;
array order and value types must remain unchanged.

When adding or renaming Python tests, run
`python3 scripts/test-python.py --check-inventory` from the repository root before
pushing. It checks exact suite registration without executing tests or writing
reports; a passing inventory check is not test-execution evidence.

For JavaScript, workflow or portable-fixture changes, reuse the existing targeted
gate rather than creating another check runner:

```sh
PYTHON=/path/to/python3.12 npm run check:windows-readiness -- \
  --report-dir /absolute/fresh/report-directory
```

Use the existing hash-locked Python workflow dependencies and installed Playwright
Chromium. The default includes a JavaScript build, delayed-PIN cases and the
headless direct-media fixture. For unrelated changes only, `--skip-browser` runs
the portable checks and records that narrower scope. Neither mode compiles native
code or launches an installed player. Run against the admitted commit and retain
its passing receipt with matching `head`, `dirty: false` and
`sourceUnchanged: true`; changes after that check require revalidation. Local
portable results do not replace actual Windows/Linux/macOS or device qualification.

## Ordinary tests

From the repository root, with the existing locked workspace dependencies:

```sh
cargo test --locked -p ottplay-core multiview::
cargo test --locked -p ottplay-server --test multiview_fixtures
cargo test --locked -p ottplay-server --test multiview_broker_boundary
```

The initial pre-publication isolated harness passed 129 tests and left three
media/lifecycle tests ignored. These are historical counts, not the current test
inventory; the current compiler listing and receipts determine which tests ran. It imported the actual source modules and used dependency versions
from the root lockfile; this is not full-workspace, all-platform or device
qualification. Normal Cargo integration-test discovery includes both server test
files. Windows jobs alone do not exercise the Linux/macOS encoder implementation.

## Explicit media and process fixtures

The five opt-in tests in `src-rs/server/tests/multiview_fixtures.rs` cover finite
media composition, owned-worker cancellation/cleanup, authenticated in-memory HLS
source replacement/retirement, and bounded streamed-session continuity with two
audio owners, plus real streamed-decoder short-track completion and subsequent
failure, revocation and cancellation. They run separately from the ordinary tests.

Run them only in an isolated environment using disposable generated media and
explicit absolute `OTTPLAY_MULTIVIEW_FFMPEG` and `OTTPLAY_MULTIVIEW_FFPROBE` tool
paths. Inspect the test source first; it starts owned subprocesses and uses
scratch output directories. Run one test at a time with `--ignored --exact` and
`--test-threads=1`, for example:

```sh
cargo test --locked -p ottplay-server --test multiview_fixtures \
  fixture_http_source_cas_dedupe_media_custody_and_retirement \
  -- --ignored --exact --test-threads=1
```

A separate bounded steady-state media run is evidence only for its selected-source
interval. Startup, switching, full-film lifetime, native UI/RPC and hardware
playback require their own qualification. Keep failures as evidence rather than
relabelling them as successful playback.

The decoder fixture additionally requires `OTTPLAY_MULTIVIEW_PENDING_INPUTS`
(an immutable two-input manifest with SHA-256 hashes, 96×64/10 FPS geometry and
exact audio/video counts) and a fresh `OTTPLAY_MULTIVIEW_PENDING_CASE_DIR`.
Its command-local private `TMPDIR` must contain a space, apostrophe, colon and
backslash to exercise filter-path escaping. The Unix CI helper generates these
synthetic inputs and supplies the environment. See
[`streamed_decoder.rs`](../../../server/tests/support/streamed_decoder.rs)
for the manifest schema and complete prerequisites. Invoke the exact entry with
`--ignored --exact --test-threads=1`, as above, using
`streamed_decoder_short_track_pending_and_failure` as the test name.

The decoder fixture uses generated 48 kHz stereo TS inputs with short audio and
short video in turn. A validated per-track terminal watermark can permit trailing
silence or an ended-video slate while its sibling continues. This is provisional
terminal state, not proof of clean source EOF or permission to retire custody.
Later decoder failure, cancellation or revocation invalidates that state. Positive
cases require successful owned-process completion and input-pump completion;
negative cases verify late failure, revocation and cancellation. These fixtures do
not qualify every resampler or variable-frame-rate source.

## Capture-time output scheduling

Persistent Session uses a common wall-capture clock for audio/video output. The
compositor freezes source views before recording `captured_at`; a newly captured
image cannot be assigned to an earlier output tick. Complete-write counters,
limited catch-up and bounded reuse of an earlier committed image preserve encoded
output time. A bounded three-target window retains eligible captures for the
current output tick and the next two ticks, rather than discarding all intermediate
captures while one image waits for a delayed writer. Each target uses a capture
whose timestamp is no later than that target. The current bucket remains leased
through a partial write; the window advances only after the full frame write has
completed and the previous committed reference has been replaced. Captured images
may remain eligible for two rational video periods,
including naturally sparse 1–4 FPS output. A committed image can be reused for at
most two ticks. Physical writes and pacer debt retain the stricter minimum of two
video periods, 200 ms and the configured stall limit; excess pressure terminates
that output. Each image retains its first visual-invalidation boundary even across
several rapid source changes. Nonvisual focus/audio changes preserve
an otherwise valid in-flight image with its original capture timestamp and visual
owner. Visual changes still invalidate it at their recorded boundary.
Static pictures are valid content and are not identified as scheduler repetitions
by pixel equality. This is a capture-time contract, not reconstruction of unknown
input PTS or file audio/video offsets.

Startup uses only source-free black video and silent audio with equal encoded
pre-roll duration. Their indices continue into the shared epoch. A one-way
initialization phase continues synthetic output until FFmpeg reports closed HLS
output and encoding progress; accepting bytes into an OS pipe alone is not
readiness. Source audio remains inactive until the atomic transition to steady
scheduling. Constructor success also requires complete steady writes from both
lanes. Initialization keeps the configured absolute startup deadline; it cannot
be re-entered after steady operation. The compositor starts before encoder
readiness; failed startup stops and joins it. Existing
standalone legacy encoder constructors retain their previous scheduling path.

Session reserves six output canvases, per-source storage and the existing small
buffer allowance within its configured frame-buffer cap. Transition black frames
use fixed-size zero chunks. This reservation does not bound FFmpeg RSS, operating
system pipe buffers or total process memory. Source and media fixture qualification
remains separate from source-level scheduler tests.

## Activation gates

Before production registration, qualify request admission limits, server-owned
live input streaming/demuxing, checked Plex range access, Local file custody,
credential expiry, renewable output authority, complete provider/decoder retirement,
and native end-to-end playback. Fixture `from_root` custody is not a production
Library grant; frame-buffer limits do not bound FFmpeg RSS. No existing successful
fixture or favorite digest grants permission to enable production capacity.

## Scoped Unix CI

Relevant multiview, dependency and qualification-workflow changes select hosted
Linux/macOS checks; a complete unrelated diff skips that matrix. An unknown or
incomplete diff cannot silently skip it. These checks run the actual ordinary
core multiview and server fixture suites, require the cleanup/lease/plan regression
names, then execute the five opt-in fixtures individually. They also execute the
complete broker boundary target and listener namespace, plus ordinary activation
tests for the explicitly configured private listener.

Both FFmpeg tools must expose input `fd`, and the required codecs and filters must
be present. Receipts bind source, lockfile, tool and test-binary hashes and retain
test logs and owned-process cleanup results. A missing capability or incomplete
cleanup fails qualification. The streamed fixture checks a finite selected-owner
interval, including motion, timestamps, every-50-ms audio energy/phase and the
return-audio tail. It does not establish full-film lifetime, private-v1/native-client
or TV acceptance.

Encoder diagnostics retain the first terminal error together with a closed failure
site and originating lane. If one lane forwards a failure from another, its
original site and lane remain intact. This records the first globally latched
terminal failure, not necessarily the earliest detection by wall-clock time.
Existing error getters retain their behavior; the detailed getters are read-only
and do not change snapshots or the wire protocol. The streamed fixture records
only these closed labels and its existing safe counters before cleanup.

For a selection-age failure, bounded numeric evidence records the selected
capture, retained buckets, committed sequence and most recent feed publication
attempt relative to the failing target. These are observations, not permission
to reuse an expired frame. The publication attempt covers only calls reaching
the feed mutex; it does not measure compositor work or calls rejected earlier
by the state barrier. The last-capture field may also be a visual-invalidation boundary, not a
publication attempt. Absolute clock instants and source data are not exposed.
RGB ownership conversion completes before the session control mutex is acquired;
the visual fence is still checked while publishing under that mutex.

The compositor waits toward its existing absolute next deadline using sleep
requests capped at 5 ms and checks cancellation between requests. This improves
cancellation responsiveness and avoids requesting a whole output period in one
sleep; it does not guarantee an operating-system wakeup deadline, change capture
frequency or relax the capture-age and output-debt limits.

A server-side decode-open failure can also retain a close observation from that
specific Play operation before retiring its source. The `decode_open_failure`
record uses `try_lock` on its control mutex and copies only the first-close record and
scalar session flags. It never calls the full Core snapshot, reads a cancellation
token, waits for the session mutex, or retains a source/resource handle. A busy or
poisoned mutex produces `observation_present=false`.

`observation_mode=retained_only` means no fresh Core state was sampled;
`session_cancelled=not_observed` is unknown, not evidence that cancellation was
false. A stored first-close record remains historical evidence for this session,
not necessarily the first event by wall-clock time. Source cleanup and reservation
retirement retain their original result; diagnostic output is deferred until
Pending/done and the operation receipt are complete. Do not attribute a nearby
untagged watchdog line from another session to the failed Play operation.

Normal-server fixture transport failures retain only closed error-category flags
and elapsed request milliseconds, alongside the existing status/count/digest
fields. They never retain a reqwest error, URL, headers or response body. These
flags are evidence, not permission to retry or a diagnosis of the native cause;
request deadlines and exact temporary-busy handling are unchanged.

The normal-server audio health oracle applies only to completed live segments of
this fixed 10 fps, one-second-GOP, 48 kHz stereo AAC output profile. It requires a
complete video window (at least ten decoded frames and at least one second of
actual timestamp duration), exactly one audio and one video stream, 1024-sample AAC
frames and exact internal audio timestamp continuity. Both audio endpoints must
be within one AAC quantum, `1024/48000` seconds, of the corresponding video
endpoints, using checked rational arithmetic. A common timestamp-origin shift is
allowed; an audio-only shift is not.

Probe, RGB and PCM checks share the same immutable TS identity. If the actual AAC
frames contain `N` samples per channel, mono 16 kHz PCM must contain either
`floor(N/3)` or `ceil(N/3)` samples and an even byte count. The existing eight-second
PCM maximum, process/byte budgets, and all tone, replacement, freshness and
cleanup assertions remain mandatory. This replaces the invalid one-second PCM minimum: a complete
one-second video segment can legitimately contain 46 AAC frames, or 47104 source
samples, yielding 15701/15702 mono samples rather than 16000.

This is a fixed-profile health contract, not a universal HLS validator. Endpoint
quantization and AAC priming are distinct from integer resampler rounding. The
one-sample rounding ambiguity and possible boundary-packet ambiguity remain;
loss-free edge claims require adjacent-segment coverage. Neither these checks nor
a passing diagnostic identify the cause of an earlier native timing failure.
