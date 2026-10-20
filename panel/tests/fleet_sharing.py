"""Regression checks for sharing APIs through the actual authenticated fleet forwarder."""
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import urllib.request


def exercise(master, slave, forward, node):
    line = forward('/lines', {'name': 'sharing-policy', 'kind': 'vless',
        'hops': [{'node_id': node['id'], 'port': 30999}], 'access': {'protocol': 'socks'}})
    request = urllib.request.Request(f'http://127.0.0.1:{slave.port}/api/agent/{node["id"]}/sync',
        method='POST', headers={'Content-Type': 'application/json', 'Authorization': 'Bearer '+node['token']},
        data=json.dumps({'system': {'gateway_version': 2, 'sharing_version': 0}}).encode())
    with urllib.request.urlopen(request) as response:
        assert response.status == 200
    for policy in [{'udp': 'deny'}, {'udp': 'ports', 'udp_ports': [53]}, {'denied_ports': [25]}]:
        forward('/access/'+line['id'], policy)
        result = slave.api('/publish', {}, expected=400)[0]
        assert '升级 Agent' in result['error'], result
        assert slave.api('/state')[0]['jobs'] == [], 'Rejected publish created a job'
    forward('/access/'+line['id'], {})
    assert forward('/access/'+line['id'])['denied_ports'] == []
    print('PASS first publish rejects old Agent for UDP deny, UDP ports and denied TCP ports', flush=True)

    links = forward('/externals/batch', {'urls': ['socks5://same.example:1080', 'socks5://same.example:1081']})
    ids = [x['id'] for x in links]
    assert len(forward('/resources/candidates', {'ids': ids})) == 1
    forward('/resources/groups', {'ids': ids, 'name': 'remote-group', 'automatic': True})
    groups = forward('/resources')['resources']
    resource = next(x for x in groups if x['name'] == 'remote-group')
    resource.update(monthly_gib=10, mbps=100, reset_day=21, reset_hour=3, timezone='America/Los_Angeles')
    forward('/resources', resource)
    assert slave.api('/resources')[0]['resources'][0]['mbps'] == 100
    assert master.api('/resources')[0]['resources'] == []
    assert not forward('/resources/candidates', {'ids': ids})
    forward('/resources/groups', {'ids': ids, 'automatic': False})
    forward('/resources/'+resource['id'], method='DELETE')
    print('PASS remote resource CRUD, cycle, grouping, batch import and master/slave data isolation', flush=True)

    sources = forward('/rules')
    assert len(sources) == 55
    # Use the actual HTTPS fetch and strict parser for every upstream preset.
    def update(source):
        forward('/rules/'+source['id']+'/update', {})
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(update, sources))
    sources = forward('/rules')
    for source in sources:
        assert source['pending'] and source['pending']['count'] > 0, (source['name'], source['error'])
        forward('/rules/'+source['id']+'/apply', {})
    assert all(s['active'] and s['active']['count'] for s in forward('/rules'))
    custom = forward('/rules', {'name': 'remote-custom', 'url': sources[0]['url'], 'auto_apply': False})
    forward('/rules/'+custom['id']+'/update', {})
    forward('/rules/'+custom['id']+'/apply', {})
    forward('/rules', {'id': custom['id'], 'name': 'edited-custom', 'url': sources[0]['url'], 'auto_apply': True})
    forward('/access/'+line['id'], {'restricted': True, 'sources': ['preset-danime', 'preset-googlesearch'], 'udp': 'deny'})
    assert forward('/access/'+line['id'])['restricted']
    assert len(master.api('/rules')[0]) == 55
    forward('/security/token', {}, expected=403)
    forward('/agent/'+node['id']+'/resources', {}, expected=403)
    print('PASS 55/55 presets generate and apply strict rules, remote custom rules and access policies; privileged routes remain denied', flush=True)
    if os.environ.get('CHAIN_TEST_BROWSER'):
        subprocess.run(['node', str(Path(__file__).with_name('sharing-ui.cjs')),
                        f'http://127.0.0.1:{master.port}', str(master.dir/'admin-token'),
                        master.api('/fleet')[0]['remotes'][0]['id']], check=True)
    forward('/lines/'+line['id'], method='DELETE')
    for link in links:
        forward('/externals/'+link['id'], method='DELETE')
