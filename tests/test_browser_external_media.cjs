const assert = require("node:assert/strict");
const fs = require("node:fs");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");
const ts = require("typescript");
const { chromium } = require("@playwright/test");

const source = ts.transpileModule(
    fs
        .readFileSync(
            path.join(__dirname, "../src/offline/browser-local-media.ts"),
            "utf8"
        )
        .replace(/^export /gm, ""),
    {
        compilerOptions: {
            module: ts.ModuleKind.None,
            target: ts.ScriptTarget.ES5,
        },
    }
).outputText;
require("acorn").parse(source, { ecmaVersion: 5 });

const scenarioDeadlineMs = 50000;
const cleanupDeadlineMs = 10000;
const sentinel = "issue1004-native-folder-sentinel";
let phase = "initializing";
let browser;
let browserServer;
let browserServerPromise;
let browserLaunchFailed = false;
let cleanupStarted = false;
let context;
let server;
let ownedRoot;
let scenarioDeadline;
let cleanupDeadline;

function within(promise, milliseconds, label) {
    let timer;
    return Promise.race([
        promise,
        new Promise((_, reject) => {
            timer = setTimeout(
                () => reject(new Error(label)),
                Math.max(1, milliseconds)
            );
        }),
    ]).finally(() => clearTimeout(timer));
}
function remainingScenarioMs() {
    return Math.max(1, scenarioDeadline - Date.now());
}
function safeErrorName(error) {
    const name = error && error.name;
    if (
        /^(AbortError|NotAllowedError|NotFoundError|SecurityError|TimeoutError|TypeError|AssertionError)$/.test(
            name
        )
    )
        return name;
    return "Error";
}
function processExited(child) {
    return child.exitCode !== null || child.signalCode !== null;
}
function waitForProcessExit(child) {
    if (processExited(child)) return Promise.resolve();
    return new Promise((resolve) => child.once("close", resolve));
}
function checkpoint(name) {
    phase = name;
    console.log("[external-media] " + name);
}
function fail(message) {
    throw new Error(message);
}
async function installNativeObservers(page) {
    await page.evaluate(() => {
        const counts = {
            queryCalls: 0,
            queryRejected: 0,
            queryResolved: [],
            requestCalls: 0,
            directoryValuesCalls: 0,
            directoryEntryCalls: 0,
            fileReadCalls: 0,
        };
        const handlePrototype = FileSystemHandle.prototype;
        const nativeQuery = handlePrototype.queryPermission;
        const nativeRequest = handlePrototype.requestPermission;
        const directoryPrototype = FileSystemDirectoryHandle.prototype;
        handlePrototype.queryPermission = function () {
            counts.queryCalls++;
            const result = nativeQuery.apply(this, arguments);
            result.then(
                (state) => counts.queryResolved.push(state),
                () => counts.queryRejected++
            );
            return result;
        };
        handlePrototype.requestPermission = function () {
            counts.requestCalls++;
            return nativeRequest.apply(this, arguments);
        };
        for (const method of ["values", "entries", "keys"]) {
            const nativeMethod = directoryPrototype[method];
            if (typeof nativeMethod !== "function") continue;
            directoryPrototype[method] = function () {
                counts.directoryValuesCalls++;
                return nativeMethod.apply(this, arguments);
            };
        }
        for (const method of ["getFileHandle", "getDirectoryHandle"]) {
            const nativeMethod = directoryPrototype[method];
            if (typeof nativeMethod !== "function") continue;
            directoryPrototype[method] = function () {
                counts.directoryEntryCalls++;
                return nativeMethod.apply(this, arguments);
            };
        }
        const filePrototype = FileSystemFileHandle.prototype;
        const nativeGetFile = filePrototype.getFile;
        filePrototype.getFile = function () {
            counts.fileReadCalls++;
            return nativeGetFile.apply(this, arguments);
        };
        window.__nativeCounts = () => ({
            ...counts,
            queryResolved: counts.queryResolved.slice(),
        });
    });
}
async function installDropBridge(page) {
    await page.evaluate(() => {
        window.__dropHandle = new Promise((resolve, reject) => {
            const dropZone = document.querySelector("#drop");
            for (const name of ["dragenter", "dragover"])
                dropZone.addEventListener(name, (event) =>
                    event.preventDefault()
                );
            dropZone.addEventListener(
                "drop",
                async (event) => {
                    event.preventDefault();
                    try {
                        if (!event.isTrusted)
                            throw new Error("drop was not trusted");
                        const items = [...event.dataTransfer.items].filter(
                            (item) => item.kind === "file"
                        );
                        if (items.length !== 1)
                            throw new Error("unexpected dropped items");
                        const handle = await items[0].getAsFileSystemHandle();
                        if (!handle || handle.kind !== "directory")
                            throw new Error(
                                "drop did not yield a directory handle"
                            );
                        window.__actualDroppedDirectory = handle;
                        resolve({
                            eventTrusted: event.isTrusted,
                            kind: handle.kind,
                            name: handle.name,
                        });
                    } catch (error) {
                        reject(error);
                    }
                },
                { once: true }
            );
        });
    });
}

async function main() {
    const base = fs.mkdtempSync(path.join(os.tmpdir(), "ott-issue1004-"));
    ownedRoot = base;
    const externalDirectory = path.join(base, "owned-external-directory");
    fs.mkdirSync(externalDirectory);
    fs.writeFileSync(
        path.join(externalDirectory, "owned-sentinel.mp4"),
        sentinel,
        { mode: 0o600 }
    );

    server = http.createServer((request, response) => {
        if (request.method !== "GET" || request.url !== "/") {
            response.writeHead(404);
            response.end();
            return;
        }
        response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
        response.end(
            "<!doctype html><meta charset=utf-8><div id=drop " +
                "style='width:320px;height:220px'>drop owned folder</div>" +
                "<script>" +
                source +
                "</script>"
        );
    });

    try {
        scenarioDeadline = Date.now() + scenarioDeadlineMs;
        const scenario = (async () => {
            checkpoint("serve-loopback");
            await new Promise((resolve, reject) => {
                server.once("error", reject);
                server.listen(0, "127.0.0.1", resolve);
            });

            checkpoint("launch-isolated-browser");
            const launchOptions = { headless: true };
            launchOptions.timeout = 12000;
            if (process.env.OTT_ISSUE1004_CHROMIUM_PATH)
                launchOptions.executablePath =
                    process.env.OTT_ISSUE1004_CHROMIUM_PATH;
            browserServerPromise = chromium
                .launchServer(launchOptions)
                .then(async (ownedServer) => {
                    browserServer = ownedServer;
                    if (cleanupStarted) await ownedServer.kill();
                    return ownedServer;
                })
                .catch((error) => {
                    browserLaunchFailed = true;
                    throw error;
                });
            browserServerPromise.catch(() => {});
            browserServer = await within(
                browserServerPromise,
                remainingScenarioMs(),
                "owned browser server startup exceeded the scenario deadline"
            );
            browser = await within(
                chromium.connect(browserServer.wsEndpoint()),
                remainingScenarioMs(),
                "owned browser connection exceeded the scenario deadline"
            );
            console.log(
                "[external-media] chromium-version " + browser.version()
            );
            context = await browser.newContext();

            checkpoint("open-origin-page");
            let page = await context.newPage();
            await page.goto("http://127.0.0.1:" + server.address().port + "/");
            await installNativeObservers(page);
            await installDropBridge(page);

            checkpoint("native-directory-drop");
            const box = await page.locator("#drop").boundingBox();
            if (!box) fail("drop target was unavailable");
            const cdp = await context.newCDPSession(page);
            const data = {
                dragOperationsMask: 1,
                files: [externalDirectory],
                items: [],
            };
            const x = box.x + box.width / 2;
            const y = box.y + box.height / 2;
            await cdp.send("Input.dispatchDragEvent", {
                data,
                type: "dragEnter",
                x,
                y,
            });
            await cdp.send("Input.dispatchDragEvent", {
                data,
                type: "dragOver",
                x,
                y,
            });
            await cdp.send("Input.dispatchDragEvent", {
                data,
                type: "drop",
                x,
                y,
            });
            const dropped = await within(
                page.evaluate(() => window.__dropHandle),
                remainingScenarioMs(),
                "external directory drop exceeded the scenario deadline"
            );
            assert.equal(dropped.eventTrusted, true);
            assert.equal(dropped.kind, "directory");

            checkpoint("import-native-source-and-commit-idb");
            const imported = await within(
                page.evaluate(async () => {
                    const handle = window.__actualDroppedDirectory;
                    if (!handle) throw new Error("native directory is missing");
                    // Picker bridge returns the exact native handle obtained from
                    // the trusted OS-path drop; it does not fake permissions.
                    window.showDirectoryPicker = () => Promise.resolve(handle);
                    const api = installBrowserLocalMedia(window);
                    const catalog = await api.importFolder();
                    const item = catalog.items.find(
                        (value) => value.name === "owned-sentinel.mp4"
                    );
                    const body = item
                        ? await (await fetch(item.url)).text()
                        : "";
                    const db = await new Promise((resolve, reject) => {
                        const request = indexedDB.open("ott-local-media", 1);
                        request.onsuccess = () => resolve(request.result);
                        request.onerror = () => reject(request.error);
                    });
                    const records = await new Promise((resolve, reject) => {
                        const tx = db.transaction("files", "readonly");
                        const request = tx.objectStore("files").getAll();
                        request.onsuccess = () => resolve(request.result);
                        request.onerror = () => reject(request.error);
                        tx.onabort = () => reject(tx.error);
                    });
                    const counts = window.__nativeCounts();
                    return {
                        body,
                        counts,
                        itemCount: catalog.items.length,
                        sourceAccess: catalog.sources[0]?.access || "missing",
                        sourceAvailable: catalog.sources[0]?.available === true,
                        sourceCount: catalog.sources.length,
                        storedHandleCount: records.filter(
                            (record) =>
                                record.handle &&
                                record.handle.kind === "directory"
                        ).length,
                        storedRecordCount: records.length,
                        transactionCommitted: true,
                    };
                }),
                remainingScenarioMs(),
                "initial native import exceeded the scenario deadline"
            );
            assert.equal(imported.sourceCount, 1);
            assert.equal(imported.sourceAccess, "granted");
            assert.equal(imported.sourceAvailable, true);
            assert.equal(imported.itemCount, 1);
            assert.equal(imported.body, sentinel);
            assert.equal(imported.storedRecordCount, 1);
            assert.equal(imported.storedHandleCount, 1);
            assert.equal(imported.transactionCommitted, true);
            assert.equal(imported.counts.queryCalls, 1);
            assert.deepEqual(imported.counts.queryResolved, ["granted"]);
            assert.equal(imported.counts.queryRejected, 0);
            assert.equal(imported.counts.requestCalls, 0);
            assert.ok(imported.counts.directoryValuesCalls > 0);

            checkpoint("close-seed-page");
            await page.close();
            checkpoint("open-same-origin-restored-page");
            page = await context.newPage();
            await page.goto("http://127.0.0.1:" + server.address().port + "/");
            await installNativeObservers(page);
            checkpoint("query-restored-native-handle-once");
            const restored = await within(
                page.evaluate(async () => {
                    const api = installBrowserLocalMedia(window);
                    const catalog = await api.list();
                    const counts = window.__nativeCounts();
                    const source = catalog.sources[0];
                    if (!source) throw new Error("restored source is missing");
                    const item = catalog.items.find(
                        (value) => value.sourceId === source.id
                    );
                    const mayRead = source.access === "granted";
                    const body =
                        mayRead && item && item.url
                            ? await (await fetch(item.url)).text()
                            : null;
                    return {
                        access: source.access,
                        available: source.available,
                        body,
                        bodyRead: mayRead && !!item && !!item.url,
                        counts,
                        itemCount: catalog.items.length,
                        phase: catalog.phase,
                        sourceCount: catalog.sources.length,
                    };
                }),
                remainingScenarioMs(),
                "restored source query exceeded the 10-second application bound"
            );
            console.log(
                "[external-media] restored-state " +
                    JSON.stringify({
                        access: restored.access,
                        available: restored.available,
                        itemCount: restored.itemCount,
                        phase: restored.phase,
                        sourceCount: restored.sourceCount,
                        queryCalls: restored.counts.queryCalls,
                        queryRejected: restored.counts.queryRejected,
                        queryResolved: restored.counts.queryResolved,
                        requestCalls: restored.counts.requestCalls,
                        directoryValuesCalls:
                            restored.counts.directoryValuesCalls,
                        directoryEntryCalls:
                            restored.counts.directoryEntryCalls,
                        fileReadCalls: restored.counts.fileReadCalls,
                        sentinelRead: restored.body === sentinel,
                        missingItem: restored.body === null,
                    })
            );
            assert.equal(restored.sourceCount, 1);
            assert.equal(restored.counts.queryCalls, 1);
            assert.equal(restored.counts.requestCalls, 0);
            assert.equal(restored.counts.queryRejected, 0);
            assert.equal(restored.phase, "complete");
            if (restored.counts.queryResolved.length !== 1)
                fail("restored native permission query did not settle once");
            const restoredPermission = restored.counts.queryResolved[0];
            if (
                restoredPermission === "prompt" ||
                restoredPermission === "denied"
            ) {
                assert.equal(restored.access, "permission-required");
                assert.equal(restored.available, false);
                assert.equal(restored.itemCount, 1);
                assert.equal(restored.bodyRead, false);
                assert.equal(restored.counts.directoryValuesCalls, 0);
                assert.equal(restored.counts.directoryEntryCalls, 0);
                assert.equal(restored.counts.fileReadCalls, 0);
                assert.equal(restored.body, null);
                console.log(
                    "[external-media] restored native handle correctly requires reconnect"
                );
            } else if (restoredPermission === "granted") {
                assert.equal(restored.access, "granted");
                assert.equal(restored.available, true);
                assert.equal(restored.itemCount, 1);
                assert.equal(restored.bodyRead, true);
                assert.equal(restored.body, sentinel);
                console.log(
                    "[external-media] restored native handle remained readable"
                );
            } else {
                fail("restored native permission returned an unknown state");
            }
            checkpoint("scenario-passed");
        })();
        const timeout = new Promise((_, reject) => {
            const remaining = Math.max(1, scenarioDeadline - Date.now());
            scenarioDeadline = Date.now() + remaining;
            const timer = setTimeout(
                () => reject(new Error("scenario deadline exceeded")),
                remaining
            );
            timer.unref();
        });
        await Promise.race([scenario, timeout]);
        console.log(
            "PASS external browser folder: trusted native drop, committed IDB handle, bounded query and no implicit permission request"
        );
    } catch (error) {
        console.error(
            "[external-media] failed during " +
                phase +
                " (" +
                safeErrorName(error) +
                ")"
        );
        throw error;
    } finally {
        cleanupDeadline = Date.now() + cleanupDeadlineMs;
        try {
            cleanupStarted = true;
            if (!browserServer && browserServerPromise) {
                try {
                    browserServer = await within(
                        browserServerPromise,
                        Math.max(1, cleanupDeadline - Date.now()),
                        "late owned browser startup exceeded cleanup deadline"
                    );
                } catch (_) {
                    // launchServer's own 12-second startup timeout owns cleanup
                    // if no BrowserServer was ever returned.
                }
            }
            if (!browserServer && !browserLaunchFailed)
                throw new Error(
                    "owned browser process cleanup could not be confirmed"
                );
            if (browserServer) {
                const child = browserServer.process();
                try {
                    await within(
                        browserServer.close(),
                        Math.max(
                            1,
                            Math.min(5000, cleanupDeadline - Date.now())
                        ),
                        "owned browser graceful shutdown exceeded its bound"
                    );
                } catch (_) {
                    if (!processExited(child))
                        await within(
                            browserServer.kill(),
                            Math.max(1, cleanupDeadline - Date.now()),
                            "owned browser kill exceeded cleanup deadline"
                        );
                }
                if (!processExited(child))
                    await within(
                        waitForProcessExit(child),
                        Math.max(1, cleanupDeadline - Date.now()),
                        "owned browser process exit was not confirmed"
                    );
                if (!processExited(child))
                    throw new Error("owned browser process remained alive");
                console.log(
                    "[external-media] owned-browser-exit " +
                        JSON.stringify({
                            exitCode: child.exitCode,
                            signalCode: child.signalCode,
                        })
                );
            }
        } finally {
            try {
                if (server)
                    await within(
                        new Promise((resolve) => {
                            server.close(resolve);
                            server.closeAllConnections?.();
                        }),
                        Math.max(1, cleanupDeadline - Date.now()),
                        "owned loopback server cleanup exceeded 10 seconds"
                    );
            } finally {
                if (ownedRoot)
                    fs.rmSync(ownedRoot, { force: true, recursive: true });
            }
        }
    }
}

main().catch(() => {
    process.exitCode = 1;
});
