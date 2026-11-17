const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const diagnostics = require("../scripts/firefox-crash-diagnostics.cjs");
const enabled = { CI_FIREFOX_CRASH_DIAGNOSTICS: "1" };
function posixTest(name, fn) {
    test(
        name,
        {
            skip:
                process.platform === "win32"
                    ? "Requires POSIX permission ownership; collector is unsupported on Windows"
                    : false,
        },
        fn
    );
}
function dump() {
    const bytes = Buffer.alloc(512);
    bytes.writeUInt32LE(0x504d444d, 0);
    bytes.writeUInt32LE(2, 8);
    bytes.writeUInt32LE(32, 12);
    bytes.writeUInt32LE(6, 32);
    bytes.writeUInt32LE(168, 36);
    bytes.writeUInt32LE(64, 40);
    bytes.writeUInt32LE(4, 44);
    bytes.writeUInt32LE(112, 48);
    bytes.writeUInt32LE(232, 52);
    bytes.writeUInt32LE(11, 72);
    bytes.writeBigUInt64LE(0x1012n, 88);
    bytes.writeUInt32LE(1, 232);
    bytes.writeBigUInt64LE(0x1000n, 236);
    bytes.writeUInt32LE(4096, 244);
    bytes.writeUInt32LE(12, 312);
    bytes.writeUInt32LE(344, 316);
    bytes.writeUInt32LE(0x4270454c, 344);
    Buffer.from("0102030405060708", "hex").copy(bytes, 348);
    bytes.write("SECRET /home/private/token?password=hidden", 380);
    return bytes;
}
function scope(t) {
    const owner = diagnostics.create("firefox", enabled, "linux");
    const output = fs.mkdtempSync(
        path.join(os.tmpdir(), "ott-crash-test-report-")
    );
    t.after(() => {
        fs.rmSync(owner.root, { force: true, recursive: true });
        fs.rmSync(output, { force: true, recursive: true });
    });
    return { output, owner };
}
test("strict opt-in and Firefox-only launch; inherited disable is removed", () => {
    assert.equal(diagnostics.create("firefox", {}), null);
    assert.equal(diagnostics.create("chromium", enabled), null);
    assert.equal(diagnostics.session({}), null);
    assert.equal(diagnostics.launchEnvironment(null), undefined);
});
posixTest(
    "launch environment isolates storage without full dumps or submissions",
    (t) => {
        const { owner } = scope(t);
        const env = diagnostics.childEnvironment(owner, {
            ...enabled,
            MINIDUMP_SAVE_PATH: "/private",
            MOZ_CRASHREPORTER_DISABLE: "0",
            MOZ_CRASHREPORTER_FULLDUMP: "1",
            MOZ_CRASHREPORTER_URL: "https://private.invalid/",
            ordinary: "kept",
        });
        assert.equal(env.MOZ_CRASHREPORTER_DISABLE, undefined);
        assert.equal(env.MOZ_CRASHREPORTER_FULLDUMP, undefined);
        assert.equal(env.MOZ_CRASHREPORTER_URL, undefined);
        assert.equal(env.MINIDUMP_SAVE_PATH, undefined);
        assert.equal(env.MOZ_CRASHREPORTER, "1");
        assert.equal(env.MOZ_CRASHREPORTER_NO_REPORT, "1");
        assert.equal(env.TMPDIR, path.join(owner.root, "tmp"));
        assert.equal(
            env.MOZ_CRASHREPORTER_DATA_DIRECTORY,
            path.join(owner.root, "data")
        );
        assert.equal(env.ordinary, "kept");
        assert.deepEqual(diagnostics.session(env, "linux"), owner);
        assert.equal(
            Object.keys(env).some((key) => key.includes("SANDBOX")),
            false
        );
    }
);
test("binary signature never includes memory strings, names or absolute addresses", () => {
    assert.deepEqual(diagnostics.parseDump(dump()), {
        buildId: "0102030405060708",
        exceptionCode: "0xb",
        moduleImageBytes: 4096,
        moduleOffset: "0x12",
        status: "exception",
    });
    assert.doesNotMatch(
        JSON.stringify(diagnostics.parseDump(dump())),
        /SECRET|private|password|1012|1000/
    );
});
test("malformed offsets, stream duplicates, module counts and missing exception", () => {
    for (const mutate of [
        (b) => b.writeUInt32LE(999999, 40),
        (b) => b.writeUInt32LE(6, 44),
        (b) => b.writeUInt32LE(2049, 232),
        (b) => b.writeUInt32LE(129, 8),
        (b) => b.writeUInt32LE(0, 0),
        (b) => b.writeUInt32LE(999999, 316),
    ]) {
        const bytes = dump();
        mutate(bytes);
        assert.throws(() => diagnostics.parseDump(bytes), /invalid-minidump/);
    }
    const bytes = dump();
    bytes.writeUInt32LE(0, 8);
    assert.deepEqual(diagnostics.parseDump(bytes), {
        status: "no-exception-stream",
    });
    assert.throws(
        () => diagnostics.parseDump(Buffer.alloc(31)),
        /invalid-minidump/
    );
    assert.throws(
        () => diagnostics.parseDump(Buffer.alloc(32 * 1024 * 1024 + 1)),
        /invalid-minidump/
    );
});
posixTest(
    "retention survives profile deletion, deduplicates, publishes scalar receipt and removes raw data",
    (t) => {
        const { owner, output } = scope(t);
        const file = path.join(owner.root, "tmp", "profile.dmp");
        fs.writeFileSync(file, dump());
        diagnostics.retain(owner);
        diagnostics.retain(owner);
        fs.unlinkSync(file);
        const report = diagnostics.finish(owner, output);
        assert.equal(report.status, "captured");
        assert.equal(report.dumps.length, 1);
        assert.equal(report.dumps[0].buildId, "0102030405060708");
        assert.equal(fs.existsSync(owner.root), false);
        assert.deepEqual(fs.readdirSync(output), [
            "firefox-crash-summary.json",
        ]);
        assert.doesNotMatch(
            fs.readFileSync(
                path.join(output, "firefox-crash-summary.json"),
                "utf8"
            ),
            /SECRET|private|token|password|ott-firefox-crash/
        );
    }
);
posixTest("no dumps is unavailable, not a fabricated crash signature", (t) => {
    const { owner, output } = scope(t);
    assert.deepEqual(diagnostics.finish(owner, output), {
        dumps: [],
        enabled: true,
        schema: 1,
        status: "unavailable",
    });
    assert.equal(fs.existsSync(owner.root), false);
});
posixTest("symlink dumps are not read or removed outside owned tree", (t) => {
    const { owner, output } = scope(t);
    const outside = path.join(output, "secret.dmp");
    fs.writeFileSync(outside, dump());
    fs.symlinkSync(outside, path.join(owner.root, "tmp", "link.dmp"));
    fs.symlinkSync(output, path.join(owner.root, "tmp", "directory"));
    assert.equal(diagnostics.finish(owner, output).status, "unavailable");
    assert.equal(fs.existsSync(outside), true);
});
posixTest(
    "ownership replacement prevents reads and recursive deletion",
    (t) => {
        const { owner, output } = scope(t);
        fs.writeFileSync(path.join(owner.root, ".owner"), "0".repeat(32));
        assert.throws(
            () =>
                diagnostics.session(
                    {
                        ...enabled,
                        CI_FIREFOX_CRASH_ROOT: owner.root,
                        CI_FIREFOX_CRASH_TOKEN: owner.token,
                    },
                    "linux"
                ),
            /invalid-owner/
        );
        const report = diagnostics.finish(owner, output);
        assert.equal(report.status, "unavailable-or-limit");
        assert.equal(report.cleanup, "ownership-check-failed");
        assert.equal(fs.existsSync(owner.root), true);
    }
);
posixTest(
    "count and file-size bounds fail closed and still clean private files",
    (t) => {
        const { owner, output } = scope(t);
        for (let i = 0; i < 9; i++) {
            const bytes = dump();
            bytes[511] = i;
            fs.writeFileSync(path.join(owner.root, "tmp", `${i}.dmp`), bytes);
        }
        assert.equal(
            diagnostics.finish(owner, output).status,
            "unavailable-or-limit"
        );
        assert.equal(fs.existsSync(owner.root), false);
        const second = scope(t);
        const fd = fs.openSync(
            path.join(second.owner.root, "tmp", "huge.dmp"),
            "w"
        );
        fs.ftruncateSync(fd, 32 * 1024 * 1024 + 1);
        fs.closeSync(fd);
        assert.equal(
            diagnostics.finish(second.owner, second.output).status,
            "unavailable-or-limit"
        );
        assert.equal(fs.existsSync(second.owner.root), false);
    }
);

test("unsupported platforms do not create or validate Linux-owned sessions and publish explicit unavailability", (t) => {
    for (const platform of ["win32", "darwin"]) {
        const env = {
            ...enabled,
            CI_FIREFOX_CRASH_ROOT: "/nonexistent-private-root",
            CI_FIREFOX_CRASH_TOKEN: "not-an-owner",
        };
        assert.equal(diagnostics.create("firefox", env, platform), null);
        assert.equal(diagnostics.session(env, platform), null);
        assert.equal(diagnostics.launchEnvironment(null, env), undefined);
        const child = diagnostics.childEnvironment(null, env);
        assert.equal(child.CI_FIREFOX_CRASH_DIAGNOSTICS, undefined);
        assert.equal(child.CI_FIREFOX_CRASH_ROOT, undefined);
        assert.equal(child.CI_FIREFOX_CRASH_TOKEN, undefined);
        assert.equal(diagnostics.session(child, "linux"), null);
        const output = fs.mkdtempSync(
            path.join(os.tmpdir(), "ott-crash-unsupported-")
        );
        t.after(() => fs.rmSync(output, { force: true, recursive: true }));
        const report = diagnostics.finish(null, output, {
            engine: "firefox",
            env,
            platform,
        });
        assert.deepEqual(report, {
            dumps: [],
            enabled: true,
            schema: 1,
            status: "unsupported-platform",
        });
        assert.doesNotMatch(JSON.stringify(report), /nonexistent|not-an-owner/);
        assert.equal(
            JSON.parse(
                fs.readFileSync(
                    path.join(output, "firefox-crash-summary.json"),
                    "utf8"
                )
            ).status,
            "unsupported-platform"
        );
    }
});

function instructionDump() {
    const original = dump(),
        bytes = Buffer.alloc(1024);
    original.copy(bytes);
    original.copy(bytes, 80, 64, 232);
    original.copy(bytes, 248, 232, 344);
    original.copy(bytes, 360, 344, 356);
    bytes.writeUInt32LE(3, 8);
    bytes.writeUInt32LE(80, 40);
    bytes.writeUInt32LE(248, 52);
    bytes.writeUInt32LE(7, 56);
    bytes.writeUInt32LE(56, 60);
    bytes.writeUInt32LE(600, 64);
    bytes.writeBigUInt64LE(0n, 104); // Null fault address must not hide a valid RIP signature.
    bytes.writeUInt32LE(256, 240);
    bytes.writeUInt32LE(700, 244);
    bytes.writeUInt32LE(12, 328);
    bytes.writeUInt32LE(360, 332);
    bytes.writeUInt16LE(9, 600);
    bytes.writeUInt32LE(0x8201, 620);
    bytes.writeUInt32LE(0x00100001, 748);
    bytes.writeBigUInt64LE(0x1034n, 948);
    bytes.write("SECRET absolute RIP 0x1034 private module /home/private", 962);
    return bytes;
}
test("Linux AMD64 CONTROL context yields only module-relative instruction and build ID despite null fault address", () => {
    const report = diagnostics.parseDump(instructionDump());
    assert.deepEqual(report, {
        exceptionCode: "0xb",
        instructionBuildId: "0102030405060708",
        instructionModuleOffset: "0x34",
        status: "exception",
    });
    assert.doesNotMatch(
        JSON.stringify(report),
        /SECRET|private|1034|absolute|home/
    );
});
test("instruction signature requires Linux AMD64 architecture and valid CONTROL flags, never assumes another layout", () => {
    for (const mutate of [
        (b) => b.writeUInt16LE(0, 600),
        (b) => b.writeUInt16LE(12, 600),
        (b) => b.writeUInt32LE(2, 620),
        (b) => b.writeUInt32LE(0x00100002, 748),
        (b) => b.writeUInt32LE(0x00010001, 748),
        (b) => b.writeBigUInt64LE(0x5000n, 948),
        (b) => b.writeUInt32LE(0, 240),
    ]) {
        const bytes = instructionDump();
        mutate(bytes);
        const report = diagnostics.parseDump(bytes);
        assert.equal(report.instructionModuleOffset, undefined);
        assert.equal(report.instructionBuildId, undefined);
    }
});
test("truncated or malformed context and system-info descriptors fail closed", () => {
    for (const mutate of [
        (b) => b.writeUInt32LE(255, 240),
        (b) => b.writeUInt32LE(999999, 244),
        (b) => b.writeUInt32LE(999999, 240),
        (b) => b.writeUInt32LE(55, 60),
        (b) => b.writeUInt32LE(999999, 64),
        (b) => b.writeUInt32LE(999999, 332),
    ]) {
        const bytes = instructionDump();
        mutate(bytes);
        assert.throws(() => diagnostics.parseDump(bytes), /invalid-minidump/);
    }
    assert.throws(
        () => diagnostics.parseDump(instructionDump().subarray(0, 955)),
        /invalid-minidump/
    );
});

function sparseDump(file, mib, discriminator = 0) {
    const bytes = dump();
    bytes[511] = discriminator;
    const fd = fs.openSync(file, "wx", 0o600);
    try {
        fs.writeSync(fd, bytes);
        fs.ftruncateSync(fd, mib * 1024 * 1024);
    } finally {
        fs.closeSync(fd);
    }
}
posixTest(
    "two distinct 17 MiB crashes and retained duplicates preserve both signatures within unique budget",
    (t) => {
        const { owner, output } = scope(t);
        sparseDump(path.join(owner.root, "tmp", "first.dmp"), 17, 1);
        sparseDump(path.join(owner.root, "tmp", "second.dmp"), 17, 2);
        diagnostics.retain(owner);
        assert.equal(
            fs.readdirSync(path.join(owner.root, "retained")).length,
            2
        );
        const report = diagnostics.finish(owner, output);
        assert.equal(report.status, "captured");
        assert.equal(report.dumps.length, 2);
        assert.equal(new Set(report.dumps.map((item) => item.sha256)).size, 2);
        for (const item of report.dumps) {
            assert.equal(item.bytes, 17 * 1024 * 1024);
            assert.equal(item.exceptionCode, "0xb");
            assert.equal(item.moduleOffset, "0x12");
            assert.equal(item.buildId, "0102030405060708");
        }
        assert.equal(fs.existsSync(owner.root), false);
        assert.deepEqual(fs.readdirSync(output), [
            "firefox-crash-summary.json",
        ]);
    }
);
posixTest(
    "deduplication preserves separate exact 128 MiB read ceiling and strict 64 MiB unique ceiling",
    (t) => {
        for (const [count, expected] of [
            [8, "captured"],
            [9, "unavailable-or-limit"],
        ]) {
            const { owner, output } = scope(t);
            for (let i = 0; i < count; i++)
                sparseDump(path.join(owner.root, "tmp", `${i}.dmp`), 16);
            const report = diagnostics.finish(owner, output);
            assert.equal(
                report.status,
                expected,
                `${count} identical 16 MiB copies`
            );
            assert.equal(report.dumps.length, count === 8 ? 1 : 0);
            assert.equal(fs.existsSync(owner.root), false);
        }
        const { owner, output } = scope(t);
        for (let i = 0; i < 4; i++)
            sparseDump(path.join(owner.root, "tmp", `${i}.dmp`), 17, i);
        assert.equal(
            diagnostics.finish(owner, output).status,
            "unavailable-or-limit",
            "68 MiB distinct data must still exceed unique budget"
        );
        assert.equal(fs.existsSync(owner.root), false);
    }
);

const installVideojsPhase = require("./helpers/firefox-videojs-phase.cjs");
function phaseFixture(mode, noAudio = false) {
    class Buffer {
        updating = false;
        buffered = { length: 0 };
        listeners = new Set();
        appendBuffer(value) {
            this.value = value;
            this.updating = true;
        }
        addEventListener(_, fn) {
            this.listeners.add(fn);
        }
        removeEventListener(_, fn) {
            this.listeners.delete(fn);
        }
        complete() {
            this.updating = false;
            this.buffered.length = 1;
            for (const fn of this.listeners) fn();
        }
    }
    class Source {
        readyState = "open";
        sourceBuffers = [];
        addSourceBuffer() {
            const buffer = new Buffer();
            this.sourceBuffers.push(buffer);
            return buffer;
        }
        removeSourceBuffer(buffer) {
            this.sourceBuffers.splice(this.sourceBuffers.indexOf(buffer), 1);
        }
    }
    class Video {}
    let handle = {};
    let timer;
    const microtasks = [];
    const video = new Video();
    const scope = {
        HTMLVideoElement: Video,
        MediaSource: Source,
        SourceBuffer: Buffer,
        URL: { createObjectURL: () => "blob:owned", revokeObjectURL() {} },
        __ottMedia: { current: () => handle },
        addEventListener() {},
        clearTimeout() {
            timer = undefined;
        },
        console: { log() {} },
        queueMicrotask(fn) {
            microtasks.push(fn);
        },
        removeEventListener() {},
        setTimeout(fn, ms) {
            assert.equal(ms, 8000);
            timer = fn;
            return 1;
        },
        video,
        videojs: {
            getPlayer: () => ({ tech: () => ({ el: () => scope.video }) }),
        },
    };
    const original = [
        Source.prototype.addSourceBuffer,
        Buffer.prototype.appendBuffer,
        scope.URL.createObjectURL,
        scope.URL.revokeObjectURL,
    ];
    installVideojsPhase({ mode }, scope);
    const source = new Source();
    video.currentSrc = scope.URL.createObjectURL(source);
    const audio = noAudio ? null : source.addSourceBuffer("audio/mp4");
    const buffer = source.addSourceBuffer("video/mp4");
    return {
        audio,
        buffer,
        scope,
        source,
        flush() {
            while (microtasks.length) microtasks.shift()();
        },
        retireOwner() {
            handle = {};
        },
        timeout() {
            timer();
        },
        restored() {
            assert.deepEqual(
                [
                    Source.prototype.addSourceBuffer,
                    Buffer.prototype.appendBuffer,
                    scope.URL.createObjectURL,
                    scope.URL.revokeObjectURL,
                ],
                original
            );
            assert.equal(buffer.listeners.size, 0);
            if (audio) assert.equal(audio.listeners.size, 0);
        },
    };
}
test("Video.js phase runs once after native append stack, with owned audio and video", async () => {
    const f = phaseFixture("append-active");
    let calls = 0;
    const done = f.scope.__ottFirefoxVideojsPhase.run(() => {
        assert.equal(f.buffer.updating, true);
        calls++;
    });
    const bytes = new Uint8Array([1, 2]);
    f.buffer.appendBuffer(bytes);
    assert.equal(
        f.buffer.value,
        bytes,
        "Original native append gets identical bytes"
    );
    assert.equal(calls, 0, "Transition must not reenter appendBuffer");
    f.flush();
    await done;
    assert.equal(calls, 1);
    f.buffer.appendBuffer(bytes);
    f.flush();
    assert.equal(calls, 1);
    f.restored();
});
test("Video.js completed phase requires real updateend and nonempty buffered ranges", async () => {
    const f = phaseFixture("append-completed");
    let calls = 0;
    const done = f.scope.__ottFirefoxVideojsPhase.run(() => calls++);
    f.buffer.appendBuffer(new Uint8Array());
    f.flush();
    assert.equal(calls, 0);
    f.buffer.complete();
    assert.equal(calls, 0);
    f.flush();
    await done;
    assert.equal(calls, 1);
    f.restored();
});
for (const kind of [
    "missed",
    "retired",
    "wrapper",
    "no-audio",
    "revoked",
    "foreign-source",
    "foreign-tech",
]) {
    test(`Video.js phase rejects ${kind} without fabricated transition`, async () => {
        const f = phaseFixture("append-active", kind === "no-audio");
        let calls = 0;
        if (kind === "wrapper") f.scope.video = {};
        const done = f.scope.__ottFirefoxVideojsPhase.run(() => calls++);
        const rejected = assert.rejects(done, /Firefox Video.js/);
        f.buffer.appendBuffer(new Uint8Array());
        if (kind === "missed") f.buffer.complete();
        if (kind === "retired") f.retireOwner();
        if (kind === "revoked") f.scope.URL.revokeObjectURL("blob:owned");
        if (kind === "foreign-source")
            f.scope.video.currentSrc = "blob:foreign";
        if (kind === "foreign-tech")
            f.scope.videojs.getPlayer = () => ({
                tech: () => ({ el: () => ({}) }),
            });
        f.flush();
        if (kind !== "wrapper") f.timeout();
        await rejected;
        assert.equal(calls, 0);
        f.scope.__ottFirefoxVideojsPhase.restore();
        f.restored();
    });
}
test("Video.js phase restores hooks after transition error and explicit retirement", async () => {
    const f = phaseFixture("append-active");
    const done = f.scope.__ottFirefoxVideojsPhase.run(() => {
        throw new Error("closed failure");
    });
    const rejected = assert.rejects(done, /closed failure/);
    f.buffer.appendBuffer(new Uint8Array());
    f.flush();
    await rejected;
    f.restored();
    const g = phaseFixture("append-active");
    const stopped = assert.rejects(
        g.scope.__ottFirefoxVideojsPhase.run(() => assert.fail()),
        /retired/
    );
    g.scope.__ottFirefoxVideojsPhase.restore();
    await stopped;
    g.restored();
});

for (const kind of [
    "removed-audio",
    "removed-video",
    "buffered-throws",
    "buffered-throws-observe",
]) {
    test(`Video.js live buffer membership rejects ${kind} without page error`, async () => {
        const f = phaseFixture("append-completed");
        let calls = 0;
        const rejected = assert.rejects(
            f.scope.__ottFirefoxVideojsPhase.run(() => calls++),
            /precondition/
        );
        f.buffer.appendBuffer(new Uint8Array());
        if (kind === "removed-audio") f.source.removeSourceBuffer(f.audio);
        if (kind === "buffered-throws-observe") {
            f.buffer.updating = false;
            Object.defineProperty(f.buffer, "buffered", {
                get() {
                    throw new Error("InvalidStateError");
                },
            });
            assert.doesNotThrow(() => {
                for (const listener of f.buffer.listeners) listener();
            });
        } else assert.doesNotThrow(() => f.buffer.complete());
        // Remove video after observation, before the deferred transition.
        if (kind === "removed-video") f.source.removeSourceBuffer(f.buffer);
        if (kind === "buffered-throws")
            Object.defineProperty(f.buffer, "buffered", {
                get() {
                    throw new Error("InvalidStateError");
                },
            });
        assert.doesNotThrow(() => f.flush());
        f.timeout();
        await rejected;
        assert.equal(calls, 0);
        f.restored();
    });
}

test("ordinary Plex transition remains synchronous with its original four calls", () => {
    const ts = require("typescript");
    const vm = require("node:vm");
    const file = ts.createSourceFile(
        "plex-provider.spec.cjs",
        fs.readFileSync(
            path.join(__dirname, "browser/plex-provider.spec.cjs"),
            "utf8"
        ),
        ts.ScriptTarget.Latest,
        true
    );
    let callback;
    function visit(node, inside = false) {
        inside =
            inside ||
            (ts.isFunctionDeclaration(node) &&
                node.name?.text === "openPlexChannels");
        if (
            inside &&
            ts.isCallExpression(node) &&
            ts.isPropertyAccessExpression(node.expression) &&
            node.expression.name.text === "evaluate"
        ) {
            const candidate = node.arguments[0];
            if (candidate?.getText(file).includes("const transition =")) {
                assert.ok(!callback, "exactly one initial transition callback");
                callback = candidate;
            }
        }
        ts.forEachChild(node, (child) => visit(child, inside));
    }
    visit(file);
    assert.ok(callback);
    const calls = [];
    const steps = [];
    const context = {
        window: {
            stbStop: () => calls.push("stop"),
            closeList: () => calls.push("close"),
            __ottMedia: {
                useSource: (value) => calls.push(["source", value]),
                open: (value) => calls.push(["open", value]),
            },
        },
        console: {
            log: (value) =>
                steps.push(JSON.parse(value.slice(value.indexOf("{"))).step),
        },
    };
    const result = vm.runInNewContext(
        "(" + callback.getText(file) + ")()",
        context
    );
    assert.equal(result, undefined);
    assert.deepEqual(calls, [
        "stop",
        "close",
        ["source", null],
        ["open", null],
    ]);
    assert.deepEqual(steps, [0, 1, 2, 3, 4, 5, 6, 7]);
});

test("phase diagnostics use a separate manual-only pinned caller and closed phase matrix", () => {
    const workflow = fs.readFileSync(
        path.join(__dirname, "../.github/workflows/ci.yml"),
        "utf8"
    );
    const job = workflow.split("\n  firefox-phase-diagnostic:\n")[1];
    assert.ok(job);
    assert.match(
        workflow,
        /options: \[\s*"off",\s*"both"\s*\]\s+default: "off"/
    );
    assert.match(job, /github\.event_name == 'workflow_dispatch'/);
    assert.match(job, /inputs\.firefox-videojs-phase == 'both'/);
    assert.match(job, /needs\.build\.result == 'success'/);
    assert.match(job, /needs: \[\s*build,\s*browser-compatibility\s*\]/);
    assert.match(job, /max-parallel: 1/);
    assert.match(job, /phase: \[\s*append-active,\s*append-completed\s*\]/);
    const rows = JSON.parse(job.match(/browser-matrix: '(\[[^\n]+\])'/)[1]);
    const inventory = require("../.github/browser-compatibility.json");
    const declared = inventory.find(
        (row) => row.id === "ubuntu-latest-firefox-1-62-1"
    );
    assert.ok(declared);
    assert.deepEqual(rows, [{ ...declared, tier: "required" }]);
    assert.match(
        job,
        /browser-evidence-prefix: firefox-phase-\$\{\{ matrix\.phase \}\}/
    );
    assert.doesNotMatch(job, /browser-matrix-file:|browser-release-channel:/);
    const command = job.match(/browser-test-command: ([^\n]+)/)[1];
    assert.equal(
        command,
        "CI_FIREFOX_CRASH_DIAGNOSTICS=1 CI_FIREFOX_VIDEOJS_PHASE=${{ matrix.phase }} node scripts/browser-acceptance.cjs"
    );
    assert.doesNotMatch(command, /inputs\./);
});
