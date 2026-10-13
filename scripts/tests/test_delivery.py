#!/usr/bin/env python3
"""Delivery tests use an isolated installation tree, never real users/systemd."""
import argparse
import contextlib
import io
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import chain_common as common
import chain_install as installer
import chain_release as release


class Delivery(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='chain-delivery-test-')
        self.root = Path(self.temp.name)
        self.manager = installer.Manager(self.root / 'system')

    def tearDown(self):
        self.temp.cleanup()
        self.assertFalse(self.root.exists())

    def args(self, **kwargs):
        result = argparse.Namespace(action='install', component='panel', listen=None, secure_cookie=None,
                                    panel_url=None, node_id=None, token_file=None, allow_http=False)
        for key, value in kwargs.items():
            setattr(result, key, value)
        return result

    def package(self, version='0.1.0'):
        root = self.root / ('stage-' + version)
        (root / 'bin').mkdir(parents=True)
        (root / 'scripts').mkdir()
        for name in ['chain-panel', 'chain-agent', 'xray']:
            (root / 'bin' / name).write_text('fixture-' + version)
        for name in ['chain_common.py', 'chain_install.py', 'install-server.sh']:
            shutil.copy2(Path(__file__).resolve().parents[1] / name, root / 'scripts' / name)
        files = {str(p.relative_to(root)): {'sha256': common.sha256(p), 'mode': 0o755 if p.parent.name == 'bin' or p.suffix == '.sh' else 0o644}
                 for p in root.rglob('*') if p.is_file()}
        manifest = {'format': 1, 'version': version, 'arch': common.architecture(), 'glibc_min': '2.17', 'files': files}
        common.json_write(root / 'release.json', manifest)
        return root, manifest

    def archive(self, root):
        archive = self.root / (root.name + '.tar.gz')
        with tarfile.open(archive, 'w:gz') as bundle:
            for p in root.rglob('*'):
                if p.is_file():
                    bundle.add(p, arcname=str(p.relative_to(root)), recursive=False)
        (archive.parent / 'SHA256SUMS').write_text(common.sha256(archive) + '  ' + archive.name + '\n')
        return archive

    def test_verified_archive_and_checksum_tampering(self):
        source, manifest = self.package()
        archive = self.archive(source)
        common.verify_archive_checksum(archive)
        self.assertEqual(common.safe_extract(archive, self.root / 'extract'), manifest)
        archive.write_bytes(archive.read_bytes() + b'tampered')
        with self.assertRaisesRegex(ValueError, 'SHA256'):
            common.verify_archive_checksum(archive)

    def test_archive_rejects_escape_symlink_and_duplicate(self):
        for variant in ('escape', 'symlink', 'duplicate'):
            archive = self.root / (variant + '.tar.gz')
            with tarfile.open(archive, 'w:gz') as bundle:
                entry = tarfile.TarInfo('../outside' if variant == 'escape' else 'file')
                entry.size = 1
                if variant == 'symlink':
                    entry.type = tarfile.SYMTYPE
                    entry.linkname = '/etc/passwd'
                bundle.addfile(entry, io.BytesIO(b'x'))
                if variant == 'duplicate':
                    bundle.addfile(entry, io.BytesIO(b'y'))
            with self.assertRaises(ValueError):
                common.safe_extract(archive, self.root / variant)
        self.assertFalse((self.root / 'outside').exists())

    def test_file_manifest_tampering(self):
        source, _ = self.package()
        (source / 'bin/chain-panel').write_text('altered')
        with self.assertRaisesRegex(ValueError, '校验失败'):
            common.verify_tree(source)

    def test_install_upgrade_backup_and_rollback_preserve_state(self):
        source, manifest = self.package()
        self.manager.install(source, manifest, self.args(listen='127.0.0.1:8099', secure_cookie='1'))
        initial = self.manager.state()['roles']['panel']['current']
        etc, data, _ = self.manager.role_paths('panel')
        (data / 'admin-token').write_text('private-fixture')
        (data / 'panel.db').write_bytes(b'database-fixture')
        source2, manifest2 = self.package('0.2.0')
        self.manager.install(source2, manifest2, self.args(action='update'))
        state = self.manager.state()['roles']['panel']
        self.assertEqual(state['previous'], initial)
        self.assertEqual(installer.parse_env(etc / 'panel.env')['CHAIN_LISTEN'], '127.0.0.1:8099')
        self.assertEqual((data / 'panel.db').read_bytes(), b'database-fixture')
        self.assertEqual((etc / 'panel.env').stat().st_mode & 0o777, 0o600)
        backups = list(self.manager.path('/var/backups/chain-panel').glob('*.tar.gz'))
        self.assertEqual(len(backups), 1)
        self.assertEqual(backups[0].stat().st_mode & 0o777, 0o600)
        with tarfile.open(backups[0]) as bundle:
            self.assertEqual(bundle.extractfile('data/panel.db').read(), b'database-fixture')
        self.manager.rollback('panel')
        self.assertEqual(self.manager.state()['roles']['panel']['current'], initial)
        self.assertEqual((data / 'admin-token').read_text(), 'private-fixture')

    def test_failed_upgrade_restores_links_config_and_state(self):
        source, manifest = self.package()
        self.manager.install(source, manifest, self.args())
        original = self.manager.state()
        config = self.manager.role_paths('panel')[0] / 'panel.env'
        original_config = config.read_bytes()
        source2, manifest2 = self.package('0.2.0')
        with patch.object(self.manager, 'healthy', side_effect=ValueError('fixture startup failure')):
            with self.assertRaisesRegex(ValueError, 'startup failure'):
                self.manager.install(source2, manifest2, self.args(action='update', listen='127.0.0.1:9090'))
        self.assertEqual(self.manager.state(), original)
        self.assertEqual(config.read_bytes(), original_config)
        self.assertEqual((self.manager.base / 'current-panel').resolve().name, original['roles']['panel']['current'])

    def test_foreign_installation_and_service_are_not_overwritten(self):
        source, manifest = self.package()
        unit = self.manager.role_paths('panel')[2]
        unit.parent.mkdir(parents=True)
        unit.write_text('unrelated existing service')
        with self.assertRaisesRegex(ValueError, '拒绝覆盖'):
            self.manager.install(source, manifest, self.args())
        self.assertEqual(unit.read_text(), 'unrelated existing service')
        self.assertFalse(self.manager.base.exists())
        unit.unlink()
        self.manager.base.mkdir(parents=True)
        (self.manager.base / 'unrelated').write_text('keep')
        with self.assertRaisesRegex(ValueError, '拒绝覆盖'):
            self.manager.install(source, manifest, self.args())
        self.assertEqual((self.manager.base / 'unrelated').read_text(), 'keep')

    def test_agent_identity_requirements_and_role_isolation(self):
        source, manifest = self.package()
        self.manager.install(source, manifest, self.args())
        panel = self.manager.state()['roles']['panel'].copy()
        with self.assertRaisesRegex(ValueError, '需要 --panel-url'):
            self.manager.install(source, manifest, self.args(component='agent'))
        token = self.root / 'node-token'
        token.write_text('a' * 64)
        token.chmod(0o600)
        args = self.args(component='agent', panel_url='https://panel.example.com', node_id='a94a6794-c60a-4c51-a094-b9164ddaaabc', token_file=token)
        self.manager.install(source, manifest, args)
        self.assertEqual(self.manager.state()['roles']['panel'], panel)
        etc, _, _ = self.manager.role_paths('agent')
        self.assertEqual((etc / 'token').read_text().strip(), 'a' * 64)
        self.assertEqual((etc / 'token').stat().st_mode & 0o777, 0o600)
        with self.assertRaisesRegex(ValueError, '需要 HTTPS'):
            self.manager.config('agent', self.args(component='agent', panel_url='http://10.0.0.2:8088', node_id=args.node_id))

    def test_bad_arch_and_glibc_fail_before_changes(self):
        source, manifest = self.package()
        for wrong in [{'arch': 'unsupported'}, {'glibc_min': '999.0'}]:
            with self.assertRaises(ValueError):
                self.manager.install(source, {**manifest, **wrong}, self.args())
            self.assertFalse(self.manager.base.exists())

    @unittest.skipUnless(shutil.which('systemd-analyze'), 'systemd-analyze 未安装')
    def test_service_units_parse_without_ignored_environment_files(self):
        manager = installer.Manager(self.root / 'system with spaces')
        for role in installer.ROLES:
            binary = manager.base / ('current-' + role) / 'bin' / ('chain-' + role)
            binary.parent.mkdir(parents=True)
            shutil.copy2('/bin/true', binary)
            unit = manager.role_paths(role)[2]
            unit.parent.mkdir(parents=True, exist_ok=True)
            unit.write_text(manager.unit(role))
            result = subprocess.run(['systemd-analyze', 'verify', str(unit)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn('EnvironmentFile=', result.stderr)
            self.assertNotIn('Failed to parse', result.stderr)

    def test_version_updates_all_four_manifests(self):
        repo = self.root / 'repo'
        for name in ['panel/Cargo.toml', 'panel/Cargo.lock', 'panel/frontend/package.json', 'panel/frontend/package-lock.json']:
            destination = repo / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(release.ROOT / name, destination)
        with patch.object(release, 'ROOT', repo):
            release.set_version('0.3.7')
            self.assertEqual(release.version(), '0.3.7')
            before = (repo / 'panel/Cargo.toml').read_bytes()
            with self.assertRaises(ValueError):
                release.set_version('bad; version')
            self.assertEqual((repo / 'panel/Cargo.toml').read_bytes(), before)

    def test_workspace_stops_owned_process_and_releases_port(self):
        with socket.socket() as reserve:
            reserve.bind(('127.0.0.1', 0))
            port = reserve.getsockname()[1]
        with common.Workspace(self.root) as work:
            directory = work.root
            work.manifest['ports'] = [port]
            work.record()
            code = "import socket,time,pathlib; s=socket.socket(); s.bind(('127.0.0.1', %d)); s.listen(); pathlib.Path(%r).write_text('ready'); time.sleep(45)" % (port, str(work.root / 'ready'))
            process = subprocess.Popen([sys.executable, '-c', code], env=work.env, start_new_session=True)
            work.children.append(process)
            work.manifest['groups'].append(process.pid)
            work.record()
            deadline = time.monotonic() + 5
            while not (work.root / 'ready').exists():
                self.assertLess(time.monotonic(), deadline)
                time.sleep(.02)
        self.assertIsNotNone(process.poll())
        with self.assertRaises(ProcessLookupError):
            os.killpg(process.pid, 0)
        self.assertFalse(directory.exists())
        with socket.socket() as check:
            check.bind(('127.0.0.1', port))
        print(f'CLEANUP verification PGID={process.pid}; port={port} released; directory removed.')


if __name__ == '__main__':
    unittest.main(verbosity=2)
