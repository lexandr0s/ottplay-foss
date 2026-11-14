"""Hosted-Linux tool compatibility smoke; never builds or launches OTT binaries."""

import argparse
import hashlib
import json
import os
import re
import selectors
import signal
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

DOCKER = "/usr/bin/docker"
LABEL = "org.ottplay.multiview-tools-owner"
ASSERTIONS = {
    "ffmpeg-fd", "ffprobe-fd", "stream-stats", "h264-aac-encode",
    "fd-probe", "fd-decode", "canonical-mkfifo-argv0", "files-removed",
}
LIMIT = 4 * 1024 * 1024
SMOKE = r'''#!/bin/sh
set -eu
umask 077
ulimit -f 8192
cd /work
test "$(id -u)" = 65532
for tool in ffmpeg ffprobe; do
  "/usr/bin/$tool" -v quiet -protocols > "$tool.protocols"
  awk '/^Input:/{input=1;next}/^Output:/{input=0} input && $1=="fd"{found=1} END{exit(!found)}' "$tool.protocols"
  printf 'ASSERT %s-fd\n' "$tool"
done
/usr/bin/ffmpeg -hide_banner -h full > help 2>/dev/null
test "$(wc -c < help)" -le 4194304
awk '$1=="-stats_enc_pre"{a=1} $1=="-stats_enc_pre_fmt"{b=1} END{exit(!(a&&b))}' help
printf 'ASSERT stream-stats\n'
resolved=$(readlink -f /usr/bin/mkfifo)
test "$resolved" = /bin/busybox
# Match CommandExt::arg0("mkfifo") on the canonical tool, not busybox's
# separate explicit-applet argument convention.
/bin/sh -c 'exec -a mkfifo "$1" "$2" "$3"' -- "$resolved" audio.fifo progress.fifo
test -p audio.fifo && test -p progress.fifo
test "$(stat -c %a audio.fifo)" = 600
test "$(stat -c %a progress.fifo)" = 600
printf 'ASSERT canonical-mkfifo-argv0\n'
/usr/bin/ffmpeg -nostdin -v error -f lavfi -i color=c=red:s=64x36:r=10 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 0.8 \
  -c:v libx264 -threads:v 1 -preset ultrafast -pix_fmt yuv420p \
  -c:a aac -ac 2 -ar 48000 -f mpegts sample.ts
test -s sample.ts && test "$(wc -c < sample.ts)" -le 1048576
printf 'ASSERT h264-aac-encode\n'
cat sample.ts | /usr/bin/ffprobe -v error -protocol_whitelist fd \
  -format_whitelist mpegts -f mpegts -fd 0 -i fd: \
  -show_entries stream=codec_name -of default=noprint_wrappers=1:nokey=1 > probe
grep -qx h264 probe
grep -qx aac probe
printf 'ASSERT fd-probe\n'
cat sample.ts | /usr/bin/ffmpeg -nostdin -v error -protocol_whitelist fd \
  -format_whitelist mpegts -f mpegts -fd 0 -i fd: \
  -map 0:v:0 -frames:v 8 -pix_fmt rgb24 -threads:v 1 -f rawvideo video.rgb \
  -map 0:a:0 -t 0.1 -ac 2 -ar 48000 -f s16le audio.pcm
test "$(wc -c < video.rgb)" = 55296
test -s audio.pcm && test "$(wc -c < audio.pcm)" -le 196608
printf 'ASSERT fd-decode\n'
apk info -v | sort | sed 's/^/PACKAGE /'
sha256sum /usr/bin/ffmpeg /usr/bin/ffprobe /bin/busybox
rm audio.fifo progress.fifo sample.ts probe video.rgb audio.pcm help ffmpeg.protocols ffprobe.protocols
test -z "$(ls -A /work)"
printf 'ASSERT files-removed\n'
'''


class SmokeError(Exception):
    pass


OPERATION_ERRORS = (SmokeError, OSError, ValueError, KeyError, TypeError,
                    subprocess.SubprocessError, KeyboardInterrupt)


def source_plan(raw):
    """Admit only the known stage and package operation, never execute RUN text."""
    text = raw.decode("utf-8")
    bases = re.findall(
        r"^FROM (alpine:[0-9]+\.[0-9]+@sha256:[0-9a-f]{64}) AS server-runtime$",
        text, re.MULTILINE,
    )
    if len(bases) != 1 or text.count(" AS server-runtime\n") != 1:
        raise SmokeError("Expected one pinned Alpine server-runtime base")
    stages = re.split(r"^FROM ", text, flags=re.MULTILINE)[1:]
    runtime = [stage for stage in stages if stage.splitlines()[0] == bases[0] + " AS server-runtime"]
    if len(runtime) != 1 or (
        "RUN apk add --no-cache ca-certificates \\\n && adduser -D -H -u 65532 -g nonroot nonroot\n"
    ) not in runtime[0]:
        raise SmokeError("Runtime package/user operation changed; review smoke recipe")
    if [line for line in runtime[0].splitlines() if line.startswith("RUN ")] != [
        "RUN apk add --no-cache ca-certificates \\"]:
        raise SmokeError("Additional runtime install operation requires review")
    player = [stage for stage in stages if stage.splitlines()[0] == "server-runtime AS player"]
    if len(player) != 1:
        raise SmokeError("Expected player derived from server-runtime")
    run_lines = [line for line in player[0].splitlines()[1:] if line.startswith("RUN ")]
    if run_lines != ["RUN apk add --no-cache ffmpeg"]:
        raise SmokeError("Player media package operation changed; review smoke recipe")
    return {"base": bases[0], "dockerfileSha256": hashlib.sha256(raw).hexdigest()}


def clean_environment(home):
    return {"PATH": "/usr/bin:/bin", "HOME": str(home), "LANG": "C", "LC_ALL": "C"}


def safe_tail(raw):
    text = raw[-4096:].decode("utf-8", "replace")
    text = re.sub(r"https?://[^\s]+", "[url]", text)
    text = re.sub(r"(?i)(token|authorization|password|secret)(\s*[:=]\s*)[^\s]+", r"\1\2[redacted]", text)
    return text


def bounded(argv, *, env, timeout, label, limit=LIMIT):
    """One process group, finite stdout/stderr, cleanup even on read failure."""
    process = None
    selector = selectors.DefaultSelector()
    buffers = {"stdout": bytearray(), "stderr": bytearray()}
    started = time.monotonic()
    try:
        process = subprocess.Popen(
            argv, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, start_new_session=True,
        )
        selector.register(process.stdout, selectors.EVENT_READ, "stdout")
        selector.register(process.stderr, selectors.EVENT_READ, "stderr")
        while selector.get_map():
            remaining = timeout - (time.monotonic() - started)
            if remaining <= 0:
                raise SmokeError(f"{label}: timeout")
            for key, _ in selector.select(min(remaining, 0.1)):
                part = os.read(key.fileobj.fileno(), 65536)
                if not part:
                    selector.unregister(key.fileobj)
                    continue
                cap = limit if key.data == "stdout" else 65536
                if len(buffers[key.data]) + len(part) > cap:
                    raise SmokeError(f"{label}: output limit")
                buffers[key.data].extend(part)
        code = process.wait(timeout=max(0.01, timeout - (time.monotonic() - started)))
        if code != 0:
            detail = json.dumps(safe_tail(buffers["stderr"]), ensure_ascii=True)
            raise SmokeError(f"{label}: exit {code}; stderr={detail}")
        return bytes(buffers["stdout"])
    finally:
        selector.close()
        if process is not None:
            # Kill the owned group even when the CLI exited but a descendant
            # retained a pipe. Container cleanup is a separate daemon action.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=2)
            process.stdout.close()
            process.stderr.close()


def parse_output(raw):
    assertions, packages, tools = [], [], {}
    for line in raw.decode("ascii").splitlines():
        if line.startswith("ASSERT "):
            assertions.append(line[7:])
        elif line.startswith("PACKAGE ") and re.fullmatch(r"[A-Za-z0-9_.+:-]{1,180}", line[8:]):
            packages.append(line[8:])
        else:
            match = re.fullmatch(r"([0-9a-f]{64})  (/usr/bin/ffmpeg|/usr/bin/ffprobe|/bin/busybox)", line)
            if not match or match[2] in tools:
                raise SmokeError("Unexpected smoke output")
            tools[match[2]] = match[1]
    if len(assertions) != len(ASSERTIONS) or set(assertions) != ASSERTIONS:
        raise SmokeError("Missing or duplicate tool assertion")
    if not 1 <= len(packages) <= 256 or len(set(packages)) != len(packages):
        raise SmokeError("Invalid installed package inventory")
    if not any(re.fullmatch(r"ffmpeg-[0-9].+", item) for item in packages) or len(tools) != 3:
        raise SmokeError("Missing FFmpeg package/tool identity")
    return {"assertions": sorted(assertions), "packages": packages, "tools": tools}


def recipe(plan, owner):
    return (f"FROM {plan['base']}\n"
            "RUN apk add --no-cache ca-certificates ffmpeg "
            "&& adduser -D -H -u 65532 -g nonroot nonroot\n"
            f"LABEL {LABEL}=\"{owner}\"\n"
            "COPY smoke.sh /smoke.sh\nUSER 65532:65532\n"
            'ENTRYPOINT ["/bin/sh", "/smoke.sh"]\n')


def run_smoke(repo, work, *, runner=bounded):
    owner = uuid.uuid4().hex
    name = "ottplay-mv-tools-" + owner
    tag = name + ":smoke"
    plan = source_plan((repo / "Dockerfile").read_bytes())
    evidence = {"schema": 1, "status": "failed", "owner": owner, **plan,
                "smokeSha256": hashlib.sha256(SMOKE.encode()).hexdigest(),
                "recipeSha256": hashlib.sha256(recipe(plan, owner).encode()).hexdigest(),
                "qualification": "Alpine tools only; no OTT binary, native client or deployment qualification",
                "steps": [], "cleanupErrors": []}
    start = time.monotonic()
    work_end, end = start + 260, start + 300
    home = work / "home"
    home.mkdir(mode=0o700)
    context = work / "context"
    context.mkdir(mode=0o700)
    (context / "Dockerfile").write_text(recipe(plan, owner))
    (context / "smoke.sh").write_text(SMOKE)
    env = clean_environment(home)

    def call(args, label, seconds=10, *, cleanup=False):
        remaining = (end if cleanup else work_end) - time.monotonic()
        if remaining <= 0:
            raise SmokeError(f"{label}: aggregate deadline")
        output = runner([DOCKER, *args], env=env, timeout=min(seconds, remaining), label=label)
        evidence["steps"].append(label)
        return output

    def inventory(kind, cleanup=False):
        args = [kind, "ls"]
        if kind == "container":
            args.append("--all")
        args += ["--no-trunc", "--filter", f"label={LABEL}={owner}", "--format", "{{json .}}"]
        return [json.loads(line) for line in call(args, kind + "-inventory", cleanup=cleanup).splitlines()]

    error = None
    try:
        call(["build", "--pull", "--network=default", "--tag", tag, str(context)], "install", 180)
        image = json.loads(call(["image", "inspect", "--format",
                                '{"id":{{json .Id}},"os":{{json .Os}},"arch":{{json .Architecture}}}',
                                tag], "image-id"))
        ids = image.get("id", "")
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", ids):
            raise SmokeError("Invalid installed image identity")
        if image.get("os") != "linux" or image.get("arch") not in {"amd64", "arm64"}:
            raise SmokeError("Unexpected image platform")
        evidence["imageId"] = ids
        evidence["imagePlatform"] = image["os"] + "/" + image["arch"]
        call(["create", "--name", name, "--label", f"{LABEL}={owner}",
              "--network=none", "--user=65532:65532", "--read-only", "--cap-drop=ALL",
              "--security-opt=no-new-privileges", "--pids-limit=64", "--memory=256m", "--cpus=1",
              "--tmpfs=/work:rw,nosuid,nodev,noexec,size=16m,mode=1777", ids], "create")
        output = call(["start", "--attach", name], "tools", 30)
        state = json.loads(call(["container", "inspect", "--format", "{{json .State}}", name], "state"))
        if state.get("Running") is not False or state.get("ExitCode") != 0 or state.get("OOMKilled") is not False:
            raise SmokeError("Container did not finish normally")
        evidence.update(parse_output(output))
    except OPERATION_ERRORS as caught:
        error = caught
        evidence["error"] = str(caught)
    finally:
        # Reconcile by the random label even if build/create timed out after
        # the daemon accepted it. Never prune shared images or containers.
        for kind in ("container", "image"):
            try:
                for item in inventory(kind, cleanup=True):
                    if kind == "container":
                        if item.get("Names") != name or not re.fullmatch(r"[0-9a-f]{64}", item.get("ID", "")):
                            raise SmokeError("Unexpected owned container identity")
                        call(["container", "rm", "--force", item["ID"]], "remove-container", cleanup=True)
                    else:
                        if item.get("Repository") != name or item.get("Tag") != "smoke":
                            raise SmokeError("Unexpected owned image identity")
                        image_id = item.get("ID", "")
                        if not re.fullmatch(r"sha256:[0-9a-f]{64}", image_id):
                            raise SmokeError("Invalid cleanup image identity")
                        if evidence.get("imageId", image_id) != image_id:
                            raise SmokeError("Cleanup image changed")
                        call(["image", "rm", image_id], "remove-image", cleanup=True)
                if inventory(kind, cleanup=True):
                    raise SmokeError(f"Owned {kind} remains")
            except OPERATION_ERRORS as caught:
                evidence["cleanupErrors"].append(str(caught))
    evidence["elapsedSeconds"] = round(time.monotonic() - start, 3)
    if error is None and not evidence["cleanupErrors"]:
        evidence["status"] = "passed"
    return evidence


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != "linux" or os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        parser.error("Execution is restricted to GitHub-hosted Linux")
    output = args.output.absolute()
    output.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    # Reserve the receipt before any Docker side effect; never overwrite proof.
    with output.open("x", encoding="utf-8") as receipt:
        os.chmod(output, 0o600)
        previous = signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(InterruptedError("Terminated")))
        try:
            with tempfile.TemporaryDirectory(prefix="ottplay-mv-tools-") as directory:
                result = run_smoke(Path(__file__).resolve().parents[1], Path(directory))
            json.dump(result, receipt, indent=2)
            receipt.write("\n")
            print(json.dumps({"status": result["status"], "elapsedSeconds": result["elapsedSeconds"]}))
            return 0 if result["status"] == "passed" else 1
        finally:
            signal.signal(signal.SIGTERM, previous)


if __name__ == "__main__":
    sys.exit(main())
