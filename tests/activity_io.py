#!/usr/bin/env python3
"""Activity observer regressions using real files, SQLite and loopback Telegram HTTP."""
import importlib.util
import http.server
import json
import os
import pathlib
import sqlite3
import subprocess
import tempfile
import threading
import time
import unittest

spec = importlib.util.spec_from_file_location('activity', pathlib.Path(__file__).resolve().parents[1] / 'scripts/activity_indicator.py')
activity = importlib.util.module_from_spec(spec)
spec.loader.exec_module(activity)


class ActivityIO(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temporary.name)
        self.calls = []
        self.answers = []
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                payload = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                method = self.path.rsplit('/', 1)[-1]
                owner.calls.append((method, payload))
                default = True if method == 'deleteMessage' else {
                    'message_id': payload.get('message_id', 100 + sum(m == 'sendMessage' for m, _ in owner.calls)),
                    'chat': {'id': payload['chat_id']},
                    'message_thread_id': payload.get('message_thread_id', 7)}
                value = owner.answers.pop(0) if owner.answers else {'ok': True, 'result': default}
                if value is None:
                    self.connection.shutdown(2)  # Actual ambiguous transport failure.
                    return
                body = json.dumps(value).encode()
                self.send_response(200)
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_):
                pass

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        token = self.root / 'bot.token'
        token.write_text('synthetic-activity-token')
        token.chmod(0o600)
        self.telegram = activity.Telegram({'bot_token_file': str(token), 'api_base': f'http://127.0.0.1:{self.server.server_port}'})
        self.path = self.root / 'messages.json'
        self.publisher = activity.Publisher(self.telegram, self.path)

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.temporary.cleanup()

    def test_explicit_update_receipt_explains_wait_deadline_and_liveness(self):
        path = self.root / 'update.json'
        record = {'activation':'waiting_for_idle', 'topic_key':'100:7',
                  'waiting_for':'conversations_idle', 'worker_pid':os.getpid(),
                  'busy_conversations':2, 'deadline_at':3700, 'at':100}
        activity.atomic_json(path, record)
        line = activity.pending_update_line(path, '100:7', 100)
        self.assertIn('waiting for 2 conversation(s) to finish', line)
        self.assertIn('deadline in 1h 0m', line)
        self.assertIsNone(activity.pending_update_line(path, '100:8', 100))
        report, active, automatic = activity.combine_update_status('◻️ Idle', None, None, line)
        self.assertEqual((report, active, automatic), (line, line, line))
        self.assertIn('heartbeat stale', activity.pending_update_line(path, '100:7', 130))
        worker = subprocess.Popen(['/usr/bin/true'])
        worker.wait()
        record.update(worker_pid=worker.pid)
        activity.atomic_json(path, record)
        self.assertIn('process unavailable', activity.pending_update_line(path, '100:7', 100))
        record.update(worker_pid=os.getpid())
        record.update(at=3701)
        activity.atomic_json(path, record)
        self.assertIn('deadline elapsed', activity.pending_update_line(path, '100:7', 3701))
        record.update(activation='active')
        activity.atomic_json(path, record)
        self.assertIsNone(activity.pending_update_line(path, '100:7', 3701))
        record.update(activation='waiting_for_idle', at=100)
        activity.atomic_json(path, record)
        path.chmod(0o644)
        self.assertIsNone(activity.pending_update_line(path, '100:7', 100))

    def test_one_silent_message_survives_observer_restart_and_retires_itself(self):
        self.publisher.update('100:7', 100, 7, 'turn-a', 'Working', 100)
        message = self.publisher.entries['100:7']['message_id']
        restarted = activity.Publisher(self.telegram, self.path)
        restarted.update('100:7', 100, 7, 'turn-a', 'Waiting for agents', 140)
        restarted.update('100:7', 100, 7, 'turn-a', 'Too soon', 141)
        restarted.update('100:7', 100, 7, 'turn-a', None, 180)
        self.assertEqual([method for method, _ in self.calls], ['sendMessage', 'editMessageText', 'deleteMessage'])
        self.assertTrue(self.calls[0][1]['disable_notification'])
        self.assertEqual(self.calls[0][1]['message_thread_id'], 7)
        self.assertTrue(all(payload['message_id'] == message for _, payload in self.calls[1:]))
        self.assertEqual(json.loads(self.path.read_text()), {})
        receipt = json.loads((self.root / 'retirements.json').read_text())['100:7']
        self.assertEqual(receipt['outcome'], 'deleted')
        self.assertEqual(receipt['message_id'], message)

    def test_ambiguous_send_is_not_repeated_after_restart(self):
        self.answers = [None]
        self.publisher.update('100:7', 100, 7, 'turn-a', 'Working', 100)
        restarted = activity.Publisher(self.telegram, self.path)
        restarted.update('100:7', 100, 7, 'turn-a', 'Still working', 150)
        self.assertEqual(len(self.calls), 1)
        self.assertTrue(restarted.entries['100:7']['attempted'])

    def test_new_human_request_reposts_once_and_final_answer_retires_active_outer_task(self):
        reader = activity.Rollout(self.root / 'unused', 'parent')
        now = time.time()
        stamp = lambda: time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(now - 20))
        event = lambda item: {'timestamp': stamp(), 'type': 'event_msg',
                              'payload': {'type': 'item_completed', 'item': item}}
        reader.consume(event({'type': 'UserMessage', 'id': 'request-a'}))
        line = activity.status_line({'type': 'active'}, reader, [], now)
        self.assertNotIn('updated', line)
        self.publisher.update('100:7', 100, 7, reader.request_id, line, now)
        original_message = self.publisher.entries['100:7']['message_id']
        reader.consume(event({'type': 'AgentMessage', 'phase': 'final_answer'}))
        self.assertIsNone(activity.status_line({'type': 'active'}, reader, [], now))
        # A child or a pending tool must keep the indicator alive after a final.
        self.assertIn('Waiting for 1 agent', activity.status_line({'type': 'active'}, reader, [{'type': 'active'}], now))
        reader.calls['pending'] = 'tools'
        self.assertIn('tool call', activity.status_line({'type': 'active'}, reader, [], now))
        reader.calls.clear()
        reader.consume(event({'type': 'UserMessage', 'id': 'request-b'}))
        line = activity.status_line({'type': 'active'}, reader, [], now)
        self.publisher.update('100:7', 100, 7, reader.request_id, line, now)
        self.publisher.update('100:7', 100, 7, reader.request_id, line, now + 1)
        self.assertEqual([m for m, _ in self.calls], ['sendMessage', 'deleteMessage', 'sendMessage'])
        self.assertNotEqual(original_message, self.publisher.entries['100:7']['message_id'])
        self.assertEqual(self.calls[1][1]['message_id'], original_message)
        reader.consume(event({'type': 'AgentMessage', 'phase': 'final_answer'}))
        self.publisher.update('100:7', 100, 7, reader.request_id,
                              activity.status_line({'type': 'active'}, reader, [], now), now)
        self.assertEqual(self.calls[-1][0], 'deleteMessage')
        self.assertEqual(self.publisher.entries, {})

    def test_status_only_answers_new_handled_authorized_bound_commands_once(self):
        database = sqlite3.connect(':memory:')
        database.row_factory = sqlite3.Row
        database.executescript('''CREATE TABLE incoming_updates(update_id INTEGER PRIMARY KEY,
            payload_json TEXT,status TEXT,updated_at TEXT);
            CREATE TABLE users(tg_user_id INTEGER PRIMARY KEY,allowed INTEGER);
            INSERT INTO users VALUES(1,1),(2,0);''')
        def add(identifier, text='/status', user=1, topic=7, status='handled'):
            payload = {'message': {'chat': {'id': 100}, 'message_thread_id': topic,
                'from': {'id': user}, 'message_id': identifier + 1000, 'text': text}}
            database.execute("INSERT INTO incoming_updates VALUES(?,?,?,datetime('now'))",
                             (identifier, json.dumps(payload), status))
        add(1)  # History must not trigger an unsolicited response at activation.
        path = self.root / 'status-requests.json'
        requests = activity.StatusRequests(database, self.telegram, path)
        for identifier, options in [(2, {}), (3, {'user': 2}), (4, {'topic': 99}),
                                    (5, {'text': 'secret prompt'}), (6, {'status': 'processing'})]:
            add(identifier, **options)
        reports = {'100:7': '◻️ Idle'}
        quota = lambda: 'weekly 75% available · reset in 2d 3h'
        requests.respond(reports, quota)
        self.assertEqual(len(self.calls), 1)
        payload = self.calls[0][1]
        self.assertEqual(payload['text'], '◻️ Idle · weekly 75% available · reset in 2d 3h')
        self.assertEqual(payload['reply_parameters'], {'message_id': 1002})
        self.assertEqual(payload['message_thread_id'], 7)
        self.assertNotIn('secret', path.read_text())
        restarted = activity.StatusRequests(database, self.telegram, path)
        restarted.respond(reports, quota)
        self.assertEqual(len(self.calls), 1)
        database.execute("UPDATE incoming_updates SET status='handled' WHERE update_id=6")
        self.answers = [None]
        restarted.respond(reports, quota)
        activity.StatusRequests(database, self.telegram, path).respond(reports, quota)
        self.assertEqual(len(self.calls), 2)  # Unknown delivery is never replayed.
        database.close()

    def test_disconnect_and_recovery_replace_live_claim_without_waiting_for_cadence(self):
        self.publisher.update('100:7', 100, 7, 'turn-a', 'Working', 100)
        self.publisher.update('100:7', 100, 7, 'turn-a', '⚠️ Connection unavailable', 101)
        self.publisher.update('100:7', 100, 7, 'turn-a', 'Working', 102)
        self.assertEqual([method for method, _ in self.calls], ['sendMessage', 'editMessageText', 'editMessageText'])
        self.assertIn('Connection unavailable', self.calls[1][1]['text'])
        self.assertEqual(self.calls[2][1]['text'], 'Working')

    def test_wrong_topic_receipt_never_becomes_an_edit_target(self):
        self.answers = [{'ok': True, 'result': {'message_id': 900, 'chat': {'id': 100}, 'message_thread_id': 8}}]
        self.publisher.update('100:7', 100, 7, 'turn-a', 'Working', 100)
        self.publisher.update('100:7', 100, 7, 'turn-a', 'Still working', 150)
        self.publisher.update('100:7', 100, 7, 'turn-a', None, 200)
        self.assertEqual([method for method, _ in self.calls], ['sendMessage'])

    def test_delete_failure_makes_status_inactive_without_touching_assistant_messages(self):
        self.publisher.update('100:7', 100, 7, 'turn-a', 'Working', 100)
        self.answers = [{'ok': False, 'error_code': 400, 'description': "message can't be deleted"}, {'ok': True, 'result': {}}]
        self.publisher.update('100:7', 100, 7, 'turn-a', None, 140)
        self.assertEqual(self.calls[-1][0], 'editMessageText')
        self.assertIn('Activity monitoring ended', self.calls[-1][1]['text'])
        self.assertEqual(self.calls[-1][1]['message_id'], 101)
        self.assertEqual(self.publisher.entries, {})
        self.assertEqual(json.loads((self.root / 'retirements.json').read_text())['100:7']['outcome'], 'marked_inactive')

    def test_explicit_rate_limit_delays_safe_retry(self):
        self.answers = [{'ok': False, 'error_code': 429, 'parameters': {'retry_after': 60}}]
        self.publisher.update('100:7', 100, 7, 'turn-a', 'Working', 100)
        self.publisher.update('100:7', 100, 7, 'turn-a', 'Still working', 140)
        self.assertEqual(len(self.calls), 1)
        self.publisher.update('100:7', 100, 7, 'turn-a', 'Still working', 161)
        self.assertEqual(len(self.calls), 2)
        self.assertEqual(self.publisher.entries['100:7']['message_id'], 102)

    def test_incremental_native_tools_agents_partial_records_and_binding(self):
        path = self.root / 'rollout.jsonl'
        records = [
            {'type': 'session_meta', 'payload': {'id': 'native-a'}},
            {'timestamp': '2026-10-06T12:00:00Z', 'type': 'event_msg', 'payload': {'type': 'task_started', 'turn_id': 'turn-a'}},
            {'type': 'response_item', 'payload': {'type': 'function_call', 'call_id': 'tool-1', 'name': 'secret_command', 'arguments': 'NEVER DISPLAY'}},
            {'type': 'event_msg', 'payload': {'type': 'item_completed', 'item': {'type': 'SubAgentActivity', 'kind': 'started', 'agent_thread_id': 'child-a'}}},
        ]
        path.write_text(''.join(json.dumps(record) + '\n' for record in records))
        reader = activity.Rollout(path, 'native-a')
        reader.refresh()
        self.assertEqual(len(reader.calls), 1)
        line = activity.status_line({'type': 'active'}, reader, [{'type': 'active'}], activity.timestamp('2026-10-06T12:02:00Z'))
        self.assertIn('1 tool call(s) active', line)
        self.assertIn('1 agent(s) active', line)
        self.assertNotIn('secret', line)
        self.assertNotIn('NEVER DISPLAY', line)
        partial = json.dumps({'type': 'response_item', 'payload': {'type': 'function_call_output', 'call_id': 'tool-1'}})
        with path.open('a') as stream:
            stream.write(partial[:20])
        offset = reader.offset
        reader.refresh()
        self.assertEqual(reader.offset, offset)
        with path.open('a') as stream:
            stream.write(partial[20:] + '\n')
        reader.refresh()
        self.assertEqual(reader.calls, {})
        self.assertIsNone(activity.status_line({'type': 'idle'}, reader, [{'type': 'idle'}], time.time()))
        self.assertIn('Waiting for 1 agent', activity.status_line({'type': 'idle'}, reader, [{'type': 'active'}], time.time()))
        with self.assertRaises(ValueError):
            activity.Rollout(path, 'other-native-thread').refresh()

    def test_sqlite_observation_is_readonly_and_never_reads_input_payloads(self):
        path = self.root / 'bridge.sqlite'
        with sqlite3.connect(path) as connection:
            connection.execute('CREATE TABLE sessions(id INTEGER, codex_thread_id TEXT)')
            connection.execute("INSERT INTO sessions VALUES(1,'bound-thread')")
        connection = activity.readonly(path)
        try:
            self.assertEqual(connection.execute('SELECT codex_thread_id FROM sessions').fetchone()[0], 'bound-thread')
            with self.assertRaises(sqlite3.OperationalError):
                connection.execute('DELETE FROM sessions')
        finally:
            connection.close()

    def test_observer_methods_cannot_consume_updates_or_start_turns(self):
        for method in ('getUpdates', 'getMe', 'sendDocument'):
            with self.assertRaises(ValueError):
                self.telegram.call(method, {})
        native = object.__new__(activity.Native)
        for method in ('turn/start', 'turn/steer', 'thread/resume'):
            with self.assertRaises(ValueError):
                native.rpc(method, {})
        self.assertEqual(self.calls, [])

    def test_child_status_requires_native_ancestry_not_a_rollout_reference(self):
        database = sqlite3.connect(':memory:')
        database.row_factory = sqlite3.Row
        database.execute('CREATE TABLE threads(id TEXT,source TEXT,archived INTEGER)')
        for child, parent in [('child-a', 'parent-a'), ('nested', 'child-a'), ('foreign', 'parent-b'), ('cycle', 'cycle')]:
            source = json.dumps({'subagent': {'thread_spawn': {'parent_thread_id': parent}}})
            database.execute('INSERT INTO threads VALUES(?,?,0)', (child, source))
        try:
            self.assertTrue(activity.linked_child(database, 'child-a', 'parent-a'))
            self.assertTrue(activity.linked_child(database, 'nested', 'parent-a'))
            self.assertFalse(activity.linked_child(database, 'foreign', 'parent-a'))
            self.assertFalse(activity.linked_child(database, 'cycle', 'parent-a'))
            self.assertFalse(activity.linked_child(database, 'missing', 'parent-a'))
        finally:
            database.close()

    def test_weekly_available_uses_duration_and_codex_bucket(self):
        window = lambda used, duration: {'usedPercent': used, 'windowDurationMins': duration}
        response = {'rateLimits': {'primary': window(80, 300), 'secondary': window(25, 10080)}}
        self.assertEqual(activity.weekly_available(response), 75)
        response['rateLimitsByLimitId'] = {'codex': {'primary': window(40, 10080), 'secondary': window(70, 300)},
                                         'other': {'secondary': window(90, 10080)}}
        self.assertEqual(activity.weekly_available(response), 60)
        self.assertEqual(activity.weekly_label(60), 'weekly 60% available · reset n/a')
        for value in (None, {}, {'rateLimits': {'secondary': window(40, 300)}},
                      {'rateLimits': {'secondary': window(float('nan'), 10080)}}):
            self.assertIsNone(activity.weekly_available(value))

    def test_weekly_read_is_cached_and_failure_never_keeps_an_old_percentage(self):
        class FakeNative:
            count = 0
            def rpc(self, method, params):
                self.count += 1
                self.method, self.params = method, params
                if self.count > 1:
                    raise TimeoutError()
                return {'rateLimits': {'secondary': {'usedPercent': 20, 'windowDurationMins': 10080, 'resetsAt': 200000}}}
        native, limit = FakeNative(), activity.WeeklyLimit()
        self.assertEqual(limit.read(native, 100), 80)
        self.assertEqual(limit.read(native, 159), 80)
        self.assertEqual(native.count, 1)
        self.assertEqual(limit.resets_at, 200000)
        self.assertIsNone(limit.read(native, 160))
        self.assertIsNone(limit.resets_at)
        self.assertEqual(native.method, 'account/rateLimits/read')
        self.assertIsNone(native.params)
        self.assertEqual(activity.weekly_label(limit.available), 'weekly n/a')

    def test_weekly_reset_uses_same_window_and_countdown_changes_without_refetch(self):
        window = lambda duration, reset: {'usedPercent': 25, 'windowDurationMins': duration, 'resetsAt': reset}
        response = {'rateLimits': {'primary': window(300, 100), 'secondary': window(10080, 200000)}}
        self.assertEqual(activity.weekly_snapshot(response), (75, 200000))
        response['rateLimitsByLimitId'] = {'codex': {'primary': window(10080, 300000)}}
        self.assertEqual(activity.weekly_snapshot(response), (75, 300000))
        for seconds, expected in [(183600, '2d 3h'), (7500, '2h 5m'), (180, '3m'), (59, '<1m'), (-60, '<1m')]:
            self.assertEqual(activity.weekly_label(75, 200000, 200000 - seconds),
                             f'weekly 75% available · reset in {expected}')
        for invalid in [None, True, '300000', float('nan'), float('inf'), -10]:
            response['rateLimitsByLimitId']['codex']['primary']['resetsAt'] = invalid
            self.assertEqual(activity.weekly_snapshot(response), (75, None))

    def test_unloaded_native_session_is_idle_and_child_work_remains_visible(self):
        reader = activity.Rollout(self.root / 'unused', 'parent')
        now = time.time()
        reader.work_running = False
        # Unloading a stored thread is a successful native read, not a lost connection.
        self.assertEqual(activity.status_report({'type': 'notLoaded'}, reader, [], now), '◻️ Idle')
        self.assertIsNone(activity.status_line({'type': 'notLoaded'}, reader, [], now, automatic=True))
        self.assertIn('Waiting for 1 agent', activity.status_report(
            {'type': 'notLoaded'}, reader, [{'type': 'active'}], now))
        for kind in ('unknown', 'systemError'):
            self.assertIn('unavailable', activity.status_report({'type': kind}, reader, [], now))

    def test_automatic_indicator_shows_silent_work_and_ignores_tool_identity_churn(self):
        reader = activity.Rollout(self.root / 'unused', 'parent')
        now = int(time.time())
        def exchange(kind, at):
            reader.consume({'timestamp': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(at)),
                'type': 'event_msg', 'payload': {'type': 'item_completed', 'item': kind}})
        exchange({'type': 'UserMessage', 'id': 'request'}, now)
        self.assertIn('Working', activity.status_line({'type': 'active'}, reader, [], now))
        self.assertIsNone(activity.status_line({'type': 'active'}, reader, [], now, automatic=True))
        self.assertIsNone(activity.status_line({'type': 'active'}, reader, [], now+29, automatic=True))
        self.assertIn('Working', activity.status_line({'type': 'active'}, reader, [], now+30, automatic=True))
        reader.calls['pending-a'] = 'tools'
        self.assertIn('tool call', activity.status_line({'type': 'active'}, reader, [], now+31, automatic=True))
        reader.calls = {'pending-b': 'tools'}
        self.assertIn('tool call', activity.status_line({'type': 'active'}, reader, [], now+32, automatic=True))
        exchange({'type': 'AgentMessage', 'phase': 'commentary'}, now+33)
        self.assertIsNone(activity.status_line({'type': 'active'}, reader, [], now+33, automatic=True))
        self.assertIsNone(activity.status_line({'type': 'active'}, reader, [], now+62, automatic=True))
        self.assertIn('tool call', activity.status_line({'type': 'active'}, reader, [], now+63, automatic=True))
        reader.calls.clear()
        exchange({'type': 'AgentMessage', 'phase': 'final_answer'}, now+64)
        self.assertIsNone(activity.status_line({'type': 'active'}, reader, [], now+94, automatic=True))
        self.assertIsNone(activity.status_line({'type': 'idle'}, reader, [], now+94, automatic=True))

    def test_status_cache_revalidates_binding_and_marks_stale_snapshots_unconfirmed(self):
        db = sqlite3.connect(':memory:')
        db.row_factory = sqlite3.Row
        db.executescript('''CREATE TABLE incoming_updates(update_id INTEGER PRIMARY KEY,
            payload_json TEXT,status TEXT,updated_at TEXT);
            CREATE TABLE users(tg_user_id INTEGER PRIMARY KEY,allowed INTEGER);
            CREATE TABLE sessions(chat_id INTEGER,thread_id INTEGER,codex_thread_id TEXT,creator_user_id INTEGER);
            INSERT INTO users VALUES(1,1);
            INSERT INTO sessions VALUES(100,7,'original',1);''')
        requests = activity.StatusRequests(db, self.telegram, self.root / 'status-requests.json')
        cache = activity.StatusCache()
        quota = activity.WeeklyLimit()
        quota.available, quota.resets_at = 75, time.time()+3600
        contexts = {'100:7': {'turn': 'request-a', 'active': True}}
        cache.update({'100:7':'Working'}, {'100:7':'original'}, quota, contexts)
        def add(identifier):
            db.execute("INSERT INTO incoming_updates VALUES(?,?,?,datetime('now'))", (identifier,
                json.dumps({'message':{'chat':{'id':100}, 'message_thread_id':7, 'from':{'id':1},
                'message_id':identifier+1000,'text':'/status'}}), 'handled'))
        add(1)
        db.execute("UPDATE sessions SET codex_thread_id='replacement'")
        cache.respond(requests, self.publisher)
        self.assertEqual(self.calls, [])
        cache.update({'100:7':'Working'}, {'100:7':'replacement'}, quota, contexts)
        reports, bindings, available, reset, observed, contexts = cache.value
        cache.value = (reports, bindings, available, reset, observed-30, contexts)
        cache.respond(requests, self.publisher)
        self.assertIn('unconfirmed', self.calls[0][1]['text'])
        self.assertIn('weekly n/a', self.calls[0][1]['text'])
        self.assertEqual(self.publisher.snapshot(), {})
        cache.respond(requests, self.publisher)
        self.assertEqual(len(self.calls),1)
        db.close()

    def test_status_command_is_answered_while_native_observation_is_not_running(self):
        path = self.root / 'bridge.sqlite3'
        db = sqlite3.connect(path)
        db.executescript('''CREATE TABLE incoming_updates(update_id INTEGER PRIMARY KEY,
            payload_json TEXT,status TEXT,updated_at TEXT);
            CREATE TABLE users(tg_user_id INTEGER PRIMARY KEY,allowed INTEGER);
            CREATE TABLE sessions(chat_id INTEGER,thread_id INTEGER,codex_thread_id TEXT,creator_user_id INTEGER);
            INSERT INTO users VALUES(1,1);
            INSERT INTO sessions VALUES(100,7,'bound',1);''')
        db.commit()
        cache = activity.StatusCache()
        quota = activity.WeeklyLimit()
        quota.available, quota.resets_at = 78, time.time()+86400
        cache.update({'100:7':'Working'}, {'100:7':'bound'}, quota)
        stopped = threading.Event()
        worker = threading.Thread(target=activity.watch_status_requests,
            args=({'db_path':str(path)}, self.root, cache, stopped, self.telegram))
        worker.start()
        try:
            deadline=time.monotonic()+2
            while not (self.root/'status-requests.json').exists():
                self.assertLess(time.monotonic(), deadline)
                time.sleep(.01)
            started=time.monotonic()
            db.execute("INSERT INTO incoming_updates VALUES(1,?,'handled',datetime('now'))", (json.dumps(
                {'message':{'chat':{'id':100},'message_thread_id':7,'from':{'id':1},
                'message_id':1001,'text':'/status'}}),))
            db.commit()
            while not self.calls:
                self.assertLess(time.monotonic()-started, 1.5)
                time.sleep(.01)
            self.assertIn('Working',self.calls[0][1]['text'])
            self.assertIn('weekly 78%',self.calls[0][1]['text'])
            self.assertEqual(self.calls[0][1]['reply_parameters'], {'message_id':1001})
        finally:
            stopped.set()
            worker.join(timeout=2)
            db.close()
        self.assertFalse(worker.is_alive())

    def test_active_status_commands_share_the_automatic_message_and_finish_cleanup(self):
        db = sqlite3.connect(':memory:')
        db.row_factory = sqlite3.Row
        db.executescript('''CREATE TABLE incoming_updates(update_id INTEGER PRIMARY KEY,
            payload_json TEXT,status TEXT,updated_at TEXT);
            CREATE TABLE users(tg_user_id INTEGER PRIMARY KEY,allowed INTEGER);
            CREATE TABLE sessions(chat_id INTEGER,thread_id INTEGER,codex_thread_id TEXT,creator_user_id INTEGER);
            INSERT INTO users VALUES(1,1);
            INSERT INTO sessions VALUES(100,7,'bound',1);''')
        requests = activity.StatusRequests(db, self.telegram, self.root / 'requests.json')
        cache, quota = activity.StatusCache(), activity.WeeklyLimit()
        quota.available, quota.resets_at = 78, time.time() + 86400
        cache.update({'100:7': 'Working'}, {'100:7': 'bound'}, quota,
                     {'100:7': {'turn': 'request-a', 'active': True}})
        now = time.time()
        self.publisher.update('100:7', 100, 7, 'request-a', 'Working', now)
        original = self.publisher.snapshot()['100:7']['message_id']
        for update in (1, 2):
            db.execute("INSERT INTO incoming_updates VALUES(?,?,'handled',datetime('now'))", (update,
                json.dumps({'message': {'chat': {'id': 100}, 'message_thread_id': 7,
                    'from': {'id': 1}, 'message_id': 1000 + update, 'text': '/status'}})))
            cache.respond(requests, self.publisher)
        self.assertEqual([method for method, _ in self.calls],
                         ['sendMessage', 'editMessageText', 'editMessageText'])
        self.assertEqual([receipt['message_id'] for receipt in requests.state['attempted'].values()],
                         [original, original])
        restarted = activity.Publisher(self.telegram, self.path)
        # A new commentary exchange would suppress automatic creation, but the
        # explicitly requested card must keep updating for this same request.
        restarted.update('100:7', 100, 7, 'request-a', None, now + 40,
                         active_text='Waiting for 1 agent · weekly 78% available')
        self.assertEqual(self.calls[-1][0], 'editMessageText')
        self.assertEqual(self.calls[-1][1]['message_id'], original)
        self.assertIn('Waiting for 1 agent', self.calls[-1][1]['text'])
        restarted.update('100:7', 100, 7, 'request-a', None, now + 48)
        self.assertEqual(self.calls[-1][0], 'deleteMessage')
        self.assertEqual(restarted.snapshot(), {})
        # The explicit request does not bypass silence grace for a later turn.
        restarted.update('100:7', 100, 7, 'request-b', None, now + 50, active_text='Working')
        self.assertEqual(restarted.snapshot(), {})
        db.close()

    def test_concurrent_manual_and_automatic_status_create_only_one_card(self):
        barrier = threading.Barrier(2)
        def publish(requested):
            barrier.wait(timeout=2)
            self.publisher.update('100:7', 100, 7, 'request-a', 'Working', time.time(),
                                  requested=requested, reply_to=1001 if requested else None)
        threads = [threading.Thread(target=publish, args=(requested,)) for requested in (True, False)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=3)
            self.assertFalse(thread.is_alive())
        self.assertEqual(sum(method == 'sendMessage' for method, _ in self.calls), 1)
        entry = self.publisher.snapshot()['100:7']
        self.assertTrue(entry['requested'])
        self.assertEqual(entry['message_id'], 101)

    def test_reactivated_child_shows_automatic_status_after_quiet_parent_final(self):
        path = self.root / 'reactivated.jsonl'
        now = time.time()
        stamp = lambda seconds: time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(now - seconds))
        records = [
            {'type': 'session_meta', 'payload': {'id': 'parent'}},
            {'timestamp': stamp(90), 'type': 'event_msg',
             'payload': {'type': 'task_started', 'turn_id': 'request-a'}},
            {'timestamp': stamp(80), 'type': 'event_msg', 'payload': {'type': 'item_completed',
             'item': {'type': 'AgentMessage', 'phase': 'final_answer'}}},
            {'timestamp': stamp(75), 'type': 'event_msg', 'payload': {'type': 'item_completed',
             'item': {'type': 'SubAgentActivity', 'kind': 'completed', 'agent_thread_id': 'child'}}},
            {'timestamp': stamp(31), 'type': 'event_msg', 'payload': {'type': 'item_completed',
             'item': {'type': 'SubAgentActivity', 'kind': 'interacted', 'agent_thread_id': 'child'}}},
        ]
        path.write_text(''.join(json.dumps(r) + '\n' for r in records))
        index = sqlite3.connect(':memory:')
        index.row_factory = sqlite3.Row
        index.execute('CREATE TABLE threads(id TEXT,source TEXT,archived INTEGER)')
        index.execute('INSERT INTO threads VALUES(?,?,0)', ('child', json.dumps(
            {'subagent': {'thread_spawn': {'parent_thread_id': 'parent'}}})))
        reader = activity.Rollout(path, 'parent')
        reader.refresh()
        self.assertFalse(reader.work_running)
        self.assertEqual(reader.agents, {'child'})
        children = [{'type': 'active'} for child in reader.agents
                    if activity.linked_child(index, child, 'parent')]
        line = activity.status_line({'type': 'idle'}, reader, children, now, automatic=True)
        self.assertIn('Waiting for 1 agent', line)
        self.publisher.update('100:7', 100, 7, reader.request_id, line, now)
        self.assertEqual(self.calls[0][0], 'sendMessage')  # No /status command.
        self.assertTrue(self.publisher.snapshot()['100:7']['message_id'])
        # Routine subagent events must not restart the thirty-second silence grace.
        self.assertEqual(reader.last_exchange, stamp(80))
        records[-1]['payload']['item']['kind'] = 'completed'
        with path.open('a') as stream:
            stream.write(json.dumps(records[-1]) + '\n')
        reader.refresh()
        self.assertEqual(reader.agents, set())
        line = activity.status_line({'type': 'idle'}, reader, [], now + 8, automatic=True)
        self.publisher.update('100:7', 100, 7, reader.request_id, line, now + 8)
        self.assertEqual(self.calls[-1][0], 'deleteMessage')
        index.close()

    def test_same_turn_card_moves_below_new_exchanges_then_edits_in_place(self):
        old = {'exchange': 'commentary-a', 'input_id': 1000}
        new = {'exchange': 'commentary-b', 'input_id': 1000}
        self.publisher.update('100:7', 100, 7, 'request-a', 'Working', 100,
                              requested=True, position=old)
        first = self.publisher.snapshot()['100:7']['message_id']
        # Same native request, but an assistant message is now below the card.
        self.publisher.update('100:7', 100, 7, 'request-a', None, 101,
                              active_text='Working', position=new)
        self.assertEqual([m for m, _ in self.calls], ['sendMessage', 'deleteMessage'])
        self.assertEqual(self.calls[-1][1]['message_id'], first)
        self.assertEqual(self.publisher.snapshot(), {})
        self.publisher.update('100:7', 100, 7, 'request-a', 'Working', 132,
                              active_text='Working', position=new)
        last = self.publisher.snapshot()['100:7']['message_id']
        self.assertNotEqual(first, last)
        self.publisher.update('100:7', 100, 7, 'request-a', 'Waiting for agent', 163,
                              position=new)
        self.assertEqual(self.calls[-1][0], 'editMessageText')
        self.assertEqual(self.calls[-1][1]['message_id'], last)
        # A slash command creates no native user item, but still moves the card.
        command = dict(new, input_id=1004)
        self.publisher.update('100:7', 100, 7, 'request-a', None, 164,
                              active_text='Working', position=command)
        self.assertEqual(self.calls[-1][0], 'deleteMessage')
        self.assertEqual(self.publisher.snapshot(), {})

    def test_status_command_repositions_card_after_standard_session_details(self):
        db = sqlite3.connect(':memory:'); db.row_factory = sqlite3.Row
        db.executescript('''CREATE TABLE incoming_updates(update_id INTEGER PRIMARY KEY,
            payload_json TEXT,status TEXT,updated_at TEXT);
            CREATE TABLE users(tg_user_id INTEGER PRIMARY KEY,allowed INTEGER);
            CREATE TABLE sessions(chat_id INTEGER,thread_id INTEGER,codex_thread_id TEXT,creator_user_id INTEGER);
            INSERT INTO users VALUES(1,1);
            INSERT INTO users VALUES(2,0);
            INSERT INTO sessions VALUES(100,7,'bound',1);''')
        requests = activity.StatusRequests(db, self.telegram, self.root / 'requests.json')
        old = {'exchange': 'commentary', 'input_id': 1000}
        now = time.time()
        self.publisher.update('100:7', 100, 7, 'request-a', 'Working', now, position=old)
        original = self.publisher.snapshot()['100:7']['message_id']
        for update, sender, topic in ((1, 1, 7), (2, 2, 7), (3, 1, 8)):
            db.execute("INSERT INTO incoming_updates VALUES(?,?,'handled',datetime('now'))", (update,
                json.dumps({'message': {'chat': {'id': 100}, 'message_thread_id': topic,
                    'from': {'id': sender}, 'message_id': 1000 + update,
                    'date': int(now), 'text': '/status'}})))
        latest = activity.latest_input(db, 100, 7)
        self.assertEqual(latest['message_id'], 1001)
        self.assertEqual(latest['at'], int(now))
        requests.respond({'100:7': 'Working'}, lambda: 'weekly 78% available', {'100:7': 'bound'},
                         {'100:7': {'active': True, 'turn': 'request-a', 'position': old}}, self.publisher)
        self.assertEqual([m for m, _ in self.calls], ['sendMessage', 'deleteMessage', 'sendMessage'])
        self.assertNotEqual(original, self.publisher.snapshot()['100:7']['message_id'])
        self.assertEqual(self.calls[-1][1]['reply_parameters'], {'message_id': 1001})
        self.assertEqual(self.publisher.snapshot()['100:7']['position']['input_id'], 1001)
        db.close()

    def test_async_question_does_not_mark_continuing_native_work_completed(self):
        reader = activity.Rollout(self.root / 'unused', 'parent')
        now = time.time()
        stamp = lambda seconds: time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(now - seconds))
        reader.consume({'timestamp': stamp(90), 'type': 'event_msg',
                        'payload': {'type': 'task_started', 'turn_id': 'request-a'}})
        reader.consume({'timestamp': stamp(31), 'type': 'event_msg', 'payload': {
            'type': 'item_completed', 'item': {'type': 'AgentMessage',
                'phase': 'final_answer', 'delivery': 'async', 'questions': [{}]}}})
        line = activity.status_line({'type': 'active'}, reader, [], now, automatic=True)
        self.assertIn('Working', line)
        reader.consume({'timestamp': stamp(0), 'type': 'event_msg',
                        'payload': {'type': 'task_complete'}})
        self.assertIsNone(activity.status_line({'type': 'idle'}, reader, [], now, automatic=True))

    def test_manual_request_respects_retry_deadline_and_does_not_replay_ambiguous_send(self):
        self.answers = [{'ok': False, 'error_code': 429, 'parameters': {'retry_after': 60}}]
        self.publisher.update('100:7', 100, 7, 'request-a', 'Working', 100, requested=True)
        self.publisher.update('100:7', 100, 7, 'request-a', 'Working', 140, requested=True)
        self.assertEqual(len(self.calls), 1)
        self.answers = [None]
        self.publisher.update('100:7', 100, 7, 'request-a', 'Working', 161, requested=True)
        restarted = activity.Publisher(self.telegram, self.path)
        restarted.update('100:7', 100, 7, 'request-a', 'Working', 200, requested=True)
        self.assertEqual(len(self.calls), 2)



if __name__ == '__main__':
    unittest.main()
