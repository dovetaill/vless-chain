"""Compatibility entrypoints used only while pre-native installations upgrade."""
import os
from pathlib import Path
import subprocess
import sys


def main():
    script = Path(__file__).resolve()
    if script.parent.name == "scripts":
        candidates = [script.parent.parent / "bin/chainctl"]
    elif script.parent.name == "manager":
        candidates = [script.parent.parent / f"current-{role}/bin/chainctl"
                      for role in ("panel", "agent")]
    else:
        raise SystemExit("旧版兼容入口位置无效，请运行新版 install-server.sh update")
    native = next((path for path in candidates if path.is_file() and os.access(path, os.X_OK)), None)
    if native is None:
        raise SystemExit("缺少原生 chainctl，请运行新版 install-server.sh update")
    args = sys.argv[1:]
    if script.name == "chain_maintenance.py" and not args:
        # The old maintainer exits only after persisting its upgrade receipt.
        subprocess.run([str(native), "internal-adopt"], check=True)
        args = ["maintenance"]
    elif script.name == "chain_ingress.py" and len(args) == 1:
        actions = {"apply": "internal-ingress-apply", "watch": "internal-ingress-watch",
                   "enable": "ingress-enable", "disable": "ingress-disable"}
        if args[0] not in actions:
            raise SystemExit("旧版入口操作无效")
        args = [actions[args[0]]]
    elif script.name != "chain_install.py":
        raise SystemExit("此旧版模块仅用于升级兼容，请使用原生 chainctl")
    os.execv(str(native), [str(native), *args])


if __name__ == "__main__":
    main()
