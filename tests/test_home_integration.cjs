"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const os = require("node:os");
const { createRequire } = require("node:module");
const root = path.resolve(__dirname, "..");
const dependencies = createRequire(
    path.join(process.env.OTT_HOME_TEST_REPO || root, "package.json")
);
const ts = dependencies("typescript");
const acorn = dependencies("acorn");
const {
    stageHomeIntegration: stage,
    FILES,
    OUTPUT_FILES,
    generateHostScript,
} = require("../scripts/home-integration-assets.cjs");
function stageHomeIntegration(source, target) {
    return stage(source, target, ts);
}
function addRuntime(source) {
    const file = path.join(source, "src/plugins/home-integration-runtime.ts");
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.copyFileSync(
        path.join(root, "src/plugins/home-integration-runtime.ts"),
        file
    );
}
const HOST = generateHostScript(root, ts).toString("utf8");
acorn.parse(HOST, { ecmaVersion: 5 });
function load() {
    const source = fs.readFileSync(
        path.join(root, "src/plugins/home-integration.ts"),
        "utf8"
    );
    const result = ts.transpileModule(source, {
        compilerOptions: {
            target: ts.ScriptTarget.ES5,
            module: ts.ModuleKind.CommonJS,
        },
        reportDiagnostics: true,
    });
    assert.equal((result.diagnostics || []).length, 0);
    acorn.parse(result.outputText, { ecmaVersion: 5 });
    const exports = {};
    vm.runInNewContext(result.outputText, { exports });
    return exports;
}
function fixture(options = {}) {
    const docEvents = {},
        events = {},
        timers = [],
        timeouts = [],
        owners = [],
        scripts = [];
    const d = {
        hidden: false,
        activeElement: null,
        addEventListener: (n, f) => (docEvents[n] = f),
        removeEventListener: (n) => delete docEvents[n],
    };
    let aborted = 0,
        saved = 0,
        restored = 0;
    class Node {
        constructor(tag) {
            this.tagName = tag;
            this.style = {};
            this.childNodes = [];
            this.parentNode = null;
            this.attrs = {};
        }
        appendChild(child) {
            this.childNodes.push(child);
            child.parentNode = this;
        }
        removeChild(child) {
            this.childNodes.splice(this.childNodes.indexOf(child), 1);
            child.parentNode = null;
        }
        setAttribute(k, v) {
            this.attrs[k] = v;
        }
        set textContent(value) {
            this.childNodes.forEach((c) => (c.parentNode = null));
            this.childNodes = [];
            this.text = value;
        }
        focus() {
            d.activeElement = this;
        }
    }
    const panel = new Node("div");
    panel.style.display = "none";
    d.getElementById = (id) => (id === "listAbout" ? panel : null);
    d.createElement = (tag) => {
        const node = new Node(tag);
        if (tag === "iframe")
            node.contentWindow = {
                location: {
                    protocol: "https:",
                    host: "player.example",
                    pathname: "/dist/home/index.html",
                },
                document: {
                    createEvent: () => ({
                        initEvent(n) {
                            this.type = n;
                        },
                    }),
                },
                dispatchEvent: (e) => {
                    if (e.type === "pagehide") aborted++;
                },
            };
        return node;
    };
    const previous = function previous() {};
    var foreground = true;
    const owner = {
        kind: "about",
        foreground: () => foreground,
        own(fn) {
            owners.push(fn);
            let done = false;
            return () => {
                if (!done) {
                    done = true;
                    fn();
                }
            };
        },
    };
    const w = {
        document: d,
        location: { protocol: "https:", host: "player.example" },
        aboutKeyHandler: previous,
        keys: { RETURN: 8, EXIT: 81, ENTER: 13, DOWN: 40, RIGHT: 39 },
        saveListPanelState() {
            saved++;
        },
        restoreListPanelState() {
            restored++;
        },
        addEventListener: (n, f) => (events[n] = f),
        removeEventListener: (n) => delete events[n],
        setInterval(fn) {
            timers.push(fn);
            return timers.length;
        },
        clearInterval(id) {
            timers[id - 1] = null;
        },
        setTimeout(fn) {
            timeouts.push(fn);
            return timeouts.length;
        },
        clearTimeout(id) {
            timeouts[id - 1] = null;
        },
        __ottClassicScreenPort: { screens: { current: () => owner } },
    };
    d.head = new Node("head");
    const append = d.head.appendChild.bind(d.head);
    function complete(script = scripts.at(-1)) {
        vm.runInNewContext(HOST, { window: w });
        if (script.onload) script.onload();
    }
    d.head.appendChild = (script) => {
        append(script);
        scripts.push(script);
        if (!options.deferScript) complete(script);
    };
    return {
        w,
        d,
        panel,
        owners,
        timers,
        timeouts,
        scripts,
        complete,
        docEvents,
        events,
        previous,
        retireOwner: () => {
            foreground = false;
            owners.slice().forEach((fn) => fn());
        },
        counts: () => ({ aborted, saved, restored }),
        frame: () => panel.childNodes[0]?.childNodes[1],
    };
}
test("lazy ES5 adapter opens one fixed local iframe only and returns via child close", () => {
    const api = load(),
        f = fixture(),
        home = api.installHomeIntegration(f.w);
    assert.equal(f.panel.childNodes.length, 0);
    assert.equal(f.timers.length, 0);
    assert.equal(home.open(), true);
    const frame = f.frame();
    assert.equal(frame.src, "/dist/home/index.html");
    assert.equal(home.open(), true);
    assert.equal(f.counts().saved, 1);
    frame.onload();
    let accepted;
    frame.contentWindow.OTTHomeHost.close((ok) => (accepted = ok));
    assert.equal(accepted, true);
    assert.deepEqual(f.counts(), { aborted: 1, saved: 1, restored: 1 });
    assert.equal(frame.src, "about:blank");
    assert.equal(f.w.aboutKeyHandler, f.previous);
    assert.ok(f.timers.every((t) => t === null));
    assert.equal(f.panel.childNodes.length, 0);
});
test("host Back and remote navigation preserve an accessible exit", () => {
    const api = load(),
        f = fixture();
    api.showHomeIntegration(f.w);
    const frame = f.frame();
    f.w.aboutKeyHandler(f.w.keys.DOWN);
    assert.equal(f.d.activeElement, frame);
    f.w.aboutKeyHandler(f.w.keys.RETURN);
    assert.equal(f.counts().restored, 1);
    api.showHomeIntegration(f.w);
    f.panel.childNodes[0].childNodes[0].onclick();
    assert.equal(f.counts().restored, 2);
});
test("retiring owner or replacing handler aborts child work without restoring foreign UI", () => {
    for (const retire of [
        (f) => f.owners.at(-1)(),
        (f) => {
            f.w.aboutKeyHandler = function foreign() {};
            f.timers.filter(Boolean).at(-1)();
        },
        (f) => {
            f.panel.textContent = "new owner";
            f.timers.filter(Boolean).at(-1)();
        },
    ]) {
        const f = fixture();
        load().showHomeIntegration(f.w);
        const frame = f.frame();
        retire(f);
        assert.equal(f.counts().aborted, 1);
        assert.equal(f.counts().restored, 0);
        assert.equal(frame.src, "about:blank");
        f.owners.at(-1)();
        assert.equal(f.counts().aborted, 1);
    }
});
test("kiosk and standby block entry and active owner is closed on hidden or kiosk transition", () => {
    for (const guard of [
        { __ottKiosk: { enabled: () => true } },
        { __ottKiosk: { locked: () => true } },
        { __ottStandby: { blocked: () => true } },
    ]) {
        const f = fixture();
        Object.assign(f.w, guard);
        assert.equal(load().showHomeIntegration(f.w), false);
        assert.equal(f.counts().saved, 0);
    }
    for (const transition of [
        (f) => {
            f.d.hidden = true;
            f.docEvents.visibilitychange();
        },
        (f) => {
            f.w.__ottKiosk = { enabled: () => true };
            f.timers.filter(Boolean).at(-1)();
        },
    ]) {
        const f = fixture();
        load().showHomeIntegration(f.w);
        transition(f);
        assert.equal(f.counts().aborted, 1);
        assert.equal(f.counts().restored, 1);
    }
});
test("navigated frame is rejected; stale frame callbacks cannot close a replacement", () => {
    const f = fixture(),
        home = load().installHomeIntegration(f.w);
    home.open();
    const first = f.frame();
    first.onload();
    const oldClose = first.contentWindow.OTTHomeHost.close;
    home.close();
    home.open();
    const frame = f.frame();
    let ok;
    oldClose((value) => (ok = value));
    assert.equal(ok, false);
    assert.equal(f.counts().restored, 1);
    frame.contentWindow.location.host = "evil.example";
    frame.onload();
    assert.equal(f.counts().restored, 2);
    home.open();
    const foreign = function foreign() {};
    f.w.aboutKeyHandler = foreign;
    assert.equal(
        home.open(),
        true,
        "reopening retires stale session before polling"
    );
    home.close();
    assert.equal(f.w.aboutKeyHandler, foreign);
});
test("asset staging includes five public files plus generated ES5 host and integrity manifest", () => {
    const dir = fs.mkdtempSync(
        path.join(fs.realpathSync(os.tmpdir()), "ott-home-stage-")
    );
    try {
        const source = path.join(dir, "source"),
            target = path.join(dir, "target");
        fs.mkdirSync(path.join(source, "integrations", "home"), {
            recursive: true,
        });
        for (const name of FILES)
            fs.writeFileSync(
                path.join(source, "integrations", "home", name),
                "public " + name
            );
        fs.writeFileSync(
            path.join(source, "integrations", "home", "config.private.json"),
            "SECRET"
        );
        addRuntime(source);
        fs.writeFileSync(
            path.join(source, "integrations/home/host.js"),
            "SECRET_HOST_OVERRIDE"
        );
        const manifest = stageHomeIntegration(source, target);
        assert.equal(manifest.files.length, 6);
        const generated = fs.readFileSync(
            path.join(target, "home/host.js"),
            "utf8"
        );
        assert.ok(!generated.includes("SECRET_HOST_OVERRIDE"));
        assert.ok(generated.includes("__ottHomeHostInstall"));
        acorn.parse(generated, { ecmaVersion: 5 });
        for (const name of [...OUTPUT_FILES, "assets.json"])
            assert.deepEqual(
                fs.readFileSync(path.join(target, "home", name)),
                fs.readFileSync(path.join(target, "dist", "home", name))
            );
        assert.deepEqual(
            fs.readdirSync(path.join(target, "dist", "home")).sort(),
            [...OUTPUT_FILES, "assets.json"].sort()
        );
        fs.unlinkSync(path.join(source, "integrations", "home", "app.js"));
        fs.symlinkSync(
            path.join(source, "integrations", "home", "config.private.json"),
            path.join(source, "integrations", "home", "app.js")
        );
        assert.throws(
            () => stageHomeIntegration(source, target),
            /Invalid public/
        );
    } finally {
        fs.rmSync(dir, { recursive: true, force: true });
    }
});
test("asset staging rejects dangling output symlinks without writing their target", () => {
    const dir = fs.mkdtempSync(
        path.join(fs.realpathSync(os.tmpdir()), "ott-home-link-")
    );
    try {
        const source = path.join(dir, "source"),
            target = path.join(dir, "target");
        fs.mkdirSync(path.join(source, "integrations", "home"), {
            recursive: true,
        });
        fs.mkdirSync(path.join(target, "dist", "home"), { recursive: true });
        for (const name of FILES)
            fs.writeFileSync(
                path.join(source, "integrations", "home", name),
                "public"
            );
        addRuntime(source);
        const victim = path.join(dir, "must-not-exist");
        fs.symlinkSync(victim, path.join(target, "dist", "home", "app.js"));
        assert.throws(
            () => stageHomeIntegration(source, target),
            /Unsafe Home/
        );
        assert.equal(fs.existsSync(victim), false);
    } finally {
        fs.rmSync(dir, { recursive: true, force: true });
    }
});
test("canonical index and all export stages are wired without private bridge imports", () => {
    const index = fs.readFileSync(path.join(root, "src/index.ts"), "utf8"),
        vite = fs.readFileSync(path.join(root, "vite.config.ts"), "utf8");
    assert.match(index, /showHomeIntegration\(window\)/);
    assert.match(index, /name: "Home"/);
    assert.match(
        fs.readFileSync(path.join(root, "scripts/classic-bundle.cjs"), "utf8"),
        /build\/plugins\/home-integration.js/
    );
    assert.match(vite, /stageHomeIntegration\(__dirname, outDir\)/);
    assert.match(vite, /stageHomeIntegration\(srcRoot, stageDir\)/);
    assert.match(vite, /copyRuntimeAssets\(outDir, mobileDir\)/);
});
function assetFixture(run) {
    const root = fs.mkdtempSync(
        path.join(fs.realpathSync(os.tmpdir()), "ott-home-race-")
    );
    try {
        const source = path.join(root, "source"),
            target = path.join(root, "target"),
            home = path.join(source, "integrations", "home");
        fs.mkdirSync(home, { recursive: true });
        for (const name of FILES)
            fs.writeFileSync(path.join(home, name), "public " + name);
        addRuntime(source);
        const secret = path.join(root, "private.json"),
            sentinel = "PRIVATE_SENTINEL_NOT_REAL_SECRET";
        fs.writeFileSync(secret, sentinel);
        run({ root, source, target, home, secret, sentinel });
    } finally {
        fs.rmSync(root, { recursive: true, force: true });
    }
}
test("source ancestors, leaf nonregular files and hardlinks cannot export private bytes", () => {
    for (const kind of ["ancestor", "home", "leaf", "directory", "hardlink"])
        assetFixture((f) => {
            if (kind === "ancestor") {
                fs.renameSync(
                    path.join(f.source, "integrations"),
                    path.join(f.source, "original")
                );
                fs.symlinkSync(
                    path.join(f.source, "original"),
                    path.join(f.source, "integrations")
                );
            } else if (kind === "home") {
                fs.renameSync(f.home, f.home + "-original");
                fs.symlinkSync(f.home + "-original", f.home);
            } else {
                fs.unlinkSync(path.join(f.home, "core.js"));
                if (kind === "leaf")
                    fs.symlinkSync(f.secret, path.join(f.home, "core.js"));
                else if (kind === "directory")
                    fs.mkdirSync(path.join(f.home, "core.js"));
                else fs.linkSync(f.secret, path.join(f.home, "core.js"));
            }
            assert.throws(() => stageHomeIntegration(f.source, f.target));
            assert.equal(fs.existsSync(f.target), false);
            assert.equal(fs.readFileSync(f.secret, "utf8"), f.sentinel);
        });
});
test("source leaf and parent swaps before open or after fd admission never export private bytes", () => {
    for (const phase of [
        "leaf-open",
        "parent-open",
        "leaf-read",
        "parent-read",
    ])
        assetFixture((f) => {
            const originalOpen = fs.openSync,
                originalRead = fs.readSync,
                leaf = path.join(f.home, "core.js");
            let targetFd = null,
                swapped = false;
            const privateDir = path.join(f.root, "private");
            fs.mkdirSync(privateDir);
            for (const name of FILES)
                fs.writeFileSync(path.join(privateDir, name), f.sentinel);
            function swap() {
                if (swapped) return;
                swapped = true;
                if (phase.indexOf("parent") === 0) {
                    fs.renameSync(f.home, f.home + "-original");
                    fs.symlinkSync(privateDir, f.home);
                } else {
                    fs.unlinkSync(leaf);
                    fs.symlinkSync(f.secret, leaf);
                }
            }
            fs.openSync = function (file, ...args) {
                if (file === leaf && phase.endsWith("open")) swap();
                const fd = originalOpen.call(fs, file, ...args);
                if (file === leaf) targetFd = fd;
                return fd;
            };
            fs.readSync = function (fd, ...args) {
                if (fd === targetFd && phase.endsWith("read")) swap();
                return originalRead.call(fs, fd, ...args);
            };
            try {
                assert.throws(() => stageHomeIntegration(f.source, f.target));
                assert.equal(swapped, true);
                assert.equal(fs.existsSync(f.target), false);
            } finally {
                fs.openSync = originalOpen;
                fs.readSync = originalRead;
            }
            assert.equal(fs.readFileSync(f.secret, "utf8"), f.sentinel);
        });
});
test("incremental staging rejects output leaf/parent swaps and hardlinks without altering private bytes", () => {
    for (const phase of [
        "leaf-open",
        "parent-open",
        "leaf-write",
        "parent-write",
        "hardlink",
    ])
        assetFixture((f) => {
            stageHomeIntegration(f.source, f.target);
            const output = path.join(f.target, "home"),
                leaf = path.join(output, "core.js"),
                privateDir = path.join(f.root, "private");
            fs.mkdirSync(privateDir);
            for (const name of [...OUTPUT_FILES, "assets.json"])
                fs.writeFileSync(path.join(privateDir, name), f.sentinel);
            const originalOpen = fs.openSync,
                originalWrite = fs.writeFileSync;
            let targetFd = null,
                swapped = false;
            function swap() {
                if (swapped) return;
                swapped = true;
                if (phase.indexOf("parent") === 0) {
                    fs.renameSync(output, output + "-original");
                    fs.symlinkSync(privateDir, output);
                } else {
                    fs.unlinkSync(leaf);
                    fs.symlinkSync(f.secret, leaf);
                }
            }
            if (phase === "hardlink") {
                fs.unlinkSync(leaf);
                fs.linkSync(f.secret, leaf);
            }
            fs.openSync = function (file, ...args) {
                if (file === leaf && phase.endsWith("open")) swap();
                const fd = originalOpen.call(fs, file, ...args);
                if (file === leaf) targetFd = fd;
                return fd;
            };
            fs.writeFileSync = function (file, ...args) {
                if (file === targetFd && phase.endsWith("write")) swap();
                return originalWrite.call(fs, file, ...args);
            };
            try {
                assert.throws(() => stageHomeIntegration(f.source, f.target));
                if (phase !== "hardlink") assert.equal(swapped, true);
            } finally {
                fs.openSync = originalOpen;
                fs.writeFileSync = originalWrite;
            }
            assert.equal(fs.readFileSync(f.secret, "utf8"), f.sentinel);
            for (const name of [...OUTPUT_FILES, "assets.json"])
                assert.equal(
                    fs.readFileSync(path.join(privateDir, name), "utf8"),
                    f.sentinel
                );
        });
});
test("unchanged regular incremental outputs remain supported and match public input digests", () =>
    assetFixture((f) => {
        stageHomeIntegration(f.source, f.target);
        fs.writeFileSync(path.join(f.home, "core.js"), "updated public source");
        const manifest = stageHomeIntegration(f.source, f.target);
        assert.equal(manifest.files.length, 6);
        for (const name of FILES)
            for (const output of ["home", path.join("dist", "home")])
                assert.deepEqual(
                    fs.readFileSync(path.join(f.target, output, name)),
                    fs.readFileSync(path.join(f.home, name))
                );
    }));

test("pending host script is single-flight and retired Settings cannot open after completion", () => {
    const f = fixture({ deferScript: true }),
        home = load().installHomeIntegration(f.w);
    assert.equal(home.open(), true);
    assert.equal(home.open(), true);
    assert.equal(f.scripts.length, 1);
    assert.equal(f.scripts[0].src, "/dist/home/host.js");
    assert.equal(f.panel.childNodes.length, 0);
    const late = f.scripts[0].onload;
    f.retireOwner();
    f.complete();
    late();
    assert.equal(f.panel.childNodes.length, 0);
    assert.equal(f.counts().saved, 0);
    assert.ok(f.timers.every((t) => t === null));
    assert.ok(f.timeouts.every((t) => t === null));
    assert.equal(home.open(), false);
});
test("pending host script cancels on lifecycle or kiosk and only explicit clicks retry failures", () => {
    for (const stop of [
        (f) => {
            f.d.hidden = true;
            f.docEvents.visibilitychange();
        },
        (f) => f.events.pagehide(),
        (f) => {
            f.w.__ottKiosk = { enabled: () => true };
            f.timers.filter(Boolean).at(-1)();
        },
        (f) => {
            f.w.__ottStandby = { blocked: () => true };
            f.timers.filter(Boolean).at(-1)();
        },
    ]) {
        const f = fixture({ deferScript: true });
        load().showHomeIntegration(f.w);
        const late = f.scripts[0].onload;
        stop(f);
        f.complete();
        late();
        assert.equal(f.counts().saved, 0);
        assert.equal(f.panel.childNodes.length, 0);
    }
    for (const fail of [
        (f) => f.scripts[0].onerror(),
        (f) => f.timeouts.filter(Boolean).at(-1)(),
    ]) {
        const f = fixture({ deferScript: true }),
            home = load().installHomeIntegration(f.w);
        home.open();
        const late = f.scripts[0].onload;
        fail(f);
        assert.equal(f.scripts.length, 1);
        assert.equal(f.counts().saved, 0);
        home.open();
        assert.equal(f.scripts.length, 2, "a later explicit click may retry");
        f.complete(f.scripts[1]);
        assert.equal(f.counts().saved, 1);
        late();
        assert.equal(f.counts().saved, 1);
        home.close();
        assert.equal(f.counts().restored, 1);
    }
});
test("generated ES5 host only installs a factory; fixed runtime source admission stays closed", () => {
    const f = fixture({ deferScript: true });
    vm.runInNewContext(HOST, { window: f.w });
    assert.equal(typeof f.w.__ottHomeHostInstall, "function");
    assert.equal(f.counts().saved, 0);
    assert.equal(f.panel.childNodes.length, 0);
    assert.equal(f.timers.length, 0);
    const home = load().installHomeIntegration(f.w);
    home.open();
    assert.equal(f.scripts.length, 0);
    home.close();
    for (const kind of ["symlink", "parent", "import"])
        assetFixture((a) => {
            const runtime = path.join(
                a.source,
                "src/plugins/home-integration-runtime.ts"
            );
            if (kind === "symlink") {
                fs.unlinkSync(runtime);
                fs.symlinkSync(a.secret, runtime);
            } else if (kind === "parent") {
                const dir = path.dirname(runtime);
                fs.renameSync(dir, dir + "-original");
                fs.symlinkSync(dir + "-original", dir);
            } else
                fs.writeFileSync(
                    runtime,
                    'import secret from "private-config"; export function installHomeIntegrationRuntime(){ return secret; }'
                );
            assert.throws(() => stageHomeIntegration(a.source, a.target));
            assert.equal(fs.existsSync(a.target), false);
        });
});
