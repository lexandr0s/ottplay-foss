/** Fixed-origin lazy host; the owned panel runtime is a separate staged asset. */
export function installHomeIntegration(w: any): any {
    if (w.__ottHomeIntegration) return w.__ottHomeIntegration;
    var pending: any = null;
    function blocked(): boolean {
        var k = w.__ottKiosk,
            s = w.__ottStandby;
        return !!(
            (k &&
                ((typeof k.enabled === "function" && k.enabled()) ||
                    (typeof k.locked === "function" && k.locked()))) ||
            (s && typeof s.blocked === "function" && s.blocked())
        );
    }
    function close(): void {
        if (pending) pending();
        if (w.__ottHomeRuntime) w.__ottHomeRuntime.close();
    }
    function open(): boolean {
        var d = w.document,
            p = w.__ottClassicScreenPort;
        var owner = p && p.screens && p.screens.current();
        if (
            d.hidden ||
            blocked() ||
            !owner ||
            typeof owner.foreground !== "function" ||
            !owner.foreground() ||
            typeof owner.own !== "function"
        )
            return false;
        if (pending) return true;
        if (w.__ottHomeHostInstall) return w.__ottHomeHostInstall(w).open();
        var script = d.createElement("script"),
            live = true,
            release: any = null;
        var timeout: any = null,
            poll: any = null;
        function valid(): boolean {
            return live && !d.hidden && !blocked() && owner.foreground();
        }
        function cancel(): void {
            if (!live) return;
            live = false;
            pending = null;
            script.onload = script.onerror = null;
            if (script.parentNode) script.parentNode.removeChild(script);
            w.clearTimeout(timeout);
            w.clearInterval(poll);
            d.removeEventListener("visibilitychange", hidden);
            w.removeEventListener("pagehide", cancel);
            if (release) {
                var done = release;
                release = null;
                done();
            }
        }
        function hidden(): void {
            if (!valid()) cancel();
        }
        pending = cancel;
        release = owner.own(cancel);
        if (!live) return false;
        script.src = "/dist/home/host.js";
        script.onload = function (): void {
            var allowed = valid();
            cancel();
            if (allowed && w.__ottHomeHostInstall)
                w.__ottHomeHostInstall(w).open();
        };
        script.onerror = cancel;
        d.addEventListener("visibilitychange", hidden);
        w.addEventListener("pagehide", cancel);
        timeout = w.setTimeout(cancel, 8000);
        poll = w.setInterval(hidden, 250);
        (d.head || d.documentElement).appendChild(script);
        return true;
    }
    return (w.__ottHomeIntegration = { open: open, close: close });
}
export function showHomeIntegration(w: any): boolean {
    return installHomeIntegration(w).open();
}
