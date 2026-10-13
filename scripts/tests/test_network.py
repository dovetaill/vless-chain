"""Network decisions and HTTPS failure paths; never touch host service config."""
import contextlib
import io
import json
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import chain_network as network
from chain_common import Workspace


class Network(unittest.TestCase):
    def test_nic_address_does_not_use_proxy_egress(self):
        interfaces = [
            {'ifname': 'eth0', 'flags': ['UP'], 'addr_info': [{'local': '8.8.4.4'}, {'local': '2606:4700::1111'}]},
            {'ifname': 'tun0', 'flags': ['UP'], 'addr_info': [{'local': '10.8.0.2'}]},
            {'ifname': 'eth1', 'flags': [], 'addr_info': [{'local': '9.9.9.9'}]},
        ]
        with patch.object(network.shutil, 'which', return_value='/usr/sbin/ip'), \
                patch.object(network, 'output', side_effect=[json.dumps(interfaces), json.dumps([{'prefsrc': '10.8.0.2'}])]), \
                patch.object(network.urllib.request, 'urlopen', side_effect=AssertionError('must never query egress IP')):
            self.assertEqual(network.public_ipv4(), '8.8.4.4')
        with patch.object(network.shutil, 'which', return_value='/usr/sbin/ip'), \
                patch.object(network, 'output', return_value=json.dumps(interfaces[1:2])):
            self.assertIsNone(network.public_ipv4())

    def test_multiple_nic_ips_prefer_assigned_route_source(self):
        with patch.object(network.shutil, 'which', return_value='/usr/sbin/ip'), \
                patch.object(network, 'server_addresses', return_value={'1.1.1.1', '8.8.8.8'}), \
                patch.object(network, 'output', return_value='[{"prefsrc":"8.8.8.8"}]'):
            self.assertEqual(network.public_ipv4(), '8.8.8.8')

    def test_random_port_skips_live_tcp_and_udp_sockets(self):
        for kind in (socket.SOCK_STREAM, socket.SOCK_DGRAM):
            with socket.socket(socket.AF_INET, kind) as occupied:
                occupied.bind(('127.0.0.1', 0))
                busy = occupied.getsockname()[1]
                free = network.available_port()
                with patch.object(network.secrets, 'randbelow', side_effect=[busy - 10000, free - 10000]):
                    self.assertEqual(network.available_port(), free)
            with socket.socket(socket.AF_INET, kind) as check:
                check.bind(('127.0.0.1', busy))

    def test_random_port_checks_ipv6(self):
        occupied = None
        try:
            occupied = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
            occupied.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
            occupied.bind(('::1', 0))
        except OSError:
            if occupied is not None:
                occupied.close()
            self.skipTest('IPv6 disabled')
        with occupied:
            busy = occupied.getsockname()[1]
            free = network.available_port()
            with patch.object(network.secrets, 'randbelow', side_effect=[busy - 10000, free - 10000]):
                self.assertEqual(network.available_port(), free)

    def test_domain_input_cannot_inject_nginx_directives(self):
        self.assertEqual(network.domain_name('Panel.Example.com.'), 'panel.example.com')
        for value in ('https://example.com', 'example.com:443', 'example.com; include /tmp/x;', '1.2.3.4', '*.example.com', '-bad.example.com'):
            with self.assertRaises(ValueError):
                network.domain_name(value)

    def test_dns_rejects_wrong_aaaa_and_proxy_addresses(self):
        for answers in ({'8.8.8.8', '2606:4700::1111'}, {'1.1.1.1'}, set()):
            with patch.object(network, 'domain_addresses', return_value=answers):
                with self.assertRaisesRegex(ValueError, '解析不匹配'):
                    network.validate_dns('panel.example.com', {'8.8.8.8'})
        with patch.object(network, 'domain_addresses', return_value={'8.8.8.8'}):
            self.assertEqual(network.validate_dns('panel.example.com', {'8.8.8.8', '9.9.9.9'}), {'8.8.8.8'})

    def test_public_dns_checks_both_record_types(self):
        replies = [io.BytesIO(json.dumps({'Status': 0, 'Answer': [{'type': 1, 'data': '8.8.8.8'}]}).encode()),
                   io.BytesIO(json.dumps({'Status': 0, 'Answer': [{'type': 28, 'data': '2606:4700::1111'}]}).encode())]
        with patch.object(network.urllib.request, 'urlopen', side_effect=replies) as lookup:
            self.assertEqual(network.domain_addresses('panel.example.com'), {'8.8.8.8', '2606:4700::1111'})
        self.assertIn('type=AAAA', lookup.call_args_list[1].args[0].full_url)

    def test_system_package_manager_only(self):
        with patch.object(network.shutil, 'which', side_effect=lambda name: '/usr/bin/apt-get' if name == 'apt-get' else None), \
                patch.object(network, 'run') as command:
            network.install_packages(['nginx', 'certbot'])
        self.assertEqual(command.call_args_list[0].args[0], ['apt-get', 'update'])
        self.assertEqual(command.call_args_list[1].args[0], ['apt-get', 'install', '-y', 'nginx', 'certbot'])
        with patch.object(network.shutil, 'which', side_effect=lambda name: '/usr/bin/dnf' if name == 'dnf' else None), \
                patch.object(network, 'run') as command:
            network.install_packages(['nginx'])
        self.assertEqual(command.call_args.args[0], ['dnf', 'install', '-y', 'nginx'])

    def test_nginx_include_must_be_in_http_and_preserve_other_sites(self):
        dump = ('# configuration file /etc/nginx/nginx.conf:\n'
                'include /etc/nginx/modules-enabled/*.conf;\n'
                'stream { include /etc/nginx/streams/*.conf; }\n'
                'http { include /etc/nginx/conf.d/*.conf; }\n'
                '# configuration file /etc/nginx/conf.d/blog.conf:\n'
                'server { server_name blog.example.com *.private.example.com; }\n')
        self.assertEqual(network.nginx_site_directory(dump), Path('/etc/nginx/conf.d'))
        own = Path('/etc/nginx/conf.d/chain-panel.conf')
        self.assertIsNone(network.conflicting_site(dump, 'panel.example.com', own))
        self.assertEqual(network.conflicting_site(dump, 'blog.example.com', own), '/etc/nginx/conf.d/blog.conf')
        self.assertTrue(network.conflicting_site(dump, 'panel.private.example.com', own))
        with self.assertRaises(ValueError):
            network.nginx_site_directory(dump.replace('http { include /etc/nginx/conf.d/*.conf; }', 'http {}'))

    def test_failed_certificate_restores_site_and_cleans_probe(self):
        with tempfile.TemporaryDirectory(prefix='chain-https-test-') as temp:
            root = Path(temp)
            site = network.HttpsSite('panel.example.com', 45000)
            site.site, site.webroot = root / 'site.conf', root / 'webroot'
            site.site.write_text(network.SITE_OWNER + '\n# previous config\n')
            original = site.site.read_bytes()
            with patch.object(site, 'prepare'), patch.object(site, 'reload'), \
                    patch.object(site, 'renewal_paths', return_value=tuple(root / name for name in ('hook', 'service', 'timer'))), \
                    patch.object(site, 'challenge_check'), \
                    patch.object(network, 'run', side_effect=subprocess.CalledProcessError(1, ['certbot'])):
                with self.assertRaises(subprocess.CalledProcessError):
                    with site.activate():
                        self.fail('must not install panel before certificate succeeds')
            self.assertEqual(site.site.read_bytes(), original)
            with patch.object(network.urllib.request, 'build_opener') as opener:
                opener.return_value.open.side_effect = OSError('connection refused')
                with self.assertRaisesRegex(ValueError, '验证路径不可达'):
                    site.challenge_check()
            self.assertEqual(list((site.webroot / '.well-known/acme-challenge').iterdir()), [])

    def test_certificate_hook_only_reloads_its_own_lineage(self):
        with tempfile.TemporaryDirectory(prefix='chain-renew-test-') as temp:
            root = Path(temp)
            paths = tuple(root / name for name in ('hook', 'service', 'timer'))
            site = network.HttpsSite('panel.example.com', 45000)
            with patch.object(site, 'renewal_paths', return_value=paths), \
                    patch.object(network.shutil, 'which', side_effect=lambda name: '/usr/bin/' + name), \
                    patch.object(network, 'run') as command:
                site.renewal()
            self.assertIn('RENEWED_LINEAGE', paths[0].read_text())
            self.assertIn(str(site.cert), paths[0].read_text())
            self.assertIn('--cert-name chain-panel-panel.example.com', paths[1].read_text())
            self.assertEqual(command.call_args.args[0], ['systemctl', 'is-active', '--quiet', 'timer'])

    def test_renewal_failure_rolls_back_all_managed_files(self):
        with tempfile.TemporaryDirectory(prefix='chain-renew-rollback-') as temp:
            root = Path(temp)
            paths = tuple(root / name for name in ('hook', 'service', 'chain-panel-cert-renew.timer'))
            site = network.HttpsSite('panel.example.com', 45000)
            site.site, site.cert = root / 'site.conf', root / 'cert'
            site.cert.mkdir()
            for name in ('fullchain.pem', 'privkey.pem'):
                (site.cert / name).write_text('fixture')
            site.site.write_text(network.SITE_OWNER + '\n# previous upstream\n')
            previous = site.site.read_bytes()
            def command(argv, **kwargs):
                if argv[:3] == ['systemctl', 'enable', '--now']:
                    raise subprocess.CalledProcessError(1, argv)
            with patch.object(site, 'prepare'), patch.object(site, 'reload'), patch.object(site, 'challenge_check'), \
                    patch.object(site, 'renewal_paths', return_value=paths), patch.object(network, 'run', side_effect=command), \
                    patch.object(site, 'service_state', return_value=(True, True)), \
                    patch.object(network.shutil, 'which', side_effect=lambda name: '/usr/bin/' + name):
                with self.assertRaises(subprocess.CalledProcessError):
                    with site.activate():
                        self.fail('must fail before panel installation')
            self.assertEqual(site.site.read_bytes(), previous)
            self.assertTrue(all(not path.exists() for path in paths))

    def test_foreign_renewal_symlink_is_untouched(self):
        with tempfile.TemporaryDirectory(prefix='chain-foreign-hook-') as temp:
            root = Path(temp)
            target, hook = root / 'foreign', root / 'hook'
            target.write_text('foreign hook')
            hook.symlink_to(target)
            site = network.HttpsSite('panel.example.com', 45000)
            site.site = root / 'site.conf'
            with patch.object(site, 'prepare'), patch.object(site, 'renewal_paths', return_value=(hook, root / 'service', root / 'timer')), \
                    patch.object(site, 'service_state', return_value=(True, True)), patch.object(network, 'run') as command:
                with self.assertRaisesRegex(ValueError, '拒绝覆盖'):
                    with site.activate():
                        self.fail('foreign path must fail before changes')
            self.assertTrue(hook.is_symlink())
            self.assertEqual(target.read_text(), 'foreign hook')
            self.assertFalse(site.site.exists())
            command.assert_not_called()

    def test_failed_panel_restores_previous_https_upstream(self):
        with tempfile.TemporaryDirectory(prefix='chain-upstream-rollback-') as temp:
            root = Path(temp)
            site = network.HttpsSite('panel.example.com', 45000)
            site.site, site.cert = root / 'site.conf', root / 'cert'
            site.cert.mkdir()
            for name in ('fullchain.pem', 'privkey.pem'):
                (site.cert / name).write_text('fixture')
            site.site.write_text(network.SITE_OWNER + '\n# original port\n')
            previous = site.site.read_bytes()
            with patch.object(site, 'prepare'), patch.object(site, 'reload'), patch.object(site, 'challenge_check'), \
                    patch.object(site, 'renewal'), patch.object(site, 'service_state', return_value=(True, True)), \
                    patch.object(site, 'renewal_paths', return_value=tuple(root / name for name in ('hook', 'service', 'timer'))), \
                    patch.object(network, 'run'):
                with self.assertRaisesRegex(ValueError, 'panel startup'):
                    with site.activate():
                        raise ValueError('panel startup failed')
            self.assertEqual(site.site.read_bytes(), previous)

    @unittest.skipUnless(shutil.which('nginx') and shutil.which('openssl'), 'Nginx/OpenSSL unavailable')
    def test_generated_https_config_passes_real_nginx_check(self):
        with tempfile.TemporaryDirectory(prefix='chain-nginx-check-') as parent:
            with Workspace(Path(parent)) as work:
                site = network.HttpsSite('panel.example.com', 45000, ipv6=True)
                site.webroot = work.root / 'webroot'
                site.cert = work.root / 'cert'
                site.cert.mkdir()
                work.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
                          '-subj', '/CN=panel.example.com', '-keyout', site.cert / 'privkey.pem',
                          '-out', site.cert / 'fullchain.pem'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                conf = work.root / 'nginx.conf'
                conf.write_text(f'pid {work.root}/nginx.pid;\nerror_log {work.root}/error.log;\nevents {{}}\n'
                                'http { access_log off;\n' + site.config(tls=True) + '}\n')
                work.run(['nginx', '-t', '-p', str(work.root), '-c', conf, '-e', work.root / 'error.log'])


if __name__ == '__main__':
    unittest.main(verbosity=2)
