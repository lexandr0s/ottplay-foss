# Private multiview service

Multiview combines independent inputs on the server and sends one H.264/AAC HLS
output to a compatible native player. The television decodes one output; it does
not need nine simultaneous hardware decoders. Layout, focus and audio selection
use the private `ottplay-multiview-v1` protocol.

The service is disabled by default. An ordinary server, or a protected operator
configuration without `media`, advertises zero media capacity. A configured media
host advertises its admitted profile only on the explicitly selected listener.
Installing a release does not enroll household sources or enable a public proxy.

## Supported first profile

- Unix server with explicitly enrolled server-owned HTTP(S) inputs. Direct MPEG-TS
  uses `http-mpegts-v1`; the additional live HLS-TS input uses `http-hls-ts-v1`.
  Each adapter needs its own normal-binary qualification receipt before deployment.
- One composed 640×360 output at 10 FPS, up to nine input slots; grid and PiP geometry.
- 320×180 decoded slot frames; the existing 16 MiB application-frame reservation,
  64 million input pixels/second and 128 MiB output-directory limits apply.
- Input pixel rate is measured before downscaling. Nine slots do not imply that
  nine arbitrary high-resolution streams fit the admission budget.
- Encrypted HLS, fMP4, MP4, local filesystem roots, provider allocation APIs and
  automatic Plex/VPortal profile import are outside this profile. They must not be
  represented as supported merely because a URL or catalog item exists.

The TV/client must support the private protocol and its source-identity checks.
Browser playback of the ordinary OTTPlay interface does not turn that browser
into a native multiview controller. Here.now does not host the compositor or carry
its media traffic.

## Runtime prerequisites

The server invokes explicitly configured local `ffmpeg`, `ffprobe` and `mkfifo`
executables. FFmpeg must provide H.264 encoding through `libx264` and AAC encoding.
There is no executable search through a request, source URL or inherited `PATH`.
The complete player container supplies FFmpeg; the guide-only image stays small.
For a standalone server installation, install compatible tools on that host and
use their absolute paths. Trusted Homebrew symlinks are resolved before use.

Create the output root as an owner-only directory (0700), outside the public web
root. It contains only unique encoder-epoch directories created by this service.
The service does not reuse an existing directory as an encoder output.

FFmpeg is an external program with its own licenses. See
[FFmpeg licensing](https://ffmpeg.org/legal.html) and the
[upstream Alpine package information](https://pkgs.alpinelinux.org/package/edge/community/x86_64/ffmpeg)
for the container's distribution packages; inspect the actual image's installed
package versions rather than inferring them from an example here.

## Protected operator configuration

Pass one absolute configuration path with `--multiview-config`. The file must be
regular, single-link, mode 0600, owned by the service user and outside the public
web root. Its ancestor directories must be protected from other users' writes.
Symlink paths, hardlinks, FIFOs, unknown fields, oversized documents and changing
files are rejected. Do not place credentials in Git, hosted profiles or a shared
cloud-synchronized directory.

The existing top-level fields are `version`, `server_id`, `origin`, `listener`,
`allowed_origins`, `signing_key` and `credentials`. Optional `media` adds:

```json
{
  "version": 1,
  "adapter": "http-mpegts-v1",
  "source_ref": "home-tv",
  "profile_uuid": "11111111-1111-4111-8111-111111111111",
  "config_revision": "config-1",
  "policy_revision": "policy-1",
  "allowed": [
    {"subject": "living-room", "installation_id": "REPLACE_WITH_PERSISTED_DEVICE_ID"}
  ],
  "network": [
    {"origin": "https://media.example.net", "private_addresses": []}
  ],
  "items": [
    {
      "item_ref": "channel-1",
      "favorite_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "title": "Example channel",
      "provider": "m3u",
      "input_adapter": "http-mpegts-v1",
      "url": "https://media.example.net/channel-1.ts"
    }
  ],
  "output": {
    "root": "/var/lib/ottplay/multiview",
    "ffmpeg": "/usr/bin/ffmpeg",
    "ffprobe": "/usr/bin/ffprobe",
    "mkfifo": "/usr/bin/mkfifo"
  },
  "max_duration_seconds": 14400
}
```

This is the `media` object, not a complete operator document. Replace example
identities, addresses and paths. The favorite digest is the exact canonical
identity digest of the enrolled client favorite; the example digest is not a real
channel identity. A digest matches metadata and never grants network authority.
Every allowed subject/installation pair must match a top-level control credential.
The native caller cannot submit an upstream URL, credential, path or config index.

### Choosing the input format

The `media.adapter` envelope remains `http-mpegts-v1` for compatibility. Select
an individual source with `items[].input_adapter`: omitted or `http-mpegts-v1`
means a continuous MPEG-TS response; `http-hls-ts-v1` means an HLS playlist with
MPEG-TS segments. Do not change the envelope to `http-hls-ts-v1` or infer the
adapter from a URL suffix. For example, an HLS item sets:

```json
{
  "input_adapter": "http-hls-ts-v1",
  "url": "https://media.example.net/channel-2/master.m3u8"
}
```

Those two fields supplement the complete item identity shown above. They do not
replace its favorite digest or grant access to an unlisted origin.

The HLS adapter accepts a media playlist or one same-origin master level. It pins
one self-contained variant across probe and decode; there is no adaptive bitrate
switching or separate external audio group. Segments must be complete MPEG-TS,
with finite `EXTINF` durations and an ordered media sequence. The live cursor
reloads a sliding window, avoids replaying already delivered sequence numbers and
fails closed on changed entries, rollback or a lost window. `ENDLIST` ends the
input. Encryption, discontinuities, byte ranges, fMP4 initialization maps,
low-latency parts and unsupported playlist extensions are rejected.

The same address, credential, DNS-pinning and redirect rules apply to the root,
master child and every segment. FFmpeg receives an owned byte pipe, never an
upstream URL. One segment is bounded to 8 MiB, with one retained 64 KiB delivery
chunk per stream; these network buffers are separate from frame reservations and
FFmpeg's process memory. Actual network operations retain their eight-second
bound. Waiting for a declared 11–12-second segment is a separate cancellable
presentation wait, not permission for an unlimited network stall.

All source origins must be explicitly enrolled and actually used. Redirects are
not followed. Private upstream addresses require exact allowed IP entries. Each
request checks every resolved address, pins that approved set for the request
and checks the connected peer. This is not a lifetime peer-IP pin. On HTTPS
origins, adding allowed private addresses does not exclude otherwise approved
public answers; the HTTPS hostname and certificate remain required.

By default, plain HTTP requires a literal private IP explicitly listed in
`private_addresses`. To use a public HTTP provider, explicitly opt in on its
exact origin in the protected `media.network` configuration:

```json
{
  "origin": "http://media.example.net",
  "private_addresses": [],
  "allow_public_http": true
}
```

`allow_public_http` defaults to false. It is rejected for HTTPS or any rule with
private addresses, including an unused rule. Its empty private-address list means
**every** resolved address must pass the existing public-address policy. Private,
loopback, link-local, mapped or mixed public/private answers are denied before
sending a request. Public DNS/CDN addresses may change between requests, with
each new answer checked again.

A private HTTP hostname requires a separate opt-in on its exact enrolled origin:

```json
{
  "origin": "http://media.home.example",
  "private_addresses": ["192.168.1.20"],
  "allow_private_http": true
}
```

Replace these example values with the intended origin and approved addresses.
`allow_private_http` defaults to false, is mutually exclusive with
`allow_public_http`, and is rejected for HTTPS. It requires 1–64 unique, exact
RFC1918 IPv4 or IPv6 ULA addresses; wildcards and CIDR ranges are not accepted.
Every DNS answer must be one of those listed private addresses, at the enrolled
origin's port. A public, unlisted private, loopback, link-local or IPv4-mapped
answer rejects the request; the service does not filter out a denied answer and
continue with the others. A literal IP origin must also be in that exact list.

The approved address set is pinned separately for each request and the connected
peer is checked. DNS changes are allowed only within the explicit private list;
this is not a lifetime pin or permission to accept arbitrary private addresses.
The stored URL is not rewritten to an IP address. Neither HTTP flag activates the
service or provisions a native client.

Either HTTP opt-in permits the whole stored URL, including its path and query,
to travel without encryption. HTTP also lacks cryptographic server identity and protection
against an attacker on the network path. A bearer header additionally requires
that individual item's `allow_cleartext_credentials: true`; the origin opt-in
does not waive that consent. Use HTTPS when available. HLS children must still
stay on the same enrolled origin; no query copying, redirects or additional
origins are authorized. The private control API itself still requires HTTPS.

Provider credentials remain in this protected server document; they are not
returned in catalog, status, output or diagnostic responses.

Prepare the source registry before listener binding. The startup-only enrollment
principal has `SourcesManage`; paired player credentials have only
`MultiviewControl`. The normal controller bearer is not a compositor credential.

## Dedicated private service

Use `--multiview-only` with `--multiview-config` for a dedicated instance. It serves
only `/health` and the selected private namespace, without player assets, EPG
fetches, public CORS, normalization or companion services. Example invocation:

```sh
ottplay-server --multiview-only \
  --multiview-config /etc/ottplay/multiview.json \
  --host 192.168.1.10 --port 8088 --https-port 8448 \
  --cert /etc/ottplay/tls/fullchain.pem --key /etc/ottplay/tls/key.pem
```

The operator's `listener.bind_ip`, HTTPS mode and origin port must match exactly
one actual listener. Set `origin` to the exact HTTPS origin used by the player.
LAN operation requires HTTPS and explicit `lan_approved`. Other listeners return
404 for the entire private namespace, including preflight requests. The native
client requires a certificate trusted by its HTTPS transport; a random self-signed
certificate is not sufficient. DNS can point the certificate's hostname directly
to the private host without routing the video through a public tunnel.

A separate private instance can coexist with the ordinary asset/server instance
on different ports. Do not silently change the ordinary server's bind address or
route public client traffic through the private compositor.

## NUC and k3s operation

For the permanent home instance, run the private service on the NUC in k3s,
independently of here.now and the ordinary player-asset server. Deploy a qualified **player** image
pinned by digest, with a NUC node selector, explicit resource reservations and a
private listener. A DNS record can resolve the service hostname to its current
node address; the player's HTTPS origin, certificate hostname and exposed port
must agree. Do not route its video through a public tunnel or publish upstream
provider credentials.

A Kubernetes Secret projection contains symlinks and cannot be used directly as
the protected operator document. An initialization step must copy the document
and TLS material into an owner-only volume as regular, single-link files with
the final service UID and 0600 permissions. Mount only this materialized volume
in the non-root service container. Keep its 0700 output directory on a separate,
size-limited ephemeral volume. Configuration and secrets stay outside Git.

The service reads its configuration and certificates at startup. Renewing the
certificate Secret alone does **not** reload the running listener or a previous
initialization copy. Before expiry, stop new admissions, close active sessions,
wait for confirmed source/process/output cleanup, then recreate the pod so it
copies the renewed certificate. Verify its hostname, chain, validity interval,
health and authenticated capabilities after restart; provision a fresh server
epoch on the client. If cleanup fails, retain the existing owner and investigate
instead of treating a timeout as permission for a second encoder instance.

## Native provisioning and operation

The compatible native app's sensitive `nativeMultiviewService` setting is a JSON
**string** containing exactly `server_id`, HTTPS `origin`,
`audience: "ottplay-multiview-v1"`, the installation's persisted remote Device ID
as `installation_id`, and its dedicated `control_bearer`. Empty string disables
it. It is not a portable profile field. Use the settings API's fresh runtime,
context and revision CAS; do not write guessed filesystem paths or reuse the
ordinary remote-control bearer. Never print the sensitive value in receipts.

Capabilities come from the enrolled server, not a hardcoded positive count.
Native favorites must match server-enrolled identities; favorites alone cannot
authorize a source. Active kiosk policies continue to deny multiview. The existing
native protocol supports status, layout, sources, play, focus, audio, Stop and
Off. A consumer-facing setup/fill wizard is separate from those protocol controls.

Direct replacement of an occupied slot is refused. Stop that slot, wait for the
confirmed response, request a fresh source grant, then Play. Sibling input runtime
identities and decoders survive an ordinary output rotation. The shared HLS output
changes to a fresh encoder/codec epoch, so the player must adopt the new output;
this is not a promise of uninterrupted pixels during that transition.

## Lease, expiry and cleanup

Control renews the owner lease at intervals shorter than 90 seconds. Only a fresh,
authenticated Renew may advance it; status, HLS requests and cached replays do not.
The signed output path remains stable within one output epoch, while server-side
monotonic authority expires independently. A watchdog cancels and reaps abandoned
work even when no client sends another request.

The absolute session ceiling is configured separately, at most 24 hours. Renew
cannot extend it or the credential's original expiry. Start a fresh session after
the hard end; there is no expired-session resurrection. Stop/source changes revoke
the old output before a fresh H.264/AAC encoder epoch is admitted. Bytes already
delivered to a client cannot be retracted; new reads/chunks must fail closed.

SIGINT, SIGTERM, listener failure and companion exit all drain the retained
backend owner. Cancellation is not a cleanup receipt. Failed or uncertain process
cleanup retains ownership and capacity; it must not be converted into success to
admit another encoder. Closed sessions are retired only after their cleanup proof.
A terminal quarantine is an unavailable service requiring operator recovery, not
permission to reuse its PID or allocate replacement resources indefinitely.

## Qualification boundaries

Pure source review, a passing parser test and a synthetic core composition are
not interchangeable with normal-binary or television playback. Release evidence
must identify the exact source/binary, adapter/profile and exercised lifecycle.
The normal-binary fixture uses owned synthetic media and a private-address
upstream; it does not enroll household sources. Native HTTPS/rendering and physical
picture/audio checks are tracked separately. Do not infer support for another
provider, platform, codec or raster from this first profile.
