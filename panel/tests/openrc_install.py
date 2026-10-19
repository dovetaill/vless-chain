"""Disposable Alpine: real installer, OpenRC services, native-v3 handover and traffic."""
import copy
import http.server
import json
import os
from pathlib import Path
import pwd
import re
import ssl
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlsplit
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts'))
import chain_install as installer
from chain_common import atomic_write, json_write, verify_tree
from chain_legacy import PanelAPI, discover, guided_install
from chain_platform import create_account, service, service_definition, service_state
from legacy_migration import Response, public_key
from smoke import Harness, HttpHandler, TlsServer


def clean_install(manager, source, manifest):
    assert not manager.state_file.exists()
    args = installer.make_parser().parse_args(['install', '--non-interactive', '--listen', '127.0.0.1:37243'])
    args.defer_proxy_acme = args.defer_access = True
    try:
        installer.install_components(manager, source, manifest, args)
        assert set(manager.state()['roles']) == {'panel', 'agent'}
        api = PanelAPI(manager)
        try:
            deadline = time.monotonic() + 20
            while not api.call('state')['nodes'][0]['online']:
                assert time.monotonic() < deadline
                time.sleep(0.2)
        finally:
            api.call('logout', {})
        for role in ['panel', 'agent']:
            assert service_state('chain-' + role)[0]
            assert Path('/var/lib/chain-' + role, '.chain-owner').read_text() == 'chain-' + role
        print('PASS clean first install of real panel and Agent through OpenRC', flush=True)
    finally:
        for name in ['chain-agent', 'chain-panel']:
            if Path('/etc/init.d', name).exists():
                service('stop', name, check=False)


def main():
    assert os.environ.get('CHAIN_TEST_OPENRC') == '1' and Path('/.dockerenv').is_file()
    assert os.geteuid() == 0
    source = Path(sys.argv[1]).resolve()
    manifest = verify_tree(source)
    os.environ.update(CHAIN_TEST_BIN_DIR=str(source / 'bin'), CHAIN_TEST_XRAY=str(source / 'bin/xray'))
    runtime = Path('/run/openrc')
    runtime.mkdir(exist_ok=True)
    (runtime / 'softlevel').write_text('default\n')
    atomic_write('/etc/init.d/net', '#!/sbin/openrc-run\nstart() { return 0; }\n', 0o755)
    service('start', 'net')
    manager = installer.Manager()
    assert not manager.state()['roles']
    if sys.argv[2] == 'fresh':
        clean_install(manager, source, manifest)
        return
    assert sys.argv[2] == 'recovery'
    # Seed exactly the failed v0.2.27 state observed on Alpine, preserving its log.
    if not manager.state_file.exists():
        manager.base.mkdir(parents=True)
        json_write(manager.state_file, {'owner': installer.OWNER, 'roles': {}})
        create_account('chain-panel', '/var/lib/chain-panel')
        user = pwd.getpwnam('chain-panel')
        data = Path('/var/lib/chain-panel')
        data.mkdir(mode=0o700)
        os.chown(data, user.pw_uid, user.pw_gid)
        atomic_write(data / 'service.log', 'previous failed install\n')
        os.chown(data / 'service.log', user.pw_uid, user.pw_gid)
    data = Path('/var/lib/chain-panel')
    assert sorted(p.name for p in data.iterdir()) == ['service.log']
    old_log = (data / 'service.log').read_bytes()
    old_meta = (data.stat().st_uid, data.stat().st_gid, data.stat().st_mode)
    names = []
    with tempfile.TemporaryDirectory(prefix='chain-openrc-install-') as temporary:
        h = Harness(Path(temporary))
        try:
            cert, key = h.root / 'cert.pem', h.root / 'key.pem'
            subprocess.run(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt',
                'ec_paramgen_curve:prime256v1', '-nodes', '-keyout', str(key), '-out', str(cert),
                '-days', '1', '-subj', '/CN=legacy.test'], check=True, capture_output=True)
            tls_port = h.port()
            tls = TlsServer(('127.0.0.1', tls_port), HttpHandler)
            tls.tls_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            tls.tls_context.minimum_version = ssl.TLSVersion.TLSv1_3
            tls.tls_context.load_cert_chain(cert, key)
            tls.tls_context.set_alpn_protocols(['http/1.1'])
            h.server(tls)
            h.http_port = h.port()
            h.server(http.server.ThreadingHTTPServer(('127.0.0.1', h.http_port), Response))
            panel_cert = Path('/etc/letsencrypt/live/chain-panel-panel.test')
            panel_cert.mkdir(parents=True)
            subprocess.run(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt',
                'ec_paramgen_curve:prime256v1', '-nodes', '-keyout', str(panel_cert / 'privkey.pem'),
                '-out', str(panel_cert / 'fullchain.pem'), '-days', '1', '-subj', '/CN=panel.test',
                '-addext', 'subjectAltName=DNS:panel.test'], check=True, capture_output=True)
            sources = json.loads(Path(__file__).with_name('fixtures').joinpath('native-v3.json').read_text())
            exits = []
            # Exit services are fixtures, outside the set of old entry services to migrate.
            for index, entry in enumerate(sources):
                inbound = entry['config']['inbounds'][0]
                inbound['port'] = [443, 8443, 8444, 8445][index]
                h.ports.add(inbound['port'])
                reality = inbound['streamSettings']['realitySettings']
                reality.update(target=f'127.0.0.1:{tls_port}', serverNames=['legacy.test'])
                if index:
                    exit_port = h.port()
                    exit_inbound = copy.deepcopy(inbound)
                    exit_inbound.update(listen='127.0.0.1', port=exit_port)
                    outbound = entry['config']['outbounds'][0]
                    outbound['settings'].update(address='127.0.0.1', port=exit_port)
                    exit_inbound['settings']['clients'][0]['id'] = outbound['settings']['id']
                    outbound['streamSettings']['realitySettings'].update(serverName='legacy.test',
                        publicKey=public_key(reality['privateKey']), shortId=reality['shortIds'][0])
                    access = h.root / f'exit-{index}-access.log'
                    config = h.root / f'exit-{index}.json'
                    config.write_text(json.dumps({'log': {'access': str(access), 'loglevel': 'warning'},
                        'inbounds': [exit_inbound], 'outbounds': [{'protocol': 'freedom'}]}))
                    h.launch(f'exit-{index}', h.xray, {}, ['run', '-config', str(config)])
                    exits.append(access)
                name = f'xray-chain-fixture-{index}'
                config = h.root / f'{name}.json'
                config.write_text(json.dumps(entry['config']))
                definition = service_definition(name, [h.xray, 'run', '-config', config], 'openrc-test-fixture')
                atomic_write('/etc/init.d/' + name, definition, 0o755)
                names.append(name)
                service('enable', name)
                service('start', name)
            h.wait(lambda: len([o for o in discover() if o['name'] in names]) == 4, 'four original OpenRC services')
            originals = {o['name']: o['pids'] for o in discover() if o['name'] in names}
            port = h.port()
            h.base = f'http://127.0.0.1:{port}'
            args = installer.make_parser().parse_args(['install', '--non-interactive', '--listen',
                f'127.0.0.1:{port}', '--domain', 'panel.test', '--public-url', 'https://panel.test', '--local-host', '127.0.0.1',
                '--local-target', f'localhost:{tls_port}'])
            choices = []
            def choose(question, options, default='3'):
                if question == '为新平台选择公网 HTTPS 端口，原代理端口保持不变':
                    return '1'
                if question.startswith('REALITY_UNVERIFIED'):
                    # This isolated cover server intentionally serves HTTP/1 only.
                    assert default == '2'
                    return '1'
                choices.append(question)
                assert '如何处理该服务' in question
                return '1'
            with patch.object(installer, 'choose', side_effect=choose):
                assert guided_install(manager, source, manifest, args)
            assert len(choices) == 4
            assert set(manager.state()['roles']) == {'panel', 'agent'}
            assert (data / 'service.log').read_bytes().startswith(old_log)
            assert (data.stat().st_uid, data.stat().st_gid, data.stat().st_mode) == old_meta
            h.request('/api/login', 'POST', {'token': (data / 'admin-token').read_text()})
            state = h.state()
            assert len(state['legacy_configs']) == 4, state['legacy_configs']
            assert all(profile['published'] for profile in state['legacy_configs'])
            assert not state['lines'] and not state['externals'] and not state['pending_imports']
            https_port = urlsplit(args.public_url).port or 443
            # Raw native configurations retain their original listener, so the
            # panel uses the alternate HTTPS port selected by the real installer.
            assert https_port not in {443, 8443, 8444, 8445, port}
            def https(path):
                return subprocess.check_output(['curl', '--noproxy', '*', '-fsS', '--max-time', '10',
                    '--cacert', str(panel_cert / 'fullchain.pem'), '--resolve', f'panel.test:{https_port}:127.0.0.1',
                    f'https://panel.test:{https_port}' + path])
            html = https('/')
            assert html == h.request('/')
            assets = re.findall(r'(?:src|href)="(/assets/[^\"]+)"', html.decode())
            assert assets
            for asset in assets:
                assert https(asset) == h.request(asset)
            for name, pids in originals.items():
                assert not service_state(name)[0] and not service_state(name)[1]
                assert all(not Path('/proc', str(pid)).exists() for pid in pids)
            for index, entry in enumerate(sources):
                inbound = entry['config']['inbounds'][0]
                reality = inbound['streamSettings']['realitySettings']
                client_port = h.port()
                config = h.root / f'client-{index}.json'
                config.write_text(json.dumps({'inbounds': [{'listen': '127.0.0.1', 'port': client_port,
                    'protocol': 'socks', 'settings': {'auth': 'noauth'}}], 'outbounds': [{'protocol': 'vless',
                    'settings': {'vnext': [{'address': '127.0.0.1', 'port': inbound['port'],
                        'users': [{**inbound['settings']['clients'][0], 'encryption': 'none'}]}]},
                    'streamSettings': {'network': 'tcp', 'security': 'reality', 'realitySettings': {
                        'serverName': 'legacy.test', 'publicKey': public_key(reality['privateKey']),
                        'shortId': reality['shortIds'][0], 'fingerprint': 'chrome'}}}]}))
                client = h.launch(f'client-{index}', h.xray, {}, ['run', '-config', str(config)])
                def probe():
                    result = subprocess.run(['curl', '--noproxy', '', '--socks5-hostname',
                        f'127.0.0.1:{client_port}', '--max-time', '3', '-fsS',
                        f'http://127.0.0.1:{h.http_port}/'], capture_output=True)
                    return result.returncode == 0 and result.stdout == b'chain-integration-ok'
                h.wait(probe, f'original client {index} traffic', timeout=20)
                if index:
                    assert f'127.0.0.1:{h.http_port}' in exits[index - 1].read_text()
                h.stop_process(client)
            for role in ['panel', 'agent']:
                subprocess.run(['/usr/local/bin/chainctl', 'logs', '--component', role], check=True,
                               stdout=subprocess.DEVNULL)
            print('PASS real OpenRC installer: failed-log recovery; panel and Agent; complete HTTPS assets on alternate port; original proxy 443 preserved; four old services imported; three external exits; four original clients carry traffic', flush=True)
        finally:
            for name in ['chain-ingress', 'chain-panel-cert-renew', 'chain-agent', 'chain-panel', 'nginx', *reversed(names)]:
                if Path('/etc/init.d', name).exists():
                    service('stop', name, check=False)
            h.close()
if __name__ == '__main__':
    main()
