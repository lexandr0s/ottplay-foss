import { createDroppedVideos } from "./dropped-video";

var droppedPlaylistSerial = 0;
var MAX_PLAYLIST_BYTES = 2 * 1024 * 1024;
var MAX_PLAYLIST_TOTAL_BYTES = 8 * 1024 * 1024;
var MAX_INPUT_FILES = 128;
var MAX_RECORDS = 4096;

export type DroppedPlaylistRecord = {
    reference: string;
    name: string;
    kind: "local" | "remote";
    mime: string;
    duration?: number;
};

export type ParsedDroppedPlaylist = {
    records: DroppedPlaylistRecord[];
    isHls: boolean;
};

export type DroppedPlaylistLease = {
    catalog: any;
    release: () => void;
    sourceId: string;
    itemIds: string[];
};

function failure(code: string, message: string): Error {
    var error: any = new Error(message);
    error.droppedPlaylistCode = code;
    return error;
}

function byteLength(text: string): number {
    var length = 0;
    for (var i = 0; i < text.length; i++) {
        var code = text.charCodeAt(i);
        if (code <= 0x7f) length++;
        else if (code <= 0x7ff) length += 2;
        else if (code >= 0xd800 && code <= 0xdbff) {
            if (i + 1 >= text.length)
                throw failure("playlist_invalid", "Playlist text is invalid");
            var next = text.charCodeAt(i + 1);
            if (next < 0xdc00 || next > 0xdfff)
                throw failure("playlist_invalid", "Playlist text is invalid");
            length += 4;
            i++;
        } else if (code >= 0xdc00 && code <= 0xdfff) {
            throw failure("playlist_invalid", "Playlist text is invalid");
        } else length += 3;
        if (length > MAX_PLAYLIST_BYTES)
            throw failure("playlist_limit", "Playlist is too large");
    }
    return length;
}

function remoteMime(url: string): string {
    var path = "";
    try {
        path = new URL(url).pathname.toLowerCase();
    } catch (_) {
        return "";
    }
    if (/\.m3u8?$/.test(path)) return "application/vnd.apple.mpegurl";
    if (/\.mp4$/.test(path)) return "video/mp4";
    if (/\.m4v$/.test(path)) return "video/mp4";
    if (/\.webm$/.test(path)) return "video/webm";
    if (/\.mov$/.test(path)) return "video/quicktime";
    if (/\.mkv$/.test(path)) return "video/x-matroska";
    if (/\.mp3$/.test(path)) return "audio/mpeg";
    if (/\.aac$/.test(path)) return "audio/aac";
    return "";
}

function safeRemote(value: string): boolean {
    if (!/^https?:\/\//i.test(value) || /[\u0000-\u0020\u007f]/.test(value))
        return false;
    try {
        var parsed = new URL(value);
        return (
            (parsed.protocol === "http:" || parsed.protocol === "https:") &&
            !!parsed.hostname
        );
    } catch (_) {
        return false;
    }
}

/** Parse an M3U document without reading files, resolving paths, or fetching URLs. */
export function parseDroppedPlaylist(text: string): ParsedDroppedPlaylist {
    if (typeof text !== "string")
        throw failure("playlist_invalid", "Playlist text is invalid");
    byteLength(text);
    if (text.charCodeAt(0) === 0xfeff) text = text.substring(1);
    if (/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/.test(text))
        throw failure("playlist_invalid", "Playlist text is invalid");

    var lines = text.replace(/\r\n?/g, "\n").split("\n");
    var first = 0;
    while (first < lines.length && !lines[first].trim()) first++;
    if (first >= lines.length)
        throw failure("playlist_invalid", "Playlist has no entries");
    var hasHeader = /^#EXTM3U(?:\s|$)/i.test(lines[first].trim());

    var records: DroppedPlaylistRecord[] = [];
    var pendingName = "";
    var pendingDuration: number | undefined;
    var isHls = false;
    for (var i = first + (hasHeader ? 1 : 0); i < lines.length; i++) {
        var line = lines[i].trim();
        if (!line) continue;
        if (line.charAt(0) === "#") {
            if (/^#EXT-X-/i.test(line)) isHls = true;
            if (/^#EXTINF:/i.test(line)) {
                var comma = -1;
                var quoted = false;
                for (var ci = 8; ci < line.length; ci++) {
                    if (line.charAt(ci) === '"') quoted = !quoted;
                    else if (line.charAt(ci) === "," && !quoted) {
                        comma = ci;
                        break;
                    }
                }
                var metadata = line.substring(
                    8,
                    comma < 0 ? line.length : comma
                );
                var durationToken = metadata.trim().split(/\s+/, 1)[0];
                var duration = Number(durationToken);
                pendingDuration =
                    durationToken && isFinite(duration) && duration >= -1
                        ? duration
                        : undefined;
                pendingName =
                    comma >= 0 ? line.substring(comma + 1).trim() : "";
                if (pendingName.length > 512)
                    throw failure(
                        "playlist_invalid",
                        "Playlist entry is invalid"
                    );
            }
            continue;
        }
        if (records.length >= MAX_RECORDS)
            throw failure("playlist_limit", "Playlist has too many entries");
        if (/^(?:file:|[a-zA-Z]:[\\/]|[\\/]{1,2})/.test(line))
            throw failure(
                "playlist_local_unavailable",
                "Local playlist entry is unavailable"
            );

        var kind: "local" | "remote";
        var mime: string;
        if (safeRemote(line)) {
            kind = "remote";
            mime = remoteMime(line);
        } else {
            if (/^[a-z][a-z0-9+.-]*:/i.test(line) || /^<|[<>\r\n]/.test(line))
                throw failure("playlist_invalid", "Playlist entry is invalid");
            kind = "local";
            mime = "";
        }
        records.push({
            duration: pendingDuration,
            kind: kind,
            mime: mime,
            name:
                pendingName ||
                (kind === "remote"
                    ? remoteName(line)
                    : line.split(/[\\/]/).pop() || line),
            reference: line,
        });
        pendingName = "";
        pendingDuration = undefined;
    }
    if (!records.length)
        throw failure("playlist_invalid", "Playlist has no entries");
    return { isHls: isHls, records: records };
}

function remoteName(value: string): string {
    try {
        var parsed = new URL(value);
        var leaf = parsed.pathname.split("/").pop() || parsed.hostname;
        return leaf.substring(0, 512) || "Remote stream";
    } catch (_) {
        return "Remote stream";
    }
}

function isPlaylist(file: any): boolean {
    return !!(
        file &&
        typeof file.name === "string" &&
        /\.(?:m3u|m3u8)$/i.test(file.name)
    );
}

/** True when a dropped File batch contains at least one M3U/M3U8 document. */
export function hasDroppedPlaylist(files: any): boolean {
    var count = files && files.length;
    if (typeof count !== "number" || count < 1 || count > MAX_INPUT_FILES)
        return false;
    for (var i = 0; i < count; i++) if (isPlaylist(files[i])) return true;
    return false;
}

function decodeUtf8(bytes: any, w: any): string {
    var Decoder = w && w.TextDecoder;
    if (typeof Decoder !== "function" && typeof TextDecoder !== "undefined")
        Decoder = TextDecoder;
    if (typeof Decoder === "function") {
        try {
            return new Decoder("utf-8", { fatal: true }).decode(bytes);
        } catch (_) {
            throw failure(
                "playlist_invalid",
                "Playlist text encoding is invalid"
            );
        }
    }
    var output = "";
    var i = 0;
    while (i < bytes.length) {
        var first = bytes[i++];
        var code: number;
        var needed: number;
        if (first <= 0x7f) {
            code = first;
            needed = 0;
        } else if (first >= 0xc2 && first <= 0xdf) {
            code = first & 0x1f;
            needed = 1;
        } else if (first >= 0xe0 && first <= 0xef) {
            code = first & 0x0f;
            needed = 2;
        } else if (first >= 0xf0 && first <= 0xf4) {
            code = first & 0x07;
            needed = 3;
        } else
            throw failure(
                "playlist_invalid",
                "Playlist text encoding is invalid"
            );
        if (i + needed > bytes.length)
            throw failure(
                "playlist_invalid",
                "Playlist text encoding is invalid"
            );
        for (var n = 0; n < needed; n++) {
            var next = bytes[i++];
            if ((next & 0xc0) !== 0x80)
                throw failure(
                    "playlist_invalid",
                    "Playlist text encoding is invalid"
                );
            if (
                n === 0 &&
                ((first === 0xe0 && next < 0xa0) ||
                    (first === 0xed && next >= 0xa0) ||
                    (first === 0xf0 && next < 0x90) ||
                    (first === 0xf4 && next >= 0x90))
            )
                throw failure(
                    "playlist_invalid",
                    "Playlist text encoding is invalid"
                );
            code = (code << 6) | (next & 0x3f);
        }
        if (code <= 0xffff) output += String.fromCharCode(code);
        else {
            code -= 0x10000;
            output += String.fromCharCode(
                0xd800 + (code >> 10),
                0xdc00 + (code & 0x3ff)
            );
        }
    }
    return output;
}

function readPlaylist(file: any, w: any): Promise<string> {
    if (file.size > MAX_PLAYLIST_BYTES)
        return Promise.reject(
            failure("playlist_limit", "Playlist is too large")
        );
    var read: Promise<string>;
    try {
        if (typeof file.arrayBuffer === "function")
            read = Promise.resolve(file.arrayBuffer()).then(function (buffer) {
                return decodeUtf8(new Uint8Array(buffer), w);
            });
        else if (typeof file.text === "function")
            read = Promise.resolve(file.text()).then(function (text) {
                if (typeof text !== "string" || text.indexOf("\ufffd") >= 0)
                    throw failure(
                        "playlist_invalid",
                        "Playlist text encoding is invalid"
                    );
                return text;
            });
        else {
            var Reader = w && w.FileReader;
            if (
                typeof Reader !== "function" &&
                typeof FileReader !== "undefined"
            )
                Reader = FileReader;
            if (typeof Reader === "function") {
                read = new Promise(function (resolve, reject) {
                    var reader = new Reader();
                    reader.onerror = function () {
                        reject(
                            failure(
                                "playlist_invalid",
                                "Playlist could not be read"
                            )
                        );
                    };
                    reader.onabort = function () {
                        reject(
                            failure(
                                "playlist_invalid",
                                "Playlist could not be read"
                            )
                        );
                    };
                    reader.onload = function () {
                        try {
                            if (
                                !reader.result ||
                                typeof reader.result.byteLength !== "number"
                            )
                                throw new Error();
                            resolve(
                                decodeUtf8(new Uint8Array(reader.result), w)
                            );
                        } catch (_) {
                            reject(
                                failure(
                                    "playlist_invalid",
                                    "Playlist text encoding is invalid"
                                )
                            );
                        }
                    };
                    reader.readAsArrayBuffer(file);
                });
            } else
                return Promise.reject(
                    failure("playlist_invalid", "Playlist could not be read")
                );
        }
    } catch (_) {
        return Promise.reject(
            failure("playlist_invalid", "Playlist could not be read")
        );
    }
    return read.then(
        function (text) {
            if (typeof text !== "string")
                throw failure(
                    "playlist_invalid",
                    "Playlist text encoding is invalid"
                );
            byteLength(text);
            return text;
        },
        function () {
            throw failure("playlist_invalid", "Playlist could not be read");
        }
    );
}

function normalizeRelative(value: string): string | null {
    if (!value || value.indexOf("\\") >= 0 || value.charAt(0) === "/")
        return null;
    var parts = value.split("/");
    var output: string[] = [];
    for (var i = 0; i < parts.length; i++) {
        var part = parts[i];
        if (!part || part === ".") continue;
        if (part === "..") {
            if (!output.length) return null;
            output.pop();
            continue;
        }
        if (/[\u0000-\u001f\u007f]/.test(part)) return null;
        output.push(part);
    }
    return output.length ? output.join("/") : null;
}

function pathKey(file: any): string | null {
    var relative =
        typeof file.webkitRelativePath === "string"
            ? file.webkitRelativePath
            : "";
    return normalizeRelative(relative || file.name || "");
}

function resolveLocal(reference: string, playlist: any, files: any[]): any {
    if (/^(?:file:|[a-zA-Z]:[\\/]|[\\/]{1,2})/.test(reference)) return null;
    var base = pathKey(playlist);
    if (!base) return null;
    var slash = base.lastIndexOf("/");
    var directory = slash >= 0 ? base.substring(0, slash + 1) : "";
    var wanted = normalizeRelative(directory + reference);
    if (!wanted) return null;
    var matches: any[] = [];
    for (var i = 0; i < files.length; i++)
        if (pathKey(files[i]) === wanted) matches.push(files[i]);
    return matches.length === 1 ? matches[0] : null;
}

/** Build an ephemeral catalog from a synchronous browser drop gesture. */
export function createDroppedPlaylists(
    w: any,
    files: any
): Promise<DroppedPlaylistLease> {
    var count = files && files.length;
    if (
        typeof count !== "number" ||
        count < 1 ||
        count > MAX_INPUT_FILES ||
        Math.floor(count) !== count
    )
        return Promise.reject(
            failure("playlist_limit", "Choose up to 128 dropped files")
        );
    var captured: any[] = [];
    var playlistFiles: any[] = [];
    var playlistBytes = 0;
    for (var i = 0; i < count; i++) {
        var file = files[i];
        if (
            !file ||
            !w ||
            typeof w.File !== "function" ||
            !(file instanceof w.File) ||
            typeof file.name !== "string" ||
            typeof file.size !== "number" ||
            !isFinite(file.size) ||
            file.size < 0
        )
            return Promise.reject(
                failure("playlist_invalid", "Dropped file is invalid")
            );
        if (!isPlaylist(file) && !/\.(mp4|m4v|webm|mov|mkv)$/i.test(file.name))
            return Promise.reject(
                failure("playlist_unsupported", "Unsupported dropped file")
            );
        captured.push(file);
        if (isPlaylist(file)) {
            if (
                file.size > MAX_PLAYLIST_BYTES ||
                playlistBytes + file.size > MAX_PLAYLIST_TOTAL_BYTES
            )
                return Promise.reject(
                    failure("playlist_limit", "Playlist is too large")
                );
            playlistBytes += file.size;
            playlistFiles.push(file);
        }
    }
    if (!playlistFiles.length)
        return Promise.reject(
            failure("playlist_invalid", "No playlist was dropped")
        );

    var parsedPlaylists: any[] = [];
    var playlistRecordCount = 0;
    var readChain: Promise<any> = Promise.resolve();
    for (var p = 0; p < playlistFiles.length; p++)
        (function (playlist: any) {
            var read = function (): Promise<any> {
                return readPlaylist(playlist, w).then(function (text) {
                    var parsed = parseDroppedPlaylist(text);
                    playlistRecordCount += parsed.records.length;
                    if (playlistRecordCount > MAX_RECORDS)
                        throw failure(
                            "playlist_limit",
                            "Playlist has too many entries"
                        );
                    parsedPlaylists.push({ file: playlist, parsed: parsed });
                });
            };
            // Start the first read inside the file gesture; subsequent reads
            // remain sequential so an over-limit batch stops work immediately.
            readChain = p === 0 ? read() : readChain.then(read);
        })(playlistFiles[p]);

    return readChain.then(function () {
        var expanded: any[] = [];
        var localFiles: any[] = [];
        var localSeen: any[] = [];
        var resolvedDocuments: any[] = [];
        var referencedFiles: any[] = [];
        for (var p = 0; p < parsedPlaylists.length; p++) {
            var parsed = parsedPlaylists[p];
            if (parsed.parsed.isHls)
                throw failure(
                    "playlist_hls_local_unsupported",
                    "Dropped HLS manifests with local segments are unsupported"
                );
            var resolvedRecords: any[] = [];
            for (var r = 0; r < parsed.parsed.records.length; r++) {
                var record = parsed.parsed.records[r];
                var resolved =
                    record.kind === "local"
                        ? resolveLocal(record.reference, parsed.file, captured)
                        : null;
                if (record.kind === "local") {
                    if (!resolved)
                        throw failure(
                            "playlist_local_unavailable",
                            "A playlist entry was not included in the drop"
                        );
                    if (!localSeen.includes(resolved)) {
                        localSeen.push(resolved);
                        localFiles.push(resolved);
                    }
                    if (!referencedFiles.includes(resolved))
                        referencedFiles.push(resolved);
                }
                resolvedRecords.push({ file: resolved, record: record });
            }
            resolvedDocuments.push({
                file: parsed.file,
                records: resolvedRecords,
            });
        }
        for (var i = 0; i < captured.length; i++) {
            var file = captured[i];
            if (isPlaylist(file)) {
                var documentIndex = playlistFiles.indexOf(file);
                var document = resolvedDocuments[documentIndex];
                for (var r = 0; r < document.records.length; r++) {
                    expanded.push(document.records[r]);
                    if (expanded.length > MAX_RECORDS)
                        throw failure(
                            "playlist_limit",
                            "Playlist has too many entries"
                        );
                }
            } else {
                // A co-dropped supported video is included once in original batch order.
                if (
                    /\.(mp4|m4v|webm|mov|mkv)$/i.test(file.name) &&
                    !referencedFiles.includes(file)
                ) {
                    if (!localSeen.includes(file)) {
                        localSeen.push(file);
                        localFiles.push(file);
                    }
                    expanded.push({
                        file: file,
                        record: {
                            kind: "local",
                            mime: "",
                            name: file.name,
                            reference: file.name,
                        },
                    });
                    if (expanded.length > MAX_RECORDS)
                        throw failure(
                            "playlist_limit",
                            "Playlist has too many entries"
                        );
                }
            }
        }

        var localLease: any = null;
        try {
            if (localFiles.length)
                localLease = createDroppedVideos(w, localFiles);
            if (droppedPlaylistSerial >= 9007199254740991)
                throw failure(
                    "playlist_limit",
                    "Dropped playlist is unavailable"
                );
            var leaseId =
                "dropped-playlist-" + (++droppedPlaylistSerial).toString(36);
            var sourceId = leaseId + "-source";
            var itemIds: string[] = [];
            var items: any[] = [];
            // The local playback entry point deliberately accepts one source
            // owner for a dropped batch, including mixed remote/local rows.
            var localSourceId = sourceId;
            var localIndex: any[] = [];
            if (localLease)
                for (var li = 0; li < localFiles.length; li++)
                    localIndex.push({
                        file: localFiles[li],
                        item: localLease.catalog.items[li],
                    });
            for (var e = 0; e < expanded.length; e++) {
                var entry = expanded[e];
                var id = leaseId + "-item-" + (e + 1).toString(36);
                var item: any;
                if (entry.record.kind === "remote") {
                    item = {
                        access: "granted",
                        available: true,
                        id: id,
                        mime: entry.record.mime,
                        name: entry.record.name,
                        relativePath: null,
                        sourceId: sourceId,
                        type: "stream",
                        url: entry.record.reference,
                    };
                } else {
                    var localItem: any = null;
                    for (var q = 0; q < localIndex.length; q++)
                        if (localIndex[q].file === entry.file)
                            localItem = localIndex[q].item;
                    if (!localItem)
                        throw failure(
                            "playlist_local_unavailable",
                            "A playlist entry was not included in the drop"
                        );
                    item = {
                        access: "granted",
                        available: true,
                        id: id,
                        mime: localItem.mime || entry.record.mime,
                        name: entry.record.name || localItem.name,
                        relativePath: null,
                        size: localItem.size,
                        sourceId: localSourceId,
                        type: "video",
                        url: localItem.url,
                    };
                }
                items.push(item);
                itemIds.push(id);
            }
            var hasRemote = expanded.some(function (entry) {
                return entry.record.kind === "remote";
            });
            var sources: any[] = [
                {
                    access: "granted",
                    available: true,
                    id: sourceId,
                    kind: localLease && !hasRemote ? "file" : "playlist",
                    name:
                        localLease && !hasRemote
                            ? "Dropped files"
                            : "Dropped playlist",
                },
            ];
            var released = false;
            return {
                catalog: {
                    available: items.length > 0,
                    discovered: items.length,
                    items: items,
                    persistent: false,
                    phase: "complete",
                    scanned: items.length,
                    scanning: false,
                    sources: sources,
                    storage: "browser",
                },
                itemIds: itemIds,
                release: function () {
                    if (released) return;
                    released = true;
                    if (localLease) localLease.release();
                },
                sourceId: sourceId,
            };
        } catch (error) {
            if (localLease) localLease.release();
            throw error;
        }
    });
}
