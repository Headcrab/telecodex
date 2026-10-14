#!/usr/bin/env python3
"""Topic icon journeys through actual HTTP and SQLite, with synthetic accounts.

No live Telegram, Matrix or model calls. Run with a built telecodex binary.
"""
import collections
import http.server
import json
import pathlib
import signal
import subprocess
import sys
import tempfile
import threading
import time


def stop_bridge(process, timeout=8):
    """Interrupt and reap the bridge, forcing shutdown if it exceeds the timeout.

    Check stderr for the fixture token after either shutdown path, and preserve
    a timeout as a test failure even when the forced shutdown succeeds.
    """
    process.send_signal(signal.SIGINT)
    timeout_error = None
    try:
        _, error = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired as exc:
        timeout_error = exc
        process.kill()
        _, error = process.communicate(timeout=timeout)
    assert 'synthetic-icon-token' not in error
    if timeout_error is not None:
        raise timeout_error


def main(binary):
    """Exercise icon commands against a loopback Telegram API and temporary DB."""
    with tempfile.TemporaryDirectory(prefix='telecodex-icons-') as directory:
        root = pathlib.Path(directory)
        updates, calls = collections.deque(), []
        lock = threading.RLock()
        sequence = 0
        message_id = 500
        catalog = [{'emoji': '💬', 'custom_emoji_id': str(1000 + i)} for i in range(112)]
        edit_error = None
        catalog_error = False

        class Telegram(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                """Record API requests and return catalog, message or error fixtures."""
                nonlocal message_id
                payload = json.loads(self.rfile.read(int(self.headers.get('Content-Length', '0'))) or b'{}')
                method = self.path.rsplit('/', 1)[-1]
                with lock:
                    calls.append((method, payload))
                    if method == 'getMe':
                        result = {'id': 123, 'is_bot': True, 'first_name': 'Fixture', 'has_topics_enabled': True}
                    elif method == 'getUpdates':
                        result = [updates.popleft()] if updates else []
                    elif method == 'getForumTopicIconStickers':
                        result = catalog
                    elif method in ('sendMessage', 'editMessageText'):
                        if method == 'sendMessage':
                            message_id += 1
                        result = dict(message_id=payload.get('message_id', message_id),
                                      message_thread_id=payload.get('message_thread_id'),
                                      chat={'id': payload['chat_id'], 'type': 'private'},
                                      text=payload['text'],
                                      **{'from': {'id': 123, 'is_bot': True, 'first_name': 'Fixture'}})
                    else:
                        result = True
                    response = {'ok': True, 'result': result}
                    if method == 'editForumTopic' and edit_error:
                        response = {'ok': False, 'description': edit_error}
                    if method == 'getForumTopicIconStickers' and catalog_error:
                        response = {'ok': False, 'description': 'catalog unavailable'}
                if method == 'getUpdates' and not result:
                    time.sleep(.02)
                data = json.dumps(response).encode()
                self.send_response(200)
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                try:
                    self.wfile.write(data)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def log_message(self, *_):
                """Suppress fixture HTTP logs, which contain the synthetic bot token."""
                pass

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Telegram)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        native = root / 'codex'
        native.write_text(f'#!/bin/sh\ntouch "{root}/unexpected-native-call"\nexit 7\n')
        native.chmod(0o700)
        config = root / 'local.toml'
        config.write_text(f'''db_path="{root}/state.sqlite"
startup_admin_ids=[100,101]
poll_timeout_seconds=1
[telegram]
bot_token="synthetic-icon-token"
api_base="http://127.0.0.1:{server.server_port}"
[codex]
binary="{native}"
default_cwd="{root}"
import_cli_history=false
import_desktop_history=false
''')
        process = None

        def start():
            """Start the bridge with isolated state and a model-call sentinel."""
            return subprocess.Popen([binary, str(config)], cwd=root, stdout=subprocess.PIPE,
                                    stderr=subprocess.PIPE, text=True,
                                    env={'HOME': str(root), 'CODEX_HOME': str(root / 'codex-home'),
                                         'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8', 'RUST_LOG': 'warn'})

        def admit(*, topic=7, user=100, callback=None, chat=100):
            """Queue an /icon message or callback with the requested identity."""
            nonlocal sequence
            sequence += 1
            sender = {'id': user, 'is_bot': False, 'first_name': 'Fixture'}
            if callback:
                message, data = callback
                message = dict(message, message_thread_id=topic, chat={'id': chat, 'type': 'private'})
                update = {'update_id': sequence, 'callback_query': {
                    'id': str(sequence), 'from': sender, 'message': message, 'data': data}}
            else:
                update = {'update_id': sequence, 'message': {
                    'message_id': sequence, 'message_thread_id': topic,
                    'chat': {'id': chat, 'type': 'private'}, 'from': sender, 'text': '/icon'}}
            with lock:
                updates.append(update)

        def wait(method, predicate=lambda _: True, after=0):
            """Wait for a matching API call, failing on bridge exit or deadline."""
            deadline = time.monotonic() + 12
            while time.monotonic() < deadline:
                with lock:
                    matching = [(i, payload) for i, (name, payload) in enumerate(calls)
                                if i >= after and name == method and predicate(payload)]
                if matching:
                    return matching[-1]
                assert process.poll() is None, 'bridge exited unexpectedly'
                time.sleep(.02)
            raise AssertionError(f'timed out waiting for {method}')

        def open_picker():
            """Open a picker and return its message binding and keyboard buttons."""
            after = len(calls)
            admit()
            _, payload = wait('sendMessage', lambda p: bool(p.get('reply_markup')), after)
            message = {'message_id': message_id, 'message_thread_id': 7,
                       'chat': {'id': 100, 'type': 'private'}}
            buttons = payload['reply_markup']['inline_keyboard']
            return message, buttons

        def mutations():
            """Snapshot topic-edit requests for assertions about actual effects."""
            with lock:
                return [payload for method, payload in calls if method == 'editForumTopic']

        try:
            process = start()
            _, menu = wait('setMyCommands')
            assert any(c['command'] == 'icon' for c in menu['commands'])
            message, buttons = open_picker()
            first = buttons[0][0]['callback_data']
            assert all(len(b['callback_data'].encode()) <= 64 for row in buttons for b in row)
            after = len(calls)
            admit(callback=(message, buttons[-2][0]['callback_data']))
            _, page = wait('editMessageText', lambda p: 'Page 2 of 5' in p['text'], after)
            selected = page['reply_markup']['inline_keyboard'][0][0]['callback_data']
            after = len(calls)
            admit(callback=(message, selected.split(':s:')[0] + ':p:4'))
            _, last_page = wait('editMessageText', lambda p: 'Page 5 of 5' in p['text'], after)
            last_icons = last_page['reply_markup']['inline_keyboard'][:-2]
            assert sum(map(len, last_icons)) == 16
            selected = last_icons[-1][-1]['callback_data']
            for user, topic, chat, msg in [(101, 7, 100, message), (100, 8, 100, message),
                                          (100, 7, 200, message),
                                          (100, 7, 100, dict(message, message_id=999))]:
                after = len(calls)
                admit(user=user, topic=topic, chat=chat, callback=(msg, selected))
                wait('sendMessage', lambda p: 'belongs to another' in p['text'], after)
                assert not mutations()
            # Invalid indexes, malformed actions and unallowed users cannot mutate.
            after = len(calls)
            for action in [first.rsplit(':', 1)[0] + ':9999', first.split(':s:')[0] + ':p:9999',
                           first.split(':s:')[0] + ':s:bad']:
                admit(callback=(message, action))
            admit(user=999, callback=(message, first))
            admit(callback=(message, selected))
            _, confirmation = wait('editMessageText', lambda p: 'Topic icon updated' in p['text'], after)
            assert mutations() == [{'chat_id': 100, 'message_thread_id': 7, 'icon_custom_emoji_id': '1111'}]
            assert 'name' not in mutations()[0]
            assert confirmation['reply_markup']['inline_keyboard'] == []
            after = len(calls)
            admit(callback=(message, selected))
            wait('sendMessage', lambda p: 'expired' in p['text'], after)
            assert len(mutations()) == 1
            message, buttons = open_picker()
            after = len(calls)
            admit(callback=(message, buttons[-1][0]['callback_data']))
            wait('editMessageText', lambda p: 'Default topic icon restored' in p['text'], after)
            assert mutations()[-1]['icon_custom_emoji_id'] == ''
            message, buttons = open_picker()
            count, after = len(mutations()), len(calls)
            admit(callback=(message, buttons[-1][1]['callback_data']))
            wait('editMessageText', lambda p: 'cancelled' in p['text'], after)
            assert len(mutations()) == count
            message, buttons = open_picker()
            stop_bridge(process)
            process = start()
            after = len(calls)
            admit(callback=(message, buttons[0][0]['callback_data']))
            wait('sendMessage', lambda p: 'expired' in p['text'], after)
            assert len(mutations()) == count
            for topic, fragment in [(None, 'inside the topic'), (1, 'General topic')]:
                after = len(calls)
                admit(topic=topic)
                wait('sendMessage', lambda p: fragment in p['text'], after)
            catalog = []
            after = len(calls)
            admit()
            wait('sendMessage', lambda p: 'no topic icons' in p['text'], after)
            catalog_error = True
            after = len(calls)
            admit()
            wait('sendMessage', lambda p: 'Could not load' in p['text'], after)
            catalog_error = False
            catalog = [{'emoji': '💬', 'custom_emoji_id': '1000'}]
            edit_error = 'Bad Request: TOPIC_NOT_MODIFIED'
            message, buttons = open_picker()
            after = len(calls)
            admit(callback=(message, buttons[0][0]['callback_data']))
            wait('editMessageText', lambda p: 'Topic icon updated' in p['text'], after)
            edit_error = 'Bad Request: not enough rights'
            message, buttons = open_picker()
            after = len(calls)
            admit(callback=(message, buttons[0][0]['callback_data']))
            wait('editMessageText', lambda p: 'did not confirm' in p['text'], after)
            assert not (root / 'unexpected-native-call').exists(), '/icon must not invoke Codex'
            print('PASS: catalog, pagination, ownership, topic/message binding, invalid callbacks, '
                  'selection, reset, cancellation, duplicates, restart, General/root, API failure; no model calls')
        finally:
            try:
                if process and process.poll() is None:
                    stop_bridge(process)
            finally:
                server.shutdown()
                server.server_close()


if __name__ == '__main__':
    main(str(pathlib.Path(sys.argv[1]).resolve()))
