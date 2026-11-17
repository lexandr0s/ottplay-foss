"use strict";
// Real Local plugin + storage shadow + selection. Only host lifetime and IO are
// injected; no browser, server, player, filesystem source scan or decoder runs.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");
const vm = require("node:vm");
const { File } = require("node:buffer");
const imports = require("./helpers/remote-transfer-dependency.cjs").storage;
const ROOT = path.resolve(__dirname, "..");
const clone = (v) => JSON.parse(JSON.stringify(v));
function load(name, requires = {}, globals = {}) {
    const exports = {};
    const source = fs.readFileSync(path.join(ROOT, name), "utf8");
    vm.runInNewContext(
        ts.transpileModule(source, {
            compilerOptions: {
                module: ts.ModuleKind.CommonJS,
                target: ts.ScriptTarget.ES5,
            },
        }).outputText,
        require("./helpers/source-core-runtime.cjs")(
            {
                clearTimeout,
                console,
                exports,
                require(name) {
                    assert.ok(Object.hasOwn(requires, name), name);
                    return requires[name];
                },
                setTimeout,
                URL,
                ...globals,
                exports,
            },
            name
        )
    );
    return exports;
}
function catalog(name = "one") {
    return {
        available: true,
        items: [
            {
                available: true,
                id: name,
                mime: "video/mp4",
                name: name + ".mp4",
                sourceId: "source-" + name,
                url: "blob:fixture/" + name,
            },
        ],
        persistent: false,
        scanning: false,
        sources: [
            {
                available: true,
                id: "source-" + name,
                kind: "file",
                name: name + ".mp4",
            },
        ],
    };
}
function queue(names) {
    const value = catalog(names[0]);
    value.items = names.map((name, i) => ({
        ...catalog(name).items[0],
        id: "queue-item-" + i,
        sourceId: value.sources[0].id,
    }));
    return value;
}
function fixture(random = Math.random) {
    const saved = new Map([
        ["ottplayprov", "m3u"],
        ["m3uprimaryIndex", "4"],
        [
            "offlineProfiles",
            '{"active":0,"profiles":[{"name":"saved folder"}]}',
        ],
        ["offlinechannelLibrary:offline", '{"selected":"old","rows":["old"]}'],
    ]);
    const before = new Map(saved),
        calls = [],
        releases = [],
        events = {},
        pending = [];
    let blocked = false,
        kiosk = false,
        locked = false,
        defer = false;
    const forbidden = () => {
        throw new Error(
            "Unexpected source scan, network or persistence helper"
        );
    };
    const w = {
        __ottActiveProviderDriver: { dispose() {}, id: "m3u" },
        __ottClassicPlayback: { cancel() {}, checkpointNow() {} },
        __ottCommandChannelLoad: {},
        __ottKiosk: { locked: () => kiosk },
        __ottOfflineProfiles: {
            current: () => ({ binding_id: "saved-folder" }),
            namespace: () => "offline",
        },
        __ottParental: { needs: () => locked },
        __ottStandby: {
            blocked: () => blocked,
            error: () => new Error("standby"),
        },
        addEventListener(name, handler) {
            events[name] = handler;
        },
        closeList() {},
        fetch: forbidden,
        localStorage: {
            getItem: (k) => (saved.has(k) ? saved.get(k) : null),
            key: (i) => [...saved.keys()][i] || null,
            get length() {
                return saved.size;
            },
            removeItem: (k) => saved.delete(k),
            setItem: (k, v) => saved.set(k, String(v)),
        },
    };
    const storageModule = load(
        "src/storage/index.ts",
        {},
        {
            ...imports(),
            document: { cookie: "" },
            window: w,
        }
    );
    w.stbGetItem = storageModule.stbGetItem;
    w.stbSetItem = storageModule.stbSetItem;
    w.stbDelItem = storageModule.stbDelItem;
    const api = load(
        "src/plugins/local-media.ts",
        {
            "../commands/remote-feature": { loadRemoteFeature: forbidden },
            "../offline/browser-local-media": {
                installBrowserLocalMedia: () => ({
                    list: forbidden,
                    suspend() {},
                }),
                installOfflineShell() {},
            },
            "../offline/local-media-progress": load(
                "src/offline/local-media-progress.ts"
            ),
            "../offline/playback-selection": load(
                "src/offline/playback-selection.ts"
            ),
            "../offline/profiles": { installOfflineProfiles() {} },
            "./native-bridge": {
                resolveNativePlugin: () => ({}),
                tauriInvoke: forbidden,
            },
        },
        { Math: Object.assign(Object.create(Math), { random }) }
    ).installLocalMedia(w);
    w.loadProv = (id, session) => {
        calls.push(["load", id]);
        w.__ottCommandChannelLoad = {};
        w.__ottActiveProviderDriver?.dispose();
        w.__ottActiveProviderDriver = null;
        const start = () => {
            if (session) session();
            let alive = true;
            const driver = {
                dispose() {
                    alive = false;
                    release?.();
                },
                id: id || w.stbGetItem("ottplayprov"),
            };
            w.__ottActiveProviderDriver = driver;
            const release =
                driver.id === "offline"
                    ? api.bindPlaybackOwner(() => alive)
                    : undefined;
        };
        if (defer) pending.push(start);
        else start();
    };
    return {
        api,
        before,
        calls,
        drop(name = "one", admitted = () => true) {
            const value = catalog(name);
            api.playDroppedVideo(value, admitted, () => releases.push(name));
            return value;
        },
        events,
        pending,
        releases,
        saved,
        setBlocked: (v) => {
            blocked = v;
        },
        setDeferred: (v) => {
            defer = v;
        },
        setKiosk: (v) => {
            kiosk = v;
        },
        setLocked: (v) => {
            locked = v;
        },
        w,
    };
}

test("accepted drop uses Local handoff and repeated reads retain only its in-memory catalog", async () => {
    const f = fixture();
    const input = f.drop();
    assert.deepEqual(f.calls, [["load", "offline"]]);
    const first = await f.api.playbackList();
    const second = await f.api.list();
    assert.equal(first, second);
    assert.notEqual(first, input, "caller cannot mutate the captured catalog");
    assert.equal(first.items.length, 1);
    assert.equal(first.items[0].id, "one");
    assert.equal(f.api.playbackMime(first.items[0].url), "video/mp4");
    assert.equal(f.api.playbackMime("blob:not-owned"), "");
    assert.equal(f.api.isDroppedCatalog(first), true);
    assert.equal(f.api.isDroppedCatalog(clone(first)), false);
    assert.equal(f.api.playbackFilter(first), first);
    assert.equal(f.w.stbGetItem("ottplayprov"), "offline");
    assert.deepEqual(f.saved, f.before);
    assert.deepEqual(f.releases, []);
});

test("a dropped queue preserves supplied order and every file without scanning or saving", async () => {
    const f = fixture();
    const input = queue([
        "z-last-alphabetically",
        "a-first-alphabetically",
        "z-last-alphabetically",
    ]);
    f.api.playDroppedVideo(
        input,
        () => true,
        () => f.releases.push("queue")
    );
    const value = await f.api.playbackList();
    assert.deepEqual(clone(value.items), input.items);
    assert.deepEqual(clone(f.api.playbackOrder()), [
        "local-queue-item-0",
        "local-queue-item-1",
        "local-queue-item-2",
    ]);
    assert.equal(f.api.playbackFilter(value), value);
    for (const item of value.items) {
        assert.equal(f.api.isPlaybackUrl(item.url), true);
        assert.equal(f.api.playbackMime(item.url), "video/mp4");
    }
    input.items.splice(0);
    assert.equal((await f.api.playbackList()).items.length, 3);
    assert.deepEqual(f.calls, [["load", "offline"]]);
    assert.deepEqual(f.saved, f.before);
    f.api.playbackOrdered();
    assert.equal(f.api.playbackOrder(), null);
    assert.equal((await f.api.playbackList()).items.length, 3);
    f.api.cancelDroppedVideo();
    f.api.cancelDroppedVideo();
    assert.deepEqual(f.releases, ["queue"]);
    assert.deepEqual(f.saved, f.before);
});

test("shuffle changes the starting item and preserves one order for the accepted drop", async () => {
    const f = fixture(() => 0);
    f.w.settings = { droppedFileShuffle: 1 };
    const input = queue(["same", "same", "other"]);
    f.api.playDroppedVideo(
        input,
        () => true,
        () => {}
    );
    const value = await f.api.playbackList();
    assert.deepEqual(clone(value.items.map((item) => item.id)), [
        "queue-item-1",
        "queue-item-2",
        "queue-item-0",
    ]);
    assert.deepEqual(
        clone(f.api.playbackOrder()),
        Array.from(value.items, (item) => "local-" + item.id)
    );
    assert.equal(input.items[0].id, "queue-item-0");
    assert.deepEqual(f.saved, f.before);
    f.w.settings.droppedFileShuffle = 0;
    assert.equal((await f.api.playbackList()).items[0].id, "queue-item-1");
    f.api.playDroppedVideo(
        input,
        () => true,
        () => {}
    );
    assert.equal((await f.api.playbackList()).items[0].id, "queue-item-0");
});

test("Repeat in Actions targets the active drop and leaves the ordinary Local preference alone", () => {
    const f = fixture();
    f.w.settings = { droppedFileRepeat: 0 };
    f.w.__ottPlayerSettingsHost = {
        store: {
            begin() {
                let next;
                return {
                    set(key, value) {
                        assert.equal(key, "droppedFileRepeat");
                        next = value;
                    },
                    commit(_writes, admitted) {
                        assert.equal(admitted(), true);
                        f.w.settings.droppedFileRepeat = next;
                    },
                };
            },
        },
    };
    assert.equal(f.api.cycleDroppedRepeat(), false);
    f.drop();
    for (const mode of ["one", "off", "all"]) {
        assert.equal(f.api.cycleDroppedRepeat(), true);
        assert.equal(f.api.droppedRepeatMode(), mode);
    }
    f.api.cancelDroppedVideo();
    assert.equal(f.api.droppedRepeatMode(), undefined);
    assert.deepEqual(f.saved, f.before);
});

test("invalid later queue entries reject the whole replacement and retain the previous video", async () => {
    for (const invalidate of [
        (v) => {
            v.items[1].sourceId = "another-source";
        },
        (v) => {
            v.items[1].id = v.items[0].id;
        },
        (v) => {
            v.items[1].url = "";
        },
        (v) => {
            v.items[1].available = false;
        },
        (v) => {
            v.items[1].playback = { mode: "unsupported" };
        },
        (v) => {
            v.items[1] = null;
        },
        (v) => {
            delete v.items[1];
        },
        (v) => {
            v.items = [];
        },
        (v) => {
            v.items = queue(Array(4097).fill("video")).items;
        },
    ]) {
        const f = fixture();
        f.drop("old");
        const value = queue(["first", "second"]);
        invalidate(value);
        assert.throws(() =>
            f.api.playDroppedVideo(
                value,
                () => true,
                () => f.releases.push("rejected")
            )
        );
        assert.deepEqual(f.calls, [["load", "offline"]]);
        assert.equal((await f.api.playbackList()).items[0].id, "old");
        assert.deepEqual(f.releases, ["rejected"]);
        assert.deepEqual(f.saved, f.before);
    }
});

test("an entire dropped queue survives standby and releases once on replacement", async () => {
    const f = fixture();
    f.api.playDroppedVideo(
        queue(["one", "two", "three"]),
        () => true,
        () => f.releases.push("queue")
    );
    f.setBlocked(true);
    f.w.__ottActiveProviderDriver.dispose();
    f.w.__ottActiveProviderDriver = null;
    f.api.suspend();
    assert.deepEqual(f.releases, []);
    f.setBlocked(false);
    f.w.loadProv();
    assert.equal((await f.api.playbackList()).items.length, 3);
    f.drop("replacement");
    assert.deepEqual(f.releases, ["queue"]);
    assert.equal((await f.api.playbackList()).items[0].id, "replacement");
    assert.deepEqual(f.saved, f.before);
});

test("drop fully shadows library metadata, cursor and deletion; persist request discards them", async () => {
    const f = fixture();
    f.drop();
    const storage = f.w.ottpStorage;
    const key = "offlinechannelLibrary:offline";
    assert.equal(storage.get(key), null);
    storage.set(key, '{"selected":"one","rows":["one"]}');
    assert.equal(
        storage.transaction(
            [{ after: '{"rows":["two"]}', before: storage.get(key), key }],
            () => true
        ),
        true
    );
    storage.del(key);
    assert.equal(storage.get(key), null);
    storage.set("offlineprimaryIndex", "99");
    assert.equal(storage.localPlayback.end(true), false);
    assert.equal(
        storage.localPlayback.profile([], () => true, true),
        false
    );
    assert.deepEqual(f.saved, f.before);
    assert.equal(f.api.favoriteSnapshot(), null);
    await assert.rejects(f.api.snapshot(), /cannot be saved/);
    assert.equal(f.api.finishSession(true), true);
    assert.deepEqual(f.releases, ["one"]);
    assert.equal(storage.localPlayback.active(), false);
    assert.equal(storage.get("ottplayprov"), "m3u");
    assert.deepEqual(f.saved, f.before);
});

test("invalid replacement and policy refusals release only the rejected lease", () => {
    for (const condition of [
        "shape",
        "admission",
        "kiosk",
        "settings",
        "standby",
    ]) {
        const f = fixture();
        f.drop("old");
        const old = f.w.__ottActiveProviderDriver;
        const value = catalog("rejected");
        if (condition === "shape") value.items.push(clone(value.items[0]));
        if (condition === "kiosk") f.setKiosk(true);
        if (condition === "settings") f.setLocked(true);
        if (condition === "standby") f.setBlocked(true);
        assert.throws(() =>
            f.api.playDroppedVideo(
                value,
                () => condition !== "admission",
                () => f.releases.push("rejected")
            )
        );
        assert.equal(f.w.__ottActiveProviderDriver, old);
        assert.deepEqual(f.calls, [["load", "offline"]]);
        assert.deepEqual(f.releases, ["rejected"]);
        assert.deepEqual(f.saved, f.before);
    }
});

test("accepted B retires A once; delayed A disposer cannot release B", async () => {
    const f = fixture();
    f.drop("a");
    const old = f.w.__ottActiveProviderDriver;
    f.drop("b");
    assert.deepEqual(f.releases, ["a"]);
    old.dispose();
    old.dispose();
    assert.deepEqual(f.releases, ["a"]);
    assert.equal((await f.api.playbackList()).items[0].id, "b");
    f.api.cancelDroppedVideo();
    f.api.cancelDroppedVideo();
    assert.deepEqual(f.releases, ["a", "b"]);
    assert.deepEqual(f.saved, f.before);
});

test("superseded pending A cannot start or retire accepted B", async () => {
    const f = fixture();
    f.setDeferred(true);
    f.drop("a");
    f.drop("b");
    assert.deepEqual(f.releases, ["a"]);
    assert.throws(() => f.pending[0]());
    f.pending[1]();
    assert.equal((await f.api.playbackList()).items[0].id, "b");
    assert.deepEqual(f.releases, ["a"]);
    assert.deepEqual(f.saved, f.before);
});

test("standby disposal and generic cancellation retain the lease for wake rebind", async () => {
    const f = fixture();
    f.drop();
    f.api.cancelPlayback("blob:fixture/one");
    f.setBlocked(true);
    f.w.__ottActiveProviderDriver.dispose();
    f.w.__ottActiveProviderDriver = null;
    f.api.suspend();
    f.api.cancelDroppedVideo();
    await assert.rejects(f.api.playbackList(), /standby/);
    assert.deepEqual(f.releases, []);
    f.setBlocked(false);
    f.w.loadProv();
    assert.equal((await f.api.playbackList()).items[0].id, "one");
    assert.deepEqual(f.saved, f.before);
    assert.deepEqual(f.releases, []);
});

test("retained drop identity survives standby without a driver and ends with release", () => {
    const f = fixture();
    assert.equal(f.api.hasDroppedVideo(), false);
    f.setDeferred(true);
    f.drop();
    assert.equal(
        f.api.hasDroppedVideo(),
        false,
        "pending handoff has no accepted Local session"
    );
    f.pending[0]();
    assert.equal(f.api.hasDroppedVideo(), true);
    f.setBlocked(true);
    f.w.__ottActiveProviderDriver.dispose();
    f.w.__ottActiveProviderDriver = null;
    f.api.suspend();
    assert.equal(
        f.api.hasDroppedVideo(),
        true,
        "standby retires the driver, not the owned session"
    );
    f.setBlocked(false);
    assert.equal(
        f.api.hasDroppedVideo(),
        true,
        "wake admission does not require the next driver yet"
    );
    f.api.cancelDroppedVideo();
    assert.equal(f.api.hasDroppedVideo(), false);
    assert.deepEqual(f.releases, ["one"]);
    assert.deepEqual(f.saved, f.before);
});

test("page teardown and ordinary provider selection retire only the owned transient session", () => {
    for (const leave of [
        (f) => f.events.pagehide(),
        (f) => {
            f.api.cancelDroppedVideo();
            f.w.loadProv("m3u");
        },
    ]) {
        const f = fixture();
        f.drop();
        leave(f);
        f.api.cancelDroppedVideo();
        assert.deepEqual(f.releases, ["one"]);
        assert.equal(f.w.ottpStorage.localPlayback.active(), false);
        assert.equal(f.w.stbGetItem("ottplayprov"), "m3u");
        assert.deepEqual(f.saved, f.before);
    }
});

test("ordinary remote activation retains its existing promotable session contract", async () => {
    const f = fixture(),
        value = catalog("ordinary");
    f.api.remoteActivate(
        value.sources[0].id,
        [value.items[0].id],
        value,
        () => true
    );
    const delivered = await f.api.playbackList();
    assert.equal(f.api.isDroppedCatalog(delivered), false);
    assert.equal(f.w.ottpStorage.localPlayback.end(true), true);
    assert.equal(f.saved.get("ottplayprov"), "offline");
    assert.deepEqual(f.releases, []);
});

test("expanded playlist queues keep more than 128 entries without changing the saved provider", async () => {
    const f = fixture();
    const value = queue(Array.from({ length: 4096 }, (_, i) => "station-" + i));
    value.items.forEach((item, i) => {
        item.url = "https://media.example.test/" + i + ".m3u8";
        item.mime = "application/vnd.apple.mpegurl";
    });
    f.api.playDroppedVideo(
        value,
        () => true,
        () => f.releases.push("playlist")
    );
    const delivered = await f.api.playbackList();
    assert.equal(delivered.items.length, 4096);
    assert.equal(delivered.items[4095].url, value.items[4095].url);
    assert.deepEqual(f.saved, f.before);
    f.api.cancelDroppedVideo();
    assert.deepEqual(f.releases, ["playlist"]);
});

test("real mixed playlist builder hands one temporary source to the real Local plugin", async () => {
    const f = fixture();
    const videos = load("src/offline/dropped-video.ts");
    const playlists = load(
        "src/offline/dropped-playlist.ts",
        { "./dropped-video": videos },
        { TextDecoder }
    );
    const revoked = [];
    const host = {
        File,
        TextDecoder,
        document: { createElement: () => ({ canPlayType: () => "probably" }) },
        URL: {
            createObjectURL: () => "blob:mixed-local",
            revokeObjectURL: (url) => revoked.push(url),
        },
    };
    const playlist = new File(
        [
            "#EXTM3U\n#EXTINF:-1,Remote channel\nhttps://media.example.test/live.m3u8\n#EXTINF:90,Local film\nclip.mp4\n",
        ],
        "mixed.m3u"
    );
    const lease = await playlists.createDroppedPlaylists(host, [
        playlist,
        new File(["video"], "clip.mp4", { type: "video/mp4" }),
    ]);
    f.api.playDroppedVideo(lease.catalog, () => true, lease.release);
    const delivered = await f.api.playbackList();
    const { fixture: coreFixture } = require("./test_tauri_auto_playback.cjs");
    for (const [index, expectedHls] of [
        [0, 1],
        [1, 0],
    ]) {
        const core = coreFixture({ nativeHls: false, savedPlayerMode: 1 });
        core.w.__ottLocalMedia = f.api;
        core.w.startCoreEngine(delivered.items[index].url);
        await Promise.resolve();
        await Promise.resolve();
        assert.equal(
            core.players.length,
            expectedHls,
            "real browser playlist uses each item's transport"
        );
    }
    assert.equal(delivered.sources.length, 1);
    assert.equal(delivered.items.length, 2);
    assert.deepEqual(
        Array.from(delivered.items, (item) => item.name),
        ["Remote channel", "Local film"]
    );
    assert.equal(
        delivered.items[0].url,
        "https://media.example.test/live.m3u8"
    );
    assert.equal(delivered.items[1].url, "blob:mixed-local");
    assert.ok(
        delivered.items.every(
            (item) => item.sourceId === delivered.sources[0].id
        )
    );
    assert.deepEqual(f.saved, f.before);
    f.api.cancelDroppedVideo();
    assert.deepEqual(revoked, ["blob:mixed-local"]);
});

test("owned Local catalog membership distinguishes adaptive transport from direct files", async () => {
    const cap = "a".repeat(32);
    for (const [url, mime, expected, playback] of [
        [
            "https://media.invalid/live.m3u8?token=fixture",
            "video/mp4",
            "stream",
        ],
        ["https://media.invalid/dash.mpd", "", "stream"],
        [
            "https://media.invalid/live",
            "application/vnd.apple.mpegurl",
            "stream",
        ],
        ["https://media.invalid/live", "application/dash+xml", "stream"],
        ["https://media.invalid/live.ts", "video/mp2t", "stream"],
        ["https://media.invalid/unknown", "", "stream"],
        ["https://media.invalid/film.mp4", "video/mp4", "file"],
        ["blob:fixture/local", "video/mp4", "file"],
        [`http://127.0.0.1:43210/offline-media/${cap}/one`, "", "file"],
        [
            `http://127.0.0.1:43210/offline-media/${cap}/wrong-item`,
            "",
            "stream",
        ],
        ["http://127.0.0.1:43210/one", "", "stream"],
        [`https://cdn.invalid/offline-media/${cap}/bad%ZZ`, "", "stream"],
        [
            `http://127.0.0.1:43210/offline-media/${cap}/one`,
            "video/mp4",
            "stream",
            { mode: "compatibility" },
        ],
    ]) {
        const f = fixture();
        const input = catalog();
        Object.assign(input.items[0], {
            url,
            mime,
            ...(playback ? { playback } : {}),
        });
        f.api.playDroppedVideo(
            input,
            () => true,
            () => {}
        );
        await f.api.playbackList();
        assert.equal(
            f.api.isPlaybackUrl(url),
            true,
            "membership remains independent of engine hint"
        );
        assert.equal(f.api.playbackTransport(url), expected, url);
        assert.equal(
            f.api.playbackTransport("https://unowned.invalid/film.mp4"),
            undefined
        );
        assert.deepEqual(f.saved, f.before);
    }
});

test("actual Local catalogs reach main and PiP HLS selection while direct files retain HTML5", async () => {
    const { fixture: coreFixture } = require("./test_tauri_auto_playback.cjs");
    for (const nativeShaped of [false, true]) {
        for (const [url, mime, adaptive] of [
            [
                "https://media.invalid/live.m3u8",
                "application/vnd.apple.mpegurl",
                true,
            ],
            ["https://media.invalid/film.mp4", "video/mp4", false],
            ["blob:fixture/film", "video/mp4", false],
            [
                `http://127.0.0.1:43210/offline-media/${"a".repeat(32)}/one`,
                "video/mp4",
                false,
            ],
        ]) {
            const f = fixture();
            const input = catalog();
            Object.assign(input.items[0], { url, mime });
            if (nativeShaped)
                Object.assign(input.items[0], {
                    source: "linked",
                    size: 0,
                    relativePath: null,
                });
            f.api.playDroppedVideo(
                input,
                () => true,
                () => {}
            );
            await f.api.playbackList();
            assert.equal(f.api.isPlaybackUrl(url), true);
            for (const pip of [false, true]) {
                const core = coreFixture({
                    nativeHls: false,
                    savedPlayerMode: 1,
                });
                core.w.__ottLocalMedia = f.api;
                if (pip) core.w.startCorePipEngine(url);
                else core.w.startCoreEngine(url);
                await Promise.resolve();
                await Promise.resolve();
                assert.equal(
                    core.players.length,
                    adaptive ? 1 : 0,
                    `adaptive=${adaptive}, native=${nativeShaped}, pip=${pip}`
                );
                if (!adaptive)
                    assert.equal(
                        (pip ? core.w.videoPip : core.w.video).src,
                        url
                    );
            }
            const shaka = coreFixture({ nativeHls: false, savedPlayerMode: 2 });
            shaka.w.__ottLocalMedia = f.api;
            shaka.w.startCoreEngine(url);
            await Promise.resolve();
            await Promise.resolve();
            assert.equal(
                shaka.shakaPlayers.length,
                adaptive ? 1 : 0,
                "adaptive entries also retain the selected Shaka engine"
            );
            if (adaptive) {
                for (const mode of [1, 2]) {
                    const preview = coreFixture({
                        nativeHls: false,
                        savedPlayerMode: mode,
                    });
                    preview.w.__ottLocalMedia = f.api;
                    preview.w.startCoreEngine(
                        url,
                        undefined,
                        undefined,
                        false,
                        url,
                        true
                    );
                    await Promise.resolve();
                    await Promise.resolve();
                    assert.equal(
                        mode === 1
                            ? preview.players.length
                            : preview.shakaPlayers.length,
                        1,
                        "temporary preview ownership must not force adaptive media through HTML5"
                    );
                }
            }
            assert.deepEqual(f.saved, f.before);
        }
    }
});
