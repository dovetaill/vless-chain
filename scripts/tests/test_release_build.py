#!/usr/bin/env python3
"""Delivery tests use an isolated installation tree, never real users/systemd."""
import io
import hashlib
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
import urllib.error
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import chain_common as common
import chain_release as release
import ci_release as ci
import build_cores as cores


class Delivery(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='chain-build-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def test_official_asset_checks_source_digest_draft_and_prerelease(self):
        name = 'Xray-linux-64.zip'
        item = {'name': name, 'browser_download_url': f'https://github.com/XTLS/Xray-core/releases/download/v26.10.21/{name}', 'digest': 'sha256:' + 'a' * 64}
        release = {'tag_name': 'v26.10.21', 'draft': False, 'prerelease': False, 'assets': [item]}
        with patch.object(cores, 'operating_system', return_value='linux'), patch.object(cores, 'architecture', return_value='x86_64'):
            self.assertEqual(cores.asset(release, 'xray', '26.10.21')[2], 'a' * 64)
            for key, value in [('draft', True), ('tag_name', 'v26.10.22'), ('prerelease', True), ('assets', [item, item])]:
                with self.assertRaises(ValueError):
                    cores.asset({**release, key: value}, 'xray', '26.10.21')
            for key, value in [('browser_download_url', 'https://example.test/xray.zip'), ('digest', 'md5:bad')]:
                with self.assertRaises(ValueError):
                    cores.asset({**release, 'assets': [{**item, key: value}]}, 'xray', '26.10.21')

    def test_download_token_is_only_sent_to_api_and_never_redirected(self):
        api = 'https://api.github.com/repos/XTLS/Xray-core/releases/tags/v26.10.21'
        urls = [api, 'https://github.com/XTLS/Xray-core/releases/download/v26.10.21/Xray-linux-64.zip',
                'https://raw.githubusercontent.com/MetaCubeX/mihomo/v1.19.33/LICENSE',
                'https://api.github.com.example.test/file', 'https://api.github.com@example.test/file']
        for token in ('fixture-token', ''):
            for index, url in enumerate(urls):
                with self.subTest(token=bool(token), url=url):
                    response = io.BytesIO(b'fixture')
                    response.geturl = lambda: url
                    path = self.root / f'download-{bool(token)}-{index}'
                    with patch.dict(os.environ, {'GH_TOKEN': token}), patch.object(cores.urllib.request, 'urlopen', return_value=response) as opened:
                        cores.fetch(url, path, 64)
                    request = opened.call_args.args[0]
                    expected = 'Bearer fixture-token' if token and url == api else None
                    self.assertEqual(request.get_header('Authorization'), expected)
                    redirected = cores.urllib.request.HTTPRedirectHandler().redirect_request(
                        request, None, 302, 'Found', {}, 'https://release-assets.githubusercontent.com/file')
                    self.assertIsNone(redirected.get_header('Authorization'))
                    self.assertEqual(path.read_bytes(), b'fixture')

    def test_ci_check_accepts_branch_but_release_actions_require_tag(self):
        with patch.object(sys, 'argv', ['ci_release.py', 'check', '--tag', 'main']), \
                patch.object(ci, 'check') as check:
            ci.main()
            check.assert_called_once_with()
        for action in ('validate', 'assemble', 'publish'):
            with self.subTest(action=action), \
                    patch.object(sys, 'argv', ['ci_release.py', action, '--tag', 'main']):
                with self.assertRaisesRegex(ValueError, '只能发布'):
                    ci.main()

    def test_packaged_bootstrap_dispatches_to_native_without_python(self):
        stage, _ = self.package()
        binary = stage / 'bin/chainctl'
        binary.write_text('#!/bin/sh\nprintf "%s\\n" "$@"\n')
        binary.chmod(0o755)
        script = stage / 'scripts/install-server.sh'
        for args, expected in [([], ['setup']),
                               (['--non-interactive'], ['setup', '--non-interactive']),
                               (['update', '--component', 'agent'], ['update', '--component', 'agent'])]:
            result = subprocess.run(['/bin/sh', str(script), *args], capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.splitlines(), expected)


    def test_build_cache_requires_explicit_github_workflow_opt_in(self):
        original = {'CARGO_TARGET_DIR': str(self.root / 'target'),
                    'CARGO_HOME': str(self.root / 'cargo'), 'npm_config_cache': str(self.root / 'npm')}
        for flags in ({}, {'CHAIN_BUILD_CACHE': '1'}, {'GITHUB_ACTIONS': 'true'}):
            with patch.dict(os.environ, flags, clear=True):
                env = original.copy()
                release.configure_ci_cache(env)
                self.assertEqual(env, original)
        alias = self.root / 'runner-temp-alias'
        alias.symlink_to(self.root, target_is_directory=True)
        for runner_temp in (self.root, alias):
            with self.subTest(runner_temp=runner_temp), patch.dict(os.environ,
                    {'GITHUB_ACTIONS': 'true', 'CHAIN_BUILD_CACHE': '1',
                     'RUNNER_TEMP': str(runner_temp)}, clear=True):
                env = original.copy()
                release.configure_ci_cache(env)
                cache = self.root.resolve() / 'chain-build-cache'
                self.assertEqual(env, {'CARGO_TARGET_DIR': str(cache / 'target'),
                                       'CARGO_HOME': str(cache / 'cargo'), 'npm_config_cache': str(cache / 'npm')})
                self.assertFalse(cache.exists())


    def package(self, version='0.1.0'):
        root = self.root / ('stage-' + version)
        (root / 'bin').mkdir(parents=True)
        (root / 'scripts').mkdir()
        for name in ['chain-panel', 'chain-agent', 'chainctl', 'xray']:
            (root / 'bin' / name).write_text('fixture-' + version)
        for name in ['chain_common.py', 'chain_install.py', 'chain_network.py', 'chain_platform.py']:
            shutil.copy2(Path(__file__).resolve().parents[1] / 'legacy_bridge.py', root / 'scripts' / name)
        shutil.copy2(Path(__file__).resolve().parents[1] / 'install-server.sh', root / 'scripts/install-server.sh')
        files = {str(p.relative_to(root)): {'sha256': common.sha256(p), 'mode': 0o755 if p.parent.name == 'bin' or p.suffix == '.sh' else 0o644}
                 for p in root.rglob('*') if p.is_file()}
        manifest = {'format': 1, 'version': version, 'arch': common.architecture(), 'os': common.operating_system(), 'libc': 'musl', 'macos_min': '14.0', 'files': files}
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


    def test_common_run_allows_best_effort_cleanup(self):
        self.assertEqual(common.run([sys.executable, '-c', 'raise SystemExit(3)'], check=False).returncode, 3)


    def test_online_download_missing_release_and_integrity_failure(self):
        request_error = urllib.error.HTTPError('https://api.github.com/', 404, 'Not found', {}, None)
        with patch('urllib.request.urlopen', side_effect=request_error):
            with self.assertRaisesRegex(ValueError, '尚无已发布'):
                common.download_release(self.root)
        name = f'chain-panel-0.1.0-{common.operating_system()}-{common.architecture()}.tar.gz'
        release_info = {'tag_name': 'v0.1.0', 'assets': [{'name': n, 'browser_download_url': f'https://github.com/{common.RELEASE_REPO}/releases/download/v0.1.0/{n}'} for n in [name, 'SHA256SUMS']]}
        class Response(io.BytesIO):
            def geturl(self):
                return 'https://release-assets.githubusercontent.com/fixture'
        def fetch(request, **_):
            url = request.full_url if hasattr(request, 'full_url') else request
            if 'api.github' in url:
                return Response(json.dumps(release_info).encode())
            return Response((('0' * 64 + '  ' + name + '\n').encode()) if url.endswith('SHA256SUMS') else b'tampered archive')
        with patch('urllib.request.urlopen', side_effect=fetch):
            with self.assertRaisesRegex(ValueError, 'SHA256'):
                common.download_release(self.root)


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


    def release_repo(self):
        repo = self.root / 'release-repo'
        repo.mkdir()
        def git(*args):
            return common.output(['git', *args], cwd=repo)
        git('init', '-b', 'main')
        git('config', 'user.name', 'Release Test')
        git('config', 'user.email', 'release@example.invalid')
        git('config', 'commit.gpgsign', 'false')
        git('config', 'tag.gpgsign', 'false')
        for name in ['panel/Cargo.toml', 'panel/Cargo.lock', 'panel/frontend/package.json', 'panel/frontend/package-lock.json']:
            destination = repo / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(release.ROOT / name, destination)
        for name in ['README.md', '.gitignore', 'scripts/fixture.py', '.github/fixture.yml', 'unrelated.txt']:
            destination = repo / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text('fixture\n')
        git('add', '.')
        git('commit', '-m', '测试初始提交')
        remote = self.root / 'origin.git'
        common.output(['git', 'init', '--bare', remote])
        git('remote', 'add', 'origin', str(remote))
        git('push', 'origin', 'main')
        return repo, git

    def test_release_wizard_cancel_publish_and_retry(self):
        repo, git = self.release_repo()
        (repo / 'panel/new.txt').write_text('new\n')
        git('add', 'panel/new.txt')
        git('commit', '-m', '提交业务改动')
        (repo / 'unrelated.txt').write_text('keep\n')
        (repo / 'theme.zip').write_bytes(b'keep')
        answers = ['', 'y', '']
        with patch.object(release, 'ROOT', repo), patch.object(release.sys.stdin, 'isatty', return_value=True):
            before = git('rev-parse', 'HEAD')
            old = release.version()
            parts = old.split('.')
            expected = '.'.join([*parts[:2], str(int(parts[2]) + 1)])
            tag = 'v' + expected
            with patch('builtins.input', side_effect=['', 'n']):
                release.release_wizard()
            self.assertEqual(release.version(), old)
            self.assertEqual(git('rev-parse', 'HEAD'), before)
            self.assertFalse(git('diff', '--cached', '--name-only'))
            original_run = release.run
            def fail_push(command, **kwargs):
                if command[:2] == ['git', 'push']:
                    raise subprocess.CalledProcessError(1, command)
                return original_run(command, **kwargs)
            with patch('builtins.input', side_effect=answers), patch.object(release, 'run', side_effect=fail_push):
                with self.assertRaises(subprocess.CalledProcessError):
                    release.release_wizard()
            self.assertEqual(release.version(), expected)
            commit = git('rev-parse', 'HEAD')
            self.assertEqual(git('rev-parse', tag + '^{commit}'), commit)
            self.assertNotIn(tag, git('ls-remote', '--tags', 'origin'))
            with patch('builtins.input', return_value='y') as prompt:
                release.release_wizard()
            self.assertEqual(prompt.call_count, 1)
            self.assertEqual(git('rev-parse', 'HEAD'), commit)
            self.assertIn(commit + '\trefs/heads/main', git('ls-remote', 'origin', 'refs/heads/main'))
            self.assertIn(commit + '\trefs/tags/' + tag + '^{}', git('ls-remote', '--tags', 'origin'))
            names = git('show', '--pretty=format:', '--name-only', 'HEAD')
            self.assertEqual(set(names.splitlines()), {'panel/Cargo.toml', 'panel/Cargo.lock',
                                                     'panel/frontend/package.json', 'panel/frontend/package-lock.json'})
            self.assertEqual(git('show', 'HEAD:panel/new.txt'), 'new')
            self.assertNotIn('unrelated.txt', names)
            self.assertNotIn('theme.zip', names)
            self.assertIn('背景与目的', git('log', '-1', '--format=%B'))
            self.assertIn('unrelated.txt', git('status', '--porcelain'))


    def test_release_wizard_rejects_staging_and_existing_tag(self):
        repo, git = self.release_repo()
        with patch.object(release, 'ROOT', repo), patch.object(release.sys.stdin, 'isatty', return_value=True):
            (repo / 'unrelated.txt').write_text('staged\n')
            git('add', 'unrelated.txt')
            with self.assertRaisesRegex(ValueError, '暂存区'):
                release.release_wizard()
            self.assertEqual(git('diff', '--cached', '--name-only'), 'unrelated.txt')
            git('reset', 'HEAD', '--', 'unrelated.txt')
            (repo / 'panel/pending.txt').write_text('not committed\n')
            with patch('builtins.input', side_effect=AssertionError('must reject before prompting')):
                with self.assertRaisesRegex(ValueError, '请先提交发行源码'):
                    release.release_wizard()
            (repo / 'panel/pending.txt').unlink()
            git('config', 'remote.origin.pushurl', str(self.root / 'different.git'))
            with self.assertRaisesRegex(ValueError, '拉取与推送地址不同'):
                release.release_wizard()
            git('config', '--unset', 'remote.origin.pushurl')
            git('tag', 'v99.0.0')
            with patch('builtins.input', return_value='99.0.0'):
                with self.assertRaisesRegex(ValueError, '标签已经存在'):
                    release.release_wizard()


    def test_release_wizard_custom_version(self):
        repo, git = self.release_repo()
        with patch.object(release, 'ROOT', repo), patch.object(release.sys.stdin, 'isatty', return_value=True):
            with patch('builtins.input', side_effect=['98.1.0', 'y', '']):
                release.release_wizard()
            self.assertEqual(release.version(), '98.1.0')
            commit = git('rev-parse', 'HEAD')
            self.assertIn(commit + '\trefs/tags/v98.1.0^{}', git('ls-remote', '--tags', 'origin'))


    def test_release_skip_then_push_existing_version(self):
        repo, git = self.release_repo()
        with patch.object(release, 'ROOT', repo), patch.object(release.sys.stdin, 'isatty', return_value=True):
            with self.assertRaisesRegex(ValueError, '本地没有'):
                release.main(['push'])
            before = git('rev-parse', 'HEAD')
            with patch('builtins.input', side_effect=['98.2.0', 'y', 'n']):
                release.release_wizard()
            commit = git('rev-parse', 'HEAD')
            self.assertNotEqual(before, commit)
            self.assertEqual(git('rev-parse', 'v98.2.0^{commit}'), commit)
            self.assertFalse(git('ls-remote', '--tags', 'origin'))
            self.assertIn(before + '\trefs/heads/main', git('ls-remote', 'origin', 'refs/heads/main'))
            with patch('builtins.input', return_value='n'):
                release.main(['push'])
            self.assertFalse(git('ls-remote', '--tags', 'origin'))
            with patch('builtins.input', return_value='') as prompt:
                release.main(['push'])
            self.assertEqual(prompt.call_count, 1)
            self.assertEqual(release.version(), '98.2.0')
            self.assertEqual(git('rev-parse', 'HEAD'), commit)
            self.assertIn(commit + '\trefs/tags/v98.2.0^{}', git('ls-remote', '--tags', 'origin'))


    def test_supported_release_aggregation_and_publish_reject_dirty_builds(self):
        source, manifest = self.package()
        incoming = self.root / 'incoming'
        commit = 'a' * 40
        for system, arch in [('linux', 'x86_64'), ('linux', 'aarch64'), ('darwin', 'aarch64')]:
            manifest.update(os=system, arch=arch, target=f'{arch}-unknown-linux-musl' if system == 'linux' else f'{arch}-apple-darwin', commit=commit, dirty=False)
            common.json_write(source / 'release.json', manifest)
            archive = self.archive(source)
            destination = incoming / f'release-{system}-{arch}'
            destination.mkdir(parents=True)
            name = f'chain-panel-0.1.0-{system}-{arch}.tar.gz'
            shutil.copy2(archive, destination / name)
            shutil.copy2(source / 'release.json', destination / 'release.json')
            (destination / 'SHA256SUMS').write_text(f'{common.sha256(archive)}  {name}\n')
        outgoing = self.root / 'published'
        with patch.object(ci, 'version', return_value='0.1.0'):
            ci.assemble(incoming, outgoing, 'v0.1.0', commit)
        self.assertEqual(len(list(outgoing.iterdir())), 7)
        for archive in outgoing.glob('*.tar.gz'):
            common.verify_archive_checksum(archive)
        with patch.object(ci, 'version', return_value='0.1.0'), \
                patch.object(ci, 'output', side_effect=RuntimeError('release lookup')):
            with self.assertRaisesRegex(RuntimeError, 'release lookup'):
                ci.publish(outgoing, 'v0.1.0', 'owner/repo', commit)
        manifest['dirty'] = True
        common.json_write(source / 'release.json', manifest)
        archive = self.archive(source)
        destination = incoming / 'release-darwin-aarch64'
        name = 'chain-panel-0.1.0-darwin-aarch64.tar.gz'
        shutil.copy2(archive, destination / name)
        shutil.copy2(source / 'release.json', destination / 'release.json')
        (destination / 'SHA256SUMS').write_text(f'{common.sha256(archive)}  {name}\n')
        with patch.object(ci, 'version', return_value='0.1.0'), self.assertRaisesRegex(ValueError, '拒绝发布'):
            ci.assemble(incoming, self.root / 'rejected', 'v0.1.0', commit)
        self.assertFalse((self.root / 'rejected').exists())


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
    unittest.main()
