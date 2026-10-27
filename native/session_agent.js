// 会话观察器：只在已验证模块内观察密码函数和发送缓冲，不调用微信业务函数。
'use strict';
const captured = new Map();
const fieldValues = new Map();
const cryptoContexts = new Map();
const sessionListeners = [];
const candidateSignatures = new Set();
let sessionArmed = false;
let sessionContinuous = false;

function sessionHex(address, length) {
  if (address.isNull() || length < 1 || length > 200000) return null;
  try {
    return Array.from(new Uint8Array(address.readByteArray(length)),
      byte => byte.toString(16).padStart(2, '0')).join('');
  } catch (_) { return null; }
}

function sessionField(name, address, length) {
  let values = fieldValues.get(name);
  if (!values) { values = new Set(); fieldValues.set(name, values); }
  if (values.size >= 8 && !sessionContinuous) return;
  const value = sessionHex(address, length);
  if (value !== null && !values.has(value)) {
    if (values.size >= 8) values.delete(values.values().next().value);
    values.add(value);
    captured.set(name, value);
    send({event: 'session_field', name, hex: value});
  }
}

// 持续观察按完整候选去重；不同公众号可能有完全相同的长度和封装前缀。
function sessionCandidate(hex, length) {
  const signature = sessionContinuous ? hex : `${length}:${hex.slice(0, 64)}`;
  if ((!sessionContinuous && candidateSignatures.size >= 32) || candidateSignatures.has(signature)) return;
  if (candidateSignatures.size >= 32) candidateSignatures.delete(candidateSignatures.values().next().value);
  candidateSignatures.add(signature);
  send({event: 'session_candidate', hex});
}

Object.assign(rpc.exports, {
  sessionStart(config) {
    if (sessionListeners.length) throw new Error('会话观察器已经启动');
    sessionContinuous = config.continuous === true;
    const module = Process.getModuleByName('Weixin.dll');
    const address = module.base.add(config.rva);
    const fingerprint = sessionHex(address, 16);
    const range = Process.findRangeByAddress(address);
    if (fingerprint !== config.expected || !range || !range.protection.includes('x')) {
      throw new Error('会话函数指纹不匹配');
    }
    const structures = new Map(), initializers = new Map(), transforms = new Map();
    for (const [nid, length] of [[0x37f, 16], [0x382, 24], [0x385, 32]]) {
      const pattern = `01 00 00 00 ${length.toString(16)} 00 00 00 0c 00 00 00`;
      for (const protection of ['r--', 'rw-']) {
        for (const area of module.enumerateRanges(protection)) {
          let matches;
          try { matches = Memory.scanSync(area.base, area.size, pattern); }
          catch (_) { continue; }
          for (const match of matches) {
            const base = match.address.sub(4);
            try {
              if (base.readU32() !== nid) continue;
              const init = base.add(24).readPointer(), transform = base.add(32).readPointer();
              if (![init, transform].every(p => p.compare(module.base) >= 0 &&
                  p.compare(module.base.add(module.size)) < 0 &&
                  Process.findRangeByAddress(p)?.protection.includes('x'))) continue;
              structures.set(base.toString(), length);
              initializers.set(init.toString(), init);
              transforms.set(transform.toString(), transform);
            } catch (_) { /* 不完整结构不作为采样点。 */ }
          }
        }
      }
    }
    if (!structures.size) throw new Error('没有找到已验证的密码结构');
    for (const init of initializers.values()) {
      sessionListeners.push(Interceptor.attach(init, {onEnter(args) {
        if (!sessionArmed) return;
        try {
          const length = structures.get(args[0].readPointer().toString());
          if (!length) return;
          const direction = args[3].toInt32();
          if (cryptoContexts.size >= 1024) cryptoContexts.delete(cryptoContexts.keys().next().value);
          cryptoContexts.set(args[0].toString(), {length, direction});
          if (length === 24 && (direction === 0 || direction === 1)) {
            sessionField(direction === 1 ? 'inner_enc_key' : 'inner_dec_key', args[1], 24);
          }
        } catch (_) { /* 其它密码上下文忽略。 */ }
      }}));
    }
    for (const transform of transforms.values()) {
      sessionListeners.push(Interceptor.attach(transform, {onEnter(args) {
        if (!sessionArmed || args[1].isNull()) return;
        const context = cryptoContexts.get(args[0].toString()), length = args[3].toInt32();
        if (!context || context.length !== 16 || context.direction !== 1 || length < 1 || length > 65536) return;
        const head = sessionHex(args[2], Math.min(length, 256));
        if (!head) return;
        if (head.includes('62697a6835657874')) {
          const hex = sessionHex(args[2], length);
          if (hex) sessionCandidate(hex, length);
        } else if (length <= 16) sessionField('alert_pt', args[2], length);
        else if (length <= 48) sessionField('hs_pt', args[2], length);
      }}));
    }
    sessionListeners.push(Interceptor.attach(address, {
      onEnter(args) { this.output = sessionArmed ? args[2] : null; },
      onLeave() {
        if (!sessionArmed || !this.output) return;
        try {
          const result = this.output.readPointer();
          if (result.add(40).readU32() === 32) sessionField('psk', result.add(32).readPointer(), 32);
        } catch (_) { /* 非成功的输出不采样。 */ }
      }
    }));
    sessionListeners.push(Interceptor.attach(Module.getGlobalExportByName('send'), {onEnter(args) {
      if (!sessionArmed) return;
      const length = args[2].toInt32();
      if (length < 64 || length > 200000) return;
      try {
        const head = args[1].readCString(Math.min(length, 512));
        const end = head.indexOf('\r\n\r\n');
        if (!head.startsWith('POST /mmtls/') || end < 0) return;
        const bodyLength = length - end - 4;
        if (bodyLength <= 8192) sessionField('post_body', args[1].add(end + 4), bodyLength);
      } catch (_) { /* 非目标传输忽略。 */ }
    }}));
    return {structures: structures.size};
  },
  sessionArm() { sessionArmed = true; },
  sessionStop() {
    sessionArmed = false;
    for (const listener of sessionListeners.splice(0)) listener.detach();
    captured.clear(); fieldValues.clear(); cryptoContexts.clear(); candidateSignatures.clear();
  }
});
