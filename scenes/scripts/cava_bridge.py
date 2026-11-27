#!/usr/bin/env python3
"""Run external CAVA and atomically publish its latest text frame."""
import os
from pathlib import Path
import subprocess
import tempfile

runtime = Path(os.environ["XDG_RUNTIME_DIR"])
target = runtime / "floatpaper-cava.txt"
config = Path(__file__).resolve().with_name("cava.conf")
blocks = " ▁▂▃▄▅▆▇█"
with tempfile.TemporaryDirectory(prefix="floatpaper-cava-", dir=runtime) as directory:
    staging = Path(directory) / "frame"
    with subprocess.Popen(["cava", "-p", str(config)], stdout=subprocess.PIPE, text=True) as cava:
        try:
            for line in cava.stdout:
                values = line.strip().rstrip(";").split(";")
                if len(values) != 48:
                    raise ValueError(f"Expected 48 CAVA bars, received {len(values)}: {line!r}")
                frame = "".join(blocks[max(0, min(8, int(value)))] for value in values)
                staging.write_text(frame, encoding="utf-8")
                staging.replace(target)
            raise RuntimeError(f"CAVA stopped (exit code {cava.wait()})")
        except KeyboardInterrupt:
            pass
        finally:
            if cava.poll() is None:
                cava.terminate()
            cava.wait()
            target.unlink(missing_ok=True)
