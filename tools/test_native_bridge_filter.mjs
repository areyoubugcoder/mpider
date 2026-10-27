// 验证列表回调关联：空页、错误也留证，其它 JSAPI 不进入原始采样。
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

function makeFilter() {
  const context = vm.createContext({Process: {getModuleByName: () => ({})}, rpc: {exports: {}}});
  vm.runInContext(readFileSync(new URL('./native_bridge_probe.js', import.meta.url), 'utf8'), context);
  return message => context.publicList(JSON.stringify(message));
}

test('列表请求后能关联错误和空响应', () => {
  const filter = makeFilter();
  assert.equal(filter({__msg_type: 'call', __callback_id: 'fake-list-id', func: 'H5ExtTransfer',
    params: {cgi_cmdid: 5814}}), true);
  assert.equal(filter({__msg_type: 'callback', __callback_id: 'fake-list-id',
    __params: {err_msg: 'H5ExtTransfer:fail'}}), true);
  assert.equal(filter({__msg_type: 'callback', __callback_id: 'fake-list-id',
    __params: {err_msg: 'H5ExtTransfer:ok', jsapi_resp: {resp_json: '{}'}}}), true);
});

test('其它 JSAPI 及其回调不采样', () => {
  const filter = makeFilter();
  assert.equal(filter({__msg_type: 'call', __callback_id: 'fake-other-id', func: 'other', params: {}}), false);
  assert.equal(filter({__msg_type: 'callback', __callback_id: 'fake-other-id', params: {value: 'fake'}}), false);
});

test('保留未关联但带公众号列表特征的日志', () => {
  const filter = makeFilter();
  assert.equal(filter({func: 'log', params: {msg: 'oriMsgList 与 AppMsg 字段示例'}}), true);
});

test('页面重建后其它方法复用编号时不采样', () => {
  const filter = makeFilter();
  filter({__msg_type: 'call', __callback_id: 'fake-reused-id', func: 'H5ExtTransfer',
    params: {cgi_cmdid: 5814}});
  assert.equal(filter({__msg_type: 'callback', __callback_id: 'fake-reused-id',
    __params: {err_msg: 'GetSubscriptionOption:ok'}}), false);
});
