"""Network discovery and an isolated, system-package Nginx HTTPS site."""
import contextlib
import errno
import fnmatch
import grp
import ipaddress
import json
import os
from pathlib import Path
import pwd
import re
import secrets
import shlex
import shutil
import socket
import ssl
import subprocess
import time
import urllib.error
import urllib.request

from chain_common import atomic_write, output, require, run, operating_system
from chain_platform import (service_kind, service, service_state, service_path, service_definition,
                            brew_command, nginx_conf, ensure_nginx_service)

SITE_OWNER = '# chain-panel HTTPS managed site v1'


def public_ipv4():
    """Only use assigned NIC addresses, never a proxy/NAT's outbound address."""
    if operating_system() == 'darwin':
        return next(iter(sorted(a for a in server_addresses() if ':' not in a)), None)
    if not shutil.which('ip'):
        install_packages(['iproute2' if shutil.which('apt-get') else 'iproute'])
    addresses = server_addresses()
    ipv4 = sorted(address for address in addresses if ':' not in address)
    if not ipv4:
        return None
    try:
        # This reads the kernel route and sends no packets. A tunnel's private
        # source or a proxy exit is never accepted unless assigned on a NIC.
        routes = json.loads(output(['ip', '-j', '-4', 'route', 'get', '1.1.1.1']))
        for route in routes:
            if route.get('prefsrc') in ipv4:
                return route['prefsrc']
    except (OSError, ValueError, subprocess.CalledProcessError):
        pass
    return ipv4[0]


def server_addresses(ipv4=None):
    addresses = {ipv4} if ipv4 else set()
    if operating_system() == 'darwin':
        for block in re.split(r'(?m)^(?=\S)', output(['ifconfig', '-a'])):
            if not block or 'UP' not in block.splitlines()[0] or block.startswith('lo0:'):
                continue
            for value in re.findall(r'\binet6?\s+(\S+)', block):
                address = ipaddress.ip_address(value.split('%')[0])
                if address.is_global and not address.is_multicast:
                    addresses.add(str(address))
        return addresses
    if shutil.which('ip'):
        try:
            for interface in json.loads(output(['ip', '-j', 'address', 'show', 'scope', 'global'])):
                if 'UP' not in interface.get('flags', []) or interface.get('ifname') == 'lo':
                    continue
                for item in interface.get('addr_info', []):
                    if item.get('tentative') or item.get('dadfailed'):
                        continue
                    address = ipaddress.ip_address(item['local'])
                    if address.is_global and not address.is_multicast:
                        addresses.add(str(address))
        except (OSError, ValueError, KeyError, subprocess.CalledProcessError):
            pass
    return addresses


def available_port():
    """Probe both protocol families and TCP/UDP; never reuse an occupied port."""
    for _ in range(256):
        port = 10000 + secrets.randbelow(50000)
        try:
            with contextlib.ExitStack() as stack:
                for family, host in ((socket.AF_INET, '0.0.0.0'), (socket.AF_INET6, '::')):
                    for kind in (socket.SOCK_STREAM, socket.SOCK_DGRAM):
                        try:
                            sock = stack.enter_context(socket.socket(family, kind))
                            if family == socket.AF_INET6:
                                sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
                            sock.bind((host, port))
                        except OSError as error:
                            if family == socket.AF_INET6 and error.errno in (errno.EAFNOSUPPORT, errno.EADDRNOTAVAIL):
                                continue
                            raise
                return port
        except OSError:
            continue
    raise ValueError('未找到空闲随机端口，请检查本机端口使用情况。')


def domain_name(value):
    value = value.strip().rstrip('.').lower()
    require(len(value) <= 253 and '.' in value and all(
        re.fullmatch(r'[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?', label)
        for label in value.split('.')), '请填写域名，例如 panel.example.com，不含协议、端口或路径。')
    try:
        ipaddress.ip_address(value)
    except ValueError:
        return value
    raise ValueError('这里需要域名；使用 IP 访问请将域名留空。')


def domain_addresses(domain):
    """Use public DNS, not /etc/hosts; validate both A and AAAA records."""
    result = set()
    for record_type in ('A', 'AAAA'):
        last_error = None
        for resolver in ('https://dns.google/resolve', 'https://cloudflare-dns.com/dns-query'):
            try:
                request = urllib.request.Request(f'{resolver}?name={domain}&type={record_type}',
                                                 headers={'Accept': 'application/dns-json'})
                with urllib.request.urlopen(request, timeout=8) as response:
                    document = json.loads(response.read(65536))
                require(document.get('Status') in (0, 3), '公共 DNS 查询暂时失败。')
                for record in document.get('Answer', []):
                    if record.get('type') in (1, 28):
                        result.add(str(ipaddress.ip_address(record['data'])))
                last_error = None
                break
            except (OSError, ValueError, KeyError) as error:
                last_error = error
        require(last_error is None, f'无法查询 {domain} 的 {record_type} 记录，请检查服务器 HTTPS 出站网络后重试。')
    return result


def validate_dns(domain, addresses):
    resolved = domain_addresses(domain)
    require(resolved and resolved.issubset(addresses),
            f'域名解析不匹配：{domain} → {", ".join(sorted(resolved)) or "无 A/AAAA 记录"}；'
            f'本机地址：{", ".join(sorted(addresses)) or "未检测到公网地址"}。'
            '请修正 A/AAAA 记录；使用 CDN 代理时先关闭代理，再回车重试。')
    return resolved


def install_packages(packages):
    if not packages:
        return
    print('通过系统软件源安装：' + '、'.join(packages), flush=True)
    if operating_system() == 'darwin':
        run(brew_command('install', *packages))
    elif shutil.which('apt-get'):
        run(['apt-get', 'update'])
        run(['apt-get', 'install', '-y', *packages], env={**os.environ, 'DEBIAN_FRONTEND': 'noninteractive'})
    elif shutil.which('dnf'):
        run(['dnf', 'install', '-y', *packages])
    elif shutil.which('apk'):
        names = {'iproute': 'iproute2', 'libnginx-mod-stream': 'nginx-mod-stream'}
        run(['apk', 'add', '--no-cache', *[names.get(p, p) for p in packages]])
    elif shutil.which('yum'):
        run(['yum', 'install', '-y', *packages])
    elif shutil.which('pacman'):
        names = {'iproute': 'iproute2', 'libnginx-mod-stream': 'nginx-mod-stream'}
        run(['pacman', '-S', '--needed', '--noconfirm', *[names.get(p, p) for p in packages]])
    elif shutil.which('zypper'):
        run(['zypper', '--non-interactive', 'install', *['iproute2' if p == 'iproute' else p for p in packages]])
    else:
        raise ValueError('未识别系统包管理器，请先安装：' + '、'.join(packages))


def nginx_site_directory(dump):
    """Select a wildcard include directly inside the main HTTP block."""
    main = re.split(r'^# configuration file ', dump, flags=re.M)[1:2]
    require(main, '无法读取 Nginx 主配置。')
    config_directory = Path(main[0].split('\n', 1)[0].rstrip(':')).parent
    text = main[0].split('\n', 1)[1]
    lexer = shlex.shlex(text, posix=True, punctuation_chars='{};')
    lexer.whitespace_split = True
    stack, statement = [], []
    for token in lexer:
        # shlex can group punctuation such as ";}".
        tokens = list(token) if token and all(c in '{};' for c in token) else [token]
        for part in tokens:
            if part == '{':
                stack.append(statement[0] if statement else '')
                statement = []
            elif part == '}':
                if stack:
                    stack.pop()
                statement = []
            elif part == ';':
                if stack == ['http'] and len(statement) == 2 and statement[0] == 'include':
                    path = Path(statement[1])
                    if not path.is_absolute():
                        path = config_directory / path
                    if path.is_absolute() and path.name in ('*.conf', '*') and '*' not in str(path.parent):
                        return path.parent
                statement = []
            else:
                statement.append(part)
    raise ValueError('Nginx 的 http 配置中没有可用的独立站点 include；请使用系统默认 conf.d/sites-enabled 布局。')


def conflicting_site(dump, domain, owned):
    for section in re.split(r'^# configuration file ', dump, flags=re.M)[1:]:
        filename, _, body = section.partition('\n')
        if filename.rstrip(':') == str(owned):
            continue
        if filename.rstrip(':') == str(owned.with_name('chain-proxy-acme.conf')) and body.strip() in (
                proxy_acme_config().strip(), proxy_acme_config(True).strip()):
            continue
        body = re.sub(r'#[^\n]*', '', body)
        for names in re.findall(r'\bserver_name\s+([^;]+);', body):
            for name in names.split():
                name = name.strip('"\'').lower()
                matches = fnmatch.fnmatchcase(domain, name) or (name.startswith('.') and
                          (domain == name[1:] or domain.endswith(name)))
                if name.startswith('~'):
                    try:
                        matches = bool(re.search(name.lstrip('~*'), domain))
                    except re.error:
                        matches = True
                if matches:
                    return filename.rstrip(':')
    return None


def proxy_acme_config(ipv6=False):
    ipv6_listener = '    listen [::]:80;\n' if ipv6 else ''
    return (f'{SITE_OWNER}\nserver {{\n    listen 80;\n{ipv6_listener}'
            '    server_name ~^.+$;\n'
            '    location ^~ /.well-known/acme-challenge/ {\n'
            '        root /var/lib/chain-acme;\n'
            '        default_type text/plain;\n'
            '        try_files /agent$uri /panel$uri =404;\n'
            '    }\n    location / { return 404; }\n}\n')


def listeners(ports=(80, 443)):
    if operating_system() == 'darwin':
        probe = run(['lsof', '-nP', '-iTCP:' + ','.join(map(str, ports)), '-sTCP:LISTEN', '-Fpc'],
                    check=False, capture_output=True, text=True)
        require(probe.returncode in (0, 1), '无法检测端口归属。')
        return '\n'.join(line for line in probe.stdout.splitlines() if line.startswith('c'))
    if not shutil.which('ss'):
        install_packages(['iproute2' if shutil.which('apt-get') else 'iproute'])
    return output(['ss', '-H', '-ltnp', '( ' + ' or '.join(f'sport = :{p}' for p in ports) + ' )'])


def foreign_listeners(ports=(80, 443)):
    return '\n'.join(line for line in listeners(ports).splitlines()
                     if (line != 'cnginx' if operating_system() == 'darwin' else '"nginx"' not in line))


def install_proxy_acme(role, uid, gid, undo=None):
    """Provision HTTP-01 once at installation; the panel never controls Nginx."""
    site = HttpsSite('chain-proxy-acme.invalid', 0)
    if foreign_listeners((80,)):
        print('TCP 80 由旧服务占用，保留该服务；代理请使用 REALITY 或手动证书，HTTP-01 自动签发暂不可用。')
        return False
    site.prepare(ports=(80,))
    panel_site = site.site
    panel_previous = panel_site.read_bytes() if panel_site.exists() else None
    site.site = site.site.with_name('chain-proxy-acme.conf')
    require(not site.site.is_symlink() and (not site.site.exists() or site.site.read_text().startswith(SITE_OWNER + '\n')),
            f'{site.site} 不归本安装器管理，拒绝覆盖。')
    root = Path('/var/lib/chain-acme')
    require(not root.is_symlink(), f'{root} 不能是符号链接。')
    root.mkdir(mode=0o755, parents=True, exist_ok=True)
    root.chmod(0o755)
    for path in [root / role, root / role / '.well-known', root / role / '.well-known/acme-challenge']:
        require(not path.is_symlink() and (not path.exists() or path.stat().st_uid == uid), f'{path} 归属不匹配。')
        path.mkdir(mode=0o755, exist_ok=True)
        path.chmod(0o755)
        os.chown(path, uid, gid)
    previous = site.site.read_bytes() if site.site.exists() else None
    if undo is not None:
        undo.setdefault(site.site, previous)
        undo.setdefault(panel_site, panel_previous)
    try:
        if panel_previous is not None:
            migrated = panel_previous.replace(b'try_files $uri =404;', b'try_files $uri /agent$uri /panel$uri =404;')
            if migrated != panel_previous:
                atomic_write(panel_site, migrated, 0o644)
        atomic_write(site.site, proxy_acme_config(any(':' in address for address in server_addresses())), 0o644)
        site.reload()
        service('enable', 'nginx')
        return True
    except BaseException:
        if panel_previous is not None:
            atomic_write(panel_site, panel_previous, 0o644)
        if previous is None:
            site.site.unlink(missing_ok=True)
        else:
            atomic_write(site.site, previous, 0o644)
        site.reload()
        raise


class HttpsSite:
    def __init__(self, domain, port, ipv6=False):
        self.domain, self.port, self.ipv6 = domain, port, ipv6
        self.webroot = Path('/var/lib/chain-acme')
        self.certname = 'chain-panel-' + domain
        self.cert = Path('/etc/letsencrypt/live') / self.certname
        self.site = None
        self.https_port = 443
        self.certificate = self.key = None

    def prepare(self, ports=(80, 443)):
        # Refuse foreign port owners before package installation can start Nginx.
        conflicts = foreign_listeners(ports)
        require(not conflicts, f'{"/".join(map(str, ports))} 已被其他服务占用：{conflicts}；请运行交互安装向导选择接管或换端口。')
        install_packages([name for name in ('nginx', 'certbot', 'openssl') if not shutil.which(name)])
        if operating_system() == 'darwin' and not any(Path(p).is_file() for p in
                ('/opt/homebrew/opt/openssl@3/bin/openssl', '/usr/local/opt/openssl@3/bin/openssl')):
            install_packages(['openssl@3'])
        details = subprocess.run(['nginx', '-V'], check=True, capture_output=True, text=True).stderr
        conf = nginx_conf()
        require('--conf-path=' + str(conf) in shlex.split(details),
                f'检测到自定义 Nginx；自动 HTTPS 需要系统软件包的 {conf}。')
        if service_kind() == 'systemd':
            pid = output(['systemctl', 'show', 'nginx', '--property=MainPID', '--value'])
        else:
            # Inspect every master before creating or using the platform service.
            if operating_system() == 'darwin':
                masters = [line for line in output(['ps', '-axo', 'pid=,command=']).splitlines() if 'nginx: master process' in line]
            else:
                masters = []
                for path in Path('/proc').glob('[0-9]*/cmdline'):
                    with contextlib.suppress(OSError):
                        command = path.read_bytes().decode().replace('\0', ' ').strip()
                        if command.startswith('nginx: master process '):
                            masters.append(path.parent.name + ' ' + command)
            require(len(masters) <= 1, '存在多个 Nginx master，拒绝自动修改配置。')
            pid = masters[0].split()[0] if masters else ''
            if masters:
                command = shlex.split(masters[0].split('nginx: master process ', 1)[1])
                require(command and Path(shutil.which(command[0]) or command[0]).resolve() == Path(shutil.which('nginx')).resolve(),
                        '运行中的 Nginx 与系统命令不是同一程序。')
                require('-c' not in command or (command.index('-c') + 1 < len(command) and command[command.index('-c') + 1] == str(conf)),
                        '运行中的 Nginx 使用自定义配置，拒绝接管。')
            ensure_nginx_service(SITE_OWNER)
        if operating_system() == 'linux' and pid.isdigit() and int(pid):
            process = Path('/proc') / pid
            require((process / 'exe').resolve() == Path(shutil.which('nginx')).resolve(),
                    '运行中的 Nginx 与系统命令不是同一程序，拒绝修改其配置。')
            command = shlex.split((process / 'cmdline').read_bytes().decode().replace('\0', ' '))
            if '-c' in command:
                index = command.index('-c') + 1
                require(index < len(command) and command[index] == str(conf),
                        '运行中的 Nginx 使用自定义 -c 配置，无法自动接管。')
        run(['nginx', '-t'])
        dump = output(['nginx', '-T'])
        self.site = nginx_site_directory(dump) / 'chain-panel.conf'
        require(not self.site.is_symlink(), f'{self.site} 是已有链接，拒绝覆盖。')
        require(not self.site.exists() or self.site.read_text().startswith(SITE_OWNER + '\n'),
                f'{self.site} 不归本安装器管理，拒绝覆盖。')
        conflict = conflicting_site(dump, self.domain, self.site)
        require(not conflict, f'域名 {self.domain} 已在 {conflict} 配置，请使用独立面板域名。')

    def config(self, tls=False):
        public = f'https://{self.domain}' + (f':{self.https_port}' if self.https_port != 443 else '')
        ipv6_http = '    listen [::]:80;\n' if self.ipv6 else ''
        text = (f'{SITE_OWNER}\nserver {{\n    listen 80;\n{ipv6_http}    server_name {self.domain};\n'
                f'    location ^~ /.well-known/acme-challenge/ {{\n        root {self.webroot};\n'
                '        default_type text/plain;\n        try_files $uri /agent$uri /panel$uri =404;\n    }\n')
        text += (f'    location / {{ return 301 {public}$request_uri; }}\n' if tls else
                 '    location / { return 404; }\n')
        text += '}\n'
        if tls:
            ipv6_tls = f'    listen [::]:{self.https_port} ssl;\n' if self.ipv6 else ''
            text += (f'server {{\n    listen {self.https_port} ssl;\n{ipv6_tls}    server_name {self.domain};\n'
                     f'    ssl_certificate {self.cert}/fullchain.pem;\n'
                     f'    ssl_certificate_key {self.cert}/privkey.pem;\n'
                     '    ssl_protocols TLSv1.2 TLSv1.3;\n    client_max_body_size 2m;\n'
                     f'    location / {{\n        proxy_pass http://127.0.0.1:{self.port};\n'
                     '        proxy_http_version 1.1;\n        proxy_set_header Host $http_host;\n'
                     '        proxy_set_header X-Real-IP $remote_addr;\n'
                     '        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n'
                     '        proxy_set_header X-Forwarded-Proto https;\n'
                     '        proxy_read_timeout 120s;\n    }\n}\n')
        ingress = Path('/var/lib/chain-ingress/state.json')
        if tls and ingress.is_file():
            from chain_ingress import migrate
            text = migrate(text, json.loads(ingress.read_text())['port'])
        return text

    def reload(self):
        run(['nginx', '-t'])
        if service_kind() != 'systemd':
            service('reload' if self.service_state('nginx')[0] else 'start', 'nginx')
            return
        active = subprocess.run(['systemctl', 'is-active', '--quiet', 'nginx'], check=False).returncode == 0
        run(['systemctl', 'reload' if active else 'start', 'nginx'])

    def challenge_check(self, timeout=15):
        directory = self.webroot / '.well-known/acme-challenge'
        for path in (self.webroot, self.webroot / '.well-known', directory):
            require(not path.is_symlink(), f'{path} 是已有链接，拒绝接管。')
            path.mkdir(parents=True, exist_ok=True, mode=0o755)
            path.chmod(0o755)  # root's umask 077 must not prevent Nginx traversal.
        challenge = directory / ('chain-check-' + secrets.token_hex(16))
        value = secrets.token_hex(32).encode()
        url = f'http://127.0.0.1/.well-known/acme-challenge/{challenge.name}'
        # Check the domain's local vhost without requiring public-IP hairpin routing.
        request = urllib.request.Request(url, headers={'Host': self.domain})
        try:
            atomic_write(challenge, value, 0o644)
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            deadline = time.monotonic() + timeout
            # systemctl reload returns after sending HUP, before Nginx's new
            # workers necessarily serve this vhost. Keep the same probe alive
            # while old workers may still return a default site's 404/redirect.
            while True:
                try:
                    with opener.open(request, timeout=max(.1, min(3, deadline - time.monotonic()))) as response:
                        if response.read(1024) == value:
                            return
                        detail = 'HTTP 响应内容与验证文件不匹配，请检查 Nginx 是否命中正确站点及验证目录配置。'
                except urllib.error.HTTPError as error:
                    detail = (f'HTTP {error.code} {error.reason}；请求已到达 HTTP 服务。'
                              '请检查 Nginx 站点是否生效及验证目录权限。')
                    error.close()
                except OSError as error:
                    reason = error.reason if isinstance(error, urllib.error.URLError) else error
                    detail = (f'连接错误：{reason}；请检查本机 Nginx 是否监听 TCP 80 及回环访问规则。'
                              '本机自检不能证明公网可达性，证书签发还会从外部验证。')
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise ValueError(f'本机 HTTP 验证路径不可达或内容异常（已等待 {timeout:g} 秒）：{url}（Host: {self.domain}）\n{detail}')
                time.sleep(min(.5, remaining))
        finally:
            challenge.unlink(missing_ok=True)

    def renewal_paths(self):
        if service_kind() != 'systemd':
            return (Path('/etc/letsencrypt/renewal-hooks/deploy/chain-panel-nginx'),
                    Path('/etc/chain-panel-cert-renew.sh'), service_path('chain-panel-cert-renew'))
        return (Path('/etc/letsencrypt/renewal-hooks/deploy/chain-panel-nginx'),
                Path('/etc/systemd/system/chain-panel-cert-renew.service'),
                Path('/etc/systemd/system/chain-panel-cert-renew.timer'))

    def service_state(self, name):
        if service_kind() != 'systemd':
            return service_state(name)
        return tuple(subprocess.run(['systemctl', action, '--quiet', name], stdout=subprocess.DEVNULL,
                                    stderr=subprocess.DEVNULL).returncode == 0
                     for action in ('is-active', 'is-enabled'))

    def restore_service(self, name, previous):
        current = self.service_state(name)
        if current[0] != previous[0]:
            service('start' if previous[0] else 'stop', name)
        if current[1] != previous[1]:
            service('enable' if previous[1] else 'disable', name)

    def check_renewal_paths(self):
        for path in self.renewal_paths():
            require(not path.is_symlink() and (not path.exists() or SITE_OWNER in path.read_text()),
                    f'{path} 不归本安装器管理，拒绝覆盖。')

    def renewal(self):
        # A dedicated timer also works on distributions without a Certbot timer.
        hook, service_file, timer = self.renewal_paths()
        self.check_renewal_paths()
        nginx = shutil.which('nginx')
        certbot = shutil.which('certbot')
        atomic_write(hook, f'#!/bin/sh\n{SITE_OWNER}\nset -eu\n'
                     f'[ "${{RENEWED_LINEAGE:-}}" = {shlex.quote(str(self.cert))} ] || exit 0\n'
                     f'{shlex.quote(nginx)} -t\n{shlex.quote(nginx)} -s reload\n', 0o755)
        if service_kind() != 'systemd':
            atomic_write(service_file, f'#!/bin/sh\n{SITE_OWNER}\nexec {shlex.quote(certbot)} renew --quiet '
                         f'--config-dir /etc/letsencrypt --work-dir /var/lib/letsencrypt --logs-dir /var/log/letsencrypt '
                         f'--cert-name {shlex.quote(self.certname)}\n', 0o755)
            atomic_write(timer, service_definition('chain-panel-cert-renew', ['/bin/sh', service_file],
                         SITE_OWNER, interval=43200), 0o755 if service_kind() == 'openrc' else 0o644)
            service('enable', 'chain-panel-cert-renew')
            service('restart', 'chain-panel-cert-renew')
            return
        atomic_write(service_file, f'{SITE_OWNER}\n[Unit]\nDescription=Renew Chain Panel HTTPS certificate\n'
                     '[Service]\nType=oneshot\n'
                     f'ExecStart={certbot} renew --quiet --cert-name {self.certname}\n', 0o644)
        atomic_write(timer, f'{SITE_OWNER}\n[Unit]\nDescription=Check Chain Panel certificate twice daily\n'
                     '[Timer]\nOnCalendar=*-*-* 00,12:00:00\nRandomizedDelaySec=3600\nPersistent=true\n'
                     '[Install]\nWantedBy=timers.target\n', 0o644)
        run(['systemctl', 'daemon-reload'])
        run(['systemctl', 'enable', '--now', timer.name])
        run(['systemctl', 'is-active', '--quiet', timer.name])

    def certificate_valid(self):
        certificate, key = self.certificate or self.cert / 'fullchain.pem', self.key or self.cert / 'privkey.pem'
        if not certificate.exists() and not key.exists():
            return False
        require(certificate.is_file() and key.is_file(), '已有证书或私钥缺失，请恢复文件；不会重新申请证书。')
        ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER).load_cert_chain(certificate, key, password='')
        if run(['openssl', 'x509', '-in', certificate, '-noout', '-checkend', '0'],
               check=False, capture_output=True).returncode != 0:
            return False
        run(['openssl', 'verify', '-trusted', certificate, '-partial_chain',
             '-verify_hostname', self.domain, certificate], capture_output=True)
        return True

    @contextlib.contextmanager
    def manual_activate(self):
        """An isolated listener leaves existing Nginx sites and ports untouched."""
        require(self.certificate and self.key and self.certificate_valid(), '手动 HTTPS 证书无效或已过期。')
        directory = Path('/etc/chain-panel-https')
        config = directory / 'nginx.conf'
        unit = service_path('chain-panel-https')
        temporary = Path('/var/lib/chain-panel-https')
        marker = temporary / '.chain-owner'
        try:
            worker = pwd.getpwnam('nobody')
            group = grp.getgrgid(worker.pw_gid)
        except KeyError:
            raise ValueError('无法确认 nobody 工作用户及其主组，未配置独立 HTTPS 入口。') from None
        require(worker.pw_uid != 0 and worker.pw_gid != 0, '独立 HTTPS 入口需要非特权工作用户和组。')
        require(not temporary.is_symlink() and not os.path.ismount(temporary), f'临时目录不能是链接或挂载点：{temporary}')
        require(not temporary.exists() or (temporary.is_dir() and not marker.is_symlink()
                and marker.is_file() and marker.read_text().strip() == SITE_OWNER), f'{temporary} 不归本安装器管理。')
        for name in ('client_body', 'proxy', 'fastcgi', 'uwsgi', 'scgi', 'nginx.lock', 'nginx.lock.accept'):
            path = temporary / name
            require(not path.is_symlink() and not os.path.ismount(path), f'拒绝使用链接或挂载路径：{path}')
            if path.exists() and not name.startswith('nginx.lock'):
                require(path.is_dir() and path.stat().st_uid == worker.pw_uid and path.stat().st_mode & 0o700 == 0o700,
                        f'临时目录的属主或权限不匹配，请核对后处理：{path}')
        for path in (config, unit):
            require(not path.is_symlink() and (not path.exists() or SITE_OWNER in path.read_text()), f'{path} 不归本安装器管理。')
        require(not directory.is_symlink(), f'{directory} 不能为链接。')
        previous = {p: p.read_bytes() if p.exists() else None for p in (config, unit)}
        state = self.service_state('chain-panel-https')
        if not state[0]:
            require(not listeners((self.https_port,)), f'端口 {self.https_port} 仍被占用。')
        install_packages(['nginx'] if not shutil.which('nginx') else [])
        nginx = shutil.which('nginx')
        # Nginx quoted strings do not use shell escaping.
        quote = lambda p: '"' + str(p).replace('\\', '\\\\').replace('"', '\\"').replace('$', '\\$') + '"'
        require(not any(c in str(p) for p in (self.certificate, self.key) for c in '\n\r\0'), '证书路径无效。')
        ipv6 = f'listen [::]:{self.https_port} ssl;' if self.ipv6 else ''
        # Even nginx -t creates/chowns temp paths; never inherit the host defaults.
        body = (f'{SITE_OWNER}\nuser {quote(worker.pw_name)} {quote(group.gr_name)};\n'
                f'pid /var/run/chain-panel-https.pid;\nerror_log /var/log/chain-panel-https.log;\n'
                f'lock_file {quote(temporary / "nginx.lock")};\nevents {{}}\nhttp {{ access_log off;\n' +
                ''.join(f'{name}_temp_path {quote(temporary / name)};\n'
                        for name in ('client_body', 'proxy', 'fastcgi', 'uwsgi', 'scgi')) + 'server {\n'
                f'listen {self.https_port} ssl; {ipv6}\nserver_name {self.domain};\n'
                f'ssl_certificate {quote(self.certificate)};\nssl_certificate_key {quote(self.key)};\n'
                'ssl_protocols TLSv1.2 TLSv1.3;\nclient_max_body_size 2m;\n'
                f'location / {{ proxy_pass http://127.0.0.1:{self.port};\n'
                'proxy_http_version 1.1; proxy_set_header Host $http_host;\n'
                'proxy_set_header X-Real-IP $remote_addr; proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n'
                'proxy_set_header X-Forwarded-Proto https; proxy_read_timeout 120s;\n} } }\n')
        created_temporary = False
        try:
            if not temporary.exists():
                temporary.mkdir(mode=0o755)
                created_temporary = True
                temporary.chmod(0o755)
                atomic_write(marker, SITE_OWNER + '\n')
            atomic_write(config, body, 0o600)
            run([nginx, '-t', '-c', config])
            command = [nginx, '-c', str(config), '-g', 'daemon off;']
            definition = (f'{SITE_OWNER}\n[Unit]\nDescription=Chain Panel HTTPS\nAfter=network.target\n'
                          f'[Service]\nExecStart={shlex.join(command)}\nExecReload={nginx} -c {config} -s reload\n'
                          'Restart=on-failure\n[Install]\nWantedBy=multi-user.target\n') if service_kind() == 'systemd' else service_definition('chain-panel-https', command, SITE_OWNER)
            atomic_write(unit, definition, 0o755 if service_kind() == 'openrc' else 0o644)
            if service_kind() == 'systemd':
                run(['systemctl', 'daemon-reload'])
            service('enable', 'chain-panel-https')
            service('restart', 'chain-panel-https')
            require(self.service_state('chain-panel-https')[0], '独立 HTTPS 入口启动失败。')
            print('HTTPS 复用指定证书；请保留原续期工具，并在续期部署钩子中重载 chain-panel-https 服务。')
            yield
        except BaseException:
            service('stop', 'chain-panel-https', check=False)
            service('disable', 'chain-panel-https', check=False)
            for path, old in previous.items():
                if old is None:
                    path.unlink(missing_ok=True)
                else:
                    atomic_write(path, old, 0o600 if path == config else 0o755 if service_kind() == 'openrc' else 0o644)
            if service_kind() == 'systemd':
                run(['systemctl', 'daemon-reload'], check=False)
            self.restore_service('chain-panel-https', state)
            if created_temporary and not self.service_state('chain-panel-https')[0]:
                shutil.rmtree(temporary)
            raise

    @contextlib.contextmanager
    def activate(self):
        if self.certificate:
            with self.manual_activate():
                yield
            return
        nginx_state = self.service_state('nginx')
        timer_state = self.service_state('chain-panel-cert-renew.timer')
        previous, renewal = None, {}
        site_written = False
        renewal_started = False
        try:
            self.prepare(ports=(80, self.https_port))
            self.check_renewal_paths()
            reuse_certificate = self.certificate_valid()
            previous = self.site.read_bytes() if self.site.exists() else None
            renewal = {path: (path.read_bytes(), path.stat().st_mode & 0o777) if path.exists() else None
                       for path in self.renewal_paths()}
            site_written = True
            if reuse_certificate:
                print(f'复用已有且未过期的 HTTPS 证书：{self.certname}；不重复申请。', flush=True)
            else:
                atomic_write(self.site, self.config(), 0o644)
                self.reload()
                self.challenge_check()
                print('正在申请 HTTPS 证书（Let’s Encrypt），并设置自动续期…', flush=True)
                run(['certbot', 'certonly', '--non-interactive', '--agree-tos', '--register-unsafely-without-email',
                     '--config-dir', '/etc/letsencrypt', '--work-dir', '/var/lib/letsencrypt', '--logs-dir', '/var/log/letsencrypt',
                     '--webroot', '--webroot-path', self.webroot, '--cert-name', self.certname,
                     '--domain', self.domain, '--keep-until-expiring'])
            require((self.cert / 'fullchain.pem').is_file() and (self.cert / 'privkey.pem').is_file(), '证书文件缺失。')
            atomic_write(self.site, self.config(tls=True), 0o644)
            self.reload()
            renewal_started = True
            self.renewal()
            service('enable', 'nginx')
            yield
            from chain_ingress import refresh_panel_site
            refresh_panel_site(self.site)
        except BaseException:
            if site_written:
                if previous is None:
                    self.site.unlink(missing_ok=True)
                else:
                    atomic_write(self.site, previous, 0o644)
            if renewal_started:
                if service_kind() != 'systemd':
                    service('stop', 'chain-panel-cert-renew', check=False)
                for path, value in renewal.items():
                    if value is None:
                        path.unlink(missing_ok=True)
                    else:
                        atomic_write(path, value[0], value[1])
                if service_kind() == 'systemd':
                    run(['systemctl', 'daemon-reload'], check=False)
            try:
                if site_written and nginx_state[0]:
                    self.reload()
                self.restore_service('nginx', nginx_state)
                if renewal_started:
                    self.restore_service('chain-panel-cert-renew.timer', timer_state)
            except (OSError, subprocess.CalledProcessError) as error:
                print(f'站点/续期文件已恢复，但服务状态恢复失败：{error}；请执行 nginx -t 和 systemctl status nginx 检查。')
            raise
