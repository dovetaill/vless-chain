#!/usr/bin/env python3
"""REALITY checks, atomic variants, real XHTTP clients and isolated Nginx SNI routing."""
import contextlib
import http.server
import json
import os
from pathlib import Path
import signal
import socket
import socketserver
import ssl
import subprocess
import sys
import tempfile
import threading
import time
from types import SimpleNamespace
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit

from smoke import Harness, HttpHandler, UdpEcho
from mihomo import exercise
sys.path.insert(0, str(Path(__file__).resolve().parents[2]/'scripts'))
from chain_ingress import Ingress, OWNER


def main():
    with tempfile.TemporaryDirectory(prefix='fronting-', dir=os.environ['CHAIN_TEST_ROOT']) as temporary:
        h=Harness(Path(temporary))
        watcher_stop=threading.Event()
        watcher=None
        try:
            cert,key=h.root/'cert.pem',h.root/'key.pem'
            subprocess.run(['openssl','req','-x509','-newkey','ec','-pkeyopt','ec_paramgen_curve:prime256v1',
                '-nodes','-keyout',str(key),'-out',str(cert),'-days','1','-subj','/CN=reality.test',
                '-addext','subjectAltName=DNS:reality.test,DNS:tls.test,DNS:existing.test'],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            panel_port,tls_port,existing_port,public_port,h.http_port,udp_port=[h.port() for _ in range(6)]
            h.server(http.server.ThreadingHTTPServer(('127.0.0.1',h.http_port),HttpHandler))
            h.server(socketserver.ThreadingUDPServer(('127.0.0.1',udp_port),UdpEcho))
            ingress=Ingress(h.root/'system')
            ingress.config.mkdir(parents=True)
            ingress.directory.mkdir(parents=True)
            ingress.request_path=h.root/'agent/ingress-request.json'
            ingress.state_path.write_text(json.dumps({'blocked':['existing.test']}))
            ingress.status(ready=True,applied=True,id='',error='')
            routes=ingress.config/'routes.conf'
            routes.write_text(OWNER+'\n')
            config=h.root/'nginx.conf'
            config.write_text(f"""load_module {os.environ['CHAIN_TEST_STREAM_MODULE']};
pid {h.root}/nginx.pid;
worker_processes 1;
error_log {h.root}/nginx-error.log info;
events {{ worker_connections 1024; }}
http {{
 access_log off;
 ssl_certificate {cert}; ssl_certificate_key {key}; ssl_protocols TLSv1.2 TLSv1.3;
 server {{ listen 127.0.0.1:{tls_port} ssl; http2 on; server_name reality.test; location / {{ return 200 'target-ok'; }} }}
 server {{ listen 127.0.0.1:{existing_port} ssl proxy_protocol; server_name existing.test;
   real_ip_header proxy_protocol; set_real_ip_from 127.0.0.1;
   location / {{ return 200 'existing-site $remote_addr'; }} }}
}}
stream {{
 map $ssl_preread_server_name $destination {{ hostnames; default 127.0.0.1:{existing_port}; include {routes}; }}
 server {{ listen 127.0.0.1:{public_port}; ssl_preread on; proxy_pass $destination; proxy_protocol on; }}
}}
""")
            nginx=h.launch('nginx','nginx',{},['-c',str(config),'-p',str(h.root),'-g','daemon off;'])
            def https(port,name):
                ctx=ssl.create_default_context(cafile=str(cert))
                with socket.create_connection(('127.0.0.1',port),timeout=10) as raw:
                    with ctx.wrap_socket(raw,server_hostname=name) as connection:
                        connection.sendall(f'GET / HTTP/1.1\r\nHost: {name}\r\nConnection: close\r\n\r\n'.encode())
                        result=b''
                        while chunk:=connection.recv(65536): result+=chunk
                        return result
            h.wait(lambda: b'existing-site 127.0.0.1' in https(public_port,'existing.test'),'existing HTTPS site')
            def reload():
                subprocess.run(['nginx','-t','-c',str(config),'-p',str(h.root)],check=True,capture_output=True)
                os.kill(nginx.pid,signal.SIGHUP)
                time.sleep(.15)
            ingress.reload=reload
            def watch():
                seen=''
                while not watcher_stop.wait(.03):
                    try:
                        data=json.loads(ingress.request_path.read_text())
                        if data['id']==seen: continue
                        seen=data['id']
                        with patch('chain_ingress.pwd.getpwnam',return_value=SimpleNamespace(pw_uid=os.getuid())):
                            with contextlib.suppress(Exception): ingress.apply()
                    except FileNotFoundError: pass
            watcher=threading.Thread(target=watch)
            watcher.start()
            h.base=f'http://127.0.0.1:{panel_port}'
            h.launch('panel',h.bin_dir/'chain-panel',{'CHAIN_DATA_DIR':str(h.root/'control'),'CHAIN_LISTEN':f'127.0.0.1:{panel_port}',
                'CHAIN_XRAY_BIN':str(h.xray),'SSL_CERT_FILE':str(cert)})
            h.wait(lambda:h.request('/') or True,'panel')
            h.login()
            positive=h.request('/api/reality/check','POST',{'target':f'127.0.0.1:{tls_port}','server_name':'reality.test'})
            assert positive['ok'] and positive['h2'] and positive['certificate_valid'],positive
            negative=h.request('/api/reality/check','POST',{'target':f'127.0.0.1:{tls_port}','server_name':'wrong.test'})
            assert not negative['ok'],negative
            node=h.request('/api/nodes','POST',{'name':'fronting','host':'127.0.0.1','target':f'127.0.0.1:{tls_port}','server_name':'reality.test'})
            token=h.root/'token';token.write_text(node['token']);token.chmod(0o600)
            h.launch('agent',h.bin_dir/'chain-agent',{'CHAIN_AGENT_DIR':str(h.root/'agent'),'CHAIN_PANEL_URL':h.base,'CHAIN_NODE_ID':node['id'],
                'CHAIN_NODE_TOKEN_FILE':str(token),'CHAIN_XRAY_BIN':str(h.xray),'CHAIN_INGRESS_STATUS':str(ingress.status_path),'SSL_CERT_FILE':str(cert)})
            h.wait(lambda:h.state()['nodes'][0]['online'],'agent')
            for port in h.state()['nodes'][0]['report']['system']['listening_ports']:
                if 50000<=port<65535:
                    h.request('/api/lines','POST',{'name':f'reserved-{port}','kind':'vless','enabled':False,'hops':[{'node_id':node['id'],'port':port}]})
            tls={'mode':'manual','domain':'tls.test','cert':cert.read_text(),'key':key.read_text()}
            inputs=[]
            for network,security in [('tcp','reality'),('tcp','tls'),('xhttp','reality'),('xhttp','tls')]:
                inputs.append({'name':network+' '+security,'kind':'vless','hops':[{'node_id':node['id'],'port':h.port()}],
                    'access':{'protocol':'vless','host':'127.0.0.1','transport':{'network':network,'path':'/channel','host':'','mode':'auto'},
                        'reality':{'target':f'127.0.0.1:{tls_port}','server_name':'reality.test'},'tls':tls if security=='tls' else {'mode':'off'},
                        'fallback':'builtin' if network=='tcp' and security=='tls' else ''}})
            count=len(h.state()['lines'])
            bad={**inputs[0],'name':'bad','hops':[{'node_id':'missing','port':h.port()}]}
            h.request('/api/lines/batch','POST',[inputs[0],bad],status=400)
            assert len(h.state()['lines'])==count,'partial batch was saved'
            lines=h.request('/api/lines/batch','POST',inputs)
            h.publish()
            for line in lines:
                h.probe(line)
                exported=h.request('/api/lines/'+line['id']+'/export')
                proxy=json.loads(exported['node'][2:])
                if line['access']['transport']['network']=='xhttp':
                    assert 'flow' not in proxy and 'flow' not in parse_qs(urlsplit(exported['share']).query)
                    assert proxy['xhttp-opts']['path']=='/channel'
                for text in [exported['share'],exported['node']]:
                    h.request('/api/externals','POST',{'url':text})
            exercise(h,lines,udp_port)
            assert '欢迎访问'.encode() in https(lines[1]['hops'][0]['port'],'tls.test')
            def h2_page(port):
                return subprocess.run(['curl','--silent','--show-error','--fail','--noproxy','*','--max-time','10',
                    '--http2','--cacert',str(cert),'--resolve',f'tls.test:{port}:127.0.0.1',
                    '--write-out','\n%{http_version}',f'https://tls.test:{port}/'],check=True,capture_output=True).stdout
            assert h2_page(lines[1]['hops'][0]['port']).endswith(b'\n2'), 'website fallback must support browser H2'
            print('PASS target/SNI checks, atomic batch, four original exports, Xray/Mihomo TCP+UDP and TLS website fallback',flush=True)
            # Isolated Nginx uses a random public test port; generated production exports remain 443.
            for index in [0,1]:
                data={**inputs[index],'id':lines[index]['id'],'hops':[{'node_id':node['id'],'port':443}],
                    'access':{**inputs[index]['access'],'shared_port':True}}
                if index == 1: data['access']['fallback']=f'http://127.0.0.1:{h.http_port}'
                lines[index]=h.request('/api/lines','POST',data)
            h.publish()
            assert b'existing-site 127.0.0.1' in https(public_port,'existing.test')
            assert b'chain-integration-ok' in https(public_port,'tls.test')
            assert h2_page(public_port)==b'chain-integration-ok\n2'
            original_request=h.request
            def export_test_port(path,*args,**kwargs):
                result=original_request(path,*args,**kwargs)
                if path.endswith('/export') and '/lines/' in path:
                    node=json.loads(result['node'][2:])
                    if node['port']==443:
                        node['port']=public_port
                        result['node']='- '+json.dumps(node)
                return result
            h.request=export_test_port
            exercise(h,lines[:2],udp_port)
            h.request=original_request
            # Root helper rejects taking over a pre-existing website and keeps its previous routes.
            before=routes.read_bytes()
            malicious={**inputs[1],'name':'collision','access':{**inputs[1]['access'],'shared_port':True,'tls':{**tls,'domain':'existing.test'}},'hops':[{'node_id':node['id'],'port':443}]}
            rejected=h.request('/api/lines','POST',malicious)
            h.publish(expected='rolled_back')
            assert routes.read_bytes()==before
            assert b'existing-site 127.0.0.1' in https(public_port,'existing.test')
            h.request('/api/lines/'+rejected['id'],'DELETE')
            for line in lines[:2]: h.request('/api/lines/'+line['id'],'DELETE')
            h.publish()
            assert routes.read_text()==OWNER+'\n\n'
            assert b'existing-site 127.0.0.1' in https(public_port,'existing.test')
            print('PASS Nginx SNI routing, PROXY protocol, preserved site, rollback and route deletion',flush=True)
        except BaseException:
            for log in h.root.rglob('*.log'):
                print('LOG '+str(log.relative_to(h.root))+'\n'+log.read_text(errors='replace')[-2000:],flush=True)
            raise
        finally:
            watcher_stop.set()
            if watcher: watcher.join(timeout=5)
            h.close()


if __name__=='__main__':
    signal.signal(signal.SIGTERM,lambda *_:(_ for _ in ()).throw(KeyboardInterrupt()))
    main()
