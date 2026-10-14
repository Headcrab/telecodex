#!/usr/bin/env python3
"""Read-only native activity observer; never consumes Telegram updates or runs turns."""
import argparse
import base64
import datetime as dt
import fcntl
import html
import json
import math
import os
import pathlib
import queue
import signal
import sqlite3
import stat
import struct
import subprocess
import threading
import time
import tomllib
import urllib.error
import urllib.request


def atomic_json(path, value):
    temporary = path.with_suffix('.tmp')
    with open(temporary, 'w', opener=lambda p, f: os.open(p, f, 0o600)) as stream:
        json.dump(value, stream)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)


def readonly(path):
    connection = sqlite3.connect(pathlib.Path(path).resolve().as_uri() + '?mode=ro', uri=True)
    connection.row_factory = sqlite3.Row
    connection.execute('PRAGMA query_only=ON')
    return connection


class Native:
    """Attach to the existing native daemon through its WebSocket stdio proxy."""
    def __init__(self, binary):
        self.process = subprocess.Popen([binary, 'app-server', 'proxy'],
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL)
        self.messages = queue.Queue(maxsize=128)
        self.sequence = 0
        self.closed = threading.Event()
        self.write_lock = threading.Lock()
        threading.Thread(target=self.read, daemon=True).start()
        try:
            self.messages.get(timeout=5)  # Verified WebSocket handshake.
            self.rpc('initialize', {'clientInfo': {'name': 'telecodex-activity', 'version': '1.0'},
                                    'capabilities': {'experimentalApi': False}})
            self.send({'method': 'initialized'})
        except Exception:
            self.close()
            raise

    def read_exact(self, size):
        value = self.process.stdout.read(size)
        if len(value) != size:
            raise EOFError()
        return value

    def read(self):
        try:
            key = base64.b64encode(os.urandom(16)).decode()
            request = (f'GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n'
                       f'Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n'
                       'Sec-WebSocket-Version: 13\r\n\r\n')
            self.process.stdin.write(request.encode())
            self.process.stdin.flush()
            header = bytearray()
            while not header.endswith(b'\r\n\r\n'):
                header.extend(self.read_exact(1))
                if len(header) > 8192:
                    raise ValueError('oversized handshake')
            import hashlib
            expected = base64.b64encode(hashlib.sha1(
                (key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest())
            if not header.startswith(b'HTTP/1.1 101 ') or expected not in header:
                raise ValueError('invalid handshake')
            self.messages.put(True, timeout=1)
            partial = bytearray()
            while not self.closed.is_set():
                head = self.read_exact(2)
                opcode, length = head[0] & 15, head[1] & 127
                if length == 126:
                    length = struct.unpack('!H', self.read_exact(2))[0]
                elif length == 127:
                    length = struct.unpack('!Q', self.read_exact(8))[0]
                if length > 8 * 1024 * 1024 or head[1] & 128:
                    raise ValueError('unsupported native frame')
                data = self.read_exact(length)
                if opcode == 8:
                    break
                if opcode in (9, 10):
                    if opcode == 9:
                        self.frame(data, 10)
                    continue
                partial.extend(data)
                if len(partial) > 8 * 1024 * 1024:
                    raise ValueError('oversized native message')
                if head[0] & 128:
                    message = json.loads(partial)
                    partial.clear()
                    # This client has no authority to answer server requests.
                    if 'method' not in message and 'id' in message:
                        self.messages.put(message, timeout=1)
        except (OSError, ValueError, EOFError, queue.Full):
            pass
        finally:
            self.closed.set()

    def frame(self, data, opcode=1):
        mask = os.urandom(4)
        size = len(data)
        length = bytes([128 | size]) if size < 126 else (
            b'\xfe' + struct.pack('!H', size) if size < 65536 else
            b'\xff' + struct.pack('!Q', size))
        frame = bytes([128 | opcode]) + length + mask
        frame += bytes(value ^ mask[i % 4] for i, value in enumerate(data))
        with self.write_lock:
            self.process.stdin.write(frame)
            self.process.stdin.flush()

    def send(self, value):
        self.frame(json.dumps(value).encode())

    def rpc(self, method, params):
        if method not in ('initialize', 'thread/read', 'account/rateLimits/read'):
            raise ValueError('observer method forbidden')
        self.sequence += 1
        self.send({'id': self.sequence, 'method': method, 'params': params})
        deadline = time.monotonic() + 4
        while time.monotonic() < deadline:
            value = self.messages.get(timeout=max(.01, deadline - time.monotonic()))
            if value.get('id') == self.sequence:
                if 'error' in value:
                    return None
                return value.get('result', {})
        raise TimeoutError()

    def status(self, thread_id):
        response = self.rpc('thread/read', {'threadId': thread_id, 'includeTurns': False})
        if response is None:
            return {'type': 'unknown'}
        thread = response.get('thread', {})
        if thread.get('id') != thread_id:
            raise ValueError('native thread mismatch')
        return thread.get('status', {'type': 'unknown'})

    def close(self):
        self.closed.set()
        if self.process.poll() is None:
            self.process.terminate()  # Only this proxy, never the native daemon.
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.process.stdin.close()
        self.process.stdout.close()


class Rollout:
    """Bounded initial tail and incremental metadata, restricted to linked threads."""
    def __init__(self, path, thread_id):
        self.path = pathlib.Path(path)
        self.thread_id = thread_id
        self.offset = 0
        self.identity = None
        self.calls = {}
        self.agents = set()
        self.turn_id = None
        self.started = None
        self.last_signal = None
        self.work_running = None
        self.request_id = None
        self.pending_since = None
        self.last_exchange = None
        self.skipping = False

    def consume(self, value):
        p = value.get('payload', {})
        if not isinstance(p, dict):
            return
        self.last_signal = value.get('timestamp', self.last_signal)
        kind = p.get('type')
        if kind == 'task_started':
            self.calls.clear()
            self.turn_id = p.get('turn_id')
            self.started = value.get('timestamp')
            self.work_running = True
            self.request_id = self.turn_id
            self.last_exchange = None
            self.pending_since = None
        elif kind in ('task_complete', 'turn_aborted'):
            self.calls.clear()
            self.work_running = False
        elif value.get('type') == 'response_item':
            call_id = p.get('call_id')
            if kind in ('function_call', 'custom_tool_call') and call_id:
                self.work_running = True
                # Names are classified, never copied into the public status.
                name = str(p.get('name', '')).rsplit('.', 1)[-1]
                self.calls[call_id] = 'agents' if name in ('wait', 'wait_agent') else 'tools'
            elif kind in ('function_call_output', 'custom_tool_call_output'):
                self.calls.pop(call_id, None)
        if kind == 'item_completed':
            item = p.get('item', {})
            if not isinstance(item, dict):
                return
            if item.get('type') == 'UserMessage':
                self.request_id = item.get('id') or value.get('timestamp')
                self.started = value.get('timestamp')
                self.work_running = True
                self.last_exchange = value.get('timestamp')
                self.pending_since = None
            elif item.get('type') == 'AgentMessage':
                self.last_exchange = value.get('timestamp')
                if item.get('phase') == 'final_answer' and item.get('delivery') != 'async':
                    self.work_running = False
                elif item.get('phase') == 'commentary' or item.get('delivery') == 'async':
                    self.work_running = True
            elif item.get('type') == 'Reasoning':
                self.work_running = True
            if item.get('type') == 'SubAgentActivity':
                agent = item.get('agent_thread_id')
                if agent and item.get('kind') in ('started', 'interacted'):
                    self.agents.add(agent)
                elif agent and item.get('kind') in ('completed', 'errored', 'shutdown'):
                    self.agents.discard(agent)
            elif item.get('type') == 'CollabAgentToolCall':
                self.agents.update(item.get('receiver_thread_ids', []))

    def refresh(self):
        with self.path.open('rb') as stream:
            info = os.fstat(stream.fileno())
            identity = (info.st_dev, info.st_ino)
            if self.identity != identity or info.st_size < self.offset:
                first = json.loads(stream.readline(1024 * 1024))
                if first.get('type') != 'session_meta' or first.get('payload', {}).get('id') != self.thread_id:
                    raise ValueError('rollout binding mismatch')
                self.calls.clear()
                self.agents.clear()
                self.started = self.turn_id = self.last_signal = None
                self.work_running = self.request_id = None
                self.last_exchange = None
                self.skipping = False
                self.offset = max(0, info.st_size - 4 * 1024 * 1024)
                self.identity = identity
                stream.seek(self.offset)
                if self.offset:
                    stream.readline()  # Skip an incomplete leading record.
                    self.offset = stream.tell()
            stream.seek(self.offset)
            consumed = 0
            while consumed < 4 * 1024 * 1024:
                position = stream.tell()
                line = stream.readline(1024 * 1024)
                if not line:
                    break
                if self.skipping or (len(line) == 1024 * 1024 and not line.endswith(b'\n')):
                    self.skipping = not line.endswith(b'\n')
                    self.calls.clear()  # A skipped output cannot support an exact count.
                    consumed += len(line)
                    self.offset = stream.tell()
                    continue
                if not line.endswith(b'\n'):
                    stream.seek(position)
                    break
                consumed += len(line)
                self.offset = stream.tell()
                try:
                    self.consume(json.loads(line))
                except (ValueError, TypeError):
                    continue


def timestamp(value):
    try:
        parsed = dt.datetime.fromisoformat(value.replace('Z', '+00:00'))
        return parsed.timestamp()
    except (AttributeError, ValueError, TypeError):
        return None


def linked_child(database, child, parent):
    """Validate native ancestry before reading child status; rollout references are data."""
    visited = set()
    for _ in range(8):
        if child in visited:
            return False
        visited.add(child)
        row = database.execute('SELECT source FROM threads WHERE id=? AND archived=0', (child,)).fetchone()
        if not row:
            return False
        try:
            source = json.loads(row['source'])
            ancestor = source['subagent']['thread_spawn']['parent_thread_id']
        except (ValueError, TypeError, KeyError):
            return False
        if ancestor == parent:
            return True
        child = ancestor
    return False


def weekly_snapshot(response):
    if not isinstance(response, dict):
        return None, None
    buckets = response.get('rateLimitsByLimitId')
    snapshot = buckets.get('codex') if isinstance(buckets, dict) else None
    if snapshot is None:
        snapshot = response.get('rateLimits', response.get('rate_limits'))
    if not isinstance(snapshot, dict):
        return None, None
    for name in ('primary', 'secondary'):
        window = snapshot.get(name)
        if not isinstance(window, dict):
            continue
        duration = window.get('windowDurationMins', window.get('window_minutes'))
        used = window.get('usedPercent', window.get('used_percent'))
        if duration == 7 * 24 * 60 and isinstance(used, (int, float)) and not isinstance(used, bool) and math.isfinite(used):
            reset = window.get('resetsAt', window.get('resets_at'))
            if not isinstance(reset, (int, float)) or isinstance(reset, bool) or not math.isfinite(reset) or reset <= 0:
                reset = None
            return max(0, min(100, 100 - used)), reset
    return None, None


def weekly_available(response):
    return weekly_snapshot(response)[0]


class WeeklyLimit:
    def __init__(self):
        self.available = None
        self.resets_at = None
        self.next_read = 0

    def read(self, native, now):
        if now >= self.next_read:
            self.next_read = now + 60
            try:
                self.available, self.resets_at = weekly_snapshot(native.rpc('account/rateLimits/read', None))
            except (OSError, ValueError, queue.Empty, TimeoutError):
                self.available = None  # Keep the activity indicator useful without quota data.
                self.resets_at = None
        return self.available

    def label(self, native, now):
        self.read(native, now)
        return weekly_label(self.available, self.resets_at)


def weekly_label(available, resets_at=None, now=None):
    if available is None:
        return 'weekly n/a'
    label = f'weekly {available:.0f}% available'
    if resets_at is None:
        return label + ' · reset n/a'
    remaining = max(0, int(resets_at - (time.time() if now is None else now)))
    days, hours, minutes = remaining // 86400, remaining // 3600 % 24, remaining // 60 % 60
    duration = f'{days}d {hours}h' if days else (
        f'{hours}h {minutes}m' if hours else f'{minutes}m' if minutes else '<1m')
    return label + f' · reset in {duration}'


def status_line(status, rollout, children, now, *, automatic=False):
    active_children = sum(value.get('type') == 'active' for value in children)
    kind = status.get('type', 'unknown')
    flags = status.get('activeFlags', [])
    waiting = any(flag in flags for flag in ('waitingOnApproval', 'waitingOnUserInput'))
    # Steering may leave one native outer task open across several final answers.
    # A completed answer ends the visible request unless actual work remains.
    active = active_children > 0 or (kind == 'active' and (
        rollout.work_running is not False or rollout.calls or waiting))
    if not active:
        if automatic:
            rollout.pending_since = None
        return None
    if automatic:
        # Native working does not guarantee that Telegram is showing typing.
        # Keep active work visible after quiet exchanges, regardless of tool churn.
        quiet_since = timestamp(rollout.last_exchange or rollout.started)
        if quiet_since is None:
            if rollout.pending_since is None:
                rollout.pending_since = now
            quiet_since = rollout.pending_since
        if now - quiet_since < 30:
            return None
    if 'waitingOnApproval' in flags:
        summary = 'Waiting for approval'
    elif 'waitingOnUserInput' in flags:
        summary = 'Waiting for your answer'
    elif active_children and (kind != 'active' or rollout.work_running is False or 'agents' in rollout.calls.values()):
        summary = f'Waiting for {active_children} agent(s)'
    elif rollout.calls:
        summary = f'{len(rollout.calls)} tool call(s) active'
        if active_children:
            summary += f' · {active_children} agent(s) active'
    elif active_children:
        summary = f'Working · {active_children} agent(s) active'
    else:
        summary = 'Working'
    started = timestamp(rollout.started)
    elapsed = max(0, int(now - started)) if started is not None else 0
    duration = f'{elapsed // 60}m' if elapsed >= 60 else f'{elapsed}s'
    return f'⏳ {summary} · {duration}'


def status_report(status, rollout, children, now):
    line = status_line(status, rollout, children, now)
    if line:
        return line
    if status.get('type') in ('active', 'idle', 'notLoaded'):
        return '◻️ Idle'
    return '⚠️ Native activity unavailable'


def pending_update_line(path, key, now):
    """Project an explicitly configured local updater receipt, never infer tasks."""
    if not path:
        return None
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor) as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
                return None
            data = json.loads(stream.read(65536))
        if data.get('activation') != 'waiting_for_idle' or data.get('topic_key') != key:
            return None
        if data.get('waiting_for') != 'conversations_idle':
            return '⚠️ Bot update pending · wait reason unconfirmed'
        age = now - float(data['at'])
        pid = int(data['worker_pid'])
        if pid <= 0:
            raise ValueError('invalid worker')
        os.kill(pid, 0)
        if not 0 <= age <= 25:
            return '⚠️ Bot update pending · updater heartbeat stale'
        remaining = math.ceil(float(data['deadline_at']) - now)
        if remaining <= 0:
            return '⚠️ Bot update deadline elapsed · outcome unconfirmed'
        blockers = int(data['busy_conversations'])
        if blockers < 0:
            raise ValueError('invalid count')
        minutes = math.ceil(remaining / 60)
        duration = f'{minutes // 60}h {minutes % 60}m' if minutes >= 60 else f'{minutes}m'
        reason = f'waiting for {blockers} conversation(s) to finish' if blockers else 'waiting for input delivery to settle'
        return f'⏳ Bot update · {reason} · deadline in {duration}'
    except ProcessLookupError:
        return '⚠️ Bot update pending · updater process unavailable'
    except (OSError, ValueError, KeyError, TypeError):
        return None


def combine_update_status(report, active_text, automatic_text, update):
    if not update:
        return report, active_text, automatic_text
    report = update if report == '◻️ Idle' else report + '\n' + update
    active_text = active_text + '\n' + update if active_text else update
    automatic_text = automatic_text + '\n' + update if automatic_text else update
    return report, active_text, automatic_text


class Telegram:
    def __init__(self, config):
        token_path = pathlib.Path(config['bot_token_file'])
        descriptor = os.open(token_path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor) as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
                raise ValueError('token file must be owner-only and regular')
            self.token = stream.read(4096).strip()
            if not self.token or len(self.token) >= 4096:
                raise ValueError('invalid bounded token file')
        self.base = config.get('api_base', 'https://api.telegram.org').rstrip('/')
        self.next_chat = {}
        self.schedule_lock = threading.Lock()

    def call(self, method, payload):
        if method not in ('sendMessage', 'editMessageText', 'deleteMessage'):
            raise ValueError('observer Telegram method forbidden')
        chat = payload['chat_id']
        with self.schedule_lock:
            now = time.monotonic()
            scheduled = max(now, self.next_chat.get(chat, now))
            self.next_chat[chat] = scheduled + (3.5 if chat < 0 else 1)
        time.sleep(max(0, scheduled - time.monotonic()))
        request = urllib.request.Request(f'{self.base}/bot{self.token}/{method}',
                                         data=json.dumps(payload).encode(),
                                         headers={'Content-Type': 'application/json'})
        try:
            with urllib.request.urlopen(request, timeout=8) as response:
                value = json.load(response)
        except urllib.error.HTTPError as error:
            try:
                value = json.loads(error.read(65536))
            except ValueError:
                value = {'ok': False}
        except (OSError, ValueError):
            return None  # Unknown delivery; do not log the credential-bearing URL.
        return value


class Publisher:
    def __init__(self, telegram, path):
        self.telegram, self.path = telegram, path
        self.entries = json.loads(path.read_text()) if path.exists() else {}
        self.lock = threading.RLock()

    def snapshot(self):
        with self.lock:
            return {key: dict(entry) for key, entry in self.entries.items()}

    def save(self):
        atomic_json(self.path, self.entries)

    def update(self, key, chat, topic, turn, text, now, *, requested=False,
               reply_to=None, active_text=None, position=None):
        with self.lock:
            self._update(key, chat, topic, turn, text, now, requested=requested,
                         reply_to=reply_to, active_text=active_text, position=position)
            return self.entries.get(key, {}).get('message_id')

    def _update(self, key, chat, topic, turn, text, now, *, requested,
                reply_to, active_text, position):
        entry = self.entries.get(key)
        if entry and entry.get('turn') != turn and entry.get('message_id'):
            # Repost once for a new human request, rather than editing above it.
            self.retire(key, entry)
            entry = self.entries.get(key)
            if entry:
                return  # Cleanup must finish before another message is sent.
        if entry and entry.get('turn') != turn and not entry.get('message_id'):
            entry = None  # A new native turn does not replay the old status send.
        if entry and entry.get('message_id') and position is not None and entry.get('position') != position:
            # Telegram edits do not move a card below later messages. Retire it
            # on a new exchange; automatic replacement still honors quiet grace.
            self.retire(key, entry)
            entry = self.entries.get(key)
            if entry:
                return
        if entry and entry.get('requested') and active_text:
            # Explicit status stays live during the automatic silence grace.
            text = active_text
        if text is None:
            if entry:
                self.retire(key, entry)
            return
        if entry is None:
            entry = {'chat_id': chat, 'topic': topic, 'turn': turn, 'attempted': False}
            self.entries[key] = entry
        if position is not None:
            entry['position'] = dict(position)
        if requested:
            entry['requested'] = True
            self.save()
        warning = text.startswith('⚠️')
        if entry.get('unconfirmed', False) != warning:
            entry['next_update'] = 0
        entry['unconfirmed'] = warning
        if now < entry.get('retry_after_until', 0):
            return
        if not requested and now < entry.get('next_update', 0):
            return
        entry['next_update'] = now + 30
        payload = {'chat_id': chat, 'text': html.escape(text), 'parse_mode': 'HTML'}
        message_id = entry.get('message_id')
        if message_id:
            entry['turn'] = turn
            payload['message_id'] = message_id
            result = self.telegram.call('editMessageText', payload)
        elif not entry['attempted']:
            entry['attempted'] = True
            self.save()  # Persist before crossing the Telegram boundary.
            payload['disable_notification'] = True
            if reply_to is not None:
                payload['reply_parameters'] = {'message_id': reply_to}
            if topic:
                payload['message_thread_id'] = topic
            result = self.telegram.call('sendMessage', payload)
            if result and result.get('ok'):
                message = result.get('result', {})
                if message.get('chat', {}).get('id') == chat and message.get('message_thread_id', 0) == topic:
                    entry['message_id'] = message['message_id']
                    entry['turn'] = turn
                else:
                    entry['binding_error'] = True
        else:
            return  # An ambiguous send is never automatically duplicated.
        if result and not result.get('ok'):
            code = result.get('error_code')
            if code == 429:
                entry['next_update'] = now + max(30, result.get('parameters', {}).get('retry_after', 30))
                entry['retry_after_until'] = entry['next_update']
                if not message_id:
                    entry['attempted'] = False  # Explicitly rejected, not ambiguous.
            elif message_id and code == 400 and 'message to edit not found' in result.get('description', '').lower():
                entry.pop('message_id')
                entry['attempted'] = False
        self.save()

    def retire(self, key, entry):
        with self.lock:
            current = self.entries.get(key)
            if current and current.get('turn') == entry.get('turn'):
                self._retire(key, current)

    def _retire(self, key, entry):
        if time.time() < entry.get('cleanup_retry', 0):
            return
        entry['cleanup_retry'] = time.time() + 30
        self.save()
        message_id = entry.get('message_id')
        outcome = 'no_receipt'
        if message_id:
            payload = {'chat_id': entry['chat_id'], 'message_id': message_id}
            result = self.telegram.call('deleteMessage', payload)
            absent = result and result.get('error_code') == 400 and 'message to delete not found' in result.get('description', '').lower()
            outcome = 'already_absent' if absent else 'deleted'
            if not (result and result.get('ok') and result.get('result') is True) and not absent:
                # Keep failed cleanup tracked across restart; never leave "Working".
                payload.update(text='◻️ Activity monitoring ended', parse_mode='HTML')
                result = self.telegram.call('editMessageText', payload)
                if not result or not result.get('ok'):
                    return
                outcome = 'marked_inactive'
        receipt_path = self.path.with_name('retirements.json')
        receipts = json.loads(receipt_path.read_text()) if receipt_path.exists() else {}
        receipts[key] = {'message_id': message_id, 'outcome': outcome, 'at': time.time()}
        receipts = dict(sorted(receipts.items(), key=lambda item: item[1]['at'])[-32:])
        atomic_json(receipt_path, receipts)
        self.entries.pop(key, None)
        self.save()


class StatusRequests:
    """Supplement explicit /status commands already handled by the sole bridge."""
    def __init__(self, database, telegram, path):
        self.database, self.telegram, self.path = database, telegram, path
        self.state = json.loads(path.read_text()) if path.exists() else {
            'floor': database.execute('SELECT COALESCE(MAX(update_id),0) FROM incoming_updates').fetchone()[0],
            'attempted': {}}
        atomic_json(self.path, self.state)

    def respond(self, reports, weekly, bindings=None, contexts=None, publisher=None):
        recent = self.database.execute("SELECT MIN(update_id) FROM incoming_updates WHERE datetime(updated_at)>=datetime('now','-120 seconds')").fetchone()[0]
        if recent is not None:
            self.state['floor'] = max(self.state['floor'], recent - 1)
        # Project only command routing metadata. No prompt or other input is read.
        rows = self.database.execute('''SELECT i.update_id,
            json_extract(i.payload_json,'$.message.chat.id') AS chat,
            COALESCE(json_extract(i.payload_json,'$.message.message_thread_id'),0) AS topic,
            json_extract(i.payload_json,'$.message.message_id') AS message
            FROM incoming_updates i JOIN users u
              ON u.tg_user_id=json_extract(i.payload_json,'$.message.from.id')
            WHERE i.update_id>? AND i.status='handled' AND u.allowed=1
              AND datetime(i.updated_at)>=datetime('now','-120 seconds')
              AND trim(json_extract(i.payload_json,'$.message.text'))='/status'
            ORDER BY i.update_id LIMIT 256''', (self.state['floor'],)).fetchall()
        for row in rows:
            identity = str(row['update_id'])
            if identity in self.state['attempted']:
                continue
            key = f"{row['chat']}:{row['topic']}"
            if key not in reports:
                continue  # Requires an authorized, current native topic binding.
            if bindings is not None:
                current = self.database.execute('''SELECT s.codex_thread_id FROM sessions s
                    JOIN users u ON u.tg_user_id=s.creator_user_id
                    WHERE s.chat_id=? AND s.thread_id=? AND u.allowed=1''',
                    (row['chat'], row['topic'])).fetchone()
                if not current or current['codex_thread_id'] != bindings.get(key):
                    continue  # A cached report cannot follow a replaced binding.
            self.state['attempted'][identity] = {'attempted': True}
            if len(self.state['attempted']) > 256:
                oldest = min(self.state['attempted'], key=int)
                self.state['floor'] = max(self.state['floor'], int(oldest))
                del self.state['attempted'][oldest]
            atomic_json(self.path, self.state)  # Never replay an ambiguous delivery.
            text = reports[key] + ' · ' + weekly()
            context = (contexts or {}).get(key, {})
            if publisher is not None and context.get('active'):
                position = dict(context.get('position', {}))
                latest = latest_input(self.database, row['chat'], row['topic'])
                position['input_id'] = latest['message_id'] if latest else row['message']
                message_id = publisher.update(key, row['chat'], row['topic'],
                    context['turn'], text, time.time(), requested=True,
                    reply_to=row['message'], position=position if context.get('position') else None)
                if message_id is not None:
                    self.state['attempted'][identity]['message_id'] = message_id
                    atomic_json(self.path, self.state)
                continue
            payload = {'chat_id': row['chat'], 'text': html.escape(text),
                       'parse_mode': 'HTML', 'disable_notification': True,
                       'reply_parameters': {'message_id': row['message']}}
            if row['topic']:
                payload['message_thread_id'] = row['topic']
            result = self.telegram.call('sendMessage', payload)
            if result and result.get('ok'):
                message = result.get('result', {})
                if message.get('chat', {}).get('id') == row['chat'] and message.get('message_thread_id', 0) == row['topic']:
                    self.state['attempted'][identity]['message_id'] = message.get('message_id')
                    atomic_json(self.path, self.state)


class StatusCache:
    """Replace complete snapshots atomically; command reads never wait on native I/O."""
    def __init__(self):
        self.value = None

    def update(self, reports, bindings, weekly, contexts=None):
        value = (dict(reports), dict(bindings), weekly.available, weekly.resets_at, time.time())
        if contexts is not None:
            value += ({key: dict(context) for key, context in contexts.items()},)
        self.value = value

    def respond(self, requests, publisher=None):
        value = self.value
        if value is None:
            return
        reports, bindings, available, resets_at, observed = value[:5]
        contexts = value[5] if len(value) > 5 else None
        if time.time() - observed > 20:
            reports = {key: '⚠️ Native activity unavailable · state unconfirmed' for key in reports}
            available = resets_at = None
            contexts = None
        requests.respond(reports, lambda: weekly_label(available, resets_at), bindings,
                         contexts, publisher)


def watch_status_requests(config, state, cache, stopped, telegram, publisher=None):
    database = readonly(config['db_path'])
    try:
        requests = StatusRequests(database, telegram, state / 'status-requests.json')
        version, snapshot = None, None
        while not stopped.is_set():
            try:
                current = database.execute('PRAGMA data_version').fetchone()[0]
                value = cache.value
                if current != version or value is not snapshot:
                    cache.respond(requests, publisher)
                    version, snapshot = current, value
            except (OSError, ValueError, sqlite3.Error):
                pass  # Leave persisted attempts intact and retry observation, never delivery.
            stopped.wait(.25)
    finally:
        database.close()


def latest_input(database, chat, topic):
    """Telegram commands are exchanges even when they start no native model turn."""
    return database.execute('''SELECT json_extract(i.payload_json,'$.message.message_id') AS message_id,
        COALESCE(json_extract(i.payload_json,'$.message.date'),unixepoch(i.updated_at)) AS at
        FROM incoming_updates i JOIN users u
          ON u.tg_user_id=json_extract(i.payload_json,'$.message.from.id')
        WHERE u.allowed=1 AND json_extract(i.payload_json,'$.message.chat.id')=?
          AND COALESCE(json_extract(i.payload_json,'$.message.message_thread_id'),0)=?
          AND json_extract(i.payload_json,'$.message.message_id') IS NOT NULL
        ORDER BY i.update_id DESC LIMIT 1''',
        (chat, topic)).fetchone()


def run(args):
    config = tomllib.loads(pathlib.Path(args.config).read_text())
    state = pathlib.Path(args.state_dir)
    state.mkdir(parents=True, exist_ok=True, mode=0o700)
    if state.stat().st_uid != os.getuid() or state.stat().st_mode & 0o077:
        raise ValueError('activity state directory must be owner-only')
    lock = open(state / 'observer.lock', 'a', opener=lambda p, f: os.open(p, f, 0o600))
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    bridge = readonly(config['db_path'])
    native_home = pathlib.Path(os.environ.get('CODEX_HOME', pathlib.Path.home() / '.codex'))
    native_db = readonly(native_home / 'state_5.sqlite')
    publisher = None if args.probe else Publisher(Telegram(config['telegram']), state / 'messages.json')
    stopped = threading.Event()
    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, lambda *_: stopped.set())
    readers, native = {}, None
    weekly = WeeklyLimit()
    cache = StatusCache()
    status_worker = None
    if publisher and args.status_requests:
        status_worker = threading.Thread(target=watch_status_requests,
            args=(config, state, cache, stopped, publisher.telegram, publisher), daemon=True)
        status_worker.start()
    try:
        while not stopped.is_set():
            observations = []
            reports = {}
            bindings = {}
            contexts = {}
            publications = []
            try:
                if native is None:
                    native = Native(str(config['codex'].get('binary', 'codex')))
                weekly.read(native, time.monotonic())
                topics = bridge.execute('''SELECT s.chat_id,s.thread_id,s.codex_thread_id,
                    (SELECT status FROM turns WHERE session_id=s.id ORDER BY id DESC LIMIT 1) AS turn_status
                    FROM sessions s JOIN users u ON u.tg_user_id=s.creator_user_id
                    WHERE s.codex_thread_id IS NOT NULL AND u.allowed=1''').fetchall()
                known = set()
                for topic in topics:
                    tid = topic['codex_thread_id']
                    key = f"{topic['chat_id']}:{topic['thread_id']}"
                    known.add(key)
                    row = native_db.execute('SELECT rollout_path FROM threads WHERE id=? AND archived=0', (tid,)).fetchone()
                    if not row:
                        entry = publisher.snapshot().get(key) if publisher else None
                        if entry:
                            publisher.retire(key, entry)
                        continue
                    path = pathlib.Path(row['rollout_path']).resolve()
                    if not path.is_relative_to((native_home / 'sessions').resolve()):
                        raise ValueError('rollout outside native session directory')
                    reader = readers.setdefault(path, Rollout(path, tid))
                    if reader.thread_id != tid:
                        raise ValueError('cached rollout binding mismatch')
                    try:
                        reader.refresh()
                    except (OSError, ValueError):
                        if publisher and key in publisher.snapshot():
                            publisher.update(key, topic['chat_id'], topic['thread_id'], reader.turn_id,
                                '⚠️ Native activity unavailable · rollout unconfirmed', time.time())
                        continue
                    status = native.status(tid)
                    children = [native.status(child) for child in sorted(reader.agents)[:32]
                                if linked_child(native_db, child, tid)]
                    now = time.time()
                    report = status_report(status, reader, children, now)
                    active_text = status_line(status, reader, children, now)
                    text = status_line(status, reader, children, now, automatic=True)
                    update = pending_update_line(args.pending_update_record, key, now)
                    report, active_text, text = combine_update_status(report, active_text, text, update)
                    exchange = timestamp(reader.last_exchange)
                    if update and exchange is not None and now - exchange < 30:
                        text = None
                    latest = latest_input(bridge, topic['chat_id'], topic['thread_id'])
                    position = {'exchange': reader.last_exchange,
                                'input_id': latest['message_id'] if latest else None}
                    reports[key] = report
                    bindings[key] = tid
                    contexts[key] = {'turn': reader.request_id or reader.turn_id,
                                     'active': bool(active_text), 'position': position}
                    if status.get('type') in ('unknown', 'systemError') and publisher and key in publisher.snapshot():
                        text = '⚠️ Native activity unavailable · state unconfirmed'
                    if text is None and status.get('type') == 'idle' and topic['turn_status'] == 'running':
                        text = '⏳ Finishing delivery · native work ended'
                    if latest and latest['at'] is not None and now - latest['at'] < 30:
                        text = None
                    # Fast turns stay clean; existing active turns qualify immediately.
                    started = timestamp(reader.started)
                    if text and started is not None and now - started < 30:
                        text = None
                    if text:
                        text += ' · ' + weekly.label(native, time.monotonic())
                    if active_text:
                        active_text += ' · ' + weekly.label(native, time.monotonic())
                    observations.append({'status': status.get('type'), 'visible': bool(text)})
                    if publisher:
                        publications.append((key, topic['chat_id'], topic['thread_id'],
                                             reader.request_id or reader.turn_id, text, now,
                                             active_text, position))
                # Publish the native snapshot before automatic Telegram delivery can wait.
                cache.update(reports, bindings, weekly, contexts)
                if publisher:
                    for publication in publications:
                        publisher.update(*publication[:6], active_text=publication[6],
                                         position=publication[7])
                    for key, entry in publisher.snapshot().items():
                        if key not in known:
                            publisher.retire(key, entry)
                atomic_json(state / 'health.json', {'updated_at': time.time(), 'connected': True,
                            'topics': len(observations), 'active': sum(x['visible'] for x in observations),
                            'weekly_available': weekly.available,
                            'weekly_resets_at': weekly.resets_at,
                            'status_requests_alive': status_worker.is_alive() if status_worker else False,
                            'published': sum(bool(x.get('message_id')) for x in publisher.snapshot().values()) if publisher else 0})
                if args.probe:
                    print(json.dumps({'connected': True, 'topics': observations, 'weekly_available': weekly.available}))
                    return
            except (OSError, ValueError, sqlite3.Error, queue.Empty, TimeoutError):
                if native:
                    native.close()
                native = None
                # Keep routing, but never answer with an old live claim after disconnect.
                if cache.value is not None:
                    previous_reports, previous_bindings, *_ = cache.value
                    unavailable = {key: '⚠️ Native connection unavailable · state unconfirmed'
                                   for key in previous_reports}
                    weekly.available = weekly.resets_at = None
                    cache.update(unavailable, previous_bindings, weekly)
                if publisher:
                    for key, entry in publisher.snapshot().items():
                        publisher.update(key, entry['chat_id'], entry['topic'], entry['turn'],
                                         '⚠️ Native connection unavailable · activity unconfirmed', time.time())
                atomic_json(state / 'health.json', {'updated_at': time.time(), 'connected': False})
                if args.probe:
                    raise RuntimeError('native activity probe unavailable') from None
            stopped.wait(8)
    finally:
        stopped.set()
        if status_worker:
            status_worker.join(timeout=2)
        if native:
            native.close()
        if publisher:
            for key, entry in publisher.snapshot().items():
                publisher.retire(key, entry)
        bridge.close()
        native_db.close()
        lock.close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True)
    parser.add_argument('--state-dir', required=True)
    parser.add_argument('--pending-update-record', help='owner-only heartbeat receipt for an explicitly configured finite bot updater')
    parser.add_argument('--probe', action='store_true', help='read-only; no token or Telegram calls')
    parser.add_argument('--status-requests', action='store_true',
                        help='supplement newly handled /status commands with activity and weekly quota')
    try:
        run(parser.parse_args())
    except Exception as error:
        # Exception strings can contain private filenames or credential-bearing URLs.
        raise SystemExit(f'activity observer failed ({type(error).__name__})') from None
