# Native browser folder-permission reproduction

`npm run test:browser:external` checks the external-folder permission path involved in issue 1004. It creates one synthetic media file inside a runner-owned temporary directory, uses a trusted Chromium file drop to obtain a native `FileSystemDirectoryHandle`, and passes that exact handle through `installBrowserLocalMedia.importFolder()`. The source record is committed to IndexedDB before the seed page closes. The test then opens a new same-origin page and calls the real `installBrowserLocalMedia.list()` once against the restored record.

The test does not replace or simulate `queryPermission()`. Its observer calls the native method with the original receiver and arguments, returns the same native promise, and records settlement. It never calls `requestPermission()`, grants permission after reopen, changes the existing 10-second application deadline, or retries. If the restored native query settles to `prompt` or `denied`, the catalog must show `permission-required`, remain unavailable, and avoid directory enumeration or file reads. If it settles to `granted`, the test reads and verifies the exact synthetic sentinel bytes.

This test uses a picker bridge only to hand the app the exact native directory handle acquired by the trusted file-drop event. It does not fake a handle or its permission state. The browser test uses a loopback origin, no external network, and no persistent browser profile. Playwright owns the browser server process: startup has a 12-second timeout, teardown first requests graceful server close for at most 5 seconds, then kills only that owned server if needed, and requires process exit confirmation within the 10-second cleanup allowance. The owned temporary directory is removed after browser cleanup. Its scenario budget is 50 seconds plus that bounded cleanup allowance.

## What this test does not establish

It does not reproduce or fix the historical unresolved OPFS query. The existing `tests/test_browser_direct_media.cjs` OPFS-based case remains unchanged and must continue to run with its original timeout, assertions, and failure-only diagnostics. The two cases cover different handle classes:

- OPFS/bucket handles must have `granted` permission state under the [File System Access specification](https://wicg.github.io/file-system-access/#permission-state-constraints).
- A local external handle restored from IndexedDB may return `prompt` under the [queryPermission guidance](https://wicg.github.io/file-system-access/#api-filesystemhandle-querypermission); the test verifies that the application exposes reconnect/permission-required state without requesting permission automatically.

Chromium’s protocol supports native file drops through [`Input.dispatchDragEvent`](https://chromedevtools.github.io/devtools-protocol/tot/Input/). Chromium’s own [File System Access drag/drop browser test](https://chromium.googlesource.com/chromium/src/+/main/content/browser/file_system_access/file_system_access_drag_drop_browsertest.cc) exercises dropped files and directories using `getAsFileSystemHandle()`.

For a locally installed Chromium executable other than the revision installed by the checked-in Playwright version, the test accepts `OTT_ISSUE1004_CHROMIUM_PATH`; CI should use its normally pinned Playwright-managed browser. Record the actual Chromium and Playwright versions with every result. A pass in a different runtime is not a reproduction or dismissal of the historical failure.

## CI routing

The ordinary PR Quality gate runs this test in its existing Windows Chromium
browser job, before the unchanged direct-media/OPFS case. The manually invoked
Linux CI build also runs it before that case. Registration as an npm script alone
is not automatic PR coverage; `tests/test_ci_inventory.cjs` checks the actual
workflow call graph and keeps the manual-only inventory strict.

The local macOS result is separate from either hosted platform qualification.
