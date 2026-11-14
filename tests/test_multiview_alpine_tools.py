"""Offline contracts for the hosted dependency smoke; no Docker/media execution."""

import json
import os
import runpy
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
M = runpy.run_path(str(ROOT / "scripts/check-multiview-alpine-tools.py"))
BASE = "alpine:3.24@sha256:" + "a" * 64
SOURCE = (
    f"FROM {BASE} AS server-runtime\n"
    "RUN apk add --no-cache ca-certificates \\\n && adduser -D -H -u 65532 -g nonroot nonroot\n"
    "COPY --from=rust-build /app/server ./server\n"
    "FROM server-runtime AS player\nUSER root\nRUN apk add --no-cache ffmpeg\nUSER nonroot\n"
).encode()


def successful_output():
    return ("\n".join("ASSERT " + name for name in sorted(M["ASSERTIONS"])) +
            "\nPACKAGE ffmpeg-8.0-r1\nPACKAGE busybox-1.37.0-r2\n" +
            "\n".join("b" * 64 + "  " + path for path in
                      ("/usr/bin/ffmpeg", "/usr/bin/ffprobe", "/bin/busybox")) + "\n").encode()


class DockerFake:
    def __init__(self, failure=None, output=None, foreign=False):
        self.failure = failure
        self.output = successful_output() if output is None else output
        self.foreign = foreign
        self.commands = []
        self.image, self.container = False, False
        self.image_id = "sha256:" + "a" * 64
        self.name = None

    def __call__(self, args, *, env, timeout, label, **_):
        self.commands.append((args, env, timeout, label))
        assert 0 < timeout <= 180
        assert set(env) == {"PATH", "HOME", "LANG", "LC_ALL"}
        if label == "install":
            self.image = True
            self.name = args[args.index("--tag") + 1].removesuffix(":smoke")
        if label == "create":
            self.container = True
        if self.failure == label:
            raise M["SmokeError"]("controlled failure: " + label)
        if label == "image-id":
            return json.dumps({"id": self.image_id, "os": "linux", "arch": "amd64"}).encode()
        if label == "tools":
            return self.output
        if label == "state":
            return b'{"Running":false,"ExitCode":0,"OOMKilled":false}'
        if label == "container-inventory":
            return (json.dumps({"Names": "foreign" if self.foreign else self.name,
                                "ID": "c" * 64}).encode() if self.container else b"")
        if label == "image-inventory":
            return (json.dumps({"Repository": self.name, "Tag": "smoke", "ID": self.image_id}).encode()
                    if self.image else b"")
        if label == "remove-container":
            self.container = False
        if label == "remove-image":
            self.image = False
        return b""


class AlpineTools(unittest.TestCase):
    def bounded_fake(self, out=b"", err=b"", code=0, limit=1024, clock=None):
        streams = {3: [out, b""], 4: [err, b""]}
        process = SimpleNamespace(
            pid=123, stdout=SimpleNamespace(fileno=lambda: 3, close=Mock()),
            stderr=SimpleNamespace(fileno=lambda: 4, close=Mock()), wait=Mock(return_value=code),
        )
        registered = {}
        selector = Mock()
        selector.register.side_effect = lambda file, _, name: registered.update(
            {file.fileno(): SimpleNamespace(fileobj=file, data=name)})
        selector.unregister.side_effect = lambda file: registered.pop(file.fileno())
        selector.get_map.side_effect = lambda: registered
        selector.select.side_effect = lambda _: [(key, 1) for key in list(registered.values())]
        self.process = process
        with patch.object(M["subprocess"], "Popen", return_value=process) as popen, \
                patch.object(M["selectors"], "DefaultSelector", return_value=selector), \
                patch.object(M["os"], "read", side_effect=lambda fd, _: streams[fd].pop(0)), \
                patch.object(M["os"], "killpg", create=True) as kill, \
                patch.object(M["signal"], "SIGKILL", 9, create=True), \
                patch.object(M["time"], "monotonic", side_effect=clock or [0, 0, 0, 0]):
            self.popen, self.kill = popen, kill
            return M["bounded"](["owned-command"], env={"PATH": "/usr/bin:/bin"},
                                timeout=1, label="probe", limit=limit)

    def execute(self, fake):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Dockerfile").write_bytes(SOURCE)
            work = root / "private"
            work.mkdir()
            return M["run_smoke"](root, work, runner=fake)

    def test_current_docker_source_has_exact_admitted_recipe(self):
        plan = M["source_plan"]((ROOT / "Dockerfile").read_bytes())
        self.assertRegex(plan["base"], r"^alpine:3\.24@sha256:[a-f0-9]{64}$")
        generated = M["recipe"](plan, "c" * 32)
        self.assertNotIn("cargo", generated)
        self.assertNotIn("npm", generated)
        self.assertNotIn("COPY --from", generated)
        self.assertIn("USER 65532:65532", generated)

    def test_changed_source_operations_fail_closed(self):
        for source in (
            SOURCE.replace(b"@sha256:" + b"a" * 64, b""),
            SOURCE + b"RUN curl https://example.test\n",
            SOURCE.replace(b"apk add --no-cache ffmpeg", b"apk add --no-cache ffmpeg; true"),
            SOURCE.replace(b"-u 65532", b"-u 0"),
            SOURCE.replace(b"COPY --from=rust-build", b"RUN apk add other\nCOPY --from=rust-build"),
            SOURCE + f"FROM {BASE} AS server-runtime\n".encode(),
        ):
            with self.subTest(source=source), self.assertRaises(M["SmokeError"]):
                M["source_plan"](source)

    def test_success_requires_tools_state_and_confirmed_cleanup(self):
        fake = DockerFake()
        result = self.execute(fake)
        self.assertEqual(result["status"], "passed")
        self.assertEqual(result["imagePlatform"], "linux/amd64")
        self.assertEqual(len(result["assertions"]), 8)
        self.assertFalse(fake.container or fake.image)
        self.assertEqual(result["cleanupErrors"], [])
        removed = next(row[0] for row in fake.commands if row[3] == "remove-image")
        self.assertEqual(removed[-1], fake.image_id)

    def test_create_side_effect_then_failure_still_reconciles(self):
        fake = DockerFake(failure="create")
        result = self.execute(fake)
        self.assertEqual(result["status"], "failed")
        self.assertIn("controlled failure: create", result["error"])
        self.assertFalse(fake.container or fake.image)
        self.assertIn("remove-container", result["steps"])

    def test_build_side_effect_then_failure_still_reconciles(self):
        fake = DockerFake(failure="install")
        result = self.execute(fake)
        self.assertEqual(result["status"], "failed")
        self.assertFalse(fake.image)
        self.assertIn("remove-image", result["steps"])

    def test_tools_failure_remains_failure_after_successful_cleanup(self):
        fake = DockerFake(failure="tools")
        result = self.execute(fake)
        self.assertEqual(result["status"], "failed")
        self.assertFalse(fake.container or fake.image)

    def test_cleanup_failure_cannot_become_pass(self):
        fake = DockerFake(failure="remove-container")
        result = self.execute(fake)
        self.assertEqual(result["status"], "failed")
        self.assertTrue(result["cleanupErrors"])
        self.assertTrue(fake.container)

    def test_foreign_name_is_never_removed_even_with_own_label(self):
        fake = DockerFake(foreign=True)
        result = self.execute(fake)
        self.assertEqual(result["status"], "failed")
        self.assertNotIn("remove-container", result["steps"])
        self.assertTrue(fake.container)

    def test_missing_duplicate_or_unexpected_results_fail(self):
        for output in (
            successful_output().replace(b"ASSERT fd-decode\n", b""),
            successful_output() + b"ASSERT fd-decode\n",
            successful_output().replace(b"/bin/busybox", b"/bin/not-busybox"),
            successful_output() + b"unrecognized output\n",
        ):
            with self.subTest(output=output):
                fake = DockerFake(output=output)
                self.assertEqual(self.execute(fake)["status"], "failed")
                self.assertFalse(fake.container or fake.image)

    def test_runtime_no_network_mount_or_secret_and_bounded_resources(self):
        fake = DockerFake()
        with patch.dict(os.environ, {"GH_TOKEN": "private", "DOCKER_HOST": "tcp://bad", "HOME": "/private"}):
            self.execute(fake)
        create = next(row[0] for row in fake.commands if row[3] == "create")
        for flag in ("--network=none", "--user=65532:65532", "--read-only", "--cap-drop=ALL",
                     "--security-opt=no-new-privileges", "--pids-limit=64", "--memory=256m", "--cpus=1",
                     "--tmpfs=/work:rw,nosuid,nodev,noexec,size=16m,mode=1777"):
            self.assertIn(flag, create)
        self.assertFalse(any(item.startswith(("--mount", "--volume", "--env")) for item in create))
        for _, env, _, _ in fake.commands:
            self.assertNotIn("GH_TOKEN", env)
            self.assertNotIn("DOCKER_HOST", env)
            self.assertNotEqual(env["HOME"], "/private")

    def test_actual_shell_checks_fd_custody_argv0_and_decoded_bytes(self):
        script = M["SMOKE"]
        self.assertIn("exec -a mkfifo", script)
        self.assertIn('test "$resolved" = /bin/busybox', script)
        self.assertEqual(script.count("-fd 0 -i fd:"), 2)
        self.assertIn('test "$(wc -c < video.rgb)" = 55296', script)
        self.assertIn("test -s audio.pcm", script)
        self.assertIn("test -p audio.fifo && test -p progress.fifo", script)

    def test_scope_and_inventory_reachability(self):
        scope = runpy.run_path(str(ROOT / "scripts/ci-multiview-unix-scope.py"))
        for path in ("Dockerfile", "scripts/check-multiview-alpine-tools.py", "tests/test_multiview_alpine_tools.py"):
            self.assertTrue(scope["relevant"](path), path)
        inventory = json.loads((ROOT / "tests/python-suites.json").read_text())
        self.assertIn(["tests/test_multiview_alpine_tools.py"], inventory["portable"])

    def test_bounded_child_failure_redacts_and_closes_owned_group(self):
        with self.assertRaises(M["SmokeError"]) as raised:
            self.bounded_fake(err=b"token=private\n::error::https://u:p@example.test/?token=x", code=7)
        self.assertIn("exit 7", str(raised.exception))
        self.assertNotIn("private", str(raised.exception))
        self.assertNotIn("https://", str(raised.exception))
        self.assertNotIn("\n", str(raised.exception))
        self.kill.assert_called_once_with(123, 9)
        self.process.stdout.close.assert_called_once()
        self.process.stderr.close.assert_called_once()
        self.assertEqual(self.popen.call_args.kwargs["env"], {"PATH": "/usr/bin:/bin"})
        self.assertTrue(self.popen.call_args.kwargs["start_new_session"])

    def test_bounded_output_limit_still_kills_and_reaps(self):
        with self.assertRaisesRegex(M["SmokeError"], "output limit"):
            self.bounded_fake(out=b"x" * 65, limit=64)
        self.kill.assert_called_once()
        self.process.wait.assert_called_once_with(timeout=2)

    def test_bounded_timeout_still_kills_and_reaps(self):
        with self.assertRaisesRegex(M["SmokeError"], "timeout"):
            self.bounded_fake(clock=[0, 2])
        self.kill.assert_called_once()
        self.process.wait.assert_called_once_with(timeout=2)

    def test_workflow_smoke_precedes_native_and_is_mandatory_on_ubuntu(self):
        text = (ROOT / ".github/workflows/windows-validation.yml").read_text()
        job = text.split("  multiview-unix-fixtures:\n", 1)[1]
        smoke = job.split("      - name: Verify Alpine multiview tool compatibility\n", 1)[1].split("      - name:", 1)[0]
        self.assertIn("if: runner.os == 'Linux'", smoke)
        self.assertIn("python3 scripts/check-multiview-alpine-tools.py", smoke)
        self.assertNotIn("continue-on-error", smoke)
        self.assertLess(job.index("Verify Alpine multiview"), job.index("Run bounded isolated"))
        self.assertIn("timeout-minutes: 35", job)
        self.assertIn("/ottplay-multiview-alpine-", smoke)
        self.assertNotIn("/ottplay-multiview-unix-", smoke)


if __name__ == "__main__":
    unittest.main()
