"""Run the Windows hook-capture command exactly as the hook runner does, with timing."""
import os
import subprocess
import tempfile
import time

root = tempfile.mkdtemp(prefix="willdeep-hook-")
captured = os.path.join(root, "payload.json")
payload = '{"event":"pre_tool","tool":"run_command","detail":"cargo test"}'
for label, command in [
    ("write", f"[IO.File]::WriteAllText('{captured}', [Console]::In.ReadToEnd())"),
    ("append", f"[IO.File]::AppendAllText('{captured}.append', [Console]::In.ReadToEnd())"),
    ("input", f"$input | Set-Content -Path '{captured}.input'"),
]:
    started = time.monotonic()
    result = subprocess.run(
        ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", command],
        input=payload.encode(), capture_output=True, timeout=60,
    )
    elapsed = time.monotonic() - started
    print(f"===== {label}: exit={result.returncode} elapsed={elapsed:.2f}s", flush=True)
    print("command:", command)
    print("stdout:", result.stdout.decode(errors="replace"))
    print("stderr:", result.stderr.decode(errors="replace"))
    print("files:", os.listdir(root), flush=True)
