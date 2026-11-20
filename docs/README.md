# OTT-play FOSS documentation

The documentation for OTT-play FOSS, a self-contained IPTV/OTT player with a
local Rust HTTP server that runs on Smart TVs, set-top boxes and desktop
browsers. Pages are grouped below by the part of the project they describe; the
English feature guides carry translated editions, and the quick starts and wiki
home pages have their own indexes.

## Orientation

- [Player architecture](architecture.md) — developer entry point to the runtime layers, ownership and provider extension recipes.
- [Playback sessions and provider lifetimes](playback-session-architecture.md) — the core/host boundary that delegates playback history and seek decisions to the Kotlin host.
- [Guide ownership and retained device interfaces](guide-architecture.md) — request coalescing, the serial queue and the retained device interfaces.
- [From feature list to verified behavior](implementation-report.md) — the implementation and hardening process behind the delivered feature list.
- [Browser generations and operating systems](browser-compatibility.md) — configured targets, exact-package checks and the separate local results.
- [Reliability and media improvements](roadmap-delivery.md) — the implementation at the source revision of that page.
- [Measured JavaScript cost](roadmap-size.md) — JavaScript measurements from the recorded source snapshots.
- [Complete manual translations](manual-translations.md) — the eleven guides with complete editions in 24 languages and the review procedure behind them.
- [All translated manual editions](translations/README.md) — every `.<lang>` copy of the translated guides, grouped by guide.
- [Localized quick starts](readme/README.md) — 88 quick starts built from the shipped translated UI captions.
- [Documentation wiki home](wiki/Home.md) — the versioned wiki sources that are exported to GitHub.
- [Publishing the documentation wiki](wiki-publishing.md) — how the wiki sources are exported and which revision each page identifies.
- [Documentation images](images/README.md) — the hero render, its unchanged source and the regeneration script.

## Interface and presentation

- [Screen ownership and classic UI boundary](screen-ownership.md) — the screen controller, input router and classic UI boundary the bundle constructs.
- [Classic Core activation boundary](shared-core-activation.md) — the separate activation decisions for the shared Core package and the FOSS host.
- [Window globals inventory](window-globals.md) — historical inventory of the globals installed by `src/index.ts`.
- [Runtime asset names](asset-layout.md) — owned path names and the resource or adapter each one contains.
- [Device Profile Development](device-profiles.md) — adding a device profile with remote-control key mappings and a provider script.
- [Native shell contracts](native-shell-parity.md) — click targets, provider handling and contracts shared by Tauri, Capacitor and the web player.
- [Studio 2026 and interface settings](interface-settings.md) — choosing Classic or Studio 2026 as the interface mode.
- [Interface colors](interface-colors.md) — the Interface colors settings and the elements each colour controls.
- [Interface credits and design history](interface-history.md) — Studio 2026 design credits and the history of the interface.
- [Modern interface](modern-interface.md) — the optional Modern presentation of the same catalogue.
- [Subtitle appearance](subtitle-appearance.md) — font, size and styling options under Subtitle appearance.
- [System timezone](system-timezone.md) — the System timezone setting and how each date follows the OS timezone.
- [Interface languages and on-screen keyboard alphabets](localization.md) — the translation catalogue and the keyboard alphabets offered.
- [Browser window controls](browser-window-controls.md) — Window Controls Overlay support in installed desktop browser apps.
- [Desktop Favorites key](desktop-favorites-key.md) — the `F` shortcut that toggles Favorites on PC and PC2.

## Playback, media and providers

- [Playback startup, buffering and recovery](playback-performance.md) — how the interface starts before Hls.js and Shaka load, and how it recovers.
- [Quality limits and live stream recovery](playback-stream-policy.md) — the single playback lease and the recovery policy that retains it.
- [Live stall recovery](live-recovery-testing.md) — the optional watchdog for a previously playing main live stream.
- [Optional multiaudio normalization](audio-normalization.md) — handling several audio tracks inside the same MPEG-TS segments.
- [Native HLS audio renditions](native-hls-audio-renditions.md) — joining a video-only HLS VOD playlist with an external audio rendition.
- [Device and media backend ownership](media-backend.md) — one decoder lease per lane in `src/device/media-backend.ts`.
- [Device and decoder ownership](device-media-backend.md) — how browser, Tauri and Capacitor transport enter `__ottMediaBackend`.
- [Device resources and playback failures](player-resource-and-recovery.md) — the playback policy shared by browser, OTT server frontend, Tauri and Capacitor.
- [Media library and journal](media-library.md) — navigation frames, selection, request cancellation and the library journal.
- [Channel catalog and user state](channel-library.md) — the versioned, source-scoped document for groups and user state.
- [Media companion](media-companion.md) — the native service that adds BitTorrent file playback and packaged downloads.
- [Media companion size measurements](media-companion-size.md) — measurements of the companion client's torrent session handling and plugins.
- [Xtream movies and series](xtream-media.md) — configuring the provider and opening Media from the live catalogue.
- [Xtream media metadata](xtream-media-metadata.md) — descriptions and posters retained from the provider response.
- [Classic Stalker movies and nested series](stalker-media.md) — playable movies and episodes in the normal channel list.
- [Stalker relay for a hosted player](stalker-relay.md) — the `POST /stalker/api` endpoint exposed by the ordinary OTT server.
- [Checking a Stalker portal](stalker-portal-check.md) — the script that checks one account supplied in a local JSON file.
- [Plex on each player](plex-provider.md) — choosing Plex, signing in and linking a TV through `plex.tv/link`.
- [VPortal profiles](m3u-vportal.md) — the standalone VPortal provider.
- [PC2: Video.js and VHS](pc2-videojs.md) — the historical alternate streaming implementation behind `/f/pc2`.
- [Server EPG performance and compatibility](epg-performance.md) — how the standalone Rust server parses XMLTV records.
- [Hosted EPG service v1](hosted-epg-server.md) — running the Rust server as a restricted public guide service.
- [EPG diagnostics on the hosted player](hosted-epg-diagnostics.md) — the server-prepared public guide and its diagnostics.
- [Manual XMLTV binding and keyboard accessibility](guide-binding-accessibility.md) — fixing a channel's guide without editing the playlist.

## Local media

- [Local videos and offline playback](offline-media.md) — the canonical hosted player and offline playback entry point.
- [Local video profiles](offline-profiles.md) — selecting Local videos with `provider local` and its wire identifier.
- [Native local media sources](offline-media-native.md) — retaining references from native Add files and Add folder selections.
- [iOS: linked local media without an OTT copy](offline-media-ios-direct.md) — the document picker path that keeps files in place.
- [Android local media: linked sources](android-local-media-sources.md) — the `LocalMedia` plugin and the Storage Access Framework.
- [Tauri direct local sources](tauri-local-media-sources.md) — linking folders in the desktop shell.
- [Read-only media helper](local-media-helper.md) — the helper used when Chrome's folder access cannot start a video.
- [One-off Local playback commands](local-media-session.md) — remote Local `play` and `shuffle` commands that last one session.
- [Play desktop videos by dropping them into the player](desktop-video-drop.md) — starting a dropped video file on PC, PC2 and the Tauri shell.
- [AVI and MKV compatibility without importing originals](local-container-compatibility.md) — how Local videos keeps its channel list and controls while reading those containers.
- [Native folder scanning during playback](evidence/local-folder-range-performance-20261027.md) — the measured run over an unindexed copy of 13,657 metadata files.
- [Device detection and emulator testing](device-detection-testing.md) — where device selection happens before and after the bundle loads.
- [iOS: protected media sources](ios-source-access.md) — the source-specific Cloudflare Access sign-in.

## Platform delivery

- [Set up an Android projector with OTTPlay](android-projector-setup.md) — taking a new projector from the factory launcher to a working player.
- [Автозапуск OTTPlay как домашнего экрана Android](android-autostart-RU.md) — making OTTPlay the Android home screen (Russian).
- [Capacitor Android distribution and updates](android-distributions.md) — the Full/API 22 packages and their updates.
- [Capacitor mobile](capacitor-mobile.md) — packaging the shared player for iOS and Android.
- [Install OTT-play on an LG webOS TV](lg-webos-installation.md) — installing or maintaining the player on a webOS set.
- [LG Developer Mode: setup, installation, updates and troubleshooting](lg-developer-mode.md) — connecting a computer to a consumer LG TV in Developer Mode.
- [Install OTT-play on a Samsung Tizen TV](samsung-tizen-installation.md) — what the Samsung package contains and how it is installed.
- [webOS Simulator launcher and external CLI](webos-cli-security.md) — the Node-only launcher and the external CLI it selects.
- [Operate the LG Homebrew metadata endpoint](webos-feed-operations.md) — the catalog, manifest and description the Homebrew Channel loads.
- [Native webOS development moved to ottplay-lg](native-webos.md) — where the experimental SDL/PlxNative client is maintained.
- [Linux media packaging](linux-media-packaging.md) — installing WebKitGTK and GStreamer for the native Linux player.
- [Linux WebKit runtime qualification](linux-webkit-runtime.md) — the explicit `ubuntu-26.04` x64 runner used for qualification.
- [macOS local delivery and recovery](macos-local-delivery.md) — replacing a local web player, its macOS server and its recovery state.
- [macOS ARM64 server delivery](macos-server-release.md) — how the desktop release job builds the standalone `ottplay-server`.
- [Local build scripts (Mac)](local-build-scripts.md) — the scripts that build and install Tauri and server locally on macOS.
- [Tauri updater and macOS signing (Mode B)](tauri-updater-notarize.md) — desktop auto-update and signing configuration.
- [Native window aspect constraint](native-window-aspect-lock.md) — the 16:9 content constraint requested during interactive resizing.
- [Native remote text entry](native-swop.md) — the embedded Tauri and Capacitor editors built on the Swop URL setting.
- [Android Full and Play distributions (archived)](legacy-android/android-distributions.md) — the retired Capacitor wrapper's two Android products.
- [Google Play submission preparation (archived)](legacy-android/play-submission.md) — package, display name and listing draft for `play.ott.foss.play`.
- [Build a Google Play upload bundle (archived)](legacy-android/play-upload-signing.md) — the retired workflow that built and signed the AAB.
- [Play Store distribution](play-submission.md) — points AAB creation and publication at the separate ottplay-android project.
- [Play Store distribution (upload signing)](play-upload-signing.md) — notes that the Full signing certificate is separate from the Play upload key.

## Build, test and quality gates

- [Classic ES5 build pipeline](build-pipeline.md) — what `npm run build` runs across Vite, TypeScript and the classic linker.
- [Build warning review](build-warnings.md) — why compiler and tool diagnostics stay enabled in a successful build.
- [Legacy engine compatibility](es5-compatibility.md) — the ES5 target and constraints the classic script keeps.
- [External library upgrades with ES5 compatibility](external-library-upgrades.md) — what a new dependency must fit before it is accepted.
- [Native container builds and cache verification](container-builds.md) — the Linux AMD64 and ARM64 builds on matching native runners.
- [CI orchestration](CI-ORCHESTRATION.md) — how the quality gate owns pull-request and merge-queue validation.
- [Python suites and coverage](testing-python.md) — installing the test requirements and running the suites.
- [Base OTT bundles for device testing](mode-a-test-bundle.md) — the `ottplay-foss-modea-<commit>` artifact uploaded by CI.
- [Capacitor device smoke (Mode B)](mode-b-device-smoke.md) — repeatable simulator, emulator and real-device smoke runs.
- [Tauri desktop smoke (Mode B)](mode-b-tauri-smoke.md) — repeatable macOS, Windows and Linux desktop smoke runs.
- [Windows installer qualification](windows-installer-ci.md) — the installer lifecycle job in `native-parity.yml`.
- [Windows native video geometry acceptance](windows-video-geometry-ci.md) — purpose and scope of the geometry acceptance job.
- [Opt-in Firefox content-crash diagnostics](firefox-crash-diagnostics.md) — collecting a bounded crash signature in the acceptance runner.
- [Collection performance checks](collection-performance.md) — what the October 2026 collection changes preserved.
- [Demo recordings with video-use](demo-video.md) — the optional local editing integration and its pinned helpers.
- [Temporary performance capture](performance-capture.md) — the bounded, off-by-default session for investigating performance.

## Diagnostics and troubleshooting

- [Playback diagnostics](diagnostic-security.md) — the HUD and the in-memory event ring behind `?debug=1`.
- [Remote diagnostics](remote-diagnostics.md) — the scoped operator access an enabled controller connection authorizes.
- [Runtime diagnostics across platforms](runtime-debug.md) — using `ott PLAYER debug` to inspect an enabled remote-control connection.
- [Remote diagnostic workbench](remote-workbench.md) — the workbench that extends an existing remote-control connection.
- [Notification presentation](remote-notifications.md) — the card renderer shared by local feedback and remote notifications.

## Remote control and settings

- [Connect to a command server](remote-command-server.md) — the per-player command queues provided by the separate control server.
- [Deliver command-server discovery to each player](discovery-deployment.md) — how a website update reaches players that load it.
- [Remote settings documentation index](remote-settings/README.md) — routes to the four pages covering the typed `player_settings` API.
- [Typed remote player settings](remote-player-settings.md) — the API, authorization and basic read/write commands.
- [Remote language and connection settings](remote-settings-lifecycle.md) — preferences applied after acknowledgement, with rollback.
- [Remaining interface and diagnostic preferences](remote-settings-extras.md) — visibility, local media, debug and ordering settings.
- [Remote settings coverage and platform boundaries](development/remote-settings-coverage.md) — editable registry and provider fields, and the platform limits.
- [Individual provider settings and Plex folders](remote-provider-settings.md) — provider settings shown while that provider is active.
- [Legacy remote favorites across profiles](remote-favorites.md) — the legacy `favorites` protocol and its `fav-…` bookmarks.
- [Mixed Favorites](mixed-favorites.md) — the single ordered Global Favorites collection per installation.
- [Server EPG queries from the control CLI](remote-epg-control.md) — listing guide entries with `ott PLAYER p` and `p --list`.
- [Explicit playback handoff](remote-handoff.md) — running `ott handoff SOURCE TARGET` between players.
- [Recoverable multi-key settings](settings-recovery.md) — the `ottplaySettingsJournalV1` undo journal for localStorage batches.
- [Settings ownership and editor lifecycle](settings-store.md) — `src/settings/store.ts` and the preference schema.
- [Cloud settings transfers](cloud-settings.md) — how `cloudSendSettings` and `cloudLoadSettings` delegate.
- [Parental PIN storage and migration](parental-pin-storage.md) — the versioned record behind four-digit PINs.

## Design notes and favourites ledger

- [Favorites binding registry foundation](design/favorites-bindings.md) — the registry module, which does not by itself enable a sync capability.
- [Favorites ledger recovery](design/favorites-ledger-recovery.md) — the recovery protocol and the matching implementation it requires.
- [Private favorites receipt ledger v2](design/favorites-receipt-ledger-v2.md) — the frozen serialization used by `favorites-recovery-codec.ts`.

## Security, privacy and provenance

- [Security design and verification](security-design.md) — scope, trust boundaries and how they are verified.
- [Playlist proxy security](proxy-security.md) — the shared Rust `m3u::proxy_stream` used by Mode A and Tauri.
- [Provider artwork transport](artwork-transport.md) — when an HTTPS player document upgrades an HTTP poster URL.
- [Rust dependency security review](rust-dependency-security.md) — the review of the workspace `Cargo.lock` and the two crates.
- [Inherited component notices](legacy-provenance.md) — notices for components inherited with `stbPlayer.js`.
- [Privacy Policy — OTT-play FOSS](privacy-policy.md) — the published privacy policy, last updated 2026-10-27.
- [OpenSSF Passing evidence](openssf-evidence.md) — the source-backed work record for the Passing badge.

## Release and CI evidence

- [Release version inputs](release-versioning.md) — the `.release-policy.json` fields that identify the release version.
- [Compact stable distribution manifests](release-distribution.md) — the per-repository opt-in for compact stable metadata.
- [Qualified GitHub CLI for release operations](release-github-cli.md) — the pinned CLI build generated release jobs install.
- [Reviewed test-only automatic pushes](automatic-release-test-paths.md) — the exact allowlist that makes test-only pushes eligible.
- [Client interface color size evidence](evidence/client-interface-colors-size-20261106/README.md) — the original inputs and inventories from hosted run 42219826374.
- [Font preview CSP asset ceiling correction](evidence/interface-font-csp-budget-20261106/README.md) — the six size checks that failed and the correction.
- [Samsung TV delivery review: SamSUCKS](samsucks-delivery-review.md) — the delivery review recorded on 30 October 2026.
- [STB source review and player improvements](stb-player-improvements.md) — the review of the starred repository list and the resulting improvements.
- [Documentation wiki sidebar](wiki/_Sidebar.md) — the navigation links used by the exported wiki.
