"""Closed owned-file FFprobe batch; always supervised by the existing CI Driver PGID."""
import argparse
import hashlib
import json
import os
import selectors
import signal
import stat
import subprocess
import sys
import time
from pathlib import Path

PLAN_CAP = RECEIPT_CAP = 128 * 1024
OUTPUT_CAP = 16 * 1024**2
OUTPUT_TOTAL = 128 * 1024**2
ROWS = 153
STREAM_CAP = 65536
CANCELLED = False
COMPLETED = 0


class BatchError(RuntimeError):
    pass


def require(value, reason):
    if not value:
        raise BatchError(reason)


def request_cancel(*_):
    global CANCELLED
    CANCELLED = True


def tick(end):
    require(not CANCELLED, 'cancelled')
    require(time.monotonic() < end, 'batch_deadline')


def identity(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_nlink, info.st_size,
            info.st_uid, info.st_mtime_ns, info.st_ctime_ns)


def digest(path, cap, end, *, sealed=False, tool=False):
    tick(end)
    before = path.lstat()
    require(stat.S_ISREG(before.st_mode) and 0 < before.st_size <= cap
            and (before.st_nlink >= 1 if tool else before.st_nlink == 1)
            and (before.st_uid in (0, os.geteuid()) if tool else before.st_uid == os.geteuid()), 'file_shape')
    if sealed:
        require(not before.st_mode & 0o222, 'input_not_sealed')
    if tool:
        require(before.st_mode & 0o111 and not before.st_mode & 0o022, 'tool_mode')
    value = hashlib.sha256()
    count = 0
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), 'rb') as stream:
        require(identity(os.fstat(stream.fileno())) == identity(before), 'file_replaced')
        while block := stream.read(min(65536, cap + 1 - count)):
            tick(end)
            count += len(block)
            require(count <= cap and count <= before.st_size, 'file_grew')
            value.update(block)
        require(identity(os.fstat(stream.fileno())) == identity(before), 'file_changed')
    require(identity(path.lstat()) == identity(before), 'path_changed')
    return value.hexdigest(), before.st_size


def owned_path(raw, root, *, exists):
    require(type(raw) is str, 'path_type')
    path = Path(raw)
    require(path.is_absolute() and '..' not in path.parts and path.is_relative_to(root), 'path_escape')
    parent = path.parent
    while True:
        info = parent.lstat()
        require(stat.S_ISDIR(info.st_mode) and not parent.is_symlink()
                and info.st_uid == os.geteuid() and stat.S_IMODE(info.st_mode) == 0o700, 'private_ancestor')
        if parent == root:
            break
        require(parent.is_relative_to(root), 'ancestor_escape')
        parent = parent.parent
    if exists:
        require(not path.is_symlink(), 'symlink_input')
    else:
        require(not path.exists() and not path.is_symlink(), 'output_not_fresh')
    return path


def validate_plan(plan, source, end, reserved=()):
    require(type(plan) is dict and set(plan) == {'schema', 'root', 'worker_sha256', 'ffprobe', 'rows'}
            and plan['schema'] == 'ottplay.multiview.hls-probes.v1', 'plan_schema')
    root = Path(plan['root'])
    require(root.is_absolute() and root.resolve(strict=True) == root and root.is_dir(), 'root_shape')
    info = root.lstat()
    require(info.st_uid == os.geteuid() and stat.S_IMODE(info.st_mode) == 0o700, 'root_private')
    require(digest(source, 1024**2, end)[0] == plan['worker_sha256'], 'worker_changed')
    tool = plan['ffprobe']
    require(type(tool) is dict and set(tool) == {'path', 'sha256', 'bytes'}, 'tool_schema')
    binary = Path(tool['path'])
    require(binary.is_absolute() and binary.name == 'ffprobe'
            and binary.resolve(strict=True) == binary, 'tool_path')
    require(digest(binary, 1024**3, end, tool=True) == (tool['sha256'], tool['bytes']), 'tool_identity')
    require(type(plan['rows']) is list and len(plan['rows']) == ROWS, 'row_count')
    inputs, outputs = set(), set()
    for row in plan['rows']:
        require(type(row) is dict and set(row) == {'input', 'input_sha256', 'output'}, 'row_schema')
        source_path = owned_path(row['input'], root, exists=True)
        output = owned_path(row['output'], root, exists=False)
        require(source_path not in inputs and output not in outputs and source_path != output
                and source_path not in reserved and output not in reserved, 'duplicate_path')
        require(digest(source_path, 16 * 1024**2, end, sealed=True)[0] == row['input_sha256'], 'input_identity')
        inputs.add(source_path)
        outputs.add(output)
    require(not inputs.intersection(outputs), 'input_output_overlap')
    return root, binary


def probe(binary, source, output, root, end, completed_bytes):
    """One owned handle registered before any selector/pipe setup; no new session."""
    process = None
    selector = None
    buffers = {'stdout': bytearray(), 'stderr': bytearray()}
    child_end = min(end, time.monotonic() + 30)
    try:
        tick(child_end)
        process = subprocess.Popen(
            [str(binary), '-v', 'error', '-protocol_whitelist', 'file', '-format_whitelist', 'mpegts',
             '-f', 'mpegts', '-show_streams', '-show_packets', '-show_data_hash', 'sha256', '-show_entries',
             'stream=index,codec_name,time_base:packet=stream_index,pts,dts,duration,data_hash',
             '-of', 'json', '-o', str(output), str(source)],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            start_new_session=False,
            env={'PATH': '/usr/bin:/bin', 'HOME': str(root), 'TMPDIR': str(root), 'LANG': 'C', 'LC_ALL': 'C'},
        )
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ, 'stdout')
        selector.register(process.stderr, selectors.EVENT_READ, 'stderr')
        while selector.get_map() or process.poll() is None:
            tick(child_end)
            if output.exists() or output.is_symlink():
                info = output.lstat()
                require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and info.st_uid == os.geteuid()
                        and info.st_size <= OUTPUT_CAP and completed_bytes + info.st_size <= OUTPUT_TOTAL,
                        'packet_output_limit')
            events = selector.select(min(0.1, max(0, child_end - time.monotonic()))) if selector.get_map() else []
            if not selector.get_map():
                time.sleep(0.05)
            for key, _ in events:
                part = os.read(key.fileobj.fileno(), 65536)
                if not part:
                    selector.unregister(key.fileobj)
                else:
                    require(len(buffers[key.data]) + len(part) <= STREAM_CAP, 'child_pipe_limit')
                    buffers[key.data].extend(part)
        require(process.wait(timeout=max(0.01, child_end - time.monotonic())) == 0, 'probe_exit')
        require(not buffers['stdout'] and not buffers['stderr'], 'unexpected_probe_output')
    finally:
        # Do not let selector/receipt failures discard the actual owned handle.
        try:
            if process is not None:
                if process.returncode is None:
                    try:
                        process.kill()
                    except OSError:
                        pass  # Only authoritative wait below can prove retirement.
                try:
                    process.wait(timeout=5)
                except (subprocess.SubprocessError, OSError):
                    # Keep the leader/actual child custody alive. The mandatory
                    # outer Driver PGID guard supplies the final hard deadline.
                    while process.returncode is None:
                        try:
                            process.wait(timeout=0.2)
                        except (subprocess.SubprocessError, OSError):
                            continue
                    raise BatchError('cleanup_uncertain')
        finally:
            if selector is not None:
                selector.close()
            if process is not None:
                for stream in (process.stdout, process.stderr):
                    if stream is not None:
                        stream.close()


def execute(plan, source, end, reserved=()):
    global COMPLETED
    COMPLETED = 0
    root, binary = validate_plan(plan, source, end, reserved)
    rows, total = [], 0
    for row in plan['rows']:
        tick(end)
        before = digest(Path(row['input']), 16 * 1024**2, end, sealed=True)
        require(before[0] == row['input_sha256'], 'input_changed')
        require(digest(binary, 1024**3, end, tool=True) == (plan['ffprobe']['sha256'], plan['ffprobe']['bytes']), 'tool_changed')
        owned_path(row['output'], root, exists=False)
        probe(binary, Path(row['input']), Path(row['output']), root, end, total)
        sha, size = digest(Path(row['output']), OUTPUT_CAP, end)
        total += size
        require(total <= OUTPUT_TOTAL, 'aggregate_output_limit')
        require(digest(Path(row['input']), 16 * 1024**2, end, sealed=True) == before, 'input_changed')
        rows.append({'input': row['input'], 'output': row['output'], 'input_sha256': before[0],
                     'output_sha256': sha, 'bytes': size, 'exit': 0, 'reaped': True})
        COMPLETED = len(rows)
    # Bind all153 actual files again before a PASS receipt can be constructed.
    require(sum(Path(row['output']).lstat().st_size for row in rows) <= OUTPUT_TOTAL, 'aggregate_output_limit')
    for row in rows:
        require(digest(Path(row['output']), OUTPUT_CAP, end) == (row['output_sha256'], row['bytes']), 'output_changed')
        require(digest(Path(row['input']), 16 * 1024**2, end, sealed=True)[0] == row['input_sha256'], 'input_changed')
    require(digest(source, 1024**2, end)[0] == plan['worker_sha256'], 'worker_changed')
    require(digest(binary, 1024**3, end, tool=True) == (plan['ffprobe']['sha256'], plan['ffprobe']['bytes']), 'tool_changed')
    return {'schema': 'ottplay.multiview.hls-probe-receipt.v1', 'status': 'passed',
            'worker_sha256': plan['worker_sha256'], 'tool_sha256': plan['ffprobe']['sha256'],
            'rows': rows, 'output_bytes': total, 'outer_pgid_guard_required': True}


def main():
    end = time.monotonic() + 230
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--plan', type=Path, required=True)
    parser.add_argument('--receipt', type=Path, required=True)
    args = parser.parse_args()
    source = Path(__file__).resolve()
    result = None
    previous = {kind: signal.signal(kind, request_cancel) for kind in (signal.SIGTERM, signal.SIGINT)}
    try:
        info = args.plan.lstat()
        require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and info.st_uid == os.geteuid()
                and 0 < info.st_size <= PLAN_CAP, 'plan_size')
        with os.fdopen(os.open(args.plan, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), 'rb') as stream:
            raw = stream.read(PLAN_CAP + 1)
        require(0 < len(raw) <= PLAN_CAP, 'plan_size')
        plan = json.loads(raw)
        root = Path(plan['root'])
        owned_path(str(args.plan), root, exists=True)
        require(digest(args.plan, PLAN_CAP, end, sealed=True)[0] == hashlib.sha256(raw).hexdigest(), 'plan_changed')
        owned_path(str(args.receipt), root, exists=False)
        result = execute(plan, source, end, reserved=(args.plan, args.receipt))
        result['plan_sha256'] = hashlib.sha256(raw).hexdigest()
        require(digest(args.plan, PLAN_CAP, end, sealed=True)[0] == result['plan_sha256'], 'plan_changed')
        payload = json.dumps(result, separators=(',', ':')).encode()
        require(len(payload) <= RECEIPT_CAP, 'receipt_size')
        with args.receipt.open('xb') as receipt:
            os.chmod(args.receipt, 0o600)
            receipt.write(payload)
            receipt.flush()
            os.fsync(receipt.fileno())
        tick(end)
        return 0
    except (BatchError, OSError, ValueError, TypeError, KeyError, subprocess.SubprocessError, KeyboardInterrupt) as caught:
        # Outer Driver retains failure/group evidence; partial outputs stay owned
        # and are never labeled clean or converted into a passing receipt.
        reason = str(caught) if isinstance(caught, BatchError) else type(caught).__name__
        print(json.dumps({'status': 'failed', 'reason': reason, 'completed_probes': COMPLETED,
                          'partial_outputs_retained': True}), file=sys.stderr)
        return 1
    finally:
        for kind, handler in previous.items():
            signal.signal(kind, handler)


if __name__ == '__main__':
    sys.exit(main())
