"""Menu interaction checks; all service operations are replaced with mocks."""
import contextlib
import errno
import io
import os
from pathlib import Path
import pty
import select
import signal
import sqlite3
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import chain_install as installer


class Menu(unittest.TestCase):
    def test_service_menu_operates_all_installed_roles_without_submenu(self):
        for choice, action in [('4', 'status'), ('6', 'restart'), ('10', 'start'), ('11', 'stop')]:
            for roles in [('panel', 'agent'), ('agent',), ('panel',), ()]:
                with self.subTest(action=action, roles=roles), \
                        patch.object(installer, 'Manager') as manager, \
                        patch.object(installer, 'choose', side_effect=[choice, '0']) as choose, \
                        patch.object(installer, 'select_role', side_effect=AssertionError('no component submenu')), \
                        contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    manager.return_value.state.return_value = {'roles': {r: {'version': '0.2.50'} for r in roles}}
                    installer.menu(Path('/fixture'))
                    calls = [c.args[0] for c in manager.return_value.operate.call_args_list]
                    self.assertEqual([(a.action, a.component, a.root) for a in calls],
                                     [(action, r, Path('/fixture')) for r in installer.ROLES if r in roles])
                    self.assertEqual(choose.call_count, 2)

    def test_service_menu_attempts_agent_when_panel_fails(self):
        with patch.object(installer, 'Manager') as manager, \
                patch.object(installer, 'choose', side_effect=['6', '0']), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()) as errors:
            manager.return_value.state.return_value = {'roles': {r: {'version': '0.2.50'} for r in installer.ROLES}}
            manager.return_value.operate.side_effect = [ValueError('fixture start failure'), None]
            installer.menu(Path('/fixture'))
            self.assertEqual([c.args[0].component for c in manager.return_value.operate.call_args_list], list(installer.ROLES))
            self.assertIn('fixture start failure', errors.getvalue())

    def terminal(self, code, steps):
        pid, fd = pty.fork()
        if pid == 0:
            os.environ['TERM'] = 'xterm'
            os.execv(sys.executable, [sys.executable, '-B', '-c',
                     f'import sys; sys.path.insert(0, {str(Path(installer.__file__).parent)!r}); '
                     'import chain_install as m; ' + code])
        output, offset, done = b'', 0, False
        try:
            for marker, keys in steps:
                deadline = time.monotonic() + 10
                while marker not in output[offset:]:
                    self.assertLess(time.monotonic(), deadline, output)
                    if select.select([fd], [], [], .1)[0]:
                        output += os.read(fd, 65536)
                offset = len(output)
                os.write(fd, keys)
            deadline = time.monotonic() + 10
            while True:
                waited, status = os.waitpid(pid, os.WNOHANG)
                if waited:
                    done = True
                    while select.select([fd], [], [], 0)[0]:
                        try:
                            chunk = os.read(fd, 65536)
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
                            break
                        if not chunk:
                            break
                        output += chunk
                    break
                self.assertLess(time.monotonic(), deadline, output)
                if select.select([fd], [], [], .1)[0]:
                    try:
                        output += os.read(fd, 65536)
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
            self.assertEqual(os.waitstatus_to_exitcode(status), 0, output)
            return output
        finally:
            try:
                for sig in (signal.SIGTERM, signal.SIGKILL):
                    with contextlib.suppress(ProcessLookupError):
                        os.killpg(pid, sig)
                    deadline = time.monotonic() + 2
                    while time.monotonic() < deadline:
                        if not done and os.waitpid(pid, os.WNOHANG)[0]:
                            done = True
                        try:
                            os.killpg(pid, 0)
                        except ProcessLookupError:
                            break
                        time.sleep(.05)
                    else:
                        continue
                    break
                with self.assertRaises(ProcessLookupError):
                    os.killpg(pid, 0)
            finally:
                os.close(fd)

    @unittest.skipIf(installer.readline is None, 'readline unavailable; fallback tested separately')
    def test_real_terminal_editing_and_no_history(self):
        code = "print('RESULT=' + m.choose('TEST', {'1':'one','2':'two'})); print('HISTORY=' + str(m.readline.get_current_history_length()))"
        for keys, expected in ((b'12\x1b[D\x1b[3~\n', b'1'),
                               (b'12\x1b[D\x1b[C\x7f\n', b'1'),
                               (b'\x1b[A\x1b[B2\n', b'2'), (b' 2 \n', b'2'),
                               (b'\t2\n', b'2'), (b' \n', b'1')):
            with self.subTest(keys=keys):
                output = self.terminal(code, [(b'[1]: ', keys)])
                self.assertIn(b'RESULT=' + expected, output)
                self.assertIn(b'HISTORY=0', output)
                self.assertNotIn(b'^[[', output)

    def test_real_terminal_without_readline_filters_arrows(self):
        output = self.terminal("m.readline = None; print('RESULT=' + m.choose('TEST', {'1':'one','2':'two'}))",
                               [(b'[1]: ', b'\x1b[A\x1b[B 2 \n')])
        self.assertIn(b'RESULT=2', output)
        self.assertNotIn(b'^[[', output)

    def test_real_terminal_connection_code_is_not_echoed(self):
        code = ("from unittest.mock import Mock; manager = Mock(); manager.state.return_value = {'roles':{}}; "
                "args = m.make_parser().parse_args(['install','--component','agent']);\n"
                "try: m.agent_questions(manager, args)\n"
                "except KeyboardInterrupt: print('CANCELLED')")
        output = self.terminal(code, [(b': ', b'PRIVATE_TEST_SENTINEL\n'), (b': ', b'0\n')])
        self.assertNotIn(b'PRIVATE_TEST_SENTINEL', output)
        self.assertIn(b'CANCELLED', output)

    def test_real_terminal_follow_interrupt_stops_tail(self):
        with tempfile.TemporaryDirectory() as root:
            log = Path(root) / 'var/lib/chain-agent/diagnostics.log'
            log.parent.mkdir(parents=True)
            log.write_text('LOG_READY\n')
            code = (f"from pathlib import Path; from unittest.mock import Mock; manager=m.Manager(Path({root!r})); "
                    "manager.live=True; manager.require_role=Mock(); "
                    "args=m.make_parser().parse_args(['logs','--component','agent','--log-source','application','--follow']); "
                    "manager.operate(args); print('RETURNED'); input('EXIT: ')")
            output = self.terminal(code, [(b'LOG_READY', b'\x03'), (b'EXIT: ', b'\n')])
            self.assertIn('已停止跟随日志'.encode(), output)
            self.assertIn(b'RETURNED', output)

    def test_real_terminal_submenu_interrupt_returns(self):
        code = ("from pathlib import Path; from unittest.mock import Mock; manager=Mock(live=False); "
                "manager.state.return_value={'roles':{'panel':{'version':'1'},'agent':{'version':'1'}}}; "
                "m.Manager=lambda _:manager; m.menu(Path('/fixture')); "
                "manager.operate.assert_not_called(); print('CANCELLED')")
        output = self.terminal(code, [(b'[4]: ', b'5\n'), (b'[1]: ', b'\x03'), (b'[4]: ', b'0\n')])
        self.assertIn('返回主菜单'.encode(), output)
        self.assertIn(b'CANCELLED', output)

    def run_menu(self, answers, roles=('panel', 'agent'), error=None):
        manager = Mock(live=True)
        manager.state.return_value = {'roles': {r: {'version': '1.0.0'} for r in roles}}
        manager.operate.side_effect = error
        output = io.StringIO()
        with patch.object(installer, 'Manager', return_value=manager), \
                patch('builtins.input', side_effect=answers), \
                patch.object(installer, 'install_wizard') as install, \
                patch('chain_ingress.Ingress') as ingress, \
                contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
            installer.menu(Path('/fixture'))
        return manager, install, ingress, output.getvalue()

    def test_cancel_component_reinstall_logs_and_ingress(self):
        for answers in (['5', '0', '0'], ['1', '', '0'], ['2', '0', '0'],
                        ['5', '1', '0', '0'], ['14', '', '0'], ['15', '0', '0']):
            with self.subTest(answers=answers):
                manager, install, ingress, _ = self.run_menu(answers)
                manager.operate.assert_not_called()
                install.assert_not_called()
                ingress.assert_not_called()

    def test_panel_rejects_kernel_source_and_retries_in_place(self):
        manager, _, _, output = self.run_menu(['5', '1', '3', '1', '1', '0'])
        args = manager.operate.call_args.args[0]
        self.assertEqual((args.log_source, args.follow), ('service', False))
        self.assertIn('请输入上面的选项编号', output)

    def test_submenu_interrupt_and_database_error_return(self):
        manager, _, _, output = self.run_menu(['5', KeyboardInterrupt(), '13', '0'],
                                              error=sqlite3.DatabaseError('fixture damaged database'))
        manager.operate.assert_called_once()
        self.assertIn('返回主菜单', output)
        self.assertIn('fixture damaged database', output)

    def test_ingress_confirm_and_result(self):
        for choice, action in (('14', 'enable'), ('15', 'disable')):
            _, _, ingress, output = self.run_menu([choice, '1', '0'])
            ingress.return_value.operate.assert_called_once_with(action)
            self.assertIn('统一 443 已', output)

    def test_agent_code_can_cancel(self):
        manager = Mock()
        manager.state.return_value = {'roles': {}}
        args = installer.make_parser().parse_args(['install', '--component', 'agent'])
        with patch.object(installer.getpass, 'getpass', return_value='0'):
            with self.assertRaises(KeyboardInterrupt):
                installer.agent_questions(manager, args)

    def test_missing_login_token_explains_the_problem(self):
        with tempfile.TemporaryDirectory() as root:
            manager = installer.Manager(Path(root))
            output = io.StringIO()
            with patch.object(manager, 'require_role'), \
                    patch.object(installer, 'parse_env', return_value={'CHAIN_LISTEN': '127.0.0.1:8088'}), \
                    contextlib.redirect_stdout(output):
                manager.show_access()
            self.assertIn('管理员令牌文件尚未生成', output.getvalue())
            self.assertNotIn('选择「查看登录信息」', output.getvalue())

    def test_agent_update_failure_reports_partial_success(self):
        with tempfile.TemporaryDirectory() as root:
            base = Path(root)
            (base / 'current-panel').mkdir()
            manager = Mock(live=True, base=base)
            manager.state.return_value = {'roles': {'panel': {}, 'agent': {}}}
            args = installer.make_parser().parse_args(['update', '--non-interactive'])
            args.reuse_current = True
            output = io.StringIO()
            with patch.object(installer, 'verify_tree', return_value={}), \
                    patch.object(installer, 'compatible_release'), \
                    patch('chain_time.status', return_value={'NTP': 'yes', 'NTPSynchronized': 'yes'}), \
                    patch.object(installer, 'provision_local_agent', side_effect=ValueError('fixture agent failure')), \
                    contextlib.redirect_stderr(output):
                with self.assertRaisesRegex(ValueError, 'fixture agent failure'):
                    installer.install_package(manager, args)
            manager.install.assert_called_once()
            self.assertIn('部分更新完成', output.getvalue())
            self.assertIn('重新选择更新', output.getvalue())


if __name__ == '__main__':
    unittest.main()
