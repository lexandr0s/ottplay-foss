// Real Home HTML/CSS/JS and browser focus; only the gateway and poll clock are fixtures.
const fs = require("node:fs");
const path = require("node:path");
const { test, expect } = require("../helpers/browser-test.cjs");

const origin = "https://home-focus.example";
const assets = new Map(
    ["index.html", "style.css", "core.js", "webos-host.js", "app.js"].map(
        (name) => [
            "/dist/home/" + name,
            {
                body: fs.readFileSync(
                    path.join(__dirname, "../../integrations/home", name)
                ),
                contentType: name.endsWith(".html")
                    ? "text/html; charset=utf-8"
                    : name.endsWith(".css")
                      ? "text/css"
                      : "application/javascript",
            },
        ]
    )
);

function snapshot(label) {
    return {
        version: 1,
        entities: [],
        actions: [],
        apps: [{ id: "tv", label, kind: "app" }],
    };
}

test("Home poll replaces a focused launcher without stealing navigation or text-entry focus", async ({
    page,
    context,
}) => {
    const waiting = [];
    const unexpected = [];
    const errors = [];
    let requests = 0;
    page.on("pageerror", (error) => errors.push(error.message));
    await context.route("**/*", async (route) => {
        const request = route.request();
        const url = new URL(request.url());
        if (url.origin === origin && assets.has(url.pathname)) {
            expect(request.method()).toBe("GET");
            return route.fulfill(assets.get(url.pathname));
        }
        if (url.origin === origin && url.pathname === "/api/home") {
            expect(request.method()).toBe("GET");
            expect(request.headers().authorization).toBe(
                "Bearer " + "f".repeat(32)
            );
            requests++;
            if (requests === 1)
                return route.fulfill({ json: snapshot("TV initial") });
            const model = await new Promise((resolve) => waiting.push(resolve));
            return route.fulfill({ json: model });
        }
        unexpected.push(url.origin + url.pathname);
        return route.abort("blockedbyclient");
    });
    // Exercise the real createClient timer, without a 15-second wall wait per poll.
    await page.clock.install();
    await page.goto(origin + "/dist/home/index.html");
    await page.locator('[data-tab="settings"]').click();
    await page.locator("#base").fill(origin);
    await page.locator("#key").fill("f".repeat(32));
    await page.locator('#connection-form button[type="submit"]').click();
    await expect(page.locator("#connection")).toHaveText("Home connected");
    const appsTab = page.locator('[data-tab="apps"]');
    const launcher = page.locator('[data-item="app:tv"]');
    await appsTab.click();
    await expect(launcher).toBeVisible();
    await launcher.focus();
    await expect(launcher).toBeFocused();
    const old = await launcher.elementHandle();

    async function beginPoll(number) {
        await page.clock.fastForward(15000);
        await expect.poll(() => requests).toBe(number);
        await expect.poll(() => waiting.length).toBe(1);
        await expect(page.locator("#connection")).toHaveText("Connecting…");
        await expect(launcher).toHaveCount(0);
    }
    async function finishPoll(label) {
        waiting.shift()(snapshot(label));
        await expect(page.locator("#connection")).toHaveText("Home connected");
        await expect(launcher).toHaveText(label);
    }

    await beginPoll(2);
    expect(await old.evaluate((element) => element.isConnected)).toBe(false);
    expect(
        await page.evaluate(() => document.activeElement === document.body)
    ).toBe(true);
    await finishPoll("TV refreshed");
    await expect(launcher).toBeVisible();
    await expect(launcher).toBeFocused();
    expect(
        await launcher.evaluate(
            (element, previous) => element === previous,
            old
        )
    ).toBe(false);
    await old.dispose();

    await appsTab.focus();
    await beginPoll(3);
    await expect(appsTab).toBeFocused();
    await finishPoll("TV navigation poll");
    await expect(appsTab).toBeFocused();

    // Connection editing pauses Home polling. A late reply to an already-started
    // poll must neither render hidden cards nor move focus out of the real input.
    await beginPoll(4);
    await page.locator('[data-tab="settings"]').click();
    const key = page.locator("#key");
    await key.focus();
    await key.press("End");
    await key.press("ArrowLeft");
    const selection = await key.evaluate((element) => element.selectionStart);
    waiting.shift()(snapshot("TV cancelled poll"));
    await page.clock.fastForward(20000);
    expect(requests).toBe(4);
    await expect(key).toBeVisible();
    await expect(key).toBeFocused();
    expect(await key.evaluate((element) => element.selectionStart)).toBe(
        selection
    );
    await expect(launcher).toBeHidden();
    expect(errors).toEqual([]);
    expect(unexpected).toEqual([]);
});
