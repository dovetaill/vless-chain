"""Core maintenance uses only private offline trees; no system services or real configuration."""
import copy
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import chain_cores as cores
import chain_maintenance as maintenance
from chain_common import json_write, sha256, verify_tree
from chain_install import Manager, OWNER


class Cores(unittest.TestCase):
    def setUp(self):
        self.assertNotEqual(os.geteuid(), 0, 'Run core tests as a nonprivileged user')
        temporary = tempfile.TemporaryDirectory(prefix='core-tests-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.manager = Manager(self.root)
        self.source = self.manager.base / 'releases/old'
        self.manifest = self.release(self.source)
        self.manager.base.joinpath('current-agent').symlink_to('releases/old')
        self.manager.base.joinpath('current-panel').symlink_to('releases/old')
        self.state = {'owner': OWNER, 'roles': {r: {'current': 'old', 'version': '1.0.0', 'previous': None} for r in ('agent', 'panel')}}
        json_write(self.manager.state_file, self.state)
        self.data = self.root / 'var/lib/chain-agent'
        self.data.mkdir(parents=True)
        self.data.joinpath('.chain-owner').write_text('chain-agent')
        self.config = {'inbounds': [{'protocol': 'vless', 'port': 443, 'settings': {'clients': [{'id': str(uuid.uuid4())}]},
                                    'streamSettings': {'security': 'reality', 'realitySettings': {'privateKey': 'fixture-original-key'}}}],
                       'outbounds': [{'protocol': 'freedom'}]}
        json_write(self.data / 'active.json', {'config': self.config})
        json_write(self.data / 'xray.json', self.config)
        self.link = self.data / 'original-link.txt'
        self.link.write_text('vless://' + self.config['inbounds'][0]['settings']['clients'][0]['id'] + '@example.test:443?fp=chrome&pbk=fixture&sid=0011')
        self.env = {'CHAIN_AGENT_DIR': str(self.data), 'CHAIN_XRAY_BIN': str(self.manager.base / 'current-agent/bin/xray')}
        env_path = self.root / 'etc/chain-agent/agent.env'
        env_path.parent.mkdir(parents=True)
        env_path.write_text(''.join(k + '=' + json.dumps(v) + '\n' for k, v in self.env.items()))
        self.root.joinpath('var/tmp').mkdir()
        self.jobs = self.root / 'jobs'
        self.jobs.mkdir()
        self.protected = [env_path, self.data / 'active.json', self.data / 'xray.json', self.link]
        self.before = self.metadata()
        self.manager.system = Mock(active=Mock(return_value=True))
        self.manager.healthy = Mock()

    def release(self, directory):
        files = ['bin/chain-panel', 'bin/chain-agent', 'bin/xray', 'bin/mihomo', 'scripts/chain_common.py',
                 'scripts/chain_install.py', 'scripts/chain_network.py', 'scripts/install-server.sh',
                 'share/Xray-LICENSE', 'share/Mihomo-LICENSE']
        manifest = {'format': 1, 'version': '1.0.0', 'arch': 'x86_64', 'xray': '26.3.27', 'mihomo': '1.19.32', 'files': {}}
        for name in files:
            path = directory / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('old-' + name)
            manifest['files'][name] = {'sha256': sha256(path), 'mode': 0o755 if name.startswith('bin/') else 0o644}
        json_write(directory / 'release.json', manifest, 0o644)
        return manifest

    def metadata(self):
        result = {}
        for path in self.protected:
            info = path.stat()
            result[str(path)] = (path.read_bytes(), info.st_ino, info.st_uid, info.st_gid, info.st_mode, info.st_ctime_ns)
        return result

    def download(self, core, version, directory, destination, prerelease=False):
        destination.parent.mkdir(parents=True)
        destination.write_text('candidate-' + core + '-' + version)
        license_path = destination.parent.parent / 'share' / ('Xray-LICENSE' if core == 'xray' else 'Mihomo-LICENSE')
        license_path.parent.mkdir()
        license_path.write_text('candidate-license')
        return {'version': version, 'source': 'official-fixture', 'archive_sha256': 'a' * 64, 'prerelease': prerelease}

    def job(self, core='xray', **fields):
        return {'id': str(uuid.uuid4()), 'action': 'core-upgrade', 'core': core, 'version': cores.RECOMMENDED[core], **fields}

    def upgrade(self, job=None):
        job = job or self.job()
        with patch.object(cores, 'download', side_effect=self.download), patch.object(cores, 'preflight'):
            result = cores.operate(self.manager, job, self.jobs)
        json_write(self.jobs / (job['id'] + '.json'), {**job, **result})
        return job, result

    def resolve(self, job, action):
        return cores.operate(self.manager, {'id': str(uuid.uuid4()), 'action': action, 'core': job['core'], 'task_id': job['id']}, self.jobs)

    def test_core_switch_preserves_uuid_link_metadata_and_other_core(self):
        job, result = self.upgrade()
        self.assertEqual(result['state'], 'awaiting_verification')
        self.assertEqual(self.metadata(), self.before)
        current = (self.manager.base / 'current-agent').resolve()
        self.assertEqual(verify_tree(current)['xray'], cores.RECOMMENDED['xray'])
        self.assertEqual((current / 'bin/mihomo').read_bytes(), (self.source / 'bin/mihomo').read_bytes())
        self.assertEqual(self.manager.state()['roles']['panel'], self.state['roles']['panel'])
        self.assertEqual((self.manager.base / 'current-panel').resolve(), self.source)
        self.resolve(job, 'core-confirm')
        self.assertTrue(self.manager.state()['core_history']['xray']['safe'])
        self.assertFalse((self.manager.base / '.core-pending.json').exists())
        self.assertEqual(self.metadata(), self.before)
        self.assertEqual(list(self.jobs.glob('work-*')), [])

    def test_failure_recovers_normal_upgrade_but_security_failure_stops_proxy(self):
        for security in (False, True):
            with self.subTest(security=security):
                job, _ = self.upgrade(self.job(security_update=security))
                result = self.resolve(job, 'core-reject')
                self.assertEqual(self.metadata(), self.before)
                if security:
                    self.assertIn('已停止', result['detail'])
                    self.assertEqual(self.manager.state()['core_blocked']['core'], 'xray')
                    self.assertEqual(self.manager.system.call.call_args.args, ('stop', 'chain-agent.service'))
                else:
                    self.assertIn('已恢复', result['detail'])
                    self.assertEqual((self.manager.base / 'current-agent').resolve(), self.source)

    def test_confirmed_safe_previous_version_can_recover_security_update(self):
        job, _ = self.upgrade(self.job(security_update=True, rollback_safe=True))
        self.resolve(job, 'core-reject')
        self.assertEqual((self.manager.base / 'current-agent').resolve(), self.source)
        self.assertNotIn('core_blocked', self.manager.state())

    def test_security_download_failure_stops_agent_without_changing_identity(self):
        with patch.object(cores, 'download', side_effect=ValueError('checksum mismatch')):
            with self.assertRaisesRegex(ValueError, 'checksum'):
                cores.operate(self.manager, self.job(security_update=True), self.jobs)
        self.assertEqual(self.manager.system.call.call_args.args, ('stop', 'chain-agent.service'))
        self.assertEqual(self.metadata(), self.before)
        self.assertEqual((self.manager.base / 'current-agent').resolve(), self.source)

    def test_failed_precheck_never_switches_and_configuration_changes_are_not_rolled_back(self):
        with patch.object(cores, 'download', side_effect=self.download), patch.object(cores, 'preflight', side_effect=ValueError('invalid config')):
            with self.assertRaisesRegex(ValueError, 'invalid config'):
                cores.operate(self.manager, self.job(), self.jobs)
        self.assertEqual((self.manager.base / 'current-agent').resolve(), self.source)
        self.manager.system.call.assert_not_called()
        job, _ = self.upgrade()
        self.data.joinpath('active.json').write_text('{"config":{"inbounds":[]}}')
        with self.assertRaisesRegex(ValueError, '配置已变化'):
            self.resolve(job, 'core-confirm')
        self.assertIn('已停止', self.resolve(job, 'core-reject')['detail'])
        self.assertEqual(self.data.joinpath('active.json').read_text(), '{"config":{"inbounds":[]}}')

    def test_package_update_preserves_selected_core_and_retains_only_referenced_history(self):
        job, _ = self.upgrade()
        self.resolve(job, 'core-confirm')
        stage = self.root / 'new-package'
        manifest = self.release(stage)
        cores.preserve(self.manager, stage, manifest)
        self.assertEqual(verify_tree(stage)['xray'], cores.RECOMMENDED['xray'])
        self.assertEqual(manifest['mihomo'], '1.19.32')
        self.assertTrue(manifest['xray_selected'])
        self.assertEqual(self.metadata(), self.before)

    def test_security_stop_marker_survives_until_safe_core_starts(self):
        job, _ = self.upgrade(self.job(security_update=True))
        self.resolve(job, 'core-reject')
        marker = self.data / '.core-upgrade-blocked'
        self.assertEqual(json.loads(marker.read_text())['task_id'], job['id'])
        self.assertTrue(self.manager.state()['core_blocked'])
        with self.assertRaisesRegex(ValueError, '历史安全确认已不适用'):
            cores.operate(self.manager, {'id':str(uuid.uuid4()), 'action':'core-rollback', 'core':'xray'}, self.jobs)
        replacement, _ = self.upgrade(self.job(version='26.10.22', security_update=True))
        self.assertFalse(marker.exists())
        self.resolve(replacement, 'core-confirm')
        self.assertFalse(self.manager.state().get('core_blocked'))
        self.assertEqual(self.metadata(), self.before)

    def test_interrupted_switch_can_restore_original_without_overwriting_configuration(self):
        job, _ = self.upgrade()
        # Simulate interruption after journaling, before the install-state write.
        json_write(self.manager.state_file, self.state)
        self.resolve(job, 'core-reject')
        self.assertEqual((self.manager.base / 'current-agent').resolve(), self.source)
        self.assertFalse((self.manager.base / '.core-pending.json').exists())
        self.assertEqual(self.metadata(), self.before)

    def test_legacy_and_mihomo_configuration_are_included(self):
        ident = str(uuid.uuid4())
        desired = {'config': {**self.config, '_legacy': [{'id': ident, 'asset_dir': '/fixture/assets'}], '_mihomo': {'listeners': []}}}
        json_write(self.data / 'active.json', desired)
        self.assertEqual(cores.configs(self.env, 'xray'), [(self.data / 'xray.json', ''), (self.data / f'legacy-{ident}/config.json', '/fixture/assets')])
        self.assertEqual(cores.configs(self.env, 'mihomo'), [(self.data / 'mihomo/config.json', '')])

    def test_rollback_one_core_preserves_the_other_selected_core(self):
        first, _ = self.upgrade()
        self.resolve(first, 'core-confirm')
        second, _ = self.upgrade(self.job(core='mihomo'))
        self.resolve(second, 'core-confirm')
        job = {'id':str(uuid.uuid4()), 'action':'core-rollback', 'core':'xray'}
        with patch.object(cores, 'preflight'):
            result = cores.operate(self.manager, job, self.jobs)
        self.assertEqual(result['state'], 'awaiting_verification')
        manifest = verify_tree((self.manager.base / 'current-agent').resolve())
        self.assertEqual(manifest['xray'], '26.3.27')
        self.assertEqual(manifest['mihomo'], cores.RECOMMENDED['mihomo'])
        self.assertEqual(self.metadata(), self.before)

    def test_failed_confirmation_exposes_error_on_original_pending_task(self):
        job, _ = self.upgrade()
        self.data.joinpath('active.json').write_text('{"config":{}}')
        confirmation = {'id':str(uuid.uuid4()), 'action':'core-confirm', 'core':'xray', 'task_id':job['id']}
        with patch('chain_install.Manager', return_value=self.manager), patch.object(maintenance, 'installed', return_value={'agent':'1.0.0'}):
            maintenance.execute(confirmation, self.jobs)
        original = json.loads((self.jobs / (job['id'] + '.json')).read_text())
        self.assertEqual(original['state'], 'awaiting_verification')
        self.assertIn('配置已变化', original['error'])
        self.assertTrue((self.manager.base / '.core-pending.json').exists())

    def test_receipts_pin_core_version_and_security_policy(self):
        pool = Mock()
        job = self.job(security_update=True)
        self.assertEqual(maintenance.submit(job, pool, self.jobs)['state'], 'queued')
        maintenance.submit(job, pool, self.jobs)
        self.assertEqual(pool.submit.call_count, 1)
        for field, value in [('core', 'mihomo'), ('version', '26.10.22'), ('security_update', False)]:
            with self.assertRaises(ValueError):
                maintenance.submit({**job, field: value}, pool, self.jobs)
        for payload in [{**self.job(), 'core': 'bad'}, self.job(version='../../bad'), self.job(security_update='true'),
                        {'id': str(uuid.uuid4()), 'action': 'restart-agent', 'core': 'xray'}]:
            with self.assertRaises((ValueError, KeyError)):
                maintenance.validate(payload)

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

    @unittest.skipUnless(sys.platform == 'linux', 'Linux sandbox')
    def test_validation_sandbox_blocks_external_writes_permissions_and_network(self):
        scratch = self.root / 'sandbox'
        scratch.mkdir()
        protected = self.root / 'untouched'
        protected.write_text('original')
        before = protected.stat()
        code = '''import sys,os,socket
from pathlib import Path
sys.path.insert(0,sys.argv[1])
from chain_cores import confined,deny_metadata_and_network
confined(sys.argv[2]);deny_metadata_and_network()
Path(sys.argv[2],"owned").write_text("ok")
for action in [lambda:Path(sys.argv[3]).write_text("bad"),lambda:os.chmod(sys.argv[3],0o777),lambda:socket.socket()]:
 try: action()
 except PermissionError: pass
 else: raise AssertionError("sandbox escape")
'''
        result = subprocess.run([sys.executable, '-B', '-c', code, str(Path(cores.__file__).parent), str(scratch), str(protected)],
                                capture_output=True, text=True, timeout=10)
        if 'Landlock ABI 3+' in result.stderr:
            self.skipTest('Host does not expose required Landlock; production preflight will refuse to execute')
        self.assertEqual(result.returncode, 0, result.stderr)
        after = protected.stat()
        self.assertEqual(protected.read_text(), 'original')
        self.assertEqual((before.st_uid, before.st_gid, before.st_mode, before.st_ctime_ns), (after.st_uid, after.st_gid, after.st_mode, after.st_ctime_ns))


if __name__ == '__main__':
    unittest.main()
