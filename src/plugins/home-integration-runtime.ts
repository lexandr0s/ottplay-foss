/** Lazy same-origin Home dashboard. Playback, settings and credentials stay with their owners. */
export function installHomeIntegrationRuntime(w: any): any {
    if (w.__ottHomeRuntime) return w.__ottHomeRuntime;
    var current: any = null;
    function blocked(): boolean {
        var kiosk = w.__ottKiosk;
        return !!(
            (kiosk &&
                ((typeof kiosk.enabled === "function" && kiosk.enabled()) ||
                    (typeof kiosk.locked === "function" && kiosk.locked()))) ||
            (w.__ottStandby &&
                typeof w.__ottStandby.blocked === "function" &&
                w.__ottStandby.blocked())
        );
    }
    function open(): boolean {
        if (blocked()) return false;
        if (current) {
            if (current.active()) {
                current.focus();
                return true;
            }
            current.close();
        }
        var d = w.document;
        var panel = d.getElementById("listAbout");
        if (
            !panel ||
            typeof w.saveListPanelState !== "function" ||
            typeof w.restoreListPanelState !== "function"
        )
            return false;
        var previousHandler = w.aboutKeyHandler;
        var previousFocus = d.activeElement;
        var wrapper = d.createElement("div");
        var back = d.createElement("button");
        var frame = d.createElement("iframe");
        var closed = false;
        var timer: any = null;
        var observer: any = null;
        var unregister: any = null;
        var api: any;
        function owns(): boolean {
            return !closed && current === api && w.aboutKeyHandler === handler;
        }
        function teardown(restore: boolean): void {
            if (closed) return;
            var wasOwner = owns();
            closed = true;
            if (current === api) current = null;
            if (timer !== null) w.clearInterval(timer);
            if (observer) observer.disconnect();
            d.removeEventListener("visibilitychange", visibility);
            w.removeEventListener("pagehide", retire);
            frame.onload = null;
            // Notify the embedded controller before destroying its browsing context.
            // pagehide cancels dashboard XHR/probes; about:blank/removal aborts loads too.
            try {
                var child = frame.contentWindow;
                if (child) {
                    child.OTTHomeHost = null;
                    var event = child.document.createEvent("Event");
                    event.initEvent("pagehide", false, false);
                    child.dispatchEvent(event);
                }
            } catch (_) {
                /* A failed frame is still removed below. */
            }
            try {
                frame.src = "about:blank";
            } catch (_) {}
            if (wrapper.parentNode === panel) panel.removeChild(wrapper);
            if (restore && wasOwner) {
                panel.style.display = "none";
                w.aboutKeyHandler = previousHandler;
                w.restoreListPanelState();
                if (previousFocus && typeof previousFocus.focus === "function")
                    previousFocus.focus();
            }
            if (unregister) {
                var release = unregister;
                unregister = null;
                release();
            }
        }
        function retire(): void {
            teardown(false);
        }
        function visibility(): void {
            if (d.hidden) teardown(true);
        }
        function check(): void {
            if (
                !owns() ||
                wrapper.parentNode !== panel ||
                panel.style.display === "none"
            )
                teardown(false);
            else if (blocked()) teardown(true);
        }
        function handler(key: number): boolean {
            if (!owns()) return false;
            var keys = w.keys || {};
            if (
                key === keys.RETURN ||
                key === keys.EXIT ||
                key === 27 ||
                key === 461 ||
                key === 10009
            )
                teardown(true);
            else if (key === keys.DOWN || key === keys.RIGHT) frame.focus();
            else if (key === keys.ENTER && d.activeElement === back)
                teardown(true);
            return true;
        }
        wrapper.style.height = "100%";
        wrapper.style.display = "flex";
        wrapper.style.flexDirection = "column";
        back.type = "button";
        back.textContent = typeof w._ === "function" ? w._("Back") : "Back";
        back.onclick = function () {
            teardown(true);
        };
        frame.title = typeof w._ === "function" ? w._("Home") : "Home";
        frame.style.width = "100%";
        frame.style.flex = "1 1 auto";
        frame.style.minHeight = "0";
        frame.style.border = "0";
        frame.setAttribute("referrerpolicy", "no-referrer");
        // Fixed packaged asset only: no URL, HA token or bridge key in the host adapter.
        frame.src = "/dist/home/index.html";
        frame.onload = function (): void {
            if (!owns()) return;
            try {
                var child = frame.contentWindow;
                if (
                    !child ||
                    child.location.protocol !== w.location.protocol ||
                    child.location.host !== w.location.host ||
                    child.location.pathname !== "/dist/home/index.html"
                ) {
                    teardown(true);
                    return;
                }
                child.OTTHomeHost = {
                    close: function (done?: (ok: boolean) => void): void {
                        var active = owns();
                        if (active) teardown(true);
                        if (typeof done === "function") done(active);
                    },
                };
            } catch (_) {
                teardown(true);
            }
        };
        api = {
            active: owns,
            focus: function (): void {
                if (owns() && frame.focus) frame.focus();
            },
            close: function (): void {
                teardown(true);
            },
        };
        w.saveListPanelState();
        panel.textContent = "";
        panel.appendChild(wrapper);
        wrapper.appendChild(back);
        wrapper.appendChild(frame);
        panel.style.display = "block";
        current = api;
        w.aboutKeyHandler = handler;
        var port = w.__ottClassicScreenPort;
        var owner = port && port.screens && port.screens.current();
        if (owner && owner.kind === "about" && typeof owner.own === "function")
            unregister = owner.own(retire);
        d.addEventListener("visibilitychange", visibility);
        w.addEventListener("pagehide", retire);
        if (w.MutationObserver) {
            observer = new w.MutationObserver(check);
            observer.observe(panel, {
                childList: true,
                attributes: true,
                attributeFilter: ["style"],
            });
        }
        // Retire even on legacy hosts without MutationObserver or a screen port.
        timer = w.setInterval(check, 250);
        back.focus();
        return true;
    }
    return (w.__ottHomeRuntime = {
        open: open,
        close: function (): void {
            if (current) current.close();
        },
    });
}
