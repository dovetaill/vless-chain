"""Owned Nginx SNI front door. Only root changes system config; Agent submits routes."""
import argparse
import contextlib
import fcntl
import fnmatch
import json
import os
from pathlib import Path
import pwd
import re
import socket
import subprocess
import sys
import uuid

from chain_common import atomic_write, json_write, require
from chain_network import domain_name, install_packages

OWNER = '# chain-ingress managed v1'
LISTEN = re.compile(r'(?m)^([ \t]*listen[ \t]+)([^;\n]+);')


def migrate(text, port):
    """Change only ordinary HTTP TLS listeners; unsupported layouts fail before writes."""
    def replace(match):
        words = match[2].split()
        address = words[0]
        if address.rsplit(':', 1)[-1] != '443':
            return match[0]
        if 'quic' in words or 'udp' in words:
            return match[0]
        require(address in ('443', '0.0.0.0:443', '*:443', '[::]:443'), '暂不自动迁移绑定指定 IP 的 443，请先调整 Nginx 监听配置。')
        require('ssl' in words and 'proxy_protocol' not in words, '统一 443 需要普通 HTTP TLS 站点，暂不迁移非 TLS / PROXY protocol 监听。')
        words[0] = ('[::1]:' if address == '[::]:443' else '127.0.0.1:') + str(port)
        words.append('proxy_protocol')
        return match[1] + ' '.join(words) + ';'
    return LISTEN.sub(replace, text)


def route_config(routes, blocked):
    require(isinstance(routes, list) and len(routes) <= 256, '路由列表无效或超过 256 条。')
    seen, lines = set(), []
    for route in routes:
        require(isinstance(route, dict) and set(route) == {'sni', 'port'}, '路由只允许 SNI 和本机端口。')
        name = domain_name(route['sni'])
        port = route['port']
        require(type(port) is int and 50000 <= port <= 65535, '路由只允许 Agent 内部端口 50000–65535。')
        require(name not in seen, 'SNI 重复：' + name)
        for pattern in blocked:
            matches = (fnmatch.fnmatchcase(name, pattern) or
                       pattern.startswith('.') and (name == pattern[1:] or name.endswith(pattern)))
            if pattern.startswith('~'):
                try:
                    matches = bool(re.search(pattern.lstrip('~*'), name, re.I if pattern.startswith('~*') else 0))
                except re.error:
                    matches = True
            require(not matches, 'SNI 与现有网站冲突，不能覆盖：' + name)
        seen.add(name)
        lines.append(f'    {name} 127.0.0.1:{port};')
    return OWNER + '\n' + '\n'.join(sorted(lines)) + '\n'


def refresh_panel_site(path):
    """Keep the rollback snapshot in step with the panel installer's own site updates."""
    ingress = Ingress()
    if not ingress.state_path.exists():
        return
    with (ingress.directory / 'process.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        state = json.loads(ingress.state_path.read_text())
        text = path.read_text()
        def restore(match):
            words = match[2].split()
            address = words[0]
            if address in (f'127.0.0.1:{state["port"]}', f'[::1]:{state["port"]}'):
                words[0] = '[::]:443' if address.startswith('[') else '443'
                words = [word for word in words if word != 'proxy_protocol']
                return match[1] + ' '.join(words) + ';'
            return match[0]
        state['files'][str(path.resolve())] = {'before': LISTEN.sub(restore, text), 'after': text, 'mode': path.stat().st_mode & 0o777}
        state['blocked'] = sorted(set(name.strip('"\'').lower() for value in state['files'].values()
            for names in re.findall(r'(?m)^\s*server_name\s+([^;]+);', value['before']) for name in names.split() if name != '_'))
        json_write(ingress.state_path, state)


class Ingress:
    def __init__(self, root=Path('/')):
        self.root = Path(root).resolve()
        self.directory = self.path('/var/lib/chain-ingress')
        self.config = self.path('/etc/chain-ingress')
        self.state_path = self.directory / 'state.json'
        self.status_path = self.directory / 'status.json'
        self.request_path = self.path('/var/lib/chain-agent/ingress-request.json')

    def path(self, value):
        return self.root / value.lstrip('/')

    def command(self, *args):
        return subprocess.run(args, check=True, capture_output=True, text=True).stdout

    def operate(self, action):
        self.directory.mkdir(mode=0o755, parents=True, exist_ok=True)
        with (self.directory / 'process.lock').open('w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            getattr(self, action)()

    def reload(self):
        self.command('nginx', '-t')
        self.command('systemctl', 'reload', 'nginx')

    def status(self, **value):
        atomic_write(self.status_path, json.dumps(value, ensure_ascii=False), 0o644)

    def enable(self):
        if self.state_path.exists():
            require(json.loads(self.status_path.read_text()).get('ready'), '统一入口状态异常，请先检查或关闭后重试。')
            return
        require(self.path('/etc/systemd/system/chain-agent.service').exists(), '请先安装本机 Agent。')
        for name in ['chain-ingress.service', 'chain-ingress.path']:
            unit = self.path('/etc/systemd/system') / name
            require(not unit.exists() or unit.read_text().startswith(OWNER), '系统服务不归本项目管理：' + name)
        dump = self.command('nginx', '-T')
        sections = re.split(r'^# configuration file ', dump, flags=re.M)[1:]
        require(sections, '无法读取 Nginx 配置。')
        require(not re.search(r'(?m)^\s*(?:real_ip_header|set_real_ip_from)\s', dump), '已有自定义真实 IP 信任配置，需先整合 PROXY protocol，不能自动覆盖。')
        # Existing stream blocks need their own orchestration; never overwrite them.
        require(not any(re.search(r'(?m)^\s*stream\s*\{', section) for section in sections), '已有 Nginx stream 配置，不能自动接管，请先整合现有分流。')
        require(any(match[2].split()[0] in ('443', '0.0.0.0:443', '*:443') and 'ssl' in match[2].split()
                    for match in LISTEN.finditer(dump)), '统一入口需要可保留的 IPv4 HTTPS 监听。')
        ipv6 = any(match[2].split()[0] == '[::]:443' and 'ssl' in match[2].split() for match in LISTEN.finditer(dump))
        for port in range(14443, 15000):
            try:
                with socket.socket() as check:
                    check.bind(('127.0.0.1', port))
                if ipv6:
                    with socket.socket(socket.AF_INET6) as check:
                        check.bind(('::1', port))
                break
            except OSError:
                continue
        else:
            raise ValueError('没有可用的网站内部端口。')
        files, blocked = {}, []
        for section in sections:
            filename, _, body = section.partition('\n')
            path = Path(filename.rstrip(':')).resolve(strict=True)
            text = path.read_text()
            changed = migrate(text, port)
            if changed != text:
                for names in re.findall(r'(?m)^\s*server_name\s+([^;]+);', text):
                    blocked.extend(name.strip('"\'').lower() for name in names.split() if name != '_')
                files[str(path)] = {'before': text, 'after': changed, 'mode': path.stat().st_mode & 0o777}
        require(files, '未找到可保留的 Nginx HTTPS 站点。')
        main = Path(sections[0].split('\n', 1)[0].rstrip(':')).resolve(strict=True)
        text = files.get(str(main), {}).get('after', main.read_text())
        text, count = re.subn(r'(?m)^(\s*http\s*\{)', r'\1\n    real_ip_header proxy_protocol;\n    set_real_ip_from 127.0.0.1;\n    set_real_ip_from ::1;', text)
        require(count == 1, '无法定位 Nginx HTTP 主配置。')
        if 'ngx_stream_module.so' not in dump:
            module = Path('/usr/lib/nginx/modules/ngx_stream_module.so')
            if not module.is_file() and self.root == Path('/'):
                install_packages(['libnginx-mod-stream'])
                dump = self.command('nginx', '-T')
            require(module.is_file(), '缺少 Nginx stream 模块，请先安装系统的 libnginx-mod-stream。')
            if 'ngx_stream_module.so' not in dump:
                text = f'load_module {module};\n' + text
        text += f'\n{OWNER}\ninclude {self.config}/stream.conf;\n'
        files[str(main)] = {'before': main.read_text(), 'after': text, 'mode': main.stat().st_mode & 0o777}
        require(not self.config.exists(), '统一入口配置目录已存在，拒绝覆盖。')
        self.config.mkdir(mode=0o755, parents=True)
        self.directory.mkdir(mode=0o755, parents=True, exist_ok=True)
        stream = (f'{OWNER}\nstream {{\n map $ssl_preread_server_name $chain_backend {{\n'
                  f'    hostnames;\n    default 127.0.0.1:{port};\n    include {self.config}/routes.conf;\n }}\n'
                  ' server {\n    listen 443;\n' + ('    listen [::]:443;\n' if ipv6 else '') + '    ssl_preread on;\n'
                  '    proxy_pass $chain_backend;\n    proxy_protocol on;\n    proxy_connect_timeout 10s;\n    proxy_timeout 1h;\n }\n}\n')
        atomic_write(self.config / 'stream.conf', stream, 0o644)
        atomic_write(self.config / 'routes.conf', OWNER + '\n', 0o644)
        state = {'files': files, 'blocked': sorted(set(blocked)), 'port': port}
        json_write(self.state_path, state)
        try:
            for filename, value in files.items():
                atomic_write(filename, value['after'], value['mode'])
            self.reload()
            self.install_units()
            self.status(ready=True, applied=True, id='', error='')
            if self.request_path.exists():
                self.apply()
        except BaseException:
            for filename, value in files.items():
                atomic_write(filename, value['before'], value['mode'])
            with contextlib.suppress(Exception):
                self.reload()
            self.remove_units()
            for name in ['stream.conf', 'routes.conf']:
                (self.config / name).unlink(missing_ok=True)
            self.config.rmdir()
            self.state_path.unlink(missing_ok=True)
            self.status(ready=False, error='启用失败，已恢复原配置')
            raise

    def install_units(self):
        units = self.path('/etc/systemd/system')
        script = self.path('/opt/chain/current-agent/scripts/chain_ingress.py')
        service = (f'{OWNER}\n[Unit]\nDescription=Chain Nginx SNI routes\nStartLimitIntervalSec=0\n'
                   f'[Service]\nType=oneshot\nExecStart=/usr/bin/python3 -B {script} apply\n')
        watch = (f'{OWNER}\n[Unit]\nDescription=Watch Chain SNI route requests\n'
                 f'[Path]\nPathChanged={self.request_path}\nUnit=chain-ingress.service\n'
                 '[Install]\nWantedBy=multi-user.target\n')
        for name, body in [('chain-ingress.service', service), ('chain-ingress.path', watch)]:
            path = units / name
            require(not path.exists() or path.read_text().startswith(OWNER), '系统服务不归本项目管理：' + name)
            atomic_write(path, body, 0o644)
        self.command('systemctl', 'daemon-reload')
        self.command('systemctl', 'enable', '--now', 'chain-ingress.path')

    def remove_units(self):
        watch = self.path('/etc/systemd/system/chain-ingress.path')
        if watch.exists() and watch.read_text().startswith(OWNER):
            with contextlib.suppress(Exception):
                self.command('systemctl', 'disable', '--now', 'chain-ingress.path')
        for name in ['chain-ingress.service', 'chain-ingress.path']:
            path = self.path('/etc/systemd/system') / name
            if path.exists() and path.read_text().startswith(OWNER):
                path.unlink()
        self.command('systemctl', 'daemon-reload')

    def apply(self):
        state = json.loads(self.state_path.read_text())
        request_id = ''
        try:
            fd = os.open(self.request_path, os.O_RDONLY | os.O_NOFOLLOW)
            with os.fdopen(fd) as stream:
                require(os.fstat(stream.fileno()).st_uid == pwd.getpwnam('chain-agent').pw_uid, '路由请求归属不匹配。')
                body = stream.read(65537)
                require(len(body) <= 65536, '路由请求过大。')
                request = json.loads(body)
            request_id = str(uuid.UUID(request['id']))
            body = route_config(request['routes'], state['blocked'])
            path = self.config / 'routes.conf'
            old = path.read_bytes()
            try:
                atomic_write(path, body, 0o644)
                self.reload()
            except BaseException:
                atomic_write(path, old, 0o644)
                with contextlib.suppress(Exception):
                    self.reload()
                raise
            self.status(ready=True, applied=True, id=request_id, error='')
        except Exception as error:
            self.status(ready=True, applied=False, id=request_id, error=str(error))
            raise

    def disable(self):
        if not self.state_path.exists():
            return
        state = json.loads(self.state_path.read_text())
        # Concurrent manual changes are preserved, never replaced by an old snapshot.
        for filename, value in state['files'].items():
            require(Path(filename).read_text() == value['after'], '配置已被另行修改，请先核对再恢复：' + filename)
        for filename, value in state['files'].items():
            atomic_write(filename, value['before'], value['mode'])
        try:
            self.reload()
        except BaseException:
            for filename, value in state['files'].items():
                atomic_write(filename, value['after'], value['mode'])
            with contextlib.suppress(Exception):
                self.reload()
            raise
        self.remove_units()
        self.status(ready=False, applied=True, id='', error='')
        for name in ['stream.conf', 'routes.conf']:
            (self.config / name).unlink()
        self.config.rmdir()
        self.state_path.unlink()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('action', choices=['enable', 'disable', 'apply'])
    args = parser.parse_args()
    require(os.geteuid() == 0, '需要 root 权限。')
    ingress = Ingress()
    ingress.operate(args.action)


if __name__ == '__main__':
    main()
