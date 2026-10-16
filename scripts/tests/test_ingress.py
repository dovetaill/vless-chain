"""SNI config validation and transactional restoration; no host services are modified."""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from chain_ingress import Ingress, OWNER, migrate, route_config


class IngressTest(unittest.TestCase):
    def setUp(self):
        for mock in [patch('chain_ingress.service_kind', return_value='systemd'),
                     patch('chain_ingress.service_path', side_effect=lambda name:
                         Path('/etc/systemd/system') / (name + '.service'))]:
            mock.start()
            self.addCleanup(mock.stop)

    def test_routes_cannot_override_sites_or_inject_configuration(self):
        self.assertIn('example.com 127.0.0.1:50001;', route_config([{'sni':'example.com','port':50001}], []))
        self.assertIn('example.com 127.0.0.1:30000;', route_config([{'sni':'example.com','port':30000}], []))
        for routes, blocked in [
            ([{'sni':'panel.example.com','port':50001}], ['*.example.com']),
            ([{'sni':'example.com; include /etc/passwd','port':50001}], []),
            ([{'sni':'example.com','port':22}], []),
            ([{'sni':'example.com','port':50001,'target':'evil'}], []),
            ([{'sni':'example.com','port':50001}]*2, []),
        ]:
            with self.assertRaises(ValueError): route_config(routes, blocked)

    def test_migration_preserves_sites_and_udp_443(self):
        text = 'server {\n listen 443 ssl default_server;\n listen [::]:443 ssl;\n listen 443 quic reuseport;\n server_name site.example;\n ssl_certificate /unchanged/cert;\n}\n'
        value = migrate(text, 14443)
        self.assertIn('listen 127.0.0.1:14443 ssl default_server proxy_protocol;', value)
        self.assertIn('listen [::1]:14443 ssl proxy_protocol;', value)
        self.assertIn('listen 443 quic reuseport;', value)
        self.assertIn('ssl_certificate /unchanged/cert;', value)
        for value in ['listen 192.0.2.1:443 ssl;', 'listen 443;', 'listen 443 ssl proxy_protocol;']:
            with self.assertRaises(ValueError): migrate(value, 14443)

    def fixture(self, root):
        ingress = Ingress(root)
        main = root/'etc/nginx/nginx.conf'
        main.parent.mkdir(parents=True)
        main.write_text('load_module modules/ngx_stream_module.so;\nhttp {\n include sites/*.conf;\n}\n')
        site = main.parent/'site.conf'
        site.write_text('server {\n listen 443 ssl;\n server_name existing.example;\n ssl_certificate /cert;\n}\n')
        units = root/'etc/systemd/system'
        units.mkdir(parents=True)
        (units/'chain-agent.service').write_text('existing agent')
        dump = '# configuration file '+str(main)+':\n'+main.read_text()+'# configuration file '+str(site)+':\n'+site.read_text()
        return ingress, main, site, dump

    def test_enable_disable_and_failed_reload_restore_exact_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            ingress, main, site, dump = self.fixture(Path(temporary))
            before = [main.read_bytes(), site.read_bytes()]
            def command(*args): return dump if args == ('nginx','-T') else ''
            with patch.object(ingress, 'command', side_effect=command):
                ingress.enable()
                self.assertIn('proxy_protocol on;', (ingress.config/'stream.conf').read_text())
                self.assertNotIn('listen [::]:443;', (ingress.config/'stream.conf').read_text())
                self.assertEqual(json.loads(ingress.status_path.read_text())['ready'], True)
                ingress.disable()
                self.assertEqual([main.read_bytes(),site.read_bytes()], before)
                self.assertFalse(ingress.config.exists())
            with patch.object(ingress, 'command', side_effect=command), patch.object(ingress, 'reload', side_effect=ValueError('test reload failure')):
                with self.assertRaises(ValueError): ingress.enable()
            self.assertEqual([main.read_bytes(),site.read_bytes()], before)
            self.assertFalse(ingress.state_path.exists())

    def test_manual_edits_and_unowned_units_are_preserved(self):
        with tempfile.TemporaryDirectory() as temporary:
            ingress, main, site, dump = self.fixture(Path(temporary))
            unit=ingress.path('/etc/systemd/system/chain-ingress.path')
            unit.write_text('unrelated service')
            with patch.object(ingress, 'command', return_value=dump) as command:
                with self.assertRaises(ValueError): ingress.enable()
                self.assertEqual(unit.read_text(),'unrelated service')
                command.assert_not_called()
            unit.unlink()
            def command(*args): return dump if args == ('nginx','-T') else ''
            with patch.object(ingress, 'command', side_effect=command):
                ingress.enable()
                site.write_text(site.read_text()+'# manual edit\n')
                with self.assertRaises(ValueError): ingress.disable()
                self.assertTrue(site.read_text().endswith('# manual edit\n'))

    def test_alpine_empty_stream_block_is_reused_and_restored(self):
        with tempfile.TemporaryDirectory() as temporary:
            ingress, main, site, dump = self.fixture(Path(temporary))
            stream = main.parent / 'conf.d/stream.conf'
            stream.parent.mkdir()
            stream.write_text("# Alpine module default\nstream {\n"
                " log_format main '$remote_addr [$time_local] ' '$protocol $status';\n"
                " access_log /var/log/nginx/stream.log main;\n include stream.d/*.conf;\n}\n")
            dump += '# configuration file ' + str(stream) + ':\n' + stream.read_text()
            paths = [main, site, stream]
            before = [p.read_bytes() for p in paths]
            def command(*args): return dump if args == ('nginx', '-T') else ''
            with patch.object(ingress, 'command', side_effect=command):
                ingress.enable()
                self.assertIn(f'include {ingress.config}/stream.conf;', stream.read_text())
                self.assertNotIn('stream {', (ingress.config / 'stream.conf').read_text())
                ingress.disable()
                self.assertEqual([p.read_bytes() for p in paths], before)

                with patch.object(ingress, 'reload', side_effect=ValueError('test reload failure')):
                    with self.assertRaisesRegex(ValueError, 'test reload failure'):
                        ingress.enable()
                self.assertEqual([p.read_bytes() for p in paths], before)
                self.assertFalse(ingress.config.exists())
                included = main.parent / 'stream.d'
                included.mkdir()
                (included / 'custom.conf').write_text('server { listen 12345; }\n')
                with self.assertRaisesRegex(ValueError, '已有 Nginx stream'):
                    ingress.enable()
                (included / 'custom.conf').unlink()
                dump = dump.replace('include stream.d/*.conf;', 'server { listen 12345; }')
                with self.assertRaisesRegex(ValueError, '已有 Nginx stream'):
                    ingress.enable()
                self.assertEqual([p.read_bytes() for p in paths], before)

    def test_openrc_installs_and_starts_ingress_service(self):
        with tempfile.TemporaryDirectory() as temporary:
            ingress = Ingress(Path(temporary))
            with patch('chain_ingress.service_kind', return_value='openrc'), \
                    patch('chain_platform.service_kind', return_value='openrc'), \
                    patch('chain_ingress.service_path', return_value=Path('/etc/init.d/chain-ingress')), \
                    patch('chain_ingress.service') as service:
                ingress.install_units()
                self.assertIn('supervisor=supervise-daemon', ingress.unit_paths()[0].read_text())
                self.assertEqual(service.call_args_list, [unittest.mock.call('enable', 'chain-ingress'),
                                                          unittest.mock.call('restart', 'chain-ingress')])


if __name__ == '__main__': unittest.main()
