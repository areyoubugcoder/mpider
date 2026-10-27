"""持续观察用户打开的公众号主页；不包含窗口控制或主动网络请求。"""
import argparse
import hashlib
import json
import queue
import sys
import threading
import time
from collections import deque
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(sys.executable).parent / 'session-deps'))
from worker import emit, select_profile


class DiscoveryMessages:
    """按回调和响应身份关联多个公众号；相同身份并发有歧义时拒绝猜测。"""
    def __init__(self, run_id, output=emit):
        self.run_id, self.output = run_id, output
        self.sequence = 0
        self.pending = deque(maxlen=256)

    def accept(self, observation):
        try:
            value = json.loads(observation.get('text', ''))
            if not isinstance(value, dict):
                return
            if value.get('__callback_id') is None:
                return
            callback = str(value.get('__callback_id'))
            if observation.get('phase') == 'enter' and observation.get('slot') == 0 and value.get('func') in ('H5ExtTransfer', 'finderH5ExtTransfer'):
                params = value.get('params') or {}
                if int(params.get('cgi_cmdid', 0)) != 5814:
                    return
                req = params.get('req_json') or {}
                req = json.loads(req) if isinstance(req, str) else req
                user = req.get('BizUserName')
                if not isinstance(user, str) or not user:
                    return
                self.sequence += 1
                item = dict(request_id=f'watch-{self.run_id}-{self.sequence}', ts=time.time(), user_name=user,
                            home=req.get('ActionType') == 0 and not req.get('Offset'), callback=callback)
                self.pending.append(item)
                self.output('observed_request', request_id=item['request_id'], ts=item['ts'])
            elif observation.get('phase') == 'frame' and value.get('__msg_type') == 'callback':
                params = value.get('__params') or {}
                if not str(params.get('err_msg', '')).startswith(('H5ExtTransfer:', 'finderH5ExtTransfer:')):
                    return
                raw = (params.get('jsapi_resp') or {}).get('resp_json')
                body = json.loads(raw) if isinstance(raw, str) else raw
                if not isinstance(body, dict) or 'MsgList' not in body:
                    return
                user = (body.get('AccountInfo') or {}).get('UserName')
                eligible = [p for p in self.pending if p['callback'] == callback and p['user_name'] == user and time.time()-p['ts'] < 120]
                if len(eligible) != 1:
                    self.output('diagnostic', reason='ambiguous_callback')
                    return
                item = eligible[0]
                self.pending.remove(item)
                if item['home']:
                    self.output('home', request_id=item['request_id'], user_name=user, payload=params, ts=time.time(), request_ts=item['ts'])
        except (ValueError, TypeError, AttributeError):
            self.output('diagnostic', reason='malformed_bridge')


class MaterialCache:
    """仅保留有界的近期材料，逐个候选认证，禁止始终取到旧公众号模板。"""
    def __init__(self):
        self.fields, self.candidates, self.candidate_times = {}, deque(maxlen=16), {}

    def accept(self, value):
        if value.get('event') == 'session_field':
            values = self.fields.setdefault(value['name'], deque(maxlen=8))
            if value['hex'] not in values:
                values.append(value['hex'])
        elif value.get('event') == 'session_candidate' and value['hex'] not in self.candidates:
            self.candidates.append(value['hex'])
            self.candidate_times[value['hex']] = time.time()
            self.candidate_times = {c: self.candidate_times[c] for c in self.candidates}

    def derive(self, user, derive, parameters, request_ts=0, response_ts=float("inf")):
        required = ('psk', 'inner_enc_key', 'inner_dec_key', 'post_body')
        if not self.candidates or not all(self.fields.get(k) for k in required):
            return None
        latest = {k: v[-1] for k, v in self.fields.items()}
        for candidate in reversed(self.candidates):
            if not request_ts <= self.candidate_times[candidate] <= response_ts:
                continue
            payload = None
            for body in reversed(self.fields['post_body']):
                for psk in reversed(self.fields['psk']):
                    try:
                        payload = derive({**latest, 'post_body': body, 'psk': psk,
                                          'inner_enc_keys': list(self.fields['inner_enc_key']),
                                          'inner_dec_keys': list(self.fields['inner_dec_key'])}, [candidate], time.time())
                        break
                    except Exception:
                        continue
                if payload:
                    break
            if payload:
                req = parameters(payload)
                if req.get('BizUserName') == user and req.get('ActionType') == 0 and not req.get('Offset'):
                    return payload
        return None


def attach(args, inbox):
    import frida
    device = frida.get_local_device()
    profiles = json.loads(Path(args.directory, 'profile.json').read_text(encoding='utf-8-sig'))
    source = Path(args.directory, 'probe.js').read_text(encoding='utf8') + '\n' + Path(args.directory, 'session_agent.js').read_text(encoding='utf8')
    for process in device.enumerate_processes():
        if process.name.lower() != 'weixin.exe':
            continue
        session, script, guard, chosen = None, None, None, False
        try:
            session = device.attach(process.pid)
            guard = session.create_script("rpc.exports={module(){const m=Process.findModuleByName('Weixin.dll');return m?{path:m.path}:null;}}")
            guard.load()
            module = guard.exports_sync.module()
            guard.unload()
            guard = None
            profile = select_profile(profiles, hashlib.sha256(Path(module['path']).read_bytes()).hexdigest()) if module else None
            if not profile or 'session_psk' not in profile:
                continue
            emit('phase', phase='attached')
            script = session.create_script(source)
            def receive(message, data):
                try:
                    inbox.put_nowait(message)
                except queue.Full:
                    overflow.set()
            overflow = threading.Event()
            script.on('message', receive)
            session.on('detached', lambda *unused: overflow.set())
            script.load()
            emit('phase', phase='script_loaded')
            script.exports_sync.configure(profile)
            script.exports_sync.session_start({**profile['session_psk'], 'continuous': True})
            script.exports_sync.session_arm()
            emit('phase', phase='session_armed')
            chosen = True
            return session, script, overflow, profile
        except Exception:
            # 其它微信子进程无目标模块或不可附加时，继续检查下一进程。
            continue
        finally:
            if guard:
                guard.unload()
            if not chosen:
                if script:
                    script.unload()
                if session:
                    session.detach()
    raise ValueError('unsupported_or_logged_out')


def run(args):
    from session_protocol import payload_from_capture, request_parameters
    from session_vault import save
    inbox, stop, homes = queue.Queue(maxsize=1024), threading.Event(), deque(maxlen=8)
    def control():
        for line in sys.stdin:
            if json.loads(line).get('action') == 'stop':
                break
        stop.set()
    threading.Thread(target=control, daemon=True).start()
    session, script = None, None
    def output(event, **value):
        if event == 'home':
            # 先交付有效首页；准备材料失败不会吞掉已经识别到的公众号。
            homes.append(value)
        emit(event, **value)
    try:
        session, script, overflow, profile = attach(args, inbox)
        emit('ready')
        messages, cache = DiscoveryMessages(args.run_id, output), MaterialCache()
        last_attempt = 0
        while not stop.is_set():
            if overflow.is_set():
                raise ValueError('probe_disconnected_or_overflow')
            try:
                message = inbox.get(timeout=0.1)
            except queue.Empty:
                message = None
            if message:
                if message.get('type') != 'send':
                    raise ValueError('probe_error')
                value = message['payload']
                cache.accept(value)
                if value.get('event') == 'bridge':
                    messages.accept(value)
            if homes and time.monotonic()-last_attempt >= 0.5:
                last_attempt = time.monotonic()
                for home in list(homes):
                    payload = cache.derive(home['user_name'], payload_from_capture, request_parameters, home['request_ts'], home['ts'])
                    if payload:
                        payload.update(module_sha256=profile['module_sha256'], wechat_version=profile['wechat_version'])
                        raw = home['payload']['jsapi_resp']['resp_json']
                        body = json.loads(raw) if isinstance(raw, str) else raw
                        payload['target_name'] = body.get('AccountInfo', {}).get('NickName', '')
                        directory = Path(args.directory, 'sessions')
                        directory.mkdir(exist_ok=True)
                        path = directory / (home['request_id'] + '.dat')
                        evidence = directory / (home['request_id'] + '-home.dat')
                        save(path, payload)
                        save(evidence, {'body': body})
                        emit('session_ready', **home, session_path=str(path), evidence_path=str(evidence))
                        homes.remove(home)
                    elif time.time()-home['ts'] > 30:
                        emit('diagnostic', reason='session_material_incomplete')
                        homes.remove(home)
    finally:
        if script:
            emit('phase', phase='unload_begin')
            script.unload()
            emit('phase', phase='unload_done')
        if session:
            session.detach()
            emit('phase', phase='detached')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--directory', required=True)
    parser.add_argument('--run-id', required=True)
    args = parser.parse_args()
    try:
        run(args)
    except Exception:
        emit('fault', reason='识别观察器已停止，请检查微信登录状态、适配版本与运行依赖后重新开始')
    finally:
        emit('stopped')
