"""Isolated Linux installer smoke test; never installs the real application."""
import ctypes
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


def main():
    # A broken root-install branch must never touch a real system directory.
    if os.geteuid() == 0:
        raise SystemExit("Run this test as an unprivileged user.")
    source = Path(__file__).resolve().parent
    gio = ctypes.CDLL("libgio-2.0.so.0")
    gio.g_desktop_app_info_new_from_filename.argtypes = [ctypes.c_char_p]
    gio.g_desktop_app_info_new_from_filename.restype = ctypes.c_void_p
    gio.g_app_info_launch.argtypes = [ctypes.c_void_p] * 4
    gio.g_app_info_launch.restype = ctypes.c_int
    gobject = ctypes.CDLL("libgobject-2.0.so.0")
    gobject.g_object_unref.argtypes = [ctypes.c_void_p]
    validator = shutil.which("desktop-file-validate")
    if validator:
        subprocess.run([validator, source / "rterm.desktop"], check=True)

    with tempfile.TemporaryDirectory(prefix="rterm-install-test-") as temp:
        root = Path(temp)
        archive = root / "archive"
        archive.mkdir()
        for name in ("install.sh", "rterm.desktop"):
            shutil.copyfile(source / name, archive / name)
        icon = source.parents[1] / "assets/rterm.png"
        shutil.copyfile(icon, archive / "rterm.png")
        (archive / "licenses").mkdir()
        (archive / "licenses/THIRD-PARTY-LICENSES.html").write_text("<!doctype html><title>Fixture notices</title>", encoding="utf-8")
        (archive / "licenses/RTerm-LICENSE").write_text("Fixture first-party notice", encoding="utf-8")
        for name in ("Inter-OFL.txt", "JetBrainsMono-OFL.txt"):
            shutil.copyfile(source.parents[1] / "assets" / name, archive / "licenses" / name)
        shutil.copyfile(
            source.parents[1] / "vendor/alacritty_terminal/LICENSE-APACHE",
            archive / "licenses/alacritty-LICENSE-APACHE",
        )
        for name in ("rterm", "rterm-agent"):
            stub = archive / name
            stub.write_text('#!/bin/sh\nprintf "launched\\n" > "$0.launched"\n')
            stub.chmod(0o755)

        def install(home, prefix=None, data=None, success=True):
            home.mkdir(exist_ok=True)
            env = dict(os.environ, HOME=str(home))
            for name, value in (("PREFIX", prefix), ("XDG_DATA_HOME", data)):
                env.pop(name, None)
                if value is not None:
                    env[name] = str(value)
            result = subprocess.run(
                ["sh", archive / "install.sh"], env=env,
                stdin=subprocess.DEVNULL, capture_output=True, text=True,
            )
            assert (result.returncode == 0) == success, result.stdout + result.stderr

        def check(prefix, data):
            executable = prefix / "bin/rterm"
            assert os.access(executable, os.X_OK)
            assert os.access(prefix / "bin/rterm-agent", os.X_OK)
            for notice in (archive / "licenses").iterdir():
                assert (prefix / "share/doc/rterm" / notice.name).read_bytes() == notice.read_bytes()
            launcher = data / "applications/rterm.desktop"
            assert launcher.read_bytes() == (
                prefix / "share/applications/rterm.desktop"
            ).read_bytes()
            assert not launcher.is_symlink()
            assert launcher.stat().st_mode & 0o777 == 0o644
            assert "Icon=rterm\n" in launcher.read_text()
            assert (data / "icons/hicolor/256x256/apps/rterm.png").read_bytes() == icon.read_bytes()
            if validator:
                subprocess.run([validator, launcher], check=True)
            # Verify GLib recognizes the file AND launches the exact stub.
            marker = executable.with_name("rterm.launched")
            marker.unlink(missing_ok=True)
            app = gio.g_desktop_app_info_new_from_filename(os.fsencode(launcher))
            assert app, f"GLib rejected {launcher}"
            try:
                assert gio.g_app_info_launch(app, None, None, None)
            finally:
                gobject.g_object_unref(app)
            for _ in range(100):
                if marker.exists():
                    break
                time.sleep(0.05)
            assert marker.read_text() == "launched\n"

        home = root / "home"
        install(home, data="relative")
        check(home / ".local", home / ".local/share")
        assert not (home / "Desktop").exists()
        for name in ("rterm-agent", "licenses/THIRD-PARTY-LICENSES.html"):
            missing = archive / name
            saved = missing.with_suffix(".saved")
            missing.rename(saved)
            incomplete = root / "incomplete"
            install(home, incomplete, success=False)
            assert not incomplete.exists()
            saved.rename(missing)

        data = root / "xdg"
        for name in ("custom space", '100% & $`"\\=path'):
            prefix = root / name
            install(home, prefix, data)
            check(prefix, data)

        # Reinstallation replaces the launcher, not an existing link target.
        launcher = data / "applications/rterm.desktop"
        sentinel = root / "sentinel"
        sentinel.write_text("untouched")
        launcher.unlink()
        launcher.symlink_to(sentinel)
        install(home, prefix, data)
        check(prefix, data)
        assert sentinel.read_text() == "untouched"

        # Two spellings of the same directory must not cause a duplicate copy.
        alias = root / "share-alias"
        alias.symlink_to(prefix / "share", target_is_directory=True)
        install(home, prefix, alias)
        check(prefix, alias)
        install(home, "relative", data, success=False)
        install(home, root / "bad\npath", data, success=False)
    print("Portable installer and GLib launcher smoke tests passed.")


if __name__ == "__main__":
    main()
