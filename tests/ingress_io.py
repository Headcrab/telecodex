#!/usr/bin/env python3
"""Ingress regressions with actual HTTP, WebSocket and SQLite I/O. No live calls.

Usage: python3 independent-io-probes.py /absolute/telecodex-binary
Every token, HOME, CODEX_HOME, workspace, SQLite DB and server is synthetic.
Reports behavior without encoding candidate-specific expected assertions.
"""
import base64
import hashlib
import html
import http.server
import json
import pathlib
import re
import signal
import socketserver
import sqlite3
import struct
import subprocess
import sys
import tempfile
import threading
import time

BINARY = str(pathlib.Path(sys.argv[1]).resolve())


def token_probe():
    with tempfile.TemporaryDirectory(prefix='telecodex-review-token-') as name:
        root = pathlib.Path(name)
        token = 'synthetic-secret-token'

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                self.rfile.read(int(self.headers.get('Content-Length', '0')))
                body = json.dumps({'ok': True, 'result': token}).encode()
                self.send_response(200)
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        tokenfile = root / 'bot.token'
        tokenfile.write_text(token)
        tokenfile.chmod(0o600)
        cfg = root / 'local.toml'
        cfg.write_text(f'''db_path="{root}/state.sqlite"
startup_admin_ids=[100]
[telegram]
bot_token_file="{tokenfile}"
api_base="http://127.0.0.1:{server.server_port}"
[codex]
binary="{sys.executable}"
default_cwd="{root}"
import_cli_history=false
import_desktop_history=false
''')
        env = {'PATH': '/usr/bin:/bin', 'HOME': str(root),
               'CODEX_HOME': str(root / 'codex-home'), 'LANG': 'C.UTF-8'}
        try:
            result = subprocess.run([BINARY, str(cfg)], cwd=root, env=env,
                                    capture_output=True, text=True, timeout=5)
            assert result.returncode != 0
            assert 'getMe JSON decode failed' in result.stderr, 'probe did not reach Telegram response decoding'
            assert token not in result.stderr, 'malformed success leaked the bot token'
            print(json.dumps({'scenario': 'malformed_success_token',
                              'exit': result.returncode,
                              'token_in_stderr': token in result.stderr,
                              'stderr': result.stderr.strip()}), flush=True)
        finally:
            server.shutdown()
            server.server_close()


def scenario(mode):
    stalled_steer = mode == 'stalled_steer'
    retain_commentary = mode.startswith('retained_commentary')
    drafts = mode.endswith('_drafts')
    show_unfinished = '_hidden' not in mode and not stalled_steer
    foreign_events = mode.endswith('_foreign_events')
    stream_ended = mode.endswith('_eof')
    final_before_completion = mode.endswith('_continuation')
    commentary_only = mode.endswith('_only')
    empty_messages = '_empty' in mode
    markup_prefix = '_markup' in mode
    commentary = ['First completed progress.', 'Second progress: ' + 'x' * 3800]
    final_answer = 'Final answer.'
    acknowledge_steer = mode == 'accepted_steer'
    missing_binding = mode == 'missing_saved_binding'
    with tempfile.TemporaryDirectory(prefix='telecodex-review-steer-') as name:
        root = pathlib.Path(name)
        started = threading.Event()
        steered = threading.Event()
        rpc = []
        rpc_lock = threading.Lock()
        continuation_state = {}

        class Native(socketserver.BaseRequestHandler):
            def handle(self):
                stream = self.request.makefile('rb')
                headers = {}
                while True:
                    line = stream.readline()
                    if line == b'\r\n':
                        break
                    if not line:
                        return
                    if b':' in line:
                        key, value = line.split(b':', 1)
                        headers[key.strip().lower()] = value.strip()
                accept = base64.b64encode(hashlib.sha1(
                    headers[b'sec-websocket-key'] +
                    b'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest())
                self.request.sendall(
                    b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n'
                    b'Connection: Upgrade\r\nSec-WebSocket-Accept: ' + accept + b'\r\n\r\n')

                native_thread = f'synthetic-native-thread-{id(self)}'
                native_turn = f'synthetic-native-turn-{id(self)}'

                def send(value):
                    if 'method' in value and value['method'].startswith(('turn/', 'item/', 'thread/status')):
                        params = value.setdefault('params', {})
                        params.setdefault('threadId', 'synthetic-native-thread')
                        if value['method'].startswith('item/'):
                            params.setdefault('turnId', 'synthetic-native-turn')
                    serialized = json.dumps(value)
                    if stalled_steer:
                        serialized = serialized.replace('synthetic-native-thread', native_thread).replace('synthetic-native-turn', native_turn)
                    data = serialized.encode()
                    header = b'\x81' + (bytes([len(data)]) if len(data) < 126 else
                                         b'\x7e' + struct.pack('!H', len(data)))
                    self.request.sendall(header + data)

                while True:
                    head = stream.read(2)
                    if len(head) != 2:
                        return
                    length = head[1] & 127
                    if length == 126:
                        length = struct.unpack('!H', stream.read(2))[0]
                    elif length == 127:
                        length = struct.unpack('!Q', stream.read(8))[0]
                    mask = stream.read(4) if head[1] & 128 else None
                    data = stream.read(length)
                    if mask:
                        data = bytes(byte ^ mask[i % 4] for i, byte in enumerate(data))
                    if head[0] & 15 == 8:
                        return
                    value = json.loads(data)
                    method, request_id = value.get('method'), value.get('id')
                    with rpc_lock:
                        rpc.append((method, value.get('params')))
                    if method == 'initialize':
                        send({'id': request_id, 'result': {}})
                    elif method == 'account/rateLimits/read':
                        send({'id': request_id, 'result': {}})
                    elif method in ('thread/start', 'thread/resume'):
                        if missing_binding and method == 'thread/resume':
                            send({'id': request_id, 'error': {'code': -32600,
                                  'message': 'no rollout found for thread id missing-native-context'}})
                        else:
                            send({'id': request_id, 'result': {'thread': {'id': 'synthetic-native-thread'}}})
                            send({'method': 'thread/started', 'params': {'thread': {'id': 'synthetic-native-thread'}}})
                    elif method == 'turn/start':
                        send({'id': request_id, 'result': {'turn': {'id': 'synthetic-native-turn'}}})
                        started.set()
                        if retain_commentary:
                            if empty_messages:
                                send({'method':'item/completed','params':{'item':{'type':'agentMessage','id':'empty-first','phase':'commentary','text':''}}})
                                send({'method':'item/completed','params':{'item':{'type':'agentMessage','id':'space-first','phase':'final_answer','text':' \n\t'}}})
                                send({'method':'thread/status/changed','params':{'status':{'type':'idle'}}})
                            for index, text in enumerate(commentary if commentary_only else [*commentary, final_answer]):
                                item_id = f'agent-{index}'
                                send({'method': 'item/started', 'params': {'item': {
                                    'type': 'agentMessage', 'id': item_id}}})
                                if markup_prefix and index == 1:
                                    deadline = time.monotonic() + 8
                                    while time.monotonic() < deadline:
                                        with lock:
                                            published = commentary[0] in permanent.values()
                                        if published: break
                                        time.sleep(.02)
                                    time.sleep(1.2)
                                    send({'method':'item/agentMessage/delta','params':{'itemId':item_id,'delta':'**'}})
                                midpoint = len(text) // 2
                                for delta in (text[:midpoint], text[midpoint:]):
                                    send({'method': 'item/agentMessage/delta', 'params': {
                                        'itemId': item_id, 'delta': delta}})
                                send({'method': 'item/completed', 'params': {'item': {
                                    'type': 'agentMessage', 'id': item_id, 'text': text,
                                    'phase': 'commentary' if index < 2 else 'final_answer'}}})
                                if stream_ended and index == 0:
                                    return
                                if foreign_events and index == 0:
                                    for notification in [
                                        {'method': 'thread/started', 'params': {'thread': {'id': 'foreign-thread'}}},
                                        {'method': 'turn/started', 'params': {'threadId': 'foreign-thread', 'turn': {'id': 'foreign-turn'}}},
                                        {'method': 'item/completed', 'params': {'threadId': 'foreign-thread', 'turnId': 'foreign-turn', 'item': {'type': 'agentMessage', 'phase': 'final_answer', 'text': 'FOREIGN OUTPUT'}}},
                                        {'method': 'turn/completed', 'params': {'threadId': 'foreign-thread', 'turn': {'id': 'foreign-turn', 'status': 'completed'}}},
                                        {'method': 'turn/completed', 'params': {'threadId': 'synthetic-native-thread', 'turn': {'id': 'stale-turn', 'status': 'completed'}}},
                                        {'method': 'thread/status/changed', 'params': {'threadId': 'foreign-thread', 'status': {'type': 'idle'}}},
                                        {'id': 987, 'method': 'item/commandExecution/requestApproval', 'params': {'threadId': 'foreign-thread', 'turnId': 'foreign-turn', 'command': 'synthetic foreign command'}},
                                    ]:
                                        send(notification)
                                    time.sleep(.2)
                                if empty_messages:
                                    send({'method':'item/completed','params':{'item':{'type':'agentMessage','id':f'empty-{index}','phase':'final_answer','text':''}}})
                                if index < 2:
                                    send({'method': 'item/completed', 'params': {'item': {
                                        'type': 'commandExecution', 'command': 'synthetic command',
                                        'status': 'completed', 'aggregatedOutput': 'synthetic tool progress'}}})
                        if final_before_completion:
                            publication_deadline = time.monotonic() + 8
                            while time.monotonic() < publication_deadline:
                                with lock:
                                    published = final_answer in permanent.values()
                                if published:
                                    break
                                time.sleep(.02)
                            with sqlite3.connect(root / 'state.sqlite') as db:
                                continuation_state['turns'] = db.execute('SELECT status FROM turns').fetchall()
                                continuation_state['busy'] = db.execute('SELECT busy FROM sessions').fetchall()
                            with lock:
                                continuation_state['answer_published'] = final_answer in permanent.values()
                            send({'method': 'item/completed', 'params': {'item': {'type': 'collabToolCall', 'id': 'child-result', 'status': 'completed'}}})
                            send({'method': 'item/started', 'params': {'item': {'type': 'agentMessage', 'id': 'continued-answer'}}})
                            send({'method': 'item/agentMessage/delta', 'params': {'delta': 'Continued final answer.'}})
                            send({'method': 'item/completed', 'params': {'item': {'type': 'agentMessage', 'id': 'continued-answer', 'phase': 'final_answer', 'text': 'Continued final answer.'}}})
                        if retain_commentary:
                            send({'method': 'turn/completed', 'params': {'turn': {
                                'id': 'synthetic-native-turn', 'status': 'completed'}}})
                        if steered.is_set() and not stalled_steer:
                            send({'method': 'turn/completed', 'params': {'turn': {
                                'id': 'synthetic-native-turn', 'status': 'failed',
                                'error': {'message': 'synthetic later failure'}}}})
                    elif method == 'turn/steer':
                        steered.set()
                        if stalled_steer:
                            continue
                        if acknowledge_steer:
                            send({'id': request_id, 'result': {'turnId': 'synthetic-native-turn'}})
                        time.sleep(.1)
                        send({'method': 'turn/completed', 'params': {'turn': {
                            'id': 'synthetic-native-turn', 'status': 'failed',
                            'error': {'message': 'synthetic failure after accepting input'}}}})

        class NativeServer(socketserver.ThreadingTCPServer):
            allow_reuse_address = True
            daemon_threads = True

        native = NativeServer(('127.0.0.1', 0), Native)
        threading.Thread(target=native.serve_forever, daemon=True).start()
        fake = root / 'codex'
        fake.write_text(f'''#!/usr/bin/python3
import os,socket,sys,threading
if sys.argv[1:]==['login','status']:
    print('Logged in using synthetic local auth'); sys.exit(0)
if sys.argv[1:]!=['app-server','proxy']: sys.exit(7)
s=socket.create_connection(('127.0.0.1',{native.server_address[1]}),timeout=20)
def upload():
    while True:
        data=os.read(0,8192)
        if not data: break
        s.sendall(data)
threading.Thread(target=upload,daemon=True).start()
while True:
    data=s.recv(8192)
    if not data: break
    os.write(1,data)
''')
        fake.chmod(0o700)
        polls, message_id = 0, 30
        permanent, outbound, methods, empty_rejected = {}, [], [], []
        lock = threading.Lock()

        def update(number, text):
            return {'update_id': number, 'message': {'message_id': number,
                    'message_thread_id': 7,
                    'from': {'id': 100, 'is_bot': False, 'first_name': 'Synthetic'},
                    'chat': {'id': 100, 'type': 'private'}, 'text': text}}

        class Telegram(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                nonlocal polls, message_id
                payload = json.loads(self.rfile.read(int(self.headers.get('Content-Length', '0'))) or b'{}')
                method = self.path.rsplit('/', 1)[-1]
                with lock:
                    methods.append(method)
                if method == 'getMe':
                    result = {'id': 123, 'is_bot': True, 'first_name': 'Fixture',
                              'username': 'FixtureBot', 'has_topics_enabled': True}
                elif method == 'getUpdates':
                    with lock:
                        polls += 1
                        poll = polls
                    if poll == 1:
                        result = [update(1, '/status' if missing_binding else 'first human input')]
                        if stalled_steer:
                            result = [dict(update(n, 'first human input'), message=dict(update(n, 'first human input')['message'], message_thread_id=6+n, chat={'id':99+n,'type':'private'})) for n in range(1,6)]
                    elif poll == 2 and not retain_commentary:
                        if missing_binding:
                            with sqlite3.connect(root / 'state.sqlite') as db:
                                changed = db.execute("UPDATE sessions SET codex_thread_id='missing-native-context',force_fresh_thread=0 WHERE chat_id=100 AND thread_id=7").rowcount
                                assert changed == 1
                        else:
                            started.wait(12)
                        if stalled_steer:
                            deadline = time.monotonic() + 8
                            while time.monotonic() < deadline:
                                with sqlite3.connect(root / 'state.sqlite') as db:
                                    if db.execute("select count(*) from turns where status='running'").fetchone()[0] == 5: break
                                time.sleep(.02)
                        result = [update(6 if stalled_steer else 2, 'second human input')]
                    elif poll == 3 and stalled_steer:
                        steered.wait(8)
                        result = [dict(update(7, '/status'), message=dict(update(7, '/status')['message'], message_thread_id=8, chat={'id':101,'type':'private'})),
                                  update(8, '/status')]
                    else:
                        time.sleep(.15)
                        result = []
                elif method in ('sendMessage', 'editMessageText'):
                    if not html.unescape(re.sub(r'<[^>]*>', '', payload['text'])).strip():
                        empty_rejected.append(method)
                        data=json.dumps({'ok':False,'error_code':400,'description':'Bad Request: text must be non-empty'}).encode()
                        self.send_response(400);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data);return
                    with lock:
                        if method == 'sendMessage':
                            message_id += 1
                            target_id = message_id
                        else:
                            target_id = payload['message_id']
                        permanent[target_id] = html.unescape(payload['text'])
                        outbound.append((method, target_id, permanent[target_id]))
                    result = {'message_id': target_id, 'chat': {'id': 100, 'type': 'private'}}
                else:
                    result = True
                data = json.dumps({'ok': True, 'result': result}).encode()
                self.send_response(200)
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                try:
                    self.wfile.write(data)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def log_message(self, *args):
                pass

        telegram = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Telegram)
        threading.Thread(target=telegram.serve_forever, daemon=True).start()
        token = root / 'bot.token'
        token.write_text('synthetic-secret')
        token.chmod(0o600)
        cfg = root / 'local.toml'
        cfg.write_text(f'''db_path="{root}/state.sqlite"
startup_admin_ids=[100]
poll_timeout_seconds=1
edit_debounce_ms=100
[telegram]
bot_token_file="{token}"
api_base="http://127.0.0.1:{telegram.server_port}"
use_message_drafts={str(drafts).lower()}
show_unfinished_messages={str(show_unfinished).lower()}
[codex]
binary="{fake}"
default_cwd="{root}"
shared_app_server=true
auto_attach_latest_history=false
import_cli_history=false
import_desktop_history=false
''')
        env = {'PATH': '/usr/bin:/bin', 'HOME': str(root),
               'CODEX_HOME': str(root / 'codex-home'), 'LANG': 'C.UTF-8', 'RUST_LOG': 'warn'}
        process = subprocess.Popen([BINARY, str(cfg)], cwd=root, env=env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        rows, sessions = [], []
        polling_while_steering = False
        other_topic_while_steering = False
        deadline = time.monotonic() + (30 if stalled_steer else 18)
        try:
            while time.monotonic() < deadline and process.poll() is None:
                try:
                    with sqlite3.connect(root / 'state.sqlite') as db:
                        rows = db.execute('SELECT update_id,status,turn_id FROM incoming_updates ORDER BY update_id').fetchall()
                        sessions = db.execute('SELECT codex_thread_id FROM sessions').fetchall()
                    if stalled_steer:
                        states = dict((id, status) for id, status, _ in rows)
                        if states.get(6) == 'steering':
                            polling_while_steering |= polls >= 4
                            other_topic_while_steering |= states.get(7) == 'handled'
                        if states.get(6) == 'undetermined' and states.get(8) == 'handled':
                            break
                    if stream_ended and rows and rows[0][1] == 'undetermined':
                        with lock:
                            failure_published = any('Turn failed:' in text for text in permanent.values())
                        if failure_published:
                            break
                    if retain_commentary and rows == [(1, 'settled', 1)]:
                        break
                    if len(rows) == 2:
                        if missing_binding and rows[1][1] == 'undetermined':
                            break
                        if not missing_binding and rows[0][1] == 'undetermined' and rows[1][1] in ('handled', 'undetermined', 'settled'):
                            break
                except sqlite3.Error:
                    pass
                time.sleep(.1)
            process.send_signal(signal.SIGINT)
            out, err = process.communicate(timeout=5)
            with rpc_lock:
                starts = [p['input'][0]['text'] for m, p in rpc if m == 'turn/start']
                steers = [p['input'][0]['text'] for m, p in rpc if m == 'turn/steer']
                threads = [m for m, p in rpc if m in ('thread/start', 'thread/resume')]
            if stalled_steer:
                assert polling_while_steering, (rows, polls, out[-1800:], err)
                assert other_topic_while_steering, (rows, out[-1800:], err)
                assert dict((id, status) for id, status, _ in rows) == {**dict.fromkeys(range(1,6),'started'),6:'undetermined',7:'handled',8:'handled'}, rows
                assert starts == ['first human input'] * 5 and steers == ['second human input'], (starts, steers)
                assert len(set(thread for thread, in sessions)) == 5, sessions
                print(json.dumps({'scenario':mode, 'active_topics':5, 'polling_continued':True, 'other_topic_responded':True,
                                  'same_topic_controls_recovered':True, 'uncertain_input_not_replayed':True}), flush=True)
                return
            if stream_ended:
                with sqlite3.connect(root / 'state.sqlite') as db:
                    assert db.execute('SELECT status FROM turns').fetchall() == [('failed',)]
                    assert db.execute('SELECT busy FROM sessions').fetchall() == [(0,)]
                assert sessions == [('synthetic-native-thread',)], sessions
                assert len(starts) == 1 and not steers
                assert len(rows) == 1 and rows[0][1] == 'undetermined', rows
                with lock:
                    assert commentary[0] in permanent.values()
                    assert any('Turn failed:' in text for text in permanent.values())
                print(json.dumps({'scenario': mode, 'success_not_fabricated': True, 'input_preserved': True, 'exit': process.returncode}), flush=True)
                return
            if retain_commentary:
                assert rows == [(1, 'settled', 1)], (mode, rows, {'empty_rejected':empty_rejected,'failures':[text for text in permanent.values() if text.startswith('Turn failed:')]}, err)
                assert not empty_rejected, (mode,empty_rejected)
                with lock:
                    messages = [text for text in permanent.values()
                                if not text.startswith("Current Codex session:")]
                    edits = list(outbound)
                    sent_methods = list(methods)
                if not show_unfinished:
                    assert 'sendMessageDraft' not in sent_methods, (mode, sent_methods)
                    assert 'editMessageText' not in sent_methods, (mode, edits)
                assert len(messages) == (3 if commentary_only else 5 if final_before_completion else 4), (mode, messages)
                assert messages[0] == commentary[0], (mode, messages[0])
                assert sum(text.count('x') for text in messages[1:3]) == 3800
                assert messages[1].startswith('Second progress: ')
                if not commentary_only:
                    assert messages[3] == final_answer, (mode, messages[3])
                expected_finals = 0 if commentary_only else 1
                assert sum(text == final_answer for text in messages) == expected_finals
                first_commit = next(i for i, (_, _, text) in enumerate(edits)
                                    if text == commentary[0])
                first_id = edits[first_commit][1]
                assert all(target != first_id for _, target, _ in edits[first_commit + 1:])
                with sqlite3.connect(root / 'state.sqlite') as db:
                    assert db.execute('select status,assistant_text from turns').fetchall() == [
                        ('completed', commentary[-1] if commentary_only else 'Continued final answer.' if final_before_completion else final_answer)]
                if foreign_events:
                    assert sessions == [('synthetic-native-thread',)], sessions
                    assert all(method is not None for method, _ in rpc), 'answered a foreign server request'
                if final_before_completion:
                    assert continuation_state == {'turns': [('running',)], 'busy': [(1,)], 'answer_published': True}, continuation_state
                    assert messages[-1] == 'Continued final answer.', messages
                    assert len(starts) == 1 and not steers, (starts, steers)
                print(json.dumps({'scenario': mode, 'permanent_message_count': len(messages),
                                  'first_commentary_preserved': True,
                                  'long_commentary_characters': 3800,
                                  'final_answer_count': expected_finals, 'exit': process.returncode}), flush=True)
                return
            assert len(rows) == 2, (mode, rows)
            assert rows[1][1] == 'undetermined', (mode, rows)
            if missing_binding:
                assert sessions == [('missing-native-context',)], sessions
                assert threads == ['thread/resume'], threads
                assert starts == [], starts
            else:
                assert rows[0][1] == 'undetermined', rows
                assert rows[1][2] == rows[0][2] and rows[1][2] is not None, rows
                assert sessions == [('synthetic-native-thread',)], sessions
                assert starts == ['first human input'], starts
                assert steers == ['second human input'], steers
            print(json.dumps({'scenario': mode, 'updates': rows, 'session_bindings': sessions,
                              'native_thread_methods': threads,
                              'native_turn_start_inputs': starts, 'native_steer_inputs': steers,
                              'exit': process.returncode, 'log_tail': err[-400:]}), flush=True)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            telegram.shutdown()
            telegram.server_close()
            native.shutdown()
            native.server_close()


if __name__ == '__main__':
    if len(sys.argv) > 2:
        for mode in sys.argv[2:]:
            scenario(mode)
    else:
        token_probe()
        for mode in ['stalled_steer', 'accepted_steer', 'unacknowledged_steer', 'missing_saved_binding',
                     'retained_commentary_drafts', 'retained_commentary_preview',
                     'retained_commentary_only', 'retained_commentary_hidden',
                     'retained_commentary_hidden_drafts', 'retained_commentary_hidden_only',
                     'retained_commentary_empty_drafts', 'retained_commentary_empty_hidden', 'retained_commentary_markup_preview',
                     'retained_commentary_foreign_events', 'retained_commentary_continuation', 'retained_commentary_eof']:
            scenario(mode)
