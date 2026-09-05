"""An exited parent with a same-group or detached descendant holding pipes."""
import os
from pathlib import Path
import sys
import time

root = Path(sys.argv[1])
stream = sys.argv[2]
detached = sys.argv[3] == "detached"
lifetime = float(sys.argv[4])
parent_delay = float(sys.argv[5])
root.joinpath("parent.pid").write_text(str(os.getpid()))
print("stdout before parent exit", flush=True)
print("stderr before parent exit", file=sys.stderr, flush=True)
if os.fork() == 0:
    if detached:
        os.setsid()
    if stream == "stdout":
        os.close(2)
    elif stream == "stderr":
        os.close(1)
    root.joinpath("holder.pid").write_text(str(os.getpid()))
    time.sleep(lifetime)
    if stream != "stderr":
        print("stdout from descendant", flush=True)
    if stream != "stdout":
        print("stderr from descendant", file=sys.stderr, flush=True)
    os._exit(0)

# The parent exits only once the pipe holder has started. The test can observe
# parent.pid disappearing to prove that capture has entered post-exit drainage.
while not root.joinpath("holder.pid").exists():
    time.sleep(0.005)
time.sleep(parent_delay)
os._exit(0)
