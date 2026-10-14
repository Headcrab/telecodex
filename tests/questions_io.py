#!/usr/bin/env python3
"""Native question journeys through real HTTP, WebSocket and SQLite boundaries.

All accounts, native threads, bot tokens and model responses are synthetic.
No live model, Telegram or Matrix call is made.
"""
import base64
import collections
import hashlib
import http.server
import html
import json
import pathlib
import signal
import socketserver
import sqlite3
import struct
import subprocess
import sys
import tempfile
import threading
import time


class Journey:
    def __init__(self, binary, mode):
        self.binary, self.mode = binary, mode
        self.temporary = tempfile.TemporaryDirectory(prefix='telecodex-questions-')
        self.root = pathlib.Path(self.temporary.name)
        self.lock = threading.RLock()
        self.updates = collections.deque()
        self.rpc, self.messages, self.answers, self.errors = [], [], [], []
        self.sequence, self.message_id, self.thread_sequence = 0, 500, 0
        self.finished = threading.Event()
        self.question = threading.Event()
        self.delivery_reply = threading.Event()
        self.fail_question_prefix = 'Codex question 1/' if mode == 'initial_send_failure' else None
        self.failed_deliveries = 0
        owner = self

        class Native(socketserver.BaseRequestHandler):
            def handle(self):
                try:
                    owner.native_connection(self.request)
                except (ConnectionError, OSError):
                    pass  # Deliberate disconnect/restart scenarios.
                except Exception as error:
                    owner.errors.append(repr(error))
                    owner.finished.set()

        class NativeServer(socketserver.ThreadingTCPServer):
            allow_reuse_address, daemon_threads = True, True

        self.native = NativeServer(('127.0.0.1', 0), Native)
        threading.Thread(target=self.native.serve_forever, daemon=True).start()

        class Telegram(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                payload = json.loads(self.rfile.read(int(self.headers.get('Content-Length', '0'))) or b'{}')
                method = self.path.rsplit('/', 1)[-1]
                with owner.lock:
                    fail_delivery = method == 'sendMessage' and owner.fail_question_prefix is not None and html.unescape(payload['text']).startswith(owner.fail_question_prefix)
                    if fail_delivery:
                        owner.fail_question_prefix = None
                        owner.failed_deliveries += 1
                if fail_delivery:
                    body = json.dumps({'ok': False, 'description': 'synthetic temporary delivery failure'}).encode()
                    self.send_response(500)
                    self.send_header('Content-Length', str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    return
                if method == 'getMe':
                    result = {'id': 123, 'is_bot': True, 'first_name': 'Fixture',
                              'username': 'FixtureBot', 'has_topics_enabled': True}
                elif method == 'getUpdates':
                    with owner.lock:
                        result = [owner.updates.popleft()] if owner.updates else []
                    if not result:
                        time.sleep(.03)
                elif method == 'sendMessage':
                    with owner.lock:
                        owner.message_id += 1
                        result = {'message_id': owner.message_id,
                                  'chat': {'id': payload['chat_id'], 'type': 'private'},
                                  'from': {'id':123,'is_bot':True,'first_name':'Fixture'},
                                  'message_thread_id': payload.get('message_thread_id'),
                                  'text': html.unescape(payload['text'])}
                        if owner.mode == 'wrong_receipt' and payload['text'].startswith('Codex question'):
                            result['message_thread_id'] = 8
                        owner.messages.append((dict(result), payload))
                        if payload['text'].startswith('Codex question'):
                            owner.question.set()
                else:
                    result = True
                if owner.mode == 'restart_gap' and method == 'sendMessage' and payload['text'].startswith('Codex question'):
                    owner.delivery_reply.wait(timeout=10)
                body = json.dumps({'ok': True, 'result': result}).encode()
                self.send_response(200)
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def log_message(self, *_):
                pass

        self.telegram = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Telegram)
        threading.Thread(target=self.telegram.serve_forever, daemon=True).start()
        fake = self.root / 'codex'
        fake.write_text(f'''#!/usr/bin/python3
import os,socket,sys,threading
if sys.argv[1:]==['login','status']:
    print('Logged in using synthetic local auth'); sys.exit(0)
if sys.argv[1:]!=['app-server','proxy']: sys.exit(7)
s=socket.create_connection(('127.0.0.1',{self.native.server_address[1]}),timeout=30)
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
        token = self.root / 'bot.token'
        token.write_text('synthetic-question-token'); token.chmod(0o600)
        self.config = self.root / 'local.toml'
        self.config.write_text(f'''db_path="{self.root}/state.sqlite"
startup_admin_ids=[100,101]
poll_timeout_seconds=1
edit_debounce_ms=100
[telegram]
bot_token_file="{token}"
api_base="http://127.0.0.1:{self.telegram.server_port}"
show_unfinished_messages=false
[codex]
binary="{fake}"
default_cwd="{self.root}"
default_model="fixture-model"
shared_app_server=true
auto_attach_latest_history=false
import_cli_history=false
import_desktop_history=false
''')
        self.process = None

    def start(self):
        env = {'PATH': '/usr/bin:/bin', 'HOME': str(self.root), 'CODEX_HOME': str(self.root / 'codex-home'),
               'LANG': 'C.UTF-8', 'RUST_LOG': 'warn'}
        self.process = subprocess.Popen([self.binary, str(self.config)], cwd=self.root, env=env,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    def stop(self):
        if self.process:
            self.process.send_signal(signal.SIGINT)
            _, error = self.process.communicate(timeout=8)
            assert 'synthetic-question-token' not in error
            self.process = None

    def close(self):
        self.stop()
        for server in (self.telegram, self.native):
            server.shutdown(); server.server_close()
        self.temporary.cleanup()

    def admit(self, text=None, *, topic=7, user=100, callback=None, reply=None):
        with self.lock:
            self.sequence += 1
            sender = {'id': user, 'is_bot': False, 'first_name': 'Fixture'}
            if callback:
                message, data = callback
                message = dict(message, message_thread_id=topic)
                update = {'update_id': self.sequence, 'callback_query': {
                    'id': str(self.sequence), 'from': sender, 'data': data, 'message': message}}
            else:
                message = {'message_id': self.sequence, 'message_thread_id': topic,
                           'from': sender, 'chat': {'id': 100, 'type': 'private'}, 'text': text}
                if reply:
                    message['reply_to_message'] = next((dict(m[0]) for m in self.messages if m[0]['message_id']==reply), {'message_id': reply})
                update = {'update_id': self.sequence, 'message': message}
            self.updates.append(update)
            return self.sequence

    def wait(self, condition, seconds=15):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            assert not self.errors, self.errors
            assert self.process is None or self.process.poll() is None, 'bridge exited unexpectedly'
            if condition():
                return
            time.sleep(.02)
        raise AssertionError(f'{self.mode}: boundary did not complete; messages={self.messages!r}; rpc={self.rpc!r}')

    def row(self, sql, params=()):
        with sqlite3.connect(self.root / 'state.sqlite') as db:
            return db.execute(sql, params).fetchall()

    def controls(self, index, topic=7):
        with self.lock:
            rows = [m for m in self.messages if html.unescape(m[1]['text']).startswith(f'Codex question {index}/') and m[0]['message_thread_id'] == topic and 'inline_keyboard' in m[1].get('reply_markup', {})]
            return rows[-1] if rows else None

    @staticmethod
    def button(control, suffix):
        for row in control[1]['reply_markup']['inline_keyboard']:
            for button in row:
                if button.get('callback_data', '').endswith(suffix):
                    return control[0], button['callback_data']
        raise AssertionError('missing answer control')

    def native_connection(self, sock):
        stream = sock.makefile('rb')
        headers = {}
        while True:
            line = stream.readline()
            if line == b'\r\n': break
            if not line: return
            if b':' in line:
                key, value = line.split(b':', 1); headers[key.strip().lower()] = value.strip()
        accept = base64.b64encode(hashlib.sha1(headers[b'sec-websocket-key'] +
            b'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest())
        sock.sendall(b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: '+accept+b'\r\n\r\n')
        thread = None; turn = None; question_id = None
        def send(value):
            data = json.dumps(value).encode(); size = len(data)
            head = bytes([129, size]) if size < 126 else b'\x81\x7e'+struct.pack('!H',size)
            sock.sendall(head+data)
        def event(method, params):
            send({'method': method, 'params': {'threadId': thread, 'turnId': turn, **params}})
        def finish(status='completed'):
            event('turn/completed', {'turn': {'id': turn, 'status': status}})
            self.finished.set()
        def ask():
            questions = [{'id': 'scope', 'header': 'Scope', 'question': ('Choose the scope' + (' 🌿<body>' * 1000 if self.mode == 'long' else '')),
                'isOther': True, 'isSecret': self.mode == 'secret', 'options': [{'label': 'Small', 'description': 'One increment'},
                                          {'label': 'Large', 'description': 'More work'}]},
                {'id': 'notes', 'header': 'Notes', 'question': 'Any notes?', 'options': None}]
            params = {'threadId': thread, 'turnId': turn, 'itemId': 'question-item',
                      'isBlocking': self.mode != 'async', 'questions': questions}
            # Requests/resolutions from another subscribed thread must be ignored.
            send({'id': 'foreign-question', 'method': 'item/tool/requestUserInput',
                  'params': {**params, 'threadId': 'foreign-thread'}})
            send({'method': 'serverRequest/resolved', 'params': {
                'threadId': 'foreign-thread', 'requestId': question_id}})
            send({'id': question_id, 'method': 'item/tool/requestUserInput', 'params': params})
            send({'id': question_id, 'method': 'item/tool/requestUserInput', 'params': params})
            if self.mode == 'async':
                event('item/completed', {'item': {'type': 'agentMessage', 'id': 'continuing',
                    'phase': 'commentary', 'text': 'Native work continued while awaiting the answer.'}})
        while True:
            first = stream.read(2)
            if len(first) < 2: return
            opcode = first[0] & 15; size = first[1] & 127
            if size == 126: size = struct.unpack('!H',stream.read(2))[0]
            elif size == 127: size = struct.unpack('!Q',stream.read(8))[0]
            mask = stream.read(4); data = stream.read(size)
            if opcode == 8: return
            if opcode != 1: continue
            message = json.loads(bytes(v ^ mask[i%4] for i,v in enumerate(data)))
            method, request_id, params = message.get('method'), message.get('id'), message.get('params', {})
            with self.lock: self.rpc.append(message)
            if method is None and request_id == question_id and question_id is not None:
                self.answers.append(message)
                if self.mode == 'disconnect':
                    self.finished.set(); return
                event('serverRequest/resolved', {'requestId': request_id})
                event('item/completed', {'item': {'type': 'agentMessage', 'id': 'answer',
                    'phase': 'final_answer', 'text': 'Plan completed with the exact native answers.'}})
                finish()
            elif method == 'initialize': send({'id': request_id, 'result': {}})
            elif method == 'collaborationMode/list':
                send({'id': request_id, 'result': {'data': [] if self.mode == 'unsupported' else
                    [{'mode': 'plan'}, {'mode': 'default'}]}})
            elif method in ('thread/start','thread/resume'):
                with self.lock:
                    if method == 'thread/start':
                        self.thread_sequence += 1
                        thread = f'native-topic-{self.thread_sequence + 6}'
                    else:
                        assert params.get('excludeTurns') is True, params
                        thread = params['threadId']
                turn = 'turn-' + thread
                question_id = 17 if self.mode == 'numeric' else 'question-' + thread
                # A metadata-only resume still binds the existing thread and receives live events.
                send({'id': request_id, 'result': {'thread': {'id': thread, 'turns': []}, 'model': 'fixture-model'}})
            elif method == 'thread/name/set': send({'id': request_id, 'result': {}})
            elif method == 'turn/start':
                assert params['collaborationMode']=={'mode': ('default' if params['input'][0]['text']=='implement after planning' else 'plan'),'settings':{
                    'model':'fixture-model','reasoning_effort':None,'developer_instructions':None}}, params
                send({'id': request_id, 'result': {'turn': {'id': turn}}})
                event('turn/started', {'turn': {'id': turn}})
                if params['collaborationMode']['mode']=='default':
                    event('item/completed', {'item': {'type':'agentMessage','id':'default-answer',
                        'phase':'final_answer','text':'Default mode continued the same native thread.'}})
                    finish()
                else:
                    ask()
            elif method == 'turn/steer':
                send({'id': request_id, 'result': {'turnId': turn}})
            elif method == 'turn/interrupt':
                send({'id': request_id, 'result': {}})
                event('serverRequest/resolved', {'requestId': question_id}); finish('interrupted')
            elif method and request_id is not None:
                send({'id': request_id, 'result': {}})


def scenario(binary, mode):
    j = Journey(binary, mode)
    question_id = 17 if mode == 'numeric' else 'question-native-topic-7'
    try:
        j.start()
        if mode == 'initial_receipt_store_failure':
            j.wait(lambda: (j.root / 'state.sqlite').exists() and j.row("SELECT name FROM sqlite_master WHERE name='question_messages'"))
            with sqlite3.connect(j.root / 'state.sqlite') as db:
                db.execute("CREATE TRIGGER fail_question_write BEFORE INSERT ON question_messages BEGIN SELECT RAISE(FAIL, 'synthetic store failure'); END")
        original = j.admit('/plan produce a small plan')
        if mode == 'unsupported':
            j.wait(lambda: any('does not advertise plan mode' in m[1]['text'] for m in j.messages))
            assert not any(m.get('method') == 'turn/start' for m in j.rpc)
            assert j.row('SELECT collaboration_mode FROM sessions') == [('plan',)]
            print(json.dumps({'scenario':mode,'no_native_turn_submitted':True}),flush=True); return
        if mode == 'secret':
            j.wait(lambda: any('secret input' in m[1]['text'] for m in j.messages))
            assert not j.controls(1) and not j.answers
            print(json.dumps({'scenario':mode,'secret_not_rendered_in_plaintext':True}),flush=True); return
        if mode == 'wrong_receipt':
            j.wait(lambda: any('delivery did not confirm' in m[1]['text'] for m in j.messages))
            assert not j.answers
            print(json.dumps({'scenario':mode,'unbound_controls_rejected':True}),flush=True); return
        if mode in ('initial_send_failure', 'initial_receipt_store_failure'):
            j.wait(lambda: any('Use /questions to reopen' in m[1]['text'] for m in j.messages))
            assert j.row('SELECT status FROM turns') == [('running',)]
            assert not j.answers and not any(m.get('method') == 'turn/interrupt' for m in j.rpc)
            if mode == 'initial_receipt_store_failure':
                with sqlite3.connect(j.root / 'state.sqlite') as db:
                    db.execute('DROP TRIGGER fail_question_write')
            j.admit('/questions')
            j.wait(lambda: any('Pending Codex questions.' in m[1]['text'] for m in j.messages))
            picker = next(m for m in reversed(j.messages) if 'Pending Codex questions.' in m[1]['text'])
            reopened_id = j.admit(callback=(picker[0], picker[1]['reply_markup']['inline_keyboard'][0][0]['callback_data']))
            j.wait(lambda: j.row('SELECT status FROM incoming_updates WHERE update_id=?', (reopened_id,)) == [('handled',)])
        j.wait(lambda: j.controls(1))
        first = j.controls(1)
        assert len([m for m in j.messages if html.unescape(m[1]['text']).startswith('Codex question 1/') and 'inline_keyboard' in m[1].get('reply_markup', {})]) == (2 if mode == 'initial_receipt_store_failure' else 1)
        assert all(len(html.unescape(m[1]['text']).encode('utf-16-le'))//2 <= 4096 for m in j.messages)
        if mode in ('restart', 'restart_gap'):
            if mode == 'restart':
                j.wait(lambda: bool(j.row('SELECT message_id FROM question_messages')))
            j.stop(); j.delivery_reply.set(); j.start()
            j.admit('late reply',reply=first[0]['message_id'])
            j.admit(callback=j.button(first, ':0'))
            j.admit('/questions')
            j.wait(lambda: any('No pending questions' in m[1]['text'] for m in j.messages))
            assert not j.answers
            assert sum(m.get('method') == 'turn/start' for m in j.rpc)==1
            assert j.row('SELECT collaboration_mode FROM sessions') == [('plan',)]
            assert j.row('SELECT status FROM incoming_updates WHERE update_id=?',(original,))==[('undetermined',)]
            print(json.dumps({'scenario':mode,'stale_replies_not_replayed':True}),flush=True); return
        if mode in ('cancel','stop'):
            if mode == 'stop':
                j.admit(callback=j.button(first, ':text'))
                j.wait(lambda: any('Send your answer as text' in m[1]['text'] for m in j.messages))
                j.admit('/stop')
            else:
                j.admit(callback=j.button(first, ':cancel'))
            j.wait(lambda: j.finished.is_set())
            j.wait(lambda: j.row('SELECT status FROM turns') == [('cancelled',)])
            assert not j.answers
            assert any(m.get('method') == 'turn/interrupt' for m in j.rpc)
            print(json.dumps({'scenario':mode,'native_turn_interrupted':True}),flush=True); return
        if mode == 'async':
            j.wait(lambda: any('Native work continued' in m[1]['text'] for m in j.messages))
            assert j.row('SELECT status FROM turns') == [('running',)]
        option_id = None
        if mode in ('reopen_failure', 'question_send_failure', 'answer_store_failure', 'receipt_store_failure'):
            if mode == 'reopen_failure':
                j.admit('/questions')
                j.wait(lambda: any('Pending Codex questions.' in m[1]['text'] for m in j.messages))
                picker = next(m for m in reversed(j.messages) if 'Pending Codex questions.' in m[1]['text'])
                j.fail_question_prefix = 'Codex question 1/'
                failed_id = j.admit(callback=(picker[0], picker[1]['reply_markup']['inline_keyboard'][0][0]['callback_data']))
                j.wait(lambda: j.row('SELECT status FROM incoming_updates WHERE update_id=?', (failed_id,)) == [('undetermined',)])
            else:
                if mode == 'question_send_failure':
                    j.fail_question_prefix = 'Codex question 2/'
                else:
                    table = 'question_answers' if mode == 'answer_store_failure' else 'question_messages'
                    with sqlite3.connect(j.root / 'state.sqlite') as db:
                        db.execute(f"CREATE TRIGGER fail_question_write BEFORE INSERT ON {table} BEGIN SELECT RAISE(FAIL, 'synthetic store failure'); END")
                option_id = j.admit(callback=j.button(first, ':0'))
                expected_status = 'undetermined' if mode == 'answer_store_failure' else 'answering'
                j.wait(lambda: j.row('SELECT status FROM incoming_updates WHERE update_id=?', (option_id,)) == [(expected_status,)])
            after = len(j.messages)
            j.admit('/questions')
            j.wait(lambda: any('questions' in m[1]['text'].lower() for m in j.messages[after:]))
            picker = next(m for m in reversed(j.messages[after:]) if 'questions' in m[1]['text'].lower())
            assert 'Pending Codex questions.' in picker[1]['text'], 'temporary failure discarded the native dialogue'
            assert j.row('SELECT status FROM turns') == [('running',)]
            assert not j.answers and not any(m.get('method') == 'turn/interrupt' for m in j.rpc)
            if mode in ('reopen_failure', 'question_send_failure'):
                assert j.failed_deliveries == 1
            if mode in ('answer_store_failure', 'receipt_store_failure'):
                with sqlite3.connect(j.root / 'state.sqlite') as db:
                    db.execute('DROP TRIGGER fail_question_write')
            reopened_id = j.admit(callback=(picker[0], picker[1]['reply_markup']['inline_keyboard'][0][0]['callback_data']))
            j.wait(lambda: j.row('SELECT status FROM incoming_updates WHERE update_id=?', (reopened_id,)) == [('handled',)])
            index = 1 if mode in ('reopen_failure', 'answer_store_failure') else 2
            j.wait(lambda: j.controls(index) and j.controls(index)[0]['message_id'] != first[0]['message_id'])
            if index == 1:
                first = j.controls(1)
                option_id = None
        if mode == 'roundtrip':
            j.admit(callback=j.button(first, ':0'),user=101)
            j.admit(callback=j.button(first, ':0'),topic=8)
            j.admit(callback=(first[0],j.button(first, ':0')[1].replace(':0:0',':9:0')))
            j.admit('/questions')
            j.wait(lambda: any('Pending Codex questions.' in m[1]['text'] for m in j.messages))
            assert not j.answers
            picker = next(m for m in reversed(j.messages) if 'Pending Codex questions.' in m[1]['text'])
            reopen = picker[1]['reply_markup']['inline_keyboard'][0][0]['callback_data']
            j.admit(callback=(picker[0], reopen))
            j.wait(lambda: j.controls(1)[0]['message_id'] != first[0]['message_id'])
            j.admit(callback=j.button(first, ':0'))
            first = j.controls(1)
        if mode == 'two_topics':
            j.admit('/plan independent second plan',topic=8)
            j.wait(lambda: j.controls(1,8))
            other = j.controls(1,8)
            j.admit(callback=j.button(first, ':0'),topic=8)
            j.admit(callback=j.button(other, ':0'),topic=7)
            j.admit('Other scope',topic=8,reply=other[0]['message_id'])
            j.wait(lambda: j.controls(2,8))
            j.admit('Other notes',topic=8,reply=j.controls(2,8)[0]['message_id'])
            j.wait(lambda: len(j.answers)==1)
            assert j.answers[0] == {'id':'question-native-topic-8','result':{'answers':{
                'scope':{'answers':['Other scope']},'notes':{'answers':['Other notes']}}}}
            j.wait(lambda: j.row('SELECT status FROM turns ORDER BY id') == [('running',),('completed',)])
        if option_id is None:
            option_id = j.admit(callback=j.button(first, ':0'))
        j.wait(lambda: j.controls(2))
        second = j.controls(2)
        j.admit(callback=j.button(first, ':0'))  # Duplicate old control must not answer question two.
        if mode == 'roundtrip':
            j.admit('keep the scope narrow')  # Ordinary steering remains ordinary steering.
            j.wait(lambda: any(m.get('method') == 'turn/steer' for m in j.rpc))
        text_control = None
        if mode != 'numeric':
            text_control = j.admit(callback=j.button(second, ':text'))
            j.wait(lambda: any('Send your answer as text' in m[1]['text'] for m in j.messages))
        if mode in ('implicit_reply', 'force_reply'):
            prompts = [m for m in j.messages if m[1].get('reply_markup', {}).get('force_reply') is True]
            assert prompts and prompts[-1][0]['message_thread_id'] == 7, prompts
            prompt = prompts[-1]
            assert prompt[1]['reply_markup']['input_field_placeholder'] == 'Your answer'
            # HTTP fixture receipt precedes the client's response handling and SQLite commit.
            j.wait(lambda: j.row('SELECT message_id FROM question_messages WHERE message_id=?',
                                 (prompt[0]['message_id'],)))
            # Expired explicit quotes must not consume the armed free-text answer.
            stale_id = j.admit('old question reply', reply=first[0]['message_id'])
            j.wait(lambda: j.row('SELECT status FROM incoming_updates WHERE update_id=?',(stale_id,)) == [('handled',)])
            assert not j.answers
        reply = second[0]['message_id'] if mode == 'numeric' else 77 if mode == 'implicit_reply' else prompt[0]['message_id'] if mode == 'force_reply' else None
        answer_id = j.admit('Exact free text', reply=reply)
        j.wait(lambda: len(j.answers)==(2 if mode == 'two_topics' else 1))
        expected = {'id':question_id,'result':{'answers':{'scope':{'answers':['Small']},
                    'notes':{'answers':['Exact free text']}}}}
        assert j.answers[-1] == expected and len(j.answers) == (2 if mode == 'two_topics' else 1), j.answers
        assert sum(m.get('method') == 'turn/start' for m in j.rpc)==(2 if mode == 'two_topics' else 1)
        j.wait(lambda: all(r==(0,) for r in j.row('SELECT busy FROM sessions')))
        if mode == 'disconnect':
            for update_id in (option_id, answer_id):
                assert j.row('SELECT status FROM incoming_updates WHERE update_id=?',(update_id,))==[('undetermined',)]
            j.stop(); j.start(); j.admit('late answer',reply=second[0]['message_id']); j.admit('/questions')
            j.wait(lambda: any('No pending questions' in m[1]['text'] for m in j.messages))
            assert j.answers == [expected]
            assert sum(m.get('method') == 'turn/start' for m in j.rpc)==1
        else:
            j.wait(lambda: all(r==('completed',) for r in j.row('SELECT status FROM turns')))
            for update_id in (original, option_id, answer_id):
                assert j.row('SELECT status FROM incoming_updates WHERE update_id=?',(update_id,))==[('settled',)]
            if text_control is not None:
                assert j.row('SELECT status FROM incoming_updates WHERE update_id=?',(text_control,))==[('handled',)]
        if mode == 'default':
            default_id = j.admit('/default implement after planning')
            j.wait(lambda: j.row('SELECT status FROM turns ORDER BY id')==[('completed',),('completed',)])
            assert j.row('SELECT collaboration_mode FROM sessions') == [('default',)]
            assert j.row('SELECT status FROM incoming_updates WHERE update_id=?',(default_id,)) == [('settled',)]
            starts=[m for m in j.rpc if m.get('method')=='turn/start']
            assert [m['params']['collaborationMode']['mode'] for m in starts] == ['plan','default']
            assert starts[0]['params']['threadId']==starts[1]['params']['threadId']
            assert any(m[1].get('text') == 'Default mode continued the same native thread.' for m in j.messages)
        assert any(m.get('method')=='initialize' and m['params']['capabilities']['experimentalApi'] for m in j.rpc)
        print(json.dumps({'scenario':mode,'exact_request_answered_once':True,'new_turns_from_answers':0}),flush=True)
    finally:
        j.close()


if __name__ == '__main__':
    binary = str(pathlib.Path(sys.argv[1]).resolve())
    for mode in sys.argv[2:] or ['implicit_reply','force_reply','roundtrip','async','numeric','long','two_topics','default','cancel','stop','secret','restart','restart_gap','disconnect','unsupported','wrong_receipt','reopen_failure','question_send_failure','answer_store_failure','receipt_store_failure','initial_send_failure','initial_receipt_store_failure']:
        scenario(binary, mode)
