import {
    createDroppedPlaylists,
    hasDroppedPlaylist,
} from "../offline/dropped-playlist";
import { createDroppedVideos } from "../offline/dropped-video";

/** Desktop file gestures use the same temporary Local playback as remote commands. */
export function installLocalDrop(w: any): any {
    if (w.__ottLocalDrop) return w.__ottLocalDrop;
    var disposed = false;
    var sequence = 0;
    var pending: any = null;
    var active: any = null;
    var unlisten: (() => void) | null = null;
    var subscribed = false;
    var readySent = false;
    var native = !!(w.__TAURI__ || w.__TAURI_INTERNALS__);

    function ready(): void {
        if (
            disposed ||
            !native ||
            !subscribed ||
            readySent ||
            !w.__ottDesktopOpenReady
        )
            return;
        readySent = true;
        invoke("local_media_drop_ready", {}).catch(function () {
            readySent = false;
        });
    }

    function allowed(): boolean {
        try {
            return (
                !disposed &&
                !!w.__ottLocalMedia?.playDroppedVideo &&
                !w.__ottStandby?.blocked?.() &&
                !w.__ottKiosk?.enabled?.() &&
                !w.__ottKiosk?.locked?.() &&
                !w.__ottParental?.needs?.("providers") &&
                !w.__ottParental?.needs?.("settings")
            );
        } catch (_) {
            return false;
        }
    }
    function message(text: string): void {
        if (!disposed && typeof w.showShift === "function") w.showShift(text);
    }
    function failure(error: any): void {
        // Never surface native paths, signed URLs, or arbitrary IPC errors.
        var text = typeof error === "string" ? error : error?.message;
        var codes: any = {
            playlist_hls_local_unsupported:
                "Local HLS manifests are not supported; use an HTTP(S) manifest URL",
            playlist_invalid: "Playlist is empty or invalid",
            playlist_limit: "Playlist is too large",
            playlist_local_unavailable:
                "Playlist local files must be dropped together with the playlist",
            playlist_unsupported: "Playlist contains unsupported sources",
        };
        if (
            error &&
            Object.prototype.hasOwnProperty.call(
                codes,
                error.droppedPlaylistCode
            )
        )
            text = codes[error.droppedPlaylistCode];
        if (
            text ===
            "Local HLS manifests are not supported; add the HTTP(S) manifest URL as a playlist entry"
        )
            text = codes.playlist_hls_local_unsupported;
        if (text === "Dropped playlist must be UTF-8")
            text = "Playlist must use UTF-8";
        if (
            text ===
                "Dropped playlist exceeds the 2 MiB file or 8 MiB batch limit" ||
            text === "Dropped playlists exceed 4096 entries or 128 local files"
        )
            text = codes.playlist_limit;
        if (
            text === "Dropped playlist contains no playable entries" ||
            text === "Dropped playlist contains an invalid entry"
        )
            text = codes.playlist_invalid;
        var known = [
            "Local HLS manifests are not supported; use an HTTP(S) manifest URL",
            "Playlist local files must be dropped together with the playlist",
            "Playlist contains unsupported sources",
            "Playlist is empty or invalid",
            "Playlist is too large",
            "Playlist must use UTF-8",
        ];
        if (known.indexOf(text) < 0) text = "Unsupported source";
        message(w._ ? w._(text) : text);
    }
    function invoke(name: string, args: any): Promise<any> {
        var bridge = w.__TAURI__?.core || w.__TAURI__;
        try {
            if (!bridge?.invoke) throw new Error("Native playback unavailable");
            return Promise.resolve(bridge.invoke(name, args));
        } catch (error) {
            return Promise.reject(error);
        }
    }
    function release(lease: any): void {
        if (!lease || lease.released) return;
        lease.released = true;
        if (active === lease) active = null;
        try {
            lease.release();
        } catch (_) {}
    }
    function cancelPending(): void {
        sequence++;
        if (!pending) return;
        var old = pending;
        pending = null;
        w.clearTimeout(old.timer);
        release(old);
    }
    function play(lease: any): void {
        if (!allowed()) {
            release(lease);
            return;
        }
        var previous = active;
        active = lease;
        try {
            w.__ottLocalMedia.playDroppedVideo(
                lease.catalog,
                function () {
                    return active === lease && !lease.released && allowed();
                },
                function () {
                    release(lease);
                }
            );
        } catch (_) {
            release(lease);
            if (previous && !previous.released) active = previous;
            message(
                w._
                    ? w._("Local source unavailable")
                    : "Local source unavailable"
            );
        }
    }
    function hasFiles(event: any): boolean {
        var transfer = event.dataTransfer;
        if (!transfer) return false;
        if (transfer.files?.length) return true;
        var types = transfer.types || [];
        for (var i = 0; i < types.length; i++)
            if (types[i] === "Files") return true;
        return false;
    }
    function dragOver(event: any): void {
        if (!hasFiles(event)) return;
        event.preventDefault();
        event.dataTransfer.dropEffect = allowed() ? "copy" : "none";
    }
    function drop(event: any): void {
        if (!hasFiles(event)) return;
        // Never let the browser navigate away to the dropped file.
        event.preventDefault();
        event.stopPropagation();
        if (native || !allowed()) return;
        var files = event.dataTransfer.files;
        if (!files || files.length < 1 || files.length > 128) {
            failure(null);
            return;
        }
        if (hasDroppedPlaylist(files)) {
            // Capture the gesture's File references before the first await.
            var captured: any[] = [];
            for (var i = 0; i < files.length; i++) captured.push(files[i]);
            cancelPending();
            var request = sequence;
            var load = w.__ottCommandChannelLoad;
            var waiting: any = { release: function () {} };
            pending = waiting;
            waiting.timer = w.setTimeout(function () {
                if (pending === waiting) {
                    cancelPending();
                    failure(null);
                }
            }, 20000);
            createDroppedPlaylists(w, captured).then(
                function (lease) {
                    if (
                        pending !== waiting ||
                        request !== sequence ||
                        !allowed() ||
                        load !== w.__ottCommandChannelLoad
                    ) {
                        if (pending === waiting) {
                            pending = null;
                            w.clearTimeout(waiting.timer);
                        }
                        release(lease);
                        return;
                    }
                    pending = null;
                    w.clearTimeout(waiting.timer);
                    play(lease);
                },
                function (error) {
                    if (pending !== waiting) return;
                    pending = null;
                    w.clearTimeout(waiting.timer);
                    failure(error);
                }
            );
            return;
        }
        var lease: any;
        try {
            // Capture every File synchronously while the gesture owns DataTransfer.
            lease = createDroppedVideos(w, files);
        } catch (_) {
            message(w._ ? w._("Unsupported source") : "Unsupported source");
            return;
        }
        cancelPending();
        play(lease);
    }
    function nativeDrop(event: any): void {
        var grant = event?.payload?.grant;
        if (typeof grant !== "string" || !/^[a-zA-Z0-9_-]{16,128}$/.test(grant))
            return;
        var lease: any = {
            release: function () {
                invoke("local_media_release_drop", { grant: grant }).catch(
                    function () {}
                );
            },
        };
        if (!allowed()) {
            release(lease);
            return;
        }
        cancelPending();
        var request = sequence;
        // Startup may mount its optional provider after ready() has delivered
        // the OS file. The command generation owns the intent, not that driver
        // instance; a real subsequent source selection replaces this token.
        var load = w.__ottCommandChannelLoad;
        pending = lease;
        lease.timer = w.setTimeout(function () {
            if (pending !== lease) return;
            cancelPending();
            message(
                w._
                    ? w._("Local source unavailable")
                    : "Local source unavailable"
            );
        }, 20000);
        invoke("local_media_take_drop", { grant: grant }).then(
            function (catalog) {
                if (
                    pending !== lease ||
                    request !== sequence ||
                    !allowed() ||
                    load !== w.__ottCommandChannelLoad
                ) {
                    if (pending === lease) {
                        pending = null;
                        w.clearTimeout(lease.timer);
                    }
                    release(lease);
                    return;
                }
                pending = null;
                w.clearTimeout(lease.timer);
                lease.catalog = catalog;
                play(lease);
            },
            function (error) {
                if (pending !== lease) return;
                pending = null;
                w.clearTimeout(lease.timer);
                release(lease);
                failure(error);
            }
        );
    }
    function dispose(): void {
        if (disposed) return;
        disposed = true;
        cancelPending();
        release(active);
        try {
            if (unlisten) unlisten();
        } catch (_) {}
        w.removeEventListener("dragover", dragOver, true);
        w.removeEventListener("drop", drop, true);
        w.removeEventListener("pagehide", pageHide);
        if (w.__ottLocalDrop === api) delete w.__ottLocalDrop;
    }
    function pageHide(event: any): void {
        dispose();
        if (event?.persisted) {
            var restore = function (): void {
                w.removeEventListener("pageshow", restore);
                installLocalDrop(w);
            };
            w.addEventListener("pageshow", restore);
        }
    }
    var api = { dispose: dispose, ready: ready };
    w.__ottLocalDrop = api;
    w.addEventListener("dragover", dragOver, true);
    w.addEventListener("drop", drop, true);
    w.addEventListener("pagehide", pageHide);
    if (native && w.__TAURI__?.event?.listen) {
        try {
            Promise.resolve(
                w.__TAURI__.event.listen("local-media-drop", nativeDrop)
            )
                .then(function (remove: () => void) {
                    if (disposed) remove();
                    else {
                        unlisten = remove;
                        subscribed = true;
                        ready();
                    }
                })
                .catch(function () {});
        } catch (_) {}
    }
    return api;
}

if (typeof window !== "undefined") installLocalDrop(window);
