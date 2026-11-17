// Serialized into the browser. Observe native buffers; never fabricate readiness.
function installFirefoxVideojsPhase({ mode }, scope = globalThis) {
    if (!["append-active", "append-completed"].includes(mode))
        throw new Error("Unknown Firefox Video.js phase");
    const proto = scope.MediaSource.prototype;
    const bufferProto = scope.SourceBuffer.prototype;
    const add = proto.addSourceBuffer;
    const append = bufferProto.appendBuffer;
    const create = scope.URL.createObjectURL;
    const revoke = scope.URL.revokeObjectURL;
    const urls = new Map();
    const buffers = new Map();
    let pending;
    let retired = false;
    let attempts = 0;
    const listeners = [];
    function restore() {
        if (retired) return;
        retired = true;
        proto.addSourceBuffer = add;
        bufferProto.appendBuffer = append;
        scope.URL.createObjectURL = create;
        scope.URL.revokeObjectURL = revoke;
        for (const [buffer, listener] of listeners)
            buffer.removeEventListener("updateend", listener);
        scope.removeEventListener("pagehide", restore);
        urls.clear();
        buffers.clear();
        if (pending) {
            scope.clearTimeout(pending.timer);
            pending.reject(new Error("Firefox Video.js phase owner retired"));
            pending = undefined;
        }
    }
    function owned(buffer) {
        try {
            const entry = buffers.get(buffer);
            const video = scope.video;
            const player = scope.videojs?.getPlayer("video");
            const source = urls.get(video?.currentSrc);
            return !!(
                pending &&
                scope.video === pending.video &&
                scope.__ottMedia?.current() === pending.handle &&
                entry?.kind === "video" &&
                video instanceof scope.HTMLVideoElement &&
                player?.tech({ IWillNotUseThisInPlugins: true }).el() ===
                    video &&
                source === entry.source &&
                source?.readyState === "open" &&
                Array.from(source.sourceBuffers).includes(buffer) &&
                [...buffers.entries()].some(
                    ([candidate, other]) =>
                        other.source === source &&
                        other.kind === "audio" &&
                        Array.from(source.sourceBuffers).includes(candidate)
                )
            );
        } catch (_) {
            return false;
        }
    }
    function ready(buffer, completed) {
        try {
            return completed
                ? !buffer.updating && buffer.buffered.length > 0
                : buffer.updating;
        } catch (_) {
            // Removed native buffers can throw InvalidStateError on buffered.
            return false;
        }
    }
    function observe(buffer, completed) {
        if (!pending || retired || !owned(buffer)) return;
        if (
            !ready(buffer, completed) ||
            completed !== (mode === "append-completed")
        )
            return;
        const request = pending;
        const video = scope.video;
        const source = video.currentSrc;
        scope.queueMicrotask(() => {
            if (retired || pending !== request) return;
            // Revalidate after the append/updateend JS stack has unwound.
            if (
                scope.video !== video ||
                video.currentSrc !== source ||
                !owned(buffer) ||
                !ready(buffer, completed)
            )
                return;
            pending = undefined;
            scope.clearTimeout(request.timer);
            attempts++;
            try {
                scope.console.log(
                    "Firefox Video.js phase: " +
                        JSON.stringify({ attempts, mode, phaseObserved: true })
                );
            } catch (_) {
                // Best-effort observation must not replace the transition.
            }
            try {
                request.transition();
                request.resolve();
            } catch (error) {
                request.reject(error);
            } finally {
                restore();
            }
        });
    }
    proto.addSourceBuffer = function (mime) {
        const buffer = add.call(this, mime);
        const kind = /^video\//i.test(mime)
            ? "video"
            : /^audio\//i.test(mime)
              ? "audio"
              : "unknown";
        buffers.set(buffer, { kind, source: this });
        const listener = () => observe(buffer, true);
        listeners.push([buffer, listener]);
        buffer.addEventListener("updateend", listener);
        return buffer;
    };
    bufferProto.appendBuffer = function (...args) {
        const result = append.apply(this, args);
        observe(this, false);
        return result;
    };
    scope.URL.createObjectURL = function (object) {
        const url = create.call(this, object);
        if (object instanceof scope.MediaSource) urls.set(url, object);
        return url;
    };
    scope.URL.revokeObjectURL = function (url) {
        urls.delete(url);
        return revoke.call(this, url);
    };
    scope.addEventListener("pagehide", restore);
    scope.__ottFirefoxVideojsPhase = {
        restore,
        run(transition) {
            if (retired || pending || attempts)
                return Promise.reject(
                    new Error("Firefox Video.js phase unavailable")
                );
            const video = scope.video;
            const handle = scope.__ottMedia?.current();
            if (!(video instanceof scope.HTMLVideoElement) || !handle)
                return Promise.reject(
                    new Error("Firefox Video.js media owner unavailable")
                );
            return new Promise((resolve, reject) => {
                pending = { handle, reject, resolve, transition, video };
                pending.timer = scope.setTimeout(() => {
                    const request = pending;
                    pending = undefined;
                    restore();
                    request.reject(
                        new Error(
                            "Firefox Video.js phase precondition not observed"
                        )
                    );
                }, 8000);
            });
        },
    };
}
module.exports = installFirefoxVideojsPhase;
