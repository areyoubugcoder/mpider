"""多公众号观察契约回归；全部身份与模板为虚构数据。"""
import json
import unittest
from discovery_worker import DiscoveryMessages, MaterialCache


def request(callback, user, action=0, offset=None):
    return dict(phase='enter', slot=0, text=json.dumps(dict(func='H5ExtTransfer', __callback_id=callback,
        params=dict(cgi_cmdid=5814, req_json=dict(BizUserName=user, ActionType=action, Offset=offset)))))


def response(callback, user):
    params = {'err_msg': 'H5ExtTransfer:ok', 'jsapi_resp': {'resp_json': {'AccountInfo': {'UserName': user}, 'MsgList': {}}}}
    return {'phase': 'frame', 'text': json.dumps({'__msg_type': 'callback', '__callback_id': callback, '__params': params})}


class DiscoveryTests(unittest.TestCase):
    def messages(self):
        events = []
        return DiscoveryMessages('fake', lambda event, **kw: events.append((event, kw))), events

    def test_callback_reuse_across_accounts_is_disambiguated_by_identity(self):
        m, events = self.messages()
        m.accept(request(1, 'fake-a'))
        m.accept(request(1, 'fake-b'))
        m.accept(response(1, 'fake-b'))
        m.accept(response(1, 'fake-a'))
        homes = [v for k, v in events if k == 'home']
        self.assertEqual([h['user_name'] for h in homes], ['fake-b', 'fake-a'])
        self.assertNotEqual(homes[0]['request_id'], homes[1]['request_id'])
        self.assertFalse(m.pending)

    def test_same_user_ambiguous_response_never_chooses_most_recent(self):
        m, events = self.messages()
        m.accept(request(1, 'fake-a'))
        m.accept(request(1, 'fake-a'))
        m.accept(response(1, 'fake-a'))
        self.assertFalse(any(k == 'home' for k, _ in events))
        self.assertEqual(events[-1][1]['reason'], 'ambiguous_callback')

    def test_pagination_and_unknown_user_do_not_become_home(self):
        m, events = self.messages()
        m.accept(request(1, 'fake-a', 1, 'not-a-real-cursor'))
        m.accept(response(1, 'fake-other'))
        m.accept(response(1, 'fake-a'))
        self.assertFalse(any(k == 'home' for k, _ in events))
        self.assertEqual(sum(k == 'observed_request' for k, _ in events), 1)

    def test_bounded_materials_continue_after_many_accounts(self):
        cache = MaterialCache()
        for i in range(60):
            cache.accept(dict(event='session_field', name='psk', hex=str(i)))
            cache.accept(dict(event='session_candidate', hex=str(i)))
        self.assertEqual(list(cache.fields['psk']), list(map(str, range(52, 60))))
        self.assertEqual(len(cache.candidates), 16)
        for name in ['post_body', 'inner_enc_key', 'inner_dec_key']:
            cache.accept(dict(event='session_field', name=name, hex='fake'))
        attempts = []
        def derive(fields, candidates, ts):
            attempts.extend(candidates)
            return dict(BizUserName='fake-target' if candidates == ['58'] else 'fake-other', ActionType=0)
        result = cache.derive('fake-target', derive, lambda p: p)
        self.assertEqual(result['BizUserName'], 'fake-target')
        self.assertEqual(attempts, ['59', '58'])

    def test_old_matching_candidate_cannot_prepare_a_new_home_session(self):
        import time
        cache = MaterialCache()
        for name in ['psk','post_body','inner_enc_key','inner_dec_key']:
            cache.accept(dict(event='session_field', name=name, hex='fake'))
        cache.accept(dict(event='session_candidate', hex='fake-old'))
        attempts = []
        result = cache.derive('fake-target', lambda *args: attempts.append(args), lambda p:p, time.time()+1)
        self.assertIsNone(result)
        self.assertFalse(attempts)

    def test_matching_account_pagination_template_is_not_home_session(self):
        cache = MaterialCache()
        for name in ['psk','post_body','inner_enc_key','inner_dec_key']:
            cache.accept(dict(event='session_field', name=name, hex='fake'))
        cache.accept(dict(event='session_candidate', hex='fake'))
        self.assertIsNone(cache.derive('fake-target',lambda *args: dict(BizUserName='fake-target',ActionType=1,Offset='fake'),lambda p:p))


if __name__ == '__main__':
    unittest.main()
