'use strict';
// 只读观察当前版本的 JSSDK 桥及回调，不改写参数、不调用原生函数。
const wx = Process.getModuleByName('Weixin.dll');
const listeners = new Map();
const listCallbacks = new Set();
let sequence = 0;
const counts = {calls: 0, matched: 0, byHook: {}};

function readStdString(address) {
  try {
    const length = address.add(0x10).readU64().toNumber();
    const capacity = address.add(0x18).readU64().toNumber();
    if (length < 8 || length > 2000000 || capacity < length || capacity > 16000000) return null;
    const data = capacity < 16 ? address : address.readPointer();
    return data.readUtf8String(length);
  } catch (_) { return null; }
}

function publicList(text) {
  if (!text) return false;
  // 请求与回调通过 ID 关联，空响应和错误响应也需要留证，避免把漏采误判为成功。
  try {
    const message = JSON.parse(text);
    const id = message.__callback_id;
    const params = message.params || {};
    if (message.__msg_type === 'call' && ['H5ExtTransfer', 'finderH5ExtTransfer'].includes(message.func) &&
        [5814, 26365].includes(Number(params.cgi_cmdid)) && id !== undefined && id !== null) {
      listCallbacks.add(String(id));
      if (listCallbacks.size > 128) listCallbacks.delete(listCallbacks.values().next().value);
      return true;
    }
    if (message.__msg_type === 'callback' && listCallbacks.has(String(id))) {
      const response = message.__params || message.params || {};
      // 实况中页面重建会重用回调编号，同时检查方法名，排除其它 JSAPI 的同号回调。
      if (/^(?:finder)?H5ExtTransfer:/.test(response.err_msg || '') ||
          /^system:/.test(response.err_msg || '')) return true;
    }
  } catch (_) { /* 部分日志仅包含列表片段，交给特征过滤。 */ }
  return text.includes('bizprofilev2h5') || text.includes('AppMsgContextList') ||
    (text.includes('MsgList') && text.includes('AppMsg')) ||
    (text.includes('batchgetappmsgh5') && text.includes('ReqInfoList'));
}

function emit(text, meta) {
  if (!publicList(text)) return;
  counts.matched++;
  send({event: 'bridge', sequence: ++sequence, ...meta, text});
}

rpc.exports = {
  inventory() {
    return {pid: Process.id, arch: Process.arch, module: wx.name,
      module_path: wx.path, module_size: wx.size, version: Frida.version,
      modules: Process.enumerateModules().filter(m => /Weixin|xweb|v8|WeChatAppEx/i.test(m.name))
        .map(m => ({name: m.name, size: m.size}))};
  },
  configure(config) {
    if (config.single_request) throw new Error('当前探针只采样，不执行列表请求');
    for (const item of config.hooks || []) {
      if (listeners.has(item.rva)) continue;
      const offset = Number(item.rva);
      if (!Number.isSafeInteger(offset) || offset < 0x1000 || offset >= wx.size) {
        throw new Error('函数偏移不在模块内');
      }
      const address = wx.base.add(offset);
      const range = Process.findRangeByAddress(address);
      if (!range || !range.protection.includes('x')) throw new Error('候选采样点不在可执行内存中');
      if (typeof item.expected !== 'string' || !/^[0-9a-f]{32}$/i.test(item.expected)) {
        throw new Error('每个采样点必须提供离线样本的前 16 字节');
      }
      const bytes = new Uint8Array(address.readByteArray(16));
      const actual = Array.from(bytes, b => b.toString(16).padStart(2, '0')).join('');
      if (actual !== item.expected.toLowerCase()) throw new Error('函数字节与离线样本不匹配');
      const instruction = Instruction.parse(address);
      if (['int3', 'ret'].includes(instruction.mnemonic)) throw new Error('候选采样点不是有效指令');
      const callbacks = {
        onEnter(args) {
          counts.calls++;
          counts.byHook[item.rva] = (counts.byHook[item.rva] || 0) + 1;
          this.outputs = [];
          if (item.frame_string) {
            emit(readStdString(this.context.rbp.add(item.frame_string)),
              {rva: item.rva, phase: 'frame', slot: -1, thread: this.threadId});
            return;
          }
          for (let slot = 0; slot < 4; slot++) {
            this.outputs.push(args[slot]);
            emit(readStdString(args[slot]), {rva: item.rva, phase: 'enter', slot,
              thread: Process.getCurrentThreadId()});
          }
        },
        onLeave(_) {
          for (let slot = 0; slot < this.outputs.length; slot++) {
            emit(readStdString(this.outputs[slot]), {rva: item.rva, phase: 'leave', slot,
              thread: Process.getCurrentThreadId()});
          }
        }
      };
      if (item.frame_string) delete callbacks.onLeave;
      const handle = Interceptor.attach(address, callbacks);
      listeners.set(item.rva, handle);
    }
    return {hooks: Array.from(listeners.keys())};
  },
  status() { return {...counts, hooks: Array.from(listeners.keys())}; },
  stop() {
    for (const handle of listeners.values()) handle.detach();
    listeners.clear();
    return counts;
  }
};
