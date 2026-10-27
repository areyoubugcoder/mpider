"""桥消息筛选离线契约测试；不导入 Frida，不连接微信。"""
import contextlib
import io
import json
import unittest
from worker import Messages,emit,select_profile

class Profiles(unittest.TestCase):
    def test_single_and_manifest_choose_exact_hash(self):
        old={"module_sha256":"fake-old","wechat_version":"fake-old"}
        new={"module_sha256":"fake-new","wechat_version":"fake-new"}
        self.assertEqual(select_profile(old,"fake-old"),old)
        self.assertEqual(select_profile({"profiles":[old,new]},"fake-new"),new)
    def test_unknown_and_ambiguous_hash_fail_closed(self):
        item={"module_sha256":"fake-known"}
        self.assertIsNone(select_profile({"profiles":[item]},"fake-unknown"))
        self.assertIsNone(select_profile({"profiles":[item,item]},"fake-known"))

class Filtering(unittest.TestCase):
    def setUp(self):
        self.messages=Messages("fake-session","0x10","虚构公众号",None)
        self.events=[]
    def accept(self,value,phase="enter",slot=0):
        out=io.StringIO()
        with contextlib.redirect_stdout(out):self.messages.accept({"text":json.dumps(value),"phase":phase,"slot":slot})
        self.events.extend(json.loads(line) for line in out.getvalue().splitlines())
    def request(self,hwnd="0x10",callback=1,cgi=5814):
        return {"__msg_type":"call","__callback_id":callback,"func":"H5ExtTransfer","params":{"hwnd":hwnd,"cgi_cmdid":cgi,"req_json":json.dumps({"BizUserName":"fake-account","ActionType":0,"Offset":""})}}
    def response(self,params,callback=1):
        self.accept({"__msg_type":"callback","__callback_id":callback,"__params":params},"frame",-1)
    def test_one_canonical_slot_and_target_window(self):
        self.accept(self.request(),slot=3);self.accept(self.request(hwnd="0x20"));self.assertEqual(self.events,[])
        self.accept(self.request());self.assertEqual(len(self.events),1)
    def test_reused_id_for_other_methods_does_not_consume_request(self):
        self.accept(self.request());self.response({"err_msg":"system:function_not_implement"});self.assertEqual(len(self.events),1)
        self.response({"err_msg":"H5ExtTransfer:ok","base_resp":{"ret":0},"jsapi_resp":{"ret":0,"resp_json":json.dumps({"BaseResponse":{"Ret":0},"AccountInfo":{"UserName":"fake-account","NickName":"虚构公众号"},"MsgList":{"Msg":[]}})}})
        self.assertEqual([e['event'] for e in self.events],["request","response"])
    def test_native_error_is_recorded_and_not_treated_as_page(self):
        self.accept(self.request());self.response({"err_msg":"H5ExtTransfer:fail","base_resp":{"ret":0},"jsapi_resp":{"ret":1,"resp_json":'{"BaseResponse":{"Ret":7}}'}})
        self.assertEqual(self.events[-1]['event'],'response')
    def test_name_mismatch_is_fault(self):
        self.accept(self.request());self.response({"err_msg":"H5ExtTransfer:ok","jsapi_resp":{"resp_json":{"AccountInfo":{"UserName":"fake-account","NickName":"另一个号"},"MsgList":{}}}})
        self.assertEqual(self.events[-1],{"event":"fault","reason":"target_name_mismatch"})
    def test_auto_name_only_comes_from_uniquely_correlated_response(self):
        self.messages.title=""
        params={"err_msg":"H5ExtTransfer:ok","base_resp":{"ret":0},"jsapi_resp":{"ret":0,"resp_json":{"BaseResponse":{"Ret":0},"AccountInfo":{"UserName":"other-account","NickName":"其它公众号"},"MsgList":{"Msg":[]}}}}
        self.response(params)
        self.assertEqual(self.messages.title,"")
        self.accept(self.request())
        self.response(params)
        self.assertEqual(self.messages.title,"")
        self.assertEqual(len(self.events),1)
        params["jsapi_resp"]["resp_json"]["AccountInfo"]={"UserName":"fake-account","NickName":"自动识别的公众号"}
        self.response(params)
        self.assertEqual(self.messages.title,"自动识别的公众号")
        self.assertEqual(self.events[-1]["event"],"response")
        self.accept(self.request(callback=2))
        params["jsapi_resp"]["resp_json"]["AccountInfo"]["NickName"]="意外变更的名称"
        self.response(params,callback=2)
        self.assertEqual(self.events[-1]["reason"],"target_name_mismatch")
    def test_ambiguous_response_cannot_bind_auto_name(self):
        self.messages.title=""
        self.accept(self.request());self.accept(self.request())
        self.response({"err_msg":"H5ExtTransfer:ok","jsapi_resp":{"resp_json":{"AccountInfo":{"UserName":"fake-account","NickName":"虚构公众号"},"MsgList":{}}}})
        self.assertEqual(self.messages.title,"")
        self.assertEqual(self.events[-1]["reason"],"ambiguous_callback")
    def test_unrelated_h5_payload_with_same_id_keeps_request(self):
        self.accept(self.request())
        self.response({"err_msg":"H5ExtTransfer:ok","jsapi_resp":{"ret":0,"resp_json":{"BaseResponse":{"Ret":0},"live_info":{}}}})
        self.assertEqual(len(self.events),1)
    def test_ambiguous_callback_stops_instead_of_guessing(self):
        self.accept(self.request());self.accept(self.request())
        self.response({"err_msg":"H5ExtTransfer:ok","jsapi_resp":{"resp_json":{"AccountInfo":{"UserName":"fake-account","NickName":"虚构公众号"},"MsgList":{}}}})
        self.assertEqual(self.events[-1]["reason"],"ambiguous_callback")
    def test_auxiliary_request_does_not_become_paging(self):
        self.messages.biz="FAKE"
        req=self.request(cgi=26365);req["params"]["req_json"]=json.dumps({"ReqInfoList":[{"Url":"https://mp.weixin.qq.com/s?__biz=FAKE&mid=1&idx=1"}]})
        self.accept(req)
        self.response({"err_msg":"H5ExtTransfer:ok","jsapi_resp":{"resp_json":{"BaseResponse":{"Ret":0},"AppMsgContextList":[]}}})
        self.assertEqual(self.events[0]["request"]["cgi"],26365)
        self.assertIsNone(self.events[0]["request"]["action"])
        self.assertEqual(self.events[-1]["event"],"response")
    def test_transport_is_ascii_even_with_emoji_and_surrogates(self):
        out=io.StringIO()
        with contextlib.redirect_stdout(out):emit("response",payload={"title":"文章😀\ud800"})
        out.getvalue().encode("ascii")
        self.assertEqual(json.loads(out.getvalue())["payload"]["title"],"文章😀\ud800")
if __name__=='__main__':unittest.main()
