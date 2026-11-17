"use strict";
// Independent boundary tests exercise real exported modules with real File objects.
// Network, persistent storage and native path reads are forbidden, never emulated.
const assert = require("node:assert/strict");
const { File } = require("node:buffer");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const test = require("node:test");
const ts = require("typescript");
const { createHash } = require("node:crypto");
const sourceHashes = {};

function load(relative, imports = {}) {
    const exports = {};
    const filename = path.join(__dirname, "..", relative);
    const source = fs.readFileSync(filename, "utf8");
    sourceHashes[relative] = createHash("sha256").update(source).digest("hex");
    const code = ts.transpileModule(source, {
        compilerOptions: {
            module: ts.ModuleKind.CommonJS,
            target: ts.ScriptTarget.ES5,
        },
    }).outputText;
    vm.runInNewContext(
        code,
        {
            exports,
            URL,
            TextDecoder,
            Uint8Array,
            ArrayBuffer,
            require(name) {
                assert.ok(
                    Object.prototype.hasOwnProperty.call(imports, name),
                    "unapproved import: " + name
                );
                return imports[name];
            },
            fetch() {
                assert.fail("module must not fetch playlist entries");
            },
            eval() {
                assert.fail("playlist must not evaluate code");
            },
        },
        { filename }
    );
    return exports;
}
const videos = load("src/offline/dropped-video.ts");
const playlists = load("src/offline/dropped-playlist.ts", {
    "./dropped-video": videos,
});
const plugin = load("src/plugins/local-drop.ts", {
    "../offline/dropped-video": videos,
    "../offline/dropped-playlist": playlists,
});
const MiB = 1024 * 1024;
const flush = () => new Promise((resolve) => setImmediate(resolve));
const video = (name = "movie.mp4") =>
    new File(["video fixture"], name, { type: "video/mp4" });
const document = (body, name = "channels.m3u8") =>
    new File([body], name, { type: "application/vnd.apple.mpegurl" });
const remote = "https://media.invalid/movie.mp4";
function closedError(error) {
    assert.ok(
        typeof error.droppedPlaylistCode === "string",
        "rejection must carry closed error code"
    );
    for (const privateValue of [
        "private-token",
        "private-user",
        "private.invalid",
        "passwords",
        "EXECUTED",
    ])
        assert.ok(
            !String(error.message).includes(privateValue),
            "error must not echo private input"
        );
    return true;
}
function host() {
    const created = [],
        revoked = [],
        forbidden = [];
    const deny = (name) => () => {
        forbidden.push(name);
        assert.fail("unexpected side effect: " + name);
    };
    let serial = 0;
    const w = {
        File,
        TextDecoder,
        document: { createElement: () => ({ canPlayType: () => "probably" }) },
        URL: {
            createObjectURL(file) {
                assert.ok(file instanceof File);
                const url = "blob:security-fixture/" + ++serial;
                created.push({ file, url });
                return url;
            },
            revokeObjectURL(url) {
                revoked.push(url);
            },
        },
        fetch: deny("fetch"),
        XMLHttpRequest: deny("XHR"),
        loadProv: deny("loadProv"),
        eval: deny("eval"),
        showOpenFilePicker: deny("picker"),
        showDirectoryPicker: deny("directory"),
        stbSetItem: deny("persist"),
        stbDelItem: deny("remove"),
    };
    for (const key of [
        "localStorage",
        "indexedDB",
        "sessionStorage",
        "ottpStorage",
        "__TAURI__",
    ])
        Object.defineProperty(w, key, { configurable: true, get: deny(key) });
    return { w, created, revoked, forbidden };
}
function integration() {
    const f = host(),
        listeners = new Map(),
        timers = new Map(),
        played = [],
        messages = [];
    // Browser selection: a native bridge is absent, not a path-reading fallback.
    delete f.w.__TAURI__;
    let counter = 0,
        current;
    Object.assign(f.w, {
        _: (value) => value,
        __ottCommandChannelLoad: {},
        __ottActiveProviderDriver: { id: "m3u" },
        __ottKiosk: { enabled: () => false, locked: () => false },
        __ottStandby: { blocked: () => false },
        __ottParental: { needs: () => false },
        __ottLocalMedia: {
            playDroppedVideo(catalog, admitted, release) {
                assert.equal(admitted(), true);
                assert.equal(
                    catalog.sources.length,
                    1,
                    "actual Local handoff requires one session source"
                );
                assert.ok(
                    catalog.items.every(
                        (item) => item.sourceId === catalog.sources[0].id
                    ),
                    "all remote and local items must retain the one accepted source identity"
                );
                if (current) current.release();
                current = { catalog, admitted, release };
                played.push(current);
            },
        },
        addEventListener(name, fn) {
            if (!listeners.has(name)) listeners.set(name, new Set());
            listeners.get(name).add(fn);
        },
        removeEventListener(name, fn) {
            listeners.get(name)?.delete(fn);
        },
        setTimeout(fn) {
            const id = ++counter;
            timers.set(id, fn);
            return id;
        },
        clearTimeout(id) {
            timers.delete(id);
        },
        showShift(text) {
            messages.push(text);
        },
    });
    const api = plugin.installLocalDrop(f.w);
    function drop(files) {
        const event = {
            dataTransfer: { files, types: ["Files"] },
            preventDefault() {
                this.prevented = true;
            },
            stopPropagation() {
                this.stopped = true;
            },
        };
        for (const fn of listeners.get("drop") || []) fn(event);
        assert.equal(
            event.prevented,
            true,
            "File drop must not navigate the page"
        );
        return event;
    }
    return { ...f, api, drop, timers, played, messages };
}
function heldDocument(body = "#EXTM3U\nmovie.mp4\n") {
    const file = document(body, "pending.m3u");
    let resolve,
        reads = 0;
    Object.defineProperty(file, "arrayBuffer", {
        value: () => {
            reads++;
            return new Promise((done) => {
                resolve = done;
            });
        },
    });
    return {
        file,
        reads: () => reads,
        finish: () => {
            assert.equal(reads, 1);
            resolve(new TextEncoder().encode(body).buffer);
        },
    };
}

test("untrusted entry schemes and executable-looking directives cannot fetch, execute or allocate media", async () => {
    for (const entry of [
        "javascript:globalThis.EXECUTED=true",
        "data:video/mp4;base64,AAAA",
        "blob:private-token",
        "file:///private/passwords.mp4",
        "ftp://private.invalid/movie.mp4",
        "rtsp://private.invalid/live",
        "pipe:cat /private/passwords",
        "plugin://EXECUTED",
        "C:\\private\\passwords.mp4",
        "//private.invalid/live",
        "/private/passwords.mp4",
        "../passwords.mp4",
        "ffmpeg -i https://private.invalid/movie.mp4",
    ]) {
        const f = host();
        await assert.rejects(
            playlists.createDroppedPlaylists(f.w, [
                document("#EXTM3U\n" + entry + "\n"),
            ]),
            closedError,
            entry
        );
        assert.equal(f.created.length, 0, entry);
        assert.deepEqual(f.forbidden, []);
    }
    const f = host();
    const lease = await playlists.createDroppedPlaylists(f.w, [
        document(
            '#EXTM3U url-tvg="https://private.invalid/guide.xml"\n' +
                "#EXTVLCOPT:http-user-agent=EXECUTED\n#KODIPROP:inputstream=EXECUTED\n" +
                '#EXTINF:-1 tvg-id="n,1",News, evening\n' +
                remote +
                "\n"
        ),
    ]);
    assert.equal(lease.catalog.items.length, 1);
    assert.equal(lease.catalog.items[0].url, remote);
    assert.equal(lease.catalog.items[0].name, "News, evening");
    assert.deepEqual(f.forbidden, []);
    lease.release();
});

test("HLS master, finite media and low-latency manifests never become segment queues", async () => {
    for (const body of [
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nhttps://media.invalid/video.m3u8\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nhttps://media.invalid/seg.ts\n#EXT-X-ENDLIST\n",
        '#EXTM3U\n#EXT-X-PART-INF:PART-TARGET=0.5\n#EXT-X-PART:DURATION=0.5,URI="part.m4s"\n#EXTINF:2,\nseg.m4s\n',
    ]) {
        const f = host();
        assert.equal(playlists.parseDroppedPlaylist(body).isHls, true);
        await assert.rejects(
            playlists.createDroppedPlaylists(f.w, [document(body)]),
            closedError
        );
        assert.equal(f.created.length, 0);
        assert.deepEqual(f.forbidden, []);
    }
    const f = host();
    const lease = await playlists.createDroppedPlaylists(f.w, [
        document(
            "#EXTM3U\n#EXTINF:-1,Live\nhttps://media.invalid/live.m3u8?key=one\n",
            "iptv.m3u8"
        ),
    ]);
    assert.equal(lease.catalog.items.length, 1);
    assert.equal(
        lease.catalog.items[0].url,
        "https://media.invalid/live.m3u8?key=one"
    );
    lease.release();
});

test("browser local references require unique co-dropped File authority and exact object-URL cleanup", async () => {
    for (const files of [
        [document("#EXTM3U\nmovie.mp4\n")],
        [document("#EXTM3U\nmovie.mp4\n"), video(), video()],
        [document("#EXTM3U\n../movie.mp4\n"), video()],
        [document("#EXTM3U\nfile:///movie.mp4\n"), video()],
    ]) {
        const f = host();
        await assert.rejects(
            playlists.createDroppedPlaylists(f.w, files),
            closedError
        );
        assert.equal(f.created.length, 0);
    }
    const f = host(),
        exact = video();
    const lease = await playlists.createDroppedPlaylists(f.w, [
        document("#EXTM3U\n./movie.mp4\n"),
        exact,
    ]);
    assert.equal(f.created.length, 1);
    assert.equal(
        f.created[0].file,
        exact,
        "never fabricate File/path access from the playlist text"
    );
    assert.equal(lease.catalog.sources.length, 1);
    assert.ok(
        lease.catalog.items.every(
            (item) => item.sourceId === lease.catalog.sources[0].id
        )
    );
    assert.ok(
        lease.catalog.items.every((item) => item.url === f.created[0].url)
    );
    lease.release();
    lease.release();
    assert.deepEqual(f.revoked, [f.created[0].url]);
    assert.deepEqual(f.forbidden, []);
});

test("UTF-8 BOM is usable but invalid encoded bytes and unpaired parser surrogates are rejected", async () => {
    const f = host();
    const lease = await playlists.createDroppedPlaylists(f.w, [
        document("\ufeff#EXTM3U\r\n" + remote + "\r\n"),
    ]);
    assert.equal(lease.catalog.items[0].url, remote);
    lease.release();
    for (const bytes of [
        new Uint8Array([0xff]),
        new Uint8Array([0xc3, 0x28]),
        new Uint8Array([0xff, 0xfe, 0x23, 0x00]),
    ])
        await assert.rejects(
            playlists.createDroppedPlaylists(f.w, [document(bytes)]),
            closedError
        );
    for (const surrogate of ["\ud800", "\udfff"])
        assert.throws(
            () =>
                playlists.parseDroppedPlaylist(
                    "#EXTM3U\n#EXTINF:-1," + surrogate + "\n" + remote + "\n"
                ),
            closedError
        );
    assert.equal(f.created.length, 0);
});

test("2MiB per file, 8MiB batch and 128-input admission limits run before reads", async () => {
    const f = host();
    let reads = 0;
    const sized = (size, name) => {
        const file = document("#EXTM3U\n" + remote, name);
        Object.defineProperty(file, "size", { value: size });
        Object.defineProperty(file, "arrayBuffer", {
            value: () => {
                reads++;
                assert.fail("over-budget input read");
            },
        });
        return file;
    };
    await assert.rejects(
        playlists.createDroppedPlaylists(f.w, [
            sized(2 * MiB + 1, "large.m3u"),
        ]),
        closedError
    );
    await assert.rejects(
        playlists.createDroppedPlaylists(
            f.w,
            Array.from({ length: 5 }, (_, i) => sized(2 * MiB, i + ".m3u"))
        ),
        closedError
    );
    await assert.rejects(
        playlists.createDroppedPlaylists(
            f.w,
            Array.from({ length: 129 }, (_, i) => sized(1, i + ".m3u"))
        ),
        closedError
    );
    assert.equal(reads, 0);
    assert.equal(f.created.length, 0);
});

test("128 inputs and 4096 entries are accepted, but aggregate expanded entry 4097 is rejected atomically", async () => {
    const f = host();
    const inputs = Array.from({ length: 128 }, (_, i) =>
        document("#EXTM3U\n" + remote + "?i=" + i, i + ".m3u")
    );
    const batch = await playlists.createDroppedPlaylists(f.w, inputs);
    assert.equal(batch.catalog.items.length, 128);
    batch.release();
    const body = (count) =>
        "#EXTM3U\n" +
        Array.from({ length: count }, (_, i) => remote + "?i=" + i).join("\n");
    const exact = await playlists.createDroppedPlaylists(f.w, [
        document(body(4096)),
    ]);
    assert.equal(exact.catalog.items.length, 4096);
    exact.release();
    await assert.rejects(
        playlists.createDroppedPlaylists(f.w, [document(body(4097))]),
        closedError
    );
    await assert.rejects(
        playlists.createDroppedPlaylists(f.w, [
            document(body(2048), "a.m3u"),
            document(body(2049), "b.m3u"),
        ]),
        closedError
    );
    assert.equal(
        f.created.length,
        0,
        "remote queues do not create arbitrary blob copies"
    );
});

for (const change of ["new-drop", "provider", "dispose", "standby", "kiosk"]) {
    test(
        "late playlist read after " +
            change +
            " cannot replace current playback or retain stale URLs",
        async () => {
            const f = integration(),
                pending = heldDocument(),
                local = video();
            f.drop([video("old.mp4")]);
            assert.equal(f.played.length, 1);
            const files = [pending.file, local];
            f.drop(files);
            assert.equal(pending.reads(), 1);
            files.length = 0; // DataTransfer lifetime ended; the actual gesture must already be captured.
            if (change === "new-drop") f.drop([video("current.mp4")]);
            if (change === "provider") {
                f.w.__ottCommandChannelLoad = {};
                f.w.__ottActiveProviderDriver = { id: "plex" };
            }
            if (change === "dispose") f.api.dispose();
            if (change === "standby") f.w.__ottStandby.blocked = () => true;
            if (change === "kiosk") f.w.__ottKiosk.locked = () => true;
            const admitted = f.played.length;
            pending.finish();
            await flush();
            await flush();
            assert.equal(f.played.length, admitted);
            const stale = f.created.filter((item) => item.file === local);
            assert.equal(
                stale.length,
                1,
                "exercise actual late-created lease cleanup"
            );
            assert.equal(
                f.revoked.filter((url) => url === stale[0].url).length,
                1
            );
            assert.equal(f.timers.size, 0);
            assert.deepEqual(f.forbidden, []);
            if (change === "new-drop") {
                const current = f.created.find(
                    (item) => item.file.name === "current.mp4"
                );
                assert.ok(
                    !f.revoked.includes(current.url),
                    "stale cleanup must not revoke the newer active source"
                );
            }
            f.api.dispose();
            assert.equal(
                new Set(f.revoked).size,
                f.revoked.length,
                "every object URL released at most once"
            );
        }
    );
}

test("mixed remote/local playlists retain the single source identity required by actual Local admission", async () => {
    const f = host(),
        local = video();
    const lease = await playlists.createDroppedPlaylists(f.w, [
        document(
            "#EXTM3U\n#EXTINF:-1,Network\n" +
                remote +
                "\n#EXTINF:60,Local\nmovie.mp4\n"
        ),
        local,
    ]);
    assert.equal(lease.catalog.sources.length, 1);
    const sourceId = lease.catalog.sources[0].id;
    assert.ok(lease.catalog.items.every((item) => item.sourceId === sourceId));
    assert.ok(lease.catalog.items.some((item) => item.url === remote));
    assert.ok(
        lease.catalog.items.some((item) => item.url === f.created[0].url)
    );
    lease.release();
    assert.deepEqual(f.revoked, [f.created[0].url]);
});

test("exact 2MiB documents and exact 8MiB total remain usable without copying media", async () => {
    const f = host();
    const prefix = "#EXTM3U\n#",
        suffix = "\n" + remote + "\n";
    const body =
        prefix +
        "x".repeat(2 * MiB - Buffer.byteLength(prefix + suffix)) +
        suffix;
    assert.equal(Buffer.byteLength(body), 2 * MiB);
    const files = Array.from({ length: 4 }, (_, i) =>
        document(body, "limit-" + i + ".m3u")
    );
    assert.equal(
        files.reduce((sum, file) => sum + file.size, 0),
        8 * MiB
    );
    const lease = await playlists.createDroppedPlaylists(f.w, files);
    assert.equal(lease.catalog.items.length, 4);
    assert.equal(f.created.length, 0);
    assert.deepEqual(f.forbidden, []);
    lease.release();
});

test("two asynchronous playlist drops resolve out of order without stealing the newer lease", async () => {
    const f = integration(),
        a = heldDocument(),
        b = heldDocument("#EXTM3U\nb.mp4\n");
    const aVideo = video(),
        bVideo = video("b.mp4");
    f.drop([a.file, aVideo]);
    f.drop([b.file, bVideo]);
    b.finish();
    await flush();
    await flush();
    assert.equal(f.played.length, 1);
    const active = f.created.find((item) => item.file === bVideo);
    assert.ok(
        f.played[0].catalog.items.every((item) => item.url === active.url)
    );
    a.finish();
    await flush();
    await flush();
    assert.equal(f.played.length, 1);
    const stale = f.created.find((item) => item.file === aVideo);
    assert.ok(f.revoked.includes(stale.url));
    assert.ok(!f.revoked.includes(active.url));
    assert.equal(f.timers.size, 0);
    f.api.dispose();
    assert.equal(new Set(f.revoked).size, f.revoked.length);
});

test("partial object-URL allocation failure releases only the new candidates and retains the playing source", async () => {
    const f = integration();
    f.drop([video("current.mp4")]);
    const current = f.created[0];
    const allocate = f.w.URL.createObjectURL;
    f.w.URL.createObjectURL = (file) => {
        if (file.name === "broken.mp4")
            throw new Error("private-token private.invalid");
        return allocate(file);
    };
    f.drop([
        document("#EXTM3U\nnew.mp4\nbroken.mp4\n"),
        video("new.mp4"),
        video("broken.mp4"),
    ]);
    await flush();
    await flush();
    assert.equal(f.played.length, 1);
    const partial = f.created.find((item) => item.file.name === "new.mp4");
    assert.ok(partial);
    assert.deepEqual(f.revoked, [partial.url]);
    assert.ok(!f.revoked.includes(current.url));
    assert.ok(
        f.messages.every(
            (message) => !/private-token|private\.invalid/.test(message)
        )
    );
    f.api.dispose();
});

test.after(() =>
    console.log("loaded-source-sha256=" + JSON.stringify(sourceHashes))
);
