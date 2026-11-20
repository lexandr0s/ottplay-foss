"use strict";
const test = require("node:test"),
    assert = require("node:assert/strict"),
    fs = require("node:fs"),
    path = require("node:path"),
    vm = require("node:vm"),
    { createRequire } = require("node:module");
const root = path.resolve(__dirname, ".."),
    dependencies = createRequire(
        path.join(process.env.OTT_HOME_TEST_REPO || root, "package.json")
    );
const ts = dependencies("typescript");
const source = fs.readFileSync(
    path.join(root, "src/plugins/home-integration-runtime.ts"),
    "utf8"
);
const compiled = ts.transpileModule(source, {
    compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES5,
    },
}).outputText;
function fixture(legacy = false) {
    let sleep = 0,
        forwarded = 0,
        retired;
    function events(object = {}) {
        const listeners = new Map();
        object.addEventListener = (name, fn) => {
            if (!listeners.has(name)) listeners.set(name, new Set());
            listeners.get(name).add(fn);
        };
        object.removeEventListener = (name, fn) =>
            listeners.get(name)?.delete(fn);
        object.emit = (name, event = {}) => {
            event.currentTarget = object;
            for (const fn of [...(listeners.get(name) || [])]) fn(event);
            return event;
        };
        object.listeners = (name) => [...(listeners.get(name) || [])];
        return object;
    }
    function child() {
        return events({
            location: {
                protocol: "https:",
                host: "player.example",
                pathname: "/dist/home/index.html",
            },
            document: {
                createEvent() {
                    return { initEvent() {} };
                },
            },
            dispatchEvent() {},
        });
    }
    let frame;
    function element(tag) {
        const e = {
            style: {},
            parentNode: null,
            children: [],
            appendChild(c) {
                this.children.push(c);
                c.parentNode = this;
            },
            removeChild(c) {
                this.children.splice(this.children.indexOf(c), 1);
                c.parentNode = null;
            },
            setAttribute() {},
            focus() {
                d.activeElement = e;
            },
        };
        if (tag === "iframe") {
            frame = e;
            e.contentWindow = child();
        }
        return e;
    }
    const panel = element("div"),
        d = events({
            hidden: false,
            activeElement: null,
            getElementById: () => panel,
            createElement: element,
        });
    const owner = {
        kind: "about",
        foreground: () => true,
        own(fn) {
            retired = fn;
            return () => {};
        },
    };
    const w = events({
        document: d,
        location: { protocol: "https:", host: "player.example" },
        saveListPanelState() {},
        restoreListPanelState() {},
        setInterval() {
            return 1;
        },
        clearInterval() {},
        aboutKeyHandler() {},
        stbKeyHandler() {
            forwarded++;
        },
        setSleepTimeout() {
            sleep++;
        },
        __ottClassicScreenPort: { screens: { current: () => owner } },
    });
    if (legacy) delete w.__ottClassicScreenPort;
    const module = { exports: {} };
    vm.runInNewContext(compiled, { exports: module.exports, module });
    const api = module.exports.installHomeIntegrationRuntime(w);
    assert.equal(api.open(), true);
    return {
        w,
        d,
        owner,
        api,
        frame,
        child,
        sleep: () => sleep,
        forwarded: () => forwarded,
        retire: () => retired(),
    };
}
test("admitted iframe keyboard renews parent inactivity only, with bounded owner lifecycle", () => {
    const f = fixture(),
        child = f.frame.contentWindow;
    f.frame.onload();
    assert.equal(child.listeners("keydown").length, 1);
    const event = child.emit("keydown", {
        keyCode: 40,
        preventDefault() {
            throw Error("must not consume or forward keyboard");
        },
    });
    assert.equal(event.keyCode, 40);
    assert.equal(f.sleep(), 1);
    assert.equal(f.forwarded(), 0);
    f.d.hidden = true;
    child.emit("keydown");
    assert.equal(f.sleep(), 1);
    f.d.hidden = false;
    f.w.__ottStandby = { blocked: () => true };
    child.emit("keydown");
    assert.equal(f.sleep(), 1);
    delete f.w.__ottStandby;
    const queued = child.listeners("keydown")[0];
    f.api.close();
    assert.equal(child.listeners("keydown").length, 0);
    queued({ currentTarget: child });
    assert.equal(f.sleep(), 1);
});
test("reload, foreign navigation, retirement and queued old-frame keys cannot extend activity", () => {
    const f = fixture(),
        old = f.frame.contentWindow;
    f.frame.onload();
    const queued = old.listeners("keydown")[0];
    const next = f.child();
    f.frame.contentWindow = next;
    f.frame.onload();
    assert.equal(old.listeners("keydown").length, 0);
    assert.equal(next.listeners("keydown").length, 1);
    queued({ currentTarget: old });
    assert.equal(f.sleep(), 0);
    next.emit("keydown");
    assert.equal(f.sleep(), 1);
    next.location.pathname = "/foreign.html";
    next.emit("keydown");
    assert.equal(f.sleep(), 1);
    f.frame.onload();
    assert.equal(next.listeners("keydown").length, 0);
    const g = fixture();
    g.frame.onload();
    const child = g.frame.contentWindow;
    g.w.aboutKeyHandler = () => {};
    child.emit("keydown");
    assert.equal(g.sleep(), 0);
    g.retire();
    assert.equal(child.listeners("keydown").length, 0);
});

test("covered or nonforeground About cannot renew inactivity; legacy hosts retain keyboard activity", () => {
    const f = fixture();
    f.frame.onload();
    const port = f.w.__ottClassicScreenPort,
        captured = port.screens.current();
    port.screens.current = () => ({ kind: "dialog", foreground: () => true });
    f.frame.contentWindow.emit("keydown");
    assert.equal(f.sleep(), 0);
    port.screens.current = () => captured;
    captured.foreground = () => false;
    f.frame.contentWindow.emit("keydown");
    assert.equal(f.sleep(), 0);
    captured.foreground = () => true;
    f.frame.contentWindow.emit("keydown");
    assert.equal(f.sleep(), 1);
    f.api.close();
    const legacy = fixture(true);
    legacy.frame.onload();
    legacy.frame.contentWindow.emit("keydown");
    assert.equal(legacy.sleep(), 1);
    legacy.api.close();
});
