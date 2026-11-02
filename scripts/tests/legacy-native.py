"""Check legacy dispatch without running installation or system services."""
from pathlib import Path
import contextlib
import os
import subprocess
import tempfile
from unittest.mock import patch

source = Path(__file__).resolve().parents[1] / "legacy-native.py"
with tempfile.TemporaryDirectory(prefix="chain-legacy-native-") as directory:
    root = Path(directory)
    release = root / "release"
    (release / "scripts").mkdir(parents=True)
    (release / "bin").mkdir()
    native = release / "bin/chainctl"
    native.write_bytes(b"fixture: never execute")
    native.chmod(0o755)
    calls = []

    def invoke(script, args, imported=False, adopt_failure=False):
        namespace = {"__name__": "compat_import", "__file__": str(script)}
        with patch.object(os, "execv", side_effect=lambda *args: calls.append(("exec", args))), \
             patch.object(subprocess, "run", side_effect=(subprocess.CalledProcessError(1, "internal-adopt")
                          if adopt_failure else lambda *args, **kwargs: calls.append(("adopt", args, kwargs)))), \
             patch("sys.argv", [str(script), *args]):
            exec(compile(source.read_text(), str(script), "exec"), namespace)
            if not imported:
                namespace["main"]()

    for name in ("chain_common.py", "chain_platform.py", "chain_network.py", "chain_install.py", "chain_ingress.py", "chain_maintenance.py"):
        invoke(release / "scripts" / name, [], imported=True)
    assert calls == [], "importing a compatibility module must not start native operations"
    args = ["update", "--component", "panel", "--package", "package with spaces.tar.gz", "--non-interactive"]
    invoke(release / "scripts/chain_install.py", args)
    assert calls.pop() == ("exec", (str(native), [str(native), *args]))
    for action, command in {"apply": "internal-ingress-apply", "watch": "internal-ingress-watch", "enable": "ingress-enable", "disable": "ingress-disable"}.items():
        invoke(release / "scripts/chain_ingress.py", [action])
        assert calls.pop() == ("exec", (str(native), [str(native), command]))
    invoke(release / "scripts/chain_maintenance.py", [])
    assert calls == [("adopt", ([str(native), "internal-adopt"],), {"check": True}),
                     ("exec", (str(native), [str(native), "maintenance"]))]
    calls.clear()
    try:
        invoke(release / "scripts/chain_maintenance.py", [], adopt_failure=True)
    except subprocess.CalledProcessError:
        pass
    else:
        raise AssertionError("failed native migration was ignored")
    assert calls == []
    (root / "manager").mkdir()
    for role in ("agent", "panel"):
        (root / f"current-{role}").symlink_to(release, target_is_directory=True)
        invoke(root / "manager/chain_install.py", ["status"])
        expected = root / f"current-{role}/bin/chainctl"
        assert calls.pop() == ("exec", (str(expected), [str(expected), "status"]))
    for script, args in [(release / "scripts/chain_ingress.py", ["bad"]),
                         (release / "scripts/chain_maintenance.py", ["unexpected"]),
                         (root / "other/chain_install.py", [])]:
        with contextlib.suppress(SystemExit):
            invoke(script, args)
        assert calls == []
    native.unlink()
    try:
        invoke(root / "manager/chain_install.py", ["status"])
    except SystemExit:
        pass
    else:
        raise AssertionError("missing native executable was ignored")
assert not root.exists()
print("legacy native bridge assertions passed")
