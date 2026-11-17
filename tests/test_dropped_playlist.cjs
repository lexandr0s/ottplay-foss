"use strict";
const assert = require("node:assert/strict");
const { File } = require("node:buffer");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");
const vm = require("node:vm");

function loadTs(file, exports, requireFn) {
    const source = fs.readFileSync(path.join(__dirname, "..", file), "utf8");
    vm.runInNewContext(
        ts.transpileModule(source, {
            compilerOptions: {
                module: ts.ModuleKind.CommonJS,
                target: ts.ScriptTarget.ES5,
            },
        }).outputText,
        { exports, require: requireFn, URL }
    );
}

function fixture(options = {}) {
    const videoExports = {};
    loadTs("src/offline/dropped-video.ts", videoExports);
    const exports = {};
    const created = [],
        revoked = [],
        forbidden = () => {
            throw new Error("Unexpected fetch, storage or external API");
        };
    let serial = 0;
    let createCount = 0;
    const host = {
        File,
        FileReader: options.FileReader,
        URL: {
            createObjectURL(file) {
                created.push(file);
                createCount++;
                if (
                    options.createFailure ||
                    options.createFailureAt === createCount
                )
                    throw new Error("private file name");
                return "blob:fixture/" + ++serial;
            },
            revokeObjectURL(url) {
                revoked.push(url);
            },
        },
        document: {
            createElement(tag) {
                assert.equal(tag, "video");
                return { canPlayType: () => "probably" };
            },
        },
        fetch: forbidden,
        indexedDB: undefined,
        localStorage: undefined,
        sessionStorage: undefined,
        stbSetItem: forbidden,
        stbGetItem: forbidden,
    };
    loadTs("src/offline/dropped-playlist.ts", exports, (name) => {
        assert.equal(name, "./dropped-video");
        return videoExports;
    });
    function file(name, type, contents, relativePath) {
        const value = new File([contents || "fixture"], name, {
            type: type || "",
        });
        if (relativePath !== undefined)
            Object.defineProperty(value, "webkitRelativePath", {
                value: relativePath,
            });
        return value;
    }
    return { exports, file, host, created, revoked };
}

test("pure parser classifies remote/local entries, EXTINF metadata and HLS", () => {
    const f = fixture();
    const parsed = f.exports.parseDroppedPlaylist(
        "\ufeff#EXTM3U\r\n#EXTINF:-1,News & Culture\r\nhttps://cdn.example/live.m3u8?token=x\r\nfolder/movie.mp4\r\n"
    );
    assert.equal(parsed.isHls, false);
    assert.deepEqual(
        Array.from(parsed.records, (record) => [
            record.kind,
            record.name,
            record.mime,
        ]),
        [
            ["remote", "News & Culture", "application/vnd.apple.mpegurl"],
            ["local", "movie.mp4", ""],
        ]
    );
    assert.equal(parsed.records[0].duration, -1);
    assert.equal(
        f.exports.parseDroppedPlaylist(
            '#EXTM3U url-tvg="https://epg.example/list.xml"\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,part\nsegment.ts\n'
        ).isHls,
        true
    );
    assert.equal(
        f.exports.parseDroppedPlaylist(
            '#EXTM3U\n#EXTINF:-1 tvg-name="News, East",Shown name\nhttps://media.example/live.m3u8\n'
        ).records[0].name,
        "Shown name"
    );
    assert.equal(
        f.exports.parseDroppedPlaylist(
            '#EXTM3U\n#EXTINF:-1 tvg-name="News, East",Shown name\nhttps://media.example/live.m3u8\n'
        ).records[0].duration,
        -1
    );
    assert.equal(
        f.exports.parseDroppedPlaylist("#EXTM3U\na#b?.mp4\n").records[0]
            .reference,
        "a#b?.mp4"
    );
    assert.equal(
        f.exports.parseDroppedPlaylist("https://stream.example/live.m3u8\n")
            .records[0].kind,
        "remote"
    );
});

test("parser rejects malformed header, unsafe schemes, absolute paths and oversized text", () => {
    const f = fixture();
    for (const text of [
        "#EXTM3U\nfile:///Users/person/video.mp4",
        "#EXTM3U\njavascript:alert(1)",
        "#EXTM3U\n/Users/person/movie.mp4",
        "#EXTM3U\nhttps://example.test/\u0001bad.mp4",
        "#EXTM3U\n#EXTINF:-1,\ud800\nhttps://example.test/a.mp4",
    ]) {
        assert.throws(() => f.exports.parseDroppedPlaylist(text));
    }
    assert.throws(() =>
        f.exports.parseDroppedPlaylist(
            "#EXTM3U\n" + "x".repeat(2 * 1024 * 1024)
        )
    );
});

test("mixed drop preserves playlist order and creates URLs only for exact co-dropped files", async () => {
    const f = fixture();
    const playlist = f.file(
        "channels.m3u",
        "audio/x-mpegurl",
        "#EXTM3U\n#EXTINF:-1,Remote\nhttps://media.example/live.m3u8\n#EXTINF:-1,Local clip\nmovie.mp4\n",
        "folder/channels.m3u"
    );
    const movie = f.file(
        "movie.mp4",
        "video/mp4",
        "owned bytes",
        "folder/movie.mp4"
    );
    const standalone = f.file("bonus.webm", "video/webm");
    const lease = await f.exports.createDroppedPlaylists(f.host, [
        playlist,
        movie,
        standalone,
    ]);
    assert.deepEqual(
        Array.from(lease.catalog.items, (item) => item.name),
        ["Remote", "Local clip", "bonus.webm"]
    );
    assert.equal(lease.catalog.items[0].url, "https://media.example/live.m3u8");
    assert.equal(lease.catalog.items[0].mime, "application/vnd.apple.mpegurl");
    assert.equal(lease.catalog.items[1].url, "blob:fixture/1");
    assert.equal(lease.catalog.items[2].url, "blob:fixture/2");
    assert.deepEqual(f.created, [movie, standalone]);
    assert.equal(lease.catalog.persistent, false);
    assert.equal(lease.catalog.storage, "browser");
    assert.equal(lease.catalog.items.length, 3);
    assert.equal(lease.catalog.sources.length, 1);
    assert.equal(
        lease.catalog.items.every(
            (item) => item.sourceId === lease.catalog.sources[0].id
        ),
        true
    );
    lease.release();
    lease.release();
    assert.deepEqual(f.revoked, ["blob:fixture/1", "blob:fixture/2"]);
});

test("missing or ambiguous relative local references fail before allocating URLs", async () => {
    const f = fixture();
    const playlist = f.file("list.m3u", "", "#EXTM3U\nclip.mp4\n");
    await assert.rejects(
        f.exports.createDroppedPlaylists(f.host, [playlist]),
        (error) => error.droppedPlaylistCode === "playlist_local_unavailable"
    );
    const duplicateA = f.file("clip.mp4", "video/mp4", "a", "clip.mp4");
    const duplicateB = f.file("clip.mp4", "video/mp4", "b", "clip.mp4");
    await assert.rejects(
        f.exports.createDroppedPlaylists(f.host, [
            playlist,
            duplicateA,
            duplicateB,
        ]),
        (error) => error.droppedPlaylistCode === "playlist_local_unavailable"
    );
    assert.equal(f.created.length, 0);
    assert.equal(f.revoked.length, 0);
});

test("absolute playlist paths never fall back to a matching co-dropped basename", async () => {
    const f = fixture();
    const playlist = f.file(
        "list.m3u",
        "",
        "#EXTM3U\n/Users/person/clip.mp4\n"
    );
    const clip = f.file("clip.mp4", "video/mp4");
    await assert.rejects(
        f.exports.createDroppedPlaylists(f.host, [playlist, clip]),
        (error) => error.droppedPlaylistCode === "playlist_local_unavailable"
    );
    assert.equal(f.created.length, 0);
});

test("dropped HLS manifest with local segments is explicitly unsupported and URL allocation is atomic", async () => {
    const f = fixture();
    const manifest = f.file(
        "stream.m3u8",
        "application/vnd.apple.mpegurl",
        "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nsegment0.ts\n"
    );
    await assert.rejects(
        f.exports.createDroppedPlaylists(f.host, [manifest]),
        (error) =>
            error.droppedPlaylistCode === "playlist_hls_local_unsupported"
    );
    assert.equal(f.created.length, 0);
});

test("later URL allocation failure revokes every URL already created", async () => {
    const allocation = fixture({ createFailureAt: 2 });
    const playlist = allocation.file("list.m3u", "", "#EXTM3U\nclip.mp4\n");
    const video = allocation.file("clip.mp4", "video/mp4");
    const second = allocation.file("second.mp4", "video/mp4");
    await assert.rejects(
        allocation.exports.createDroppedPlaylists(allocation.host, [
            playlist,
            video,
            second,
        ])
    );
    assert.equal(allocation.created.length, 2);
    assert.deepEqual(allocation.revoked, ["blob:fixture/1"]);
});

test("playlist bytes decode strictly and use FileReader or text fallback asynchronously", async () => {
    const invalid = fixture();
    const invalidUtf8 = invalid.file(
        "bad.m3u",
        "",
        "#EXTM3U\nhttps://x.test/a.mp4\n"
    );
    Object.defineProperty(invalidUtf8, "arrayBuffer", {
        value: async () => new Uint8Array([0xc0, 0xaf]).buffer,
    });
    Object.defineProperty(invalidUtf8, "text", { value: undefined });
    await assert.rejects(
        invalid.exports.createDroppedPlaylists(invalid.host, [invalidUtf8]),
        (error) => error.droppedPlaylistCode === "playlist_invalid"
    );

    class Reader {
        constructor(bytes) {
            this.bytes = bytes;
        }
        readAsArrayBuffer() {
            this.result = this.bytes.buffer.slice(
                this.bytes.byteOffset,
                this.bytes.byteOffset + this.bytes.byteLength
            );
            queueMicrotask(() => this.onload());
        }
    }
    const validUtf8 = new TextEncoder().encode(
        "\ufeff#EXTM3U\nhttps://x.test/a.mp4\n"
    );
    const legacyReader = fixture({
        FileReader: class extends Reader {
            constructor() {
                super(validUtf8);
            }
        },
    });
    const readerFile = legacyReader.file("reader.m3u", "", "ignored");
    Object.defineProperty(readerFile, "arrayBuffer", { value: undefined });
    Object.defineProperty(readerFile, "text", { value: undefined });
    const readLease = await legacyReader.exports.createDroppedPlaylists(
        legacyReader.host,
        [readerFile]
    );
    assert.equal(readLease.catalog.items[0].url, "https://x.test/a.mp4");
    readLease.release();

    for (const bytes of [
        new Uint8Array([0xff, 0xfe, 0x23, 0x00, 0x45, 0x00]),
        new Uint8Array([0xfe, 0xff, 0x00, 0x23, 0x00, 0x45]),
    ]) {
        const badReader = fixture({
            FileReader: class extends Reader {
                constructor() {
                    super(bytes);
                }
            },
        });
        const badFile = badReader.file("wide.m3u", "", "ignored");
        Object.defineProperty(badFile, "arrayBuffer", { value: undefined });
        Object.defineProperty(badFile, "text", { value: undefined });
        await assert.rejects(
            badReader.exports.createDroppedPlaylists(badReader.host, [badFile]),
            (error) => error.droppedPlaylistCode === "playlist_invalid"
        );
    }

    const textFallback = fixture();
    textFallback.host.FileReader = undefined;
    const textFile = textFallback.file("text.m3u", "", "ignored");
    Object.defineProperty(textFile, "arrayBuffer", { value: undefined });
    Object.defineProperty(textFile, "text", {
        value: async () => "#EXTM3U\nhttps://x.test/a.mp4\n",
    });
    const textLease = await textFallback.exports.createDroppedPlaylists(
        textFallback.host,
        [textFile]
    );
    assert.equal(textLease.catalog.items[0].url, "https://x.test/a.mp4");
    textLease.release();
});

test("aggregate playlist record cap is checked before any local path lookup", async () => {
    const f = fixture();
    let pathReads = 0;
    function trackedPlaylist(name, count) {
        const lines = Array.from({ length: count }, () => "clip.mp4");
        const file = f.file(name, "", "#EXTM3U\n" + lines.join("\n") + "\n");
        Object.defineProperty(file, "webkitRelativePath", {
            get() {
                pathReads++;
                return name;
            },
        });
        return file;
    }
    const first = trackedPlaylist("first.m3u", 2048);
    const second = trackedPlaylist("second.m3u", 2049);
    await assert.rejects(
        f.exports.createDroppedPlaylists(f.host, [first, second]),
        (error) => error.droppedPlaylistCode === "playlist_limit"
    );
    assert.equal(pathReads, 0);
    assert.equal(f.created.length, 0);
});

test("aggregate cap includes standalone videos and admits the exact boundary", async () => {
    for (const count of [4095, 4096]) {
        const f = fixture();
        const playlist = f.file(
            "limit.m3u",
            "",
            "#EXTM3U\n" + "https://cdn.test/a.mp4\n".repeat(count)
        );
        const video = f.file("standalone.mp4", "video/mp4");
        const pending = f.exports.createDroppedPlaylists(f.host, [
            playlist,
            video,
        ]);
        if (count === 4096) {
            await assert.rejects(
                pending,
                (error) => error.droppedPlaylistCode === "playlist_limit"
            );
            assert.equal(f.created.length, 0);
        } else {
            const lease = await pending;
            assert.equal(lease.catalog.items.length, 4096);
            assert.equal(f.created.length, 1);
            lease.release();
            assert.equal(f.revoked.length, 1);
        }
    }
});

test("batch detection and input limits are synchronous and do not inspect contents", () => {
    const f = fixture();
    const playlist = f.file(
        "one.m3u",
        "",
        "#EXTM3U\nhttps://example.test/x.mp4"
    );
    assert.equal(f.exports.hasDroppedPlaylist([playlist]), true);
    assert.equal(f.exports.hasDroppedPlaylist([f.file("video.mp4")]), false);
    assert.equal(
        f.exports.hasDroppedPlaylist(new Array(129).fill(playlist)),
        false
    );
});

test("lease accepts only browser File instances", async () => {
    const f = fixture();
    await assert.rejects(
        f.exports.createDroppedPlaylists(f.host, [
            {
                name: "list.m3u",
                size: 20,
                text: async () => "#EXTM3U\nhttps://x.test/a.mp4",
            },
        ]),
        (error) => error.droppedPlaylistCode === "playlist_invalid"
    );
    assert.equal(f.created.length, 0);
});
